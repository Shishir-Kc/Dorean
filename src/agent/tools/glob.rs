//! `glob` tool: file discovery by glob pattern.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolOutput};
use crate::error::DoreanError;

/// Maximum number of matches returned to the model.
const MAX_MATCHES: usize = 500;

#[derive(Default)]
pub struct GlobTool;

impl GlobTool {
    pub fn new() -> Self {
        GlobTool
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "List files matching a glob pattern (e.g. `src/**/*.rs`, `*.toml`), resolved against \
         the working directory. Useful for discovery before reading or searching."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern. `**` matches across directories." },
                "base": { "type": "string", "description": "Optional directory to resolve the pattern against (default working directory)." }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, DoreanError> {
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| DoreanError::Message("glob: missing string `pattern`".to_string()))?;
        let base = args
            .get("base")
            .and_then(Value::as_str)
            .map(|s| ctx.resolve(s))
            .unwrap_or_else(|| ctx.cwd.clone());

        let full_pattern = if pattern.starts_with('/') {
            pattern.to_string()
        } else {
            base.join(pattern).to_string_lossy().into_owned()
        };

        let mut matches: Vec<String> = glob::glob(&full_pattern)
            .map_err(|e| DoreanError::Message(format!("glob `{pattern}`: {e}")))?
            .filter_map(|entry| entry.ok())
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        matches.sort();

        let truncated = matches.len() > MAX_MATCHES;
        matches.truncate(MAX_MATCHES);

        let mut out = format!("{} matches for `{pattern}`:\n", matches.len());
        for m in &matches {
            out.push_str(m);
            out.push('\n');
        }
        if truncated {
            out.push_str("… (truncated)\n");
        }
        if matches.is_empty() {
            out = format!("no files match `{pattern}`\n");
        }
        Ok(ToolOutput::new(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir())
    }

    #[tokio::test]
    async fn finds_matching_files() {
        let dir = std::env::temp_dir().join(format!("dorean-glob-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.rs"), "").unwrap();
        fs::write(dir.join("src/b.rs"), "").unwrap();
        fs::write(dir.join("Cargo.toml"), "").unwrap();

        let out = GlobTool
            .run(&ctx(), json!({ "pattern": "src/*.rs", "base": dir }))
            .await
            .unwrap()
            .text;
        assert!(out.contains("a.rs"));
        assert!(out.contains("b.rs"));
        assert!(!out.contains("Cargo.toml"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reports_no_matches() {
        let out = GlobTool
            .run(&ctx(), json!({ "pattern": "no_such_dir/*.zzz" }))
            .await
            .unwrap()
            .text;
        assert!(out.contains("no files match"));
    }
}
