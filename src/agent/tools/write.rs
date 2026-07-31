//! `write` tool: create/overwrite a file (creating parent directories).

use std::fs;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolOutput};
use crate::error::DoreanError;

/// Files larger than this are not overwritten without an explicit `force`.
const MAX_OVERWRITE_BYTES: u64 = 64 * 1024;

#[derive(Default)]
pub struct WriteTool;

impl WriteTool {
    pub fn new() -> Self {
        WriteTool
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Create a new file or overwrite an existing one with the given content. \
         Parent directories are created automatically. Use `edit` for surgical changes \
         to existing files. Overwriting a file larger than 64 KB requires `force: true` \
         as a safety check."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file, absolute or relative to the working directory." },
                "content": { "type": "string", "description": "Full file contents to write." },
                "force": { "type": "boolean", "description": "Set true to overwrite an existing large file (>= 64 KB)." }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, DoreanError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| DoreanError::Message("write: missing string `path`".to_string()))?;
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| DoreanError::Message("write: missing string `content`".to_string()))?;
        let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);

        let resolved = ctx.resolve(path);
        if let Ok(metadata) = fs::metadata(&resolved)
            && metadata.is_file()
            && metadata.len() >= MAX_OVERWRITE_BYTES
            && !force
        {
            return Err(DoreanError::Message(format!(
                "write {}: refusing to overwrite a {} byte file without `force: true` \
                 (prefer `edit` for surgical changes)",
                resolved.display(),
                metadata.len()
            )));
        }

        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                DoreanError::Message(format!(
                    "write {}: cannot create parent dir: {e}",
                    resolved.display()
                ))
            })?;
        }

        fs::write(&resolved, content)
            .map_err(|e| DoreanError::Message(format!("write {}: {e}", resolved.display())))?;

        Ok(ToolOutput::new(format!(
            "wrote {} ({} bytes)",
            resolved.display(),
            content.len()
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
    async fn writes_file_and_creates_dirs() {
        let dir = std::env::temp_dir().join(format!("dorean-write-test-{}", std::process::id()));
        let target = dir.join("nested/deep/file.txt");

        let args = json!({ "path": target, "content": "hello\nworld\n" });
        let out = WriteTool.run(&ctx(), args).await.unwrap();
        assert!(out.text.contains("wrote"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello\nworld\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn overwrites_existing_file() {
        let dir = std::env::temp_dir().join(format!("dorean-write-over-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("a.txt");
        fs::write(&target, "old").unwrap();

        let args = json!({ "path": target, "content": "new" });
        WriteTool.run(&ctx(), args).await.unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn requires_content() {
        let args = json!({ "path": "/tmp/x" });
        assert!(WriteTool.run(&ctx(), args).await.is_err());
    }

    #[tokio::test]
    async fn refuses_large_overwrite_without_force() {
        let dir = std::env::temp_dir().join(format!("dorean-write-large-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("big.txt");
        fs::write(&target, vec![b'x'; 100_000]).unwrap();

        let args = json!({ "path": target, "content": "small" });
        let err = WriteTool.run(&ctx(), args).await.unwrap_err();
        assert!(err.to_string().contains("force"));

        let args = json!({ "path": target, "content": "small", "force": true });
        WriteTool.run(&ctx(), args).await.unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "small");
        let _ = fs::remove_dir_all(&dir);
    }
}
