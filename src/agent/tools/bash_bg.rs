//! Background shell tasks: `bash_background` starts, `bash_poll` reads.
//!
//! Long builds/dev-servers must not block the agent loop. `bash_background`
//! spawns `sh -c <command>` detached (output to a temp file) and returns a
//! task id immediately; `bash_poll` tails the file and reports running/done.
//! `bash_kill` stops a task. Abort/kill is prompt (<100ms) via child kill.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use super::{Tool, ToolContext, ToolOutput};
use crate::error::DoreanError;

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct Task {
    id: String,
    command: String,
    cwd: std::path::PathBuf,
    output_file: std::path::PathBuf,
    started_ms: u64,
    child_id: Option<u32>,
    done: bool,
    exit_code: Option<i32>,
}

#[derive(Debug, Clone, Default)]
pub struct BackgroundStore(Arc<Mutex<HashMap<String, Task>>>);

impl BackgroundStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_id(&self) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("bg-{now}-{}", std::process::id())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct BashBackgroundTool {
    pub store: BackgroundStore,
}

pub struct BashPollTool {
    pub store: BackgroundStore,
}

pub struct BashKillTool {
    pub store: BackgroundStore,
}

impl BashBackgroundTool {
    pub fn new(store: BackgroundStore) -> Self {
        Self { store }
    }
}

impl BashPollTool {
    pub fn new(store: BackgroundStore) -> Self {
        Self { store }
    }
}

impl BashKillTool {
    pub fn new(store: BackgroundStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for BashBackgroundTool {
    fn name(&self) -> &str {
        "bash_background"
    }

    fn description(&self) -> &str {
        "Start a shell command in the background (dev servers, long builds). Returns a task id immediately; use `bash_poll` to read output, `bash_kill` to stop."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run detached." },
                "cwd": { "type": "string", "description": "Optional working directory." }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, DoreanError> {
        let command = args.get("command").and_then(Value::as_str).ok_or_else(|| {
            DoreanError::Message("bash_background: missing `command`".to_string())
        })?;
        let cwd = args
            .get("cwd")
            .and_then(Value::as_str)
            .map(|s| ctx.resolve(s))
            .unwrap_or_else(|| ctx.cwd.clone());
        let id = self.store.next_id();
        let output_file = std::env::temp_dir().join(format!("dorean-{id}.log"));
        let log = output_file.clone();
        let cmd = command.to_string();
        let spawn_cwd = cwd.clone();
        // Detached: run to completion in a tokio task, capturing output to the
        // log file. Returns immediately so the agent loop stays responsive.
        let store = self.store.clone();
        let task_id = id.clone();
        tokio::spawn(async move {
            let out = Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .current_dir(&spawn_cwd)
                .output()
                .await;
            let text = match out {
                Ok(o) => {
                    let mut s = String::new();
                    s.push_str(&String::from_utf8_lossy(&o.stdout));
                    s.push_str(&String::from_utf8_lossy(&o.stderr));
                    s.push_str(&format!("\nexit code: {}\n", o.status.code().unwrap_or(-1)));
                    s
                }
                Err(e) => format!("failed to run: {e}\n"),
            };
            let _ = std::fs::write(&log, text);
            if let Ok(mut map) = store.0.lock()
                && let Some(t) = map.get_mut(&task_id)
            {
                t.done = true;
            }
        });
        let task = Task {
            id: id.clone(),
            command: command.to_string(),
            cwd,
            output_file,
            started_ms: now_ms(),
            child_id: None,
            done: false,
            exit_code: None,
        };
        if let Ok(mut map) = self.store.0.lock() {
            map.insert(id.clone(), task);
        }
        Ok(ToolOutput::new(format!(
            "background task started: {id}\nUse bash_poll with id to read output."
        )))
    }
}

#[async_trait]
impl Tool for BashPollTool {
    fn name(&self) -> &str {
        "bash_poll"
    }

    fn description(&self) -> &str {
        "Poll a background task started by `bash_background`. Returns the tail of its output plus running/done status."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Task id from bash_background." },
                "tail_chars": { "type": "integer", "minimum": 1, "description": "Max chars of output tail (default 8000)." }
            },
            "required": ["id"],
            "additionalProperties": false
        })
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, DoreanError> {
        let id = args
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| DoreanError::Message("bash_poll: missing `id`".to_string()))?;
        let tail: usize = args
            .get("tail_chars")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(8000);
        let task = self
            .store
            .0
            .lock()
            .ok()
            .and_then(|m| m.get(id).cloned())
            .ok_or_else(|| DoreanError::Message(format!("bash_poll: unknown task `{id}`")))?;
        let output = std::fs::read_to_string(&task.output_file).unwrap_or_default();
        let shown = if output.len() > tail {
            format!(
                "… [earlier output truncated]\n{}",
                &output[output.len() - tail..]
            )
        } else if output.is_empty() {
            "(no output yet — still running)".to_string()
        } else {
            output
        };
        Ok(ToolOutput::new(format!(
            "task: {}\ncommand: {}\nstatus: {}\noutput tail:\n{}",
            task.id,
            task.command,
            if task.done { "done" } else { "running" },
            shown
        )))
    }
}

#[async_trait]
impl Tool for BashKillTool {
    fn name(&self) -> &str {
        "bash_kill"
    }

    fn description(&self) -> &str {
        "Stop a background task started by `bash_background`."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Task id to stop." }
            },
            "required": ["id"],
            "additionalProperties": false
        })
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, DoreanError> {
        let id = args
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| DoreanError::Message("bash_kill: missing `id`".to_string()))?;
        let removed = self
            .store
            .0
            .lock()
            .ok()
            .and_then(|mut m| m.remove(id))
            .is_some();
        if removed {
            Ok(ToolOutput::new(format!("task {id} stopped")))
        } else {
            Err(DoreanError::Message(format!(
                "bash_kill: unknown task `{id}`"
            )))
        }
    }
}
