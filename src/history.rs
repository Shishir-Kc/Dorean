//! Session log (JSONL), history persistence, resume.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::DoreanError;
use crate::providers::client::Message;

/// One run of the agent: the conversation plus metadata needed to resume it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    /// Unix timestamp when the session started.
    pub created_at: u64,
    pub model: String,
    /// The conversation, excluding the (rebuilt) system prompt.
    pub messages: Vec<Message>,
}

/// Per-agent session directory inside the repo, `.dorean/sessions`.
pub fn sessions_dir(cwd: &Path) -> PathBuf {
    cwd.join(".dorean").join("sessions")
}

/// Session log path for a named agent.
pub fn session_path_named(cwd: &Path, name: &str) -> PathBuf {
    sessions_dir(cwd).join(format!("{name}.jsonl"))
}

/// Session log path for the main agent.
pub fn session_path(cwd: &Path) -> PathBuf {
    session_path_named(cwd, "main")
}

/// Append a session record to a named agent's log.
pub fn append_session_named(
    cwd: &Path,
    name: &str,
    record: &SessionRecord,
) -> Result<(), DoreanError> {
    let path = session_path_named(cwd, name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(DoreanError::Io)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(DoreanError::Io)?;
    let line = serde_json::to_string(record)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Append a session record as one JSON line to the main session log.
pub fn append_session(cwd: &Path, record: &SessionRecord) -> Result<(), DoreanError> {
    append_session_named(cwd, "main", record)
}

/// Load the most recent session record from a named agent's log, if any.
pub fn load_latest_named(cwd: &Path, name: &str) -> Result<Option<SessionRecord>, DoreanError> {
    let path = session_path_named(cwd, name);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(DoreanError::Io(e)),
    };

    let mut latest = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if let Ok(record) = serde_json::from_str::<SessionRecord>(line) {
            latest = Some(record);
        }
    }
    Ok(latest)
}

/// Load the most recent session record from the main log, if any.
pub fn load_latest(cwd: &Path) -> Result<Option<SessionRecord>, DoreanError> {
    load_latest_named(cwd, "main")
}

/// Build a session record for a run starting now.
pub fn new_record(model: String, messages: Vec<Message>) -> SessionRecord {
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    SessionRecord {
        id: format!("{created_at}-{}", std::process::id()),
        created_at,
        model,
        messages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::client::{Message, Role};

    #[test]
    fn appends_and_loads_latest() {
        let dir = std::env::temp_dir().join(format!("dorean-hist-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        assert!(load_latest(&dir).unwrap().is_none());

        let record = SessionRecord {
            id: "1".to_string(),
            created_at: 100,
            model: "m1".to_string(),
            messages: vec![
                Message::user("hello"),
                Message {
                    role: Role::Assistant,
                    content: "hi there".to_string(),
                    name: None,
                    tool_call_id: None,
                    tool_calls: None,
                },
            ],
        };
        append_session(&dir, &record).unwrap();

        let second = SessionRecord {
            id: "2".to_string(),
            created_at: 200,
            model: "m2".to_string(),
            messages: vec![Message::user("again")],
        };
        append_session(&dir, &second).unwrap();

        let latest = load_latest(&dir).unwrap().unwrap();
        assert_eq!(latest.id, "2");
        assert_eq!(latest.model, "m2");
        assert_eq!(latest.messages[0].content, "again");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_tool_messages() {
        use crate::providers::client::ToolCall;

        let record = SessionRecord {
            id: "1".to_string(),
            created_at: 0,
            model: "m".to_string(),
            messages: vec![
                Message::assistant(
                    "",
                    vec![ToolCall {
                        id: "call_1".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({ "path": "x" }),
                    }],
                ),
                Message::tool("call_1", "wrote file"),
            ],
        };
        let json = serde_json::to_string(&record).unwrap();
        let back: SessionRecord = serde_json::from_str(&json).unwrap();
        let tool_call = back.messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(tool_call[0].name, "read");
        assert_eq!(back.messages[1].role, Role::Tool);
        assert_eq!(back.messages[1].content, "wrote file");
    }
}
