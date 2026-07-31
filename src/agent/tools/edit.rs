//! `edit` tool: exact-string (or regex) replacement in a file, atomic write.

use std::fs;

use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolOutput};
use crate::error::DoreanError;

#[derive(Default)]
pub struct EditTool;

impl EditTool {
    pub fn new() -> Self {
        EditTool
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace one occurrence of `old` with `new` in the file at `path`. \
         By default `old` must match exactly, including whitespace. Set `regex: true` \
         to treat `old` as a regular expression (regex crate syntax). If the pattern \
         appears multiple times, set `occurrence` to choose the nth match (1-based). \
         Fails if `old` is not found. The change is written atomically."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file, absolute or relative to the working directory." },
                "old": { "type": "string", "description": "Exact text (or regex when `regex` is true) to find." },
                "new": { "type": "string", "description": "Replacement text." },
                "occurrence": { "type": "integer", "minimum": 1, "description": "Which match to replace when the pattern appears multiple times (default 1)." },
                "regex": { "type": "boolean", "description": "Treat `old` as a regular expression (default false)." }
            },
            "required": ["path", "old", "new"],
            "additionalProperties": false
        })
    }

    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, DoreanError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| DoreanError::Message("edit: missing string `path`".to_string()))?;
        let old = args
            .get("old")
            .and_then(Value::as_str)
            .ok_or_else(|| DoreanError::Message("edit: missing string `old`".to_string()))?;
        let new = args
            .get("new")
            .and_then(Value::as_str)
            .ok_or_else(|| DoreanError::Message("edit: missing string `new`".to_string()))?;
        let occurrence = args.get("occurrence").and_then(Value::as_u64).unwrap_or(1);
        let is_regex = args.get("regex").and_then(Value::as_bool).unwrap_or(false);
        if occurrence == 0 {
            return Err(DoreanError::Message(
                "edit: `occurrence` must be >= 1".to_string(),
            ));
        }

        let resolved = ctx.resolve(path);
        let content = fs::read_to_string(&resolved)
            .map_err(|e| DoreanError::Message(format!("edit {}: {e}", resolved.display())))?;

        let updated = if is_regex {
            replace_regex(&content, old, new, occurrence, &resolved)?
        } else {
            replace_exact(&content, old, new, occurrence, &resolved)?
        };

        atomic_write(&resolved, updated.as_bytes())
            .map_err(|e| DoreanError::Message(format!("edit {}: {e}", resolved.display())))?;

        Ok(ToolOutput::new(format!(
            "edit {}: replaced occurrence {occurrence} ({} -> {} chars)",
            resolved.display(),
            old.len(),
            new.len()
        )))
    }
}

fn replace_exact(
    content: &str,
    old: &str,
    new: &str,
    occurrence: u64,
    resolved: &std::path::Path,
) -> Result<String, DoreanError> {
    let mut found = 0usize;
    let mut matches = content.match_indices(old);
    let (start, _) = loop {
        match matches.next() {
            Some(m) => {
                found += 1;
                if found == occurrence as usize {
                    break m;
                }
            }
            None => {
                return Err(DoreanError::Message(format!(
                    "edit {}: pattern not found (occurrence {occurrence}, total matches {found})",
                    resolved.display()
                )));
            }
        }
    };
    let end = start + old.len();
    let mut updated = content.to_string();
    updated.replace_range(start..end, new);
    Ok(updated)
}

fn replace_regex(
    content: &str,
    pattern: &str,
    new: &str,
    occurrence: u64,
    resolved: &std::path::Path,
) -> Result<String, DoreanError> {
    let regex = Regex::new(pattern).map_err(|e| {
        DoreanError::Message(format!("edit {}: invalid regex: {e}", resolved.display()))
    })?;
    let mut count = 0u64;
    let mut updated = String::with_capacity(content.len());
    let mut last = 0usize;
    for m in regex.find_iter(content) {
        count += 1;
        if count == occurrence {
            updated.push_str(&content[last..m.start()]);
            updated.push_str(new);
            last = m.end();
            // Keep the rest verbatim.
            updated.push_str(&content[last..]);
            return Ok(updated);
        }
    }
    Err(DoreanError::Message(format!(
        "edit {}: regex not matched (occurrence {occurrence}, total matches {count})",
        resolved.display()
    )))
}

/// Write to a temp file in the same directory, then rename over the target so
/// the file is never observed half-written.
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let temp = dir.join(format!(
        ".{}.dorean-tmp-{}",
        path.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default(),
        std::process::id()
    ));
    fs::write(&temp, bytes)?;
    fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir())
    }

    #[tokio::test]
    async fn replaces_first_occurrence() {
        let dir = std::env::temp_dir().join(format!("dorean-edit-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("a.rs");
        fs::write(&target, "let x = 1;\nlet y = 2;\n").unwrap();

        let args = json!({ "path": target, "old": "let x = 1;", "new": "let x = 10;" });
        let out = EditTool.run(&ctx(), args).await.unwrap();
        assert!(out.text.contains("replaced"));
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "let x = 10;\nlet y = 2;\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn picks_nth_occurrence() {
        let dir = std::env::temp_dir().join(format!("dorean-edit-nth-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("a.txt");
        fs::write(&target, "a a a").unwrap();

        let args = json!({ "path": target, "old": "a", "new": "b", "occurrence": 2 });
        EditTool.run(&ctx(), args).await.unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "a b a");
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn errors_when_pattern_missing() {
        let dir = std::env::temp_dir().join(format!("dorean-edit-miss-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("a.txt");
        fs::write(&target, "hello").unwrap();

        let args = json!({ "path": target, "old": "nope", "new": "x" });
        let err = EditTool.run(&ctx(), args).await.unwrap_err();
        assert!(err.to_string().contains("not found"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn regex_replaces_nth_match() {
        let dir = std::env::temp_dir().join(format!("dorean-edit-regex-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("a.rs");
        fs::write(&target, "fn foo() {}\nfn foo() {}\n").unwrap();

        let args = json!({
            "path": target,
            "old": r"fn foo\(\)",
            "new": "fn bar()",
            "regex": true,
            "occurrence": 2
        });
        EditTool.run(&ctx(), args).await.unwrap();
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "fn foo() {}\nfn bar() {}\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn regex_errors_when_no_match() {
        let dir =
            std::env::temp_dir().join(format!("dorean-edit-regexmiss-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("a.txt");
        fs::write(&target, "abc").unwrap();

        let args = json!({ "path": target, "old": "z+", "new": "x", "regex": true });
        let err = EditTool.run(&ctx(), args).await.unwrap_err();
        assert!(err.to_string().contains("regex not matched"));
        let _ = fs::remove_dir_all(&dir);
    }
}
