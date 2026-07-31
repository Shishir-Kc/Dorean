//! `grep` tool: regex content search over files, searched in parallel.

use std::fs;
use std::path::Path;

use async_trait::async_trait;
use rayon::prelude::*;
use regex::Regex;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolOutput};
use crate::error::DoreanError;

/// Maximum number of matches returned to the model.
const MAX_MATCHES: usize = 300;
/// Maximum bytes scanned per file for a line.
const MAX_FILE_BYTES: u64 = 1 << 20;

/// Directories never descended into.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".dorean"];

#[derive(Default)]
pub struct GrepTool;

impl GrepTool {
    pub fn new() -> Self {
        GrepTool
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents for a regex pattern (Rust regex syntax). Searches recursively \
         from `path` (default: working directory), skipping .git, target, node_modules. \
         Returns `path:line:match` results. Use for finding references and definitions."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Rust regex to search for." },
                "path": { "type": "string", "description": "Optional directory to search (default working directory)." },
                "glob": { "type": "string", "description": "Optional file-name filter, e.g. `*.rs`." }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, DoreanError> {
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| DoreanError::Message("grep: missing string `pattern`".to_string()))?;
        let base = args
            .get("path")
            .and_then(Value::as_str)
            .map(|s| ctx.resolve(s))
            .unwrap_or_else(|| ctx.cwd.clone());
        let name_filter = args.get("glob").and_then(Value::as_str);
        let name_pattern = name_filter
            .map(|g| {
                glob::Pattern::new(g)
                    .map_err(|e| DoreanError::Message(format!("grep: invalid glob `{g}`: {e}")))
            })
            .transpose()?;

        let regex = Regex::new(pattern)
            .map_err(|e| DoreanError::Message(format!("grep: invalid regex `{pattern}`: {e}")))?;

        if !base.is_dir() {
            return Ok(ToolOutput::new(format!(
                "grep: `{}` is not a directory",
                base.display()
            )));
        }

        let files = collect_files(&base, name_pattern.as_ref());
        let results: Vec<String> = files
            .par_iter()
            .filter_map(|file| search_file(file, &regex))
            .flatten()
            .collect();

        let mut results = results;
        results.sort();

        let truncated = results.len() > MAX_MATCHES;
        results.truncate(MAX_MATCHES);

        let mut out = String::new();
        for line in &results {
            out.push_str(line);
            out.push('\n');
        }
        if truncated {
            out.push_str("… (truncated)\n");
        }
        if results.is_empty() {
            out = format!("no matches for `{pattern}`\n");
        }
        Ok(ToolOutput::new(out))
    }
}

fn collect_files(base: &Path, name_pattern: Option<&glob::Pattern>) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(path);
                }
            } else if meta.is_file()
                && matches!(meta.len().checked_sub(MAX_FILE_BYTES), Some(0) | None)
            {
                let matches_name = name_pattern
                    .map(|p| {
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .map(|s| p.matches(s))
                            .unwrap_or(false)
                    })
                    .unwrap_or(true);
                if matches_name {
                    files.push(path);
                }
            }
        }
    }
    files
}

fn search_file(path: &Path, regex: &Regex) -> Option<Vec<String>> {
    let content = fs::read_to_string(path).ok()?;
    let mut hits = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if regex.is_match(line) {
            hits.push(format!("{}:{}:{}", path.display(), i + 1, line));
        }
    }
    Some(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir())
    }

    #[tokio::test]
    async fn finds_matches_with_path_and_line() {
        let dir = std::env::temp_dir().join(format!("dorean-grep-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.rs"), "fn main() {}\nlet x = 1;\n").unwrap();
        fs::write(dir.join("b.txt"), "nothing here\n").unwrap();

        let out = GrepTool
            .run(&ctx(), json!({ "pattern": "let x", "path": dir }))
            .await
            .unwrap()
            .text;
        assert!(out.contains("a.rs:2:let x = 1;"));
        assert!(!out.contains("b.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn respects_name_filter() {
        let dir = std::env::temp_dir().join(format!("dorean-grep-filt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.rs"), "needle\n").unwrap();
        fs::write(dir.join("a.txt"), "needle\n").unwrap();

        let out = GrepTool
            .run(
                &ctx(),
                json!({ "pattern": "needle", "path": dir, "glob": "*.rs" }),
            )
            .await
            .unwrap()
            .text;
        assert!(out.contains("a.rs:1:needle"));
        assert!(!out.contains("a.txt"));
        let _ = fs::remove_dir_all(&dir);
    }
}
