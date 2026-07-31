//! `read` tool: file read with optional line ranges, binary safety.

use std::fs;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolOutput};
use crate::error::DoreanError;

/// Maximum characters of file content returned to the model in one call.
const MAX_OUTPUT_CHARS: usize = 40_000;
/// Maximum line length kept per line before truncation.
const MAX_LINE_CHARS: usize = 4_000;

#[derive(Default)]
pub struct ReadTool;

impl ReadTool {
    pub fn new() -> Self {
        ReadTool
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a file. Optionally restrict to a 1-based line range [start_line, end_line]. \
         Returns the file content (binary files are reported, not dumped)."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file, absolute or relative to the working directory." },
                "start_line": { "type": "integer", "minimum": 1, "description": "First line to include (1-based)." },
                "end_line": { "type": "integer", "minimum": 1, "description": "Last line to include (inclusive)." }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, DoreanError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| DoreanError::Message("read: missing string `path`".to_string()))?;
        let resolved = ctx.resolve(path);
        let start_line = args.get("start_line").and_then(Value::as_u64).unwrap_or(1);
        let end_line = args.get("end_line").and_then(Value::as_u64);

        let bytes = fs::read(&resolved)
            .map_err(|e| DoreanError::Message(format!("read {}: {e}", resolved.display())))?;

        if bytes.contains(&0) {
            return Ok(ToolOutput::new(format!(
                "read {}: binary file ({} bytes) not shown",
                resolved.display(),
                bytes.len()
            )));
        }
        let content = String::from_utf8_lossy(&bytes).into_owned();

        let lines: Vec<&str> = if start_line == 1 && end_line.is_none() {
            content.lines().collect()
        } else {
            let end = end_line.unwrap_or(u64::MAX) as usize;
            content
                .lines()
                .skip(start_line.saturating_sub(1) as usize)
                .take(end - start_line as usize + 1)
                .collect()
        };

        let mut out = String::new();
        let mut kept = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let line_no = start_line as usize + i;
            let shown = if line.chars().count() > MAX_LINE_CHARS {
                let truncated: String = line.chars().take(MAX_LINE_CHARS).collect();
                format!("{truncated}… (truncated)")
            } else {
                (*line).to_string()
            };
            let entry = format!("{line_no:>6}\t{shown}\n");
            if kept + entry.len() > MAX_OUTPUT_CHARS {
                out.push_str(&format!("… output truncated after {kept} chars\n"));
                break;
            }
            kept += entry.len();
            out.push_str(&entry);
        }

        Ok(ToolOutput::new(format!(
            "read {} ({} bytes, {} lines shown):\n{out}",
            resolved.display(),
            bytes.len(),
            lines.len()
        )))
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
    async fn reads_file_with_line_numbers() {
        let dir = std::env::temp_dir().join(format!("dorean-read-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();

        let args = json!({ "path": dir.join("a.txt") });
        let out = ReadTool.run(&ctx(), args).await.unwrap().text;
        assert!(out.contains("1\tone"));
        assert!(out.contains("2\ttwo"));
        assert!(out.contains("3\tthree"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn respects_line_ranges() {
        let dir = std::env::temp_dir().join(format!("dorean-read-range-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "a\nb\nc\nd\ne\n").unwrap();

        let args = json!({ "path": dir.join("a.txt"), "start_line": 2, "end_line": 4 });
        let out = ReadTool.run(&ctx(), args).await.unwrap().text;
        assert!(out.contains("2\tb"));
        assert!(out.contains("3\tc"));
        assert!(out.contains("4\td"));
        assert!(!out.contains("1\ta"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reports_missing_file() {
        let args = json!({ "path": "/nonexistent/dorean/file.txt" });
        let err = ReadTool.run(&ctx(), args).await.unwrap_err();
        assert!(err.to_string().contains("read"));
    }

    #[tokio::test]
    async fn reports_binary_files() {
        let dir = std::env::temp_dir().join(format!("dorean-read-bin-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("bin"), [0u8, 159, 146, 150]).unwrap();

        let args = json!({ "path": dir.join("bin") });
        let out = ReadTool.run(&ctx(), args).await.unwrap().text;
        assert!(out.contains("binary"));
        let _ = fs::remove_dir_all(&dir);
    }
}
