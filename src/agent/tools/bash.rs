//! `bash` tool: shell execution with a working directory and timeout.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::{Tool, ToolContext, ToolOutput};
use crate::error::DoreanError;

/// Maximum combined output characters returned to the model.
const MAX_OUTPUT_CHARS: usize = 30_000;
/// Default command timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct BashTool;

impl BashTool {
    pub fn new() -> Self {
        BashTool
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command via `sh -c` in the working directory. Use for builds, tests, \
         and inspecting the environment. Returns combined stdout/stderr and the exit code. \
         Prefer file tools (`read`, `write`, `edit`) over shell for file operations."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to run." },
                "cwd": { "type": "string", "description": "Optional working directory; defaults to the session working directory." },
                "timeout_ms": { "type": "integer", "minimum": 1, "description": "Optional timeout in milliseconds (default 60000)." }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, DoreanError> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| DoreanError::Message("bash: missing string `command`".to_string()))?;
        let cwd = args
            .get("cwd")
            .and_then(Value::as_str)
            .map(|s| ctx.resolve(s))
            .unwrap_or_else(|| ctx.cwd.clone());
        let timeout = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_TIMEOUT);

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| DoreanError::Message(format!("bash: failed to spawn: {e}")))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let output = match tokio::time::timeout(timeout, async {
            let mut stdout_buf = Vec::new();
            let mut stderr_buf = Vec::new();
            if let Some(mut stdout) = stdout {
                let _ = stdout.read_to_end(&mut stdout_buf).await;
            }
            if let Some(mut stderr) = stderr {
                let _ = stderr.read_to_end(&mut stderr_buf).await;
            }
            let status = child
                .wait()
                .await
                .map_err(|e| DoreanError::Message(format!("bash: failed to wait: {e}")))?;
            Ok::<_, DoreanError>((
                String::from_utf8_lossy(&stdout_buf).into_owned(),
                String::from_utf8_lossy(&stderr_buf).into_owned(),
                status,
            ))
        })
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                let _ = child.start_kill();
                return Ok(ToolOutput::new(format!(
                    "command timed out after {} ms (killed):\n{command}",
                    timeout.as_millis()
                )));
            }
        };

        let (stdout, stderr, status) = output;
        let mut text = String::new();
        if !stderr.is_empty() {
            text.push_str("stderr:\n");
            text.push_str(&stderr);
            text.push('\n');
        }
        if !stdout.is_empty() {
            text.push_str("stdout:\n");
            text.push_str(&stdout);
            text.push('\n');
        }
        if text.is_empty() {
            text = "(no output)\n".to_string();
        }
        if text.len() > MAX_OUTPUT_CHARS {
            text.truncate(MAX_OUTPUT_CHARS);
            text.push_str("\n… output truncated\n");
        }
        text.push_str(&format!("exit code: {}\n", status.code().unwrap_or(-1)));

        Ok(ToolOutput::new(text))
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
    async fn runs_command_and_reports_exit_code() {
        let out = BashTool
            .run(&ctx(), json!({ "command": "echo hello" }))
            .await
            .unwrap()
            .text;
        assert!(out.contains("hello"));
        assert!(out.contains("exit code: 0"));
    }

    #[tokio::test]
    async fn captures_stderr_and_failure_code() {
        let out = BashTool
            .run(&ctx(), json!({ "command": "echo oops >&2; exit 3" }))
            .await
            .unwrap()
            .text;
        assert!(out.contains("oops"));
        assert!(out.contains("exit code: 3"));
    }

    #[tokio::test]
    async fn honors_cwd() {
        let dir = std::env::temp_dir().join(format!("dorean-bash-cwd-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let out = BashTool
            .run(&ctx(), json!({ "command": "pwd", "cwd": dir }))
            .await
            .unwrap()
            .text;
        assert!(out.contains(dir.to_string_lossy().as_ref()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn enforces_timeout() {
        let out = BashTool
            .run(&ctx(), json!({ "command": "sleep 5", "timeout_ms": 100 }))
            .await
            .unwrap()
            .text;
        assert!(out.contains("timed out"));
    }
}
