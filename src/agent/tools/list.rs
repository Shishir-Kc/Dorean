//! `list` tool: directory tree listing.

use std::fs;
use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolOutput};
use crate::error::DoreanError;

/// Default recursion depth.
const DEFAULT_DEPTH: usize = 3;
/// Maximum number of entries returned to the model.
const MAX_ENTRIES: usize = 500;

/// Directories never descended into.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".dorean"];

#[derive(Default)]
pub struct ListTool;

impl ListTool {
    pub fn new() -> Self {
        ListTool
    }
}

#[async_trait]
impl Tool for ListTool {
    fn name(&self) -> &str {
        "list"
    }

    fn description(&self) -> &str {
        "List the contents of a directory as an indented tree (default depth 3, default path: \
         working directory). Directories end with `/`; files show their size. Use to get the \
         lay of the land before reading files."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Optional directory to list (default working directory)." },
                "depth": { "type": "integer", "minimum": 1, "maximum": 8, "description": "Recursion depth (default 3)." }
            },
            "additionalProperties": false
        })
    }

    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, DoreanError> {
        let base = args
            .get("path")
            .and_then(Value::as_str)
            .map(|s| ctx.resolve(s))
            .unwrap_or_else(|| ctx.cwd.clone());
        let depth = args
            .get("depth")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_DEPTH as u64) as usize;

        if !base.is_dir() {
            return Err(DoreanError::Message(format!(
                "list: `{}` is not a directory",
                base.display()
            )));
        }

        let mut out = format!("{}\n", base.display());
        let mut count = 0usize;
        walk(&base, &mut out, 0, depth, &mut count);

        if count >= MAX_ENTRIES {
            out.push_str("… (truncated)\n");
        }
        Ok(ToolOutput::new(out))
    }
}

fn walk(dir: &Path, out: &mut String, level: usize, depth: usize, count: &mut usize) {
    if level >= depth {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        if *count >= MAX_ENTRIES {
            return;
        }
        *count += 1;
        let path = entry.path();
        let meta = entry.metadata();
        let name = entry.file_name().to_string_lossy().into_owned();
        let indent = "  ".repeat(level + 1);

        match meta {
            Ok(meta) if meta.is_dir() => {
                if SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                out.push_str(&format!("{indent}{name}/\n"));
                walk(&path, out, level + 1, depth, count);
            }
            Ok(meta) => {
                out.push_str(&format!("{indent}{name} ({} B)\n", meta.len()));
            }
            Err(_) => {
                out.push_str(&format!("{indent}{name} (unreadable)\n"));
            }
        }
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
    async fn lists_tree_with_indentation() {
        let dir = std::env::temp_dir().join(format!("dorean-list-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "").unwrap();
        fs::write(dir.join("Cargo.toml"), "").unwrap();

        let out = ListTool
            .run(&ctx(), json!({ "path": dir, "depth": 2 }))
            .await
            .unwrap()
            .text;
        assert!(out.contains("src/"));
        assert!(out.contains("main.rs"));
        assert!(out.contains("Cargo.toml"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn errors_on_missing_dir() {
        assert!(
            ListTool
                .run(&ctx(), json!({ "path": "/nonexistent/dorean" }))
                .await
                .is_err()
        );
    }
}
