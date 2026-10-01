//! Lifecycle hooks: deterministic shell commands around tool calls.
//!
//! Parity with Claude Code hooks: `PreToolUse` / `PostToolUse` / `Stop`
//! entries in `.dorean/hooks.json` run a shell command with the tool call as
//! JSON on stdin. Exit 0 = allow (stdout appended to context), exit 2 = deny
//! with stderr as the reason, other = non-blocking warning. Timeouts never
//! block the agent.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// When a hook fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    Stop,
    SessionStart,
}

/// One hook entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    pub event: HookEvent,
    /// Optional tool-name filter (e.g. only `bash`). None = all tools.
    pub tool: Option<String>,
    pub command: String,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

fn default_timeout() -> u64 {
    5000
}

/// Outcome of running hooks for one event.
#[derive(Debug, Clone)]
pub enum HookOutcome {
    Allow { extra_context: String },
    Deny { reason: String },
}

/// Load hooks from `.dorean/hooks.json`. Missing/invalid → empty set.
pub fn load_hooks(cwd: &Path) -> Vec<Hook> {
    let path = cwd.join(".dorean").join("hooks.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        hooks: Vec<Hook>,
    }
    serde_json::from_str::<File>(&text)
        .or_else(|_| serde_json::from_str::<Vec<Hook>>(&text).map(|hooks| File { hooks }))
        .map(|f| f.hooks)
        .unwrap_or_default()
}

/// Run matching hooks. Never errors — failures degrade to warnings.
pub fn run_hooks(
    hooks: &[Hook],
    event: HookEvent,
    tool_name: &str,
    payload: &serde_json::Value,
    cwd: &Path,
) -> HookOutcome {
    let mut context = String::new();
    for hook in hooks
        .iter()
        .filter(|h| h.event == event && h.tool.as_deref().map(|t| t == tool_name).unwrap_or(true))
    {
        let input = serde_json::json!({
            "event": format!("{:?}", event),
            "tool": tool_name,
            "payload": payload,
        });
        match run_one(&hook.command, &input, cwd, hook.timeout_ms) {
            HookRun::Allow { stdout } => {
                if !stdout.trim().is_empty() {
                    context.push_str(stdout.trim());
                    context.push('\n');
                }
            }
            HookRun::Deny { stderr } => {
                return HookOutcome::Deny {
                    reason: if stderr.trim().is_empty() {
                        format!("hook denied {tool_name}")
                    } else {
                        stderr.trim().to_string()
                    },
                };
            }
            HookRun::Warn => {}
        }
    }
    HookOutcome::Allow {
        extra_context: context,
    }
}

enum HookRun {
    Allow { stdout: String },
    Deny { stderr: String },
    Warn,
}

fn run_one(command: &str, input: &serde_json::Value, cwd: &Path, timeout_ms: u64) -> HookRun {
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return HookRun::Warn,
    };
    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.to_string().as_bytes());
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms.max(100));
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = child
                    .stdout
                    .take()
                    .map(|mut o| {
                        use std::io::Read;
                        let mut s = String::new();
                        let _ = o.read_to_string(&mut s);
                        s
                    })
                    .unwrap_or_default();
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut o| {
                        use std::io::Read;
                        let mut s = String::new();
                        let _ = o.read_to_string(&mut s);
                        s
                    })
                    .unwrap_or_default();
                return match status.code() {
                    Some(0) => HookRun::Allow { stdout },
                    Some(2) => HookRun::Deny { stderr },
                    _ => HookRun::Warn,
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return HookRun::Warn;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return HookRun::Warn,
        }
    }
}

/// Tool-name → allowed tools audit helper (used by `/hooks` display).
pub fn summarize(hooks: &[Hook]) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for h in hooks {
        *map.entry(format!("{:?}", h.event)).or_insert(0) += 1;
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_hooks_file_yields_empty() {
        let hooks = load_hooks(Path::new("/nonexistent-dorean-xyz"));
        assert!(hooks.is_empty());
    }

    #[test]
    fn allow_hook_passes_stdout_as_context() {
        let hooks = vec![Hook {
            event: HookEvent::PreToolUse,
            tool: None,
            command: "cat; echo hook-ok".to_string(),
            timeout_ms: 2000,
        }];
        let outcome = run_hooks(
            &hooks,
            HookEvent::PreToolUse,
            "bash",
            &serde_json::json!({}),
            Path::new("/tmp"),
        );
        match outcome {
            HookOutcome::Allow { extra_context } => assert!(extra_context.contains("hook-ok")),
            HookOutcome::Deny { .. } => panic!("should allow"),
        }
    }

    #[test]
    fn exit_2_denies() {
        let hooks = vec![Hook {
            event: HookEvent::PreToolUse,
            tool: Some("bash".to_string()),
            command: "echo no dangerous >&2; exit 2".to_string(),
            timeout_ms: 2000,
        }];
        let outcome = run_hooks(
            &hooks,
            HookEvent::PreToolUse,
            "bash",
            &serde_json::json!({}),
            Path::new("/tmp"),
        );
        assert!(matches!(outcome, HookOutcome::Deny { .. }));
    }
}
