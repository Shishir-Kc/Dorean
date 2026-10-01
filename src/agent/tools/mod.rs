//! Agent tools: file reads, edits, shell, search.
//!
//! Every tool implements the [`Tool`] trait; a [`ToolRegistry`] holds the
//! set offered to the model and dispatches tool calls by name. Tools resolve
//! relative paths against the [`ToolContext`] working directory and return a
//! plain-text [`ToolOutput`] that is fed back to the model as a `tool`
//! message.

pub mod bash;
pub mod bash_bg;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod list;
pub mod read;
pub mod write;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;

use crate::error::DoreanError;
use crate::permissions::PermissionPolicy;
use crate::providers::client::{ToolCall, ToolSpec};

/// Working directory tools operate in, plus the permission gate.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    permissions: Option<PermissionPolicy>,
}

impl ToolContext {
    /// A context with no permission gate (tests, permissive callers).
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        ToolContext {
            cwd: cwd.into(),
            permissions: None,
        }
    }

    /// Attach a permission policy that gates `write`/`edit`/`bash`.
    pub fn with_permissions(mut self, permissions: PermissionPolicy) -> Self {
        self.permissions = Some(permissions);
        self
    }

    /// The active permission policy, if any.
    pub fn permissions(&self) -> Option<&PermissionPolicy> {
        self.permissions.as_ref()
    }

    /// Resolve a possibly-relative path against the context working
    /// directory.
    pub fn resolve(&self, path: impl AsRef<Path>) -> PathBuf {
        let path = path.as_ref();
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }
}

/// Plain-text result of a tool run, fed back to the model verbatim.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub text: String,
}

impl ToolOutput {
    pub fn new(text: impl Into<String>) -> Self {
        ToolOutput { text: text.into() }
    }
}

/// A tool the model can invoke.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Canonical name used in tool calls (e.g. `read`).
    fn name(&self) -> &str;

    /// Human-readable description of what the tool does.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's input arguments.
    fn schema(&self) -> Value;

    /// Execute the tool with parsed arguments, resolving paths against `ctx`.
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, DoreanError>;

    /// The OpenAI `tools` entry for this tool.
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(self.name(), self.description(), self.schema())
    }
}

/// The full set of tools offered to the model.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    /// The standard tool set.
    pub fn builtin() -> Self {
        Self::builtin_filtered(&[
            "read",
            "write",
            "edit",
            "bash",
            "bash_background",
            "bash_poll",
            "bash_kill",
            "glob",
            "grep",
            "list",
        ])
    }

    /// The standard tool set restricted to `allowed` names (sub-agents).
    pub fn builtin_filtered(allowed: &[&str]) -> Self {
        let bg_store = bash_bg::BackgroundStore::new();
        let all: Vec<Box<dyn Tool>> = vec![
            Box::new(read::ReadTool::new()),
            Box::new(write::WriteTool::new()),
            Box::new(edit::EditTool::new()),
            Box::new(bash::BashTool::new()),
            Box::new(bash_bg::BashBackgroundTool::new(bg_store.clone())),
            Box::new(bash_bg::BashPollTool::new(bg_store.clone())),
            Box::new(bash_bg::BashKillTool::new(bg_store)),
            Box::new(glob::GlobTool::new()),
            Box::new(grep::GrepTool::new()),
            Box::new(list::ListTool::new()),
        ];
        let mut registry = ToolRegistry::default();
        for tool in all {
            if allowed.contains(&tool.name()) {
                registry.tools.push(tool);
            }
        }
        registry
    }

    pub fn register(&mut self, tool: impl Tool + 'static) {
        self.tools.push(Box::new(tool));
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    /// All tool specs, for the `tools` request parameter and prompt docs.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec()).collect()
    }

    /// Execute a completed tool call, returning the text to feed back to the
    /// model. Failures (unknown tool, bad args, denied permission, tool error)
    /// are returned as error text so the model can recover rather than
    /// aborting the loop.
    pub async fn run(&self, ctx: &ToolContext, call: &ToolCall) -> String {
        let Some(tool) = self.get(&call.name) else {
            return format!("Error: unknown tool `{}`", call.name);
        };
        if let Some(policy) = ctx.permissions()
            && let Err(denied) = policy.check_tool(&call.name, &call.arguments, &ctx.cwd)
        {
            return format!("Permission denied: {denied}");
        }
        match tool.run(ctx, call.arguments.clone()).await {
            Ok(output) => output.text,
            Err(e) => format!("Error: tool `{}` failed: {e}", tool.name()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_all_tools() {
        let registry = ToolRegistry::builtin();
        for name in [
            "read",
            "write",
            "edit",
            "bash",
            "bash_background",
            "bash_poll",
            "bash_kill",
            "glob",
            "grep",
            "list",
        ] {
            assert!(registry.get(name).is_some(), "missing tool {name}");
        }
        assert_eq!(registry.specs().len(), 10);
    }

    #[test]
    fn specs_have_openai_shape() {
        let registry = ToolRegistry::builtin();
        let spec = &registry.specs()[0];
        let json = serde_json::to_value(spec).unwrap();
        assert_eq!(json["type"], "function");
        assert!(json["function"]["name"].is_string());
        assert!(json["function"]["description"].is_string());
        assert!(json["function"]["parameters"]["type"].is_string());
    }

    #[tokio::test]
    async fn unknown_tool_becomes_error_text() {
        let registry = ToolRegistry::builtin();
        let ctx = ToolContext::new(std::env::temp_dir());
        let call = ToolCall {
            id: "c".to_string(),
            name: "nope".to_string(),
            arguments: Value::Null,
        };
        let output = registry.run(&ctx, &call).await;
        assert!(output.starts_with("Error: unknown tool"));
    }
}
