//! Todo tracking for orchestrated runs.
//!
//! - master list: `.dorean/todos.md` (merged by the orchestrator);
//! - per-agent list: `.dorean/todos/<agent>.md` (writeable by that agent only).
//!
//! Both share a lightweight markdown format: a `## <agent>` heading followed
//! by `- [ ]` / `- [~]` / `- [x]` / `- [!]` list items. IDs are derived
//! deterministically from `agent:slug(title)` so sessions can be resumed and
//! updates matched across files.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::DoreanError;

/// Lifecycle of a todo item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
    Blocked,
}

impl TodoStatus {
    /// The markdown checkbox marker for this status.
    pub(crate) fn marker(&self) -> &'static str {
        match self {
            TodoStatus::Pending => " ",
            TodoStatus::InProgress => "~",
            TodoStatus::Done => "x",
            TodoStatus::Blocked => "!",
        }
    }
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Done => "done",
            TodoStatus::Blocked => "blocked",
        };
        f.write_str(text)
    }
}

/// One todo item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub agent: String,
    pub title: String,
    pub status: TodoStatus,
}

impl TodoItem {
    pub fn new(agent: impl Into<String>, title: impl Into<String>) -> Self {
        let agent = agent.into();
        let title = title.into();
        TodoItem {
            id: item_id(&agent, &title),
            agent,
            title,
            status: TodoStatus::Pending,
        }
    }
}

/// Deterministic id for an item: `agent:slug(title)`.
pub fn item_id(agent: &str, title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if (ch.is_whitespace() || ch == '-') && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.len() > 48 {
        slug.truncate(48);
        slug.push_str("-…");
    }
    format!("{agent}:{slug}")
}

// --- Paths ---------------------------------------------------------------

/// Master todo list path, `.dorean/todos.md`.
pub fn master_path(cwd: &Path) -> PathBuf {
    cwd.join(".dorean").join("todos.md")
}

/// Per-agent todo directory, `.dorean/todos/`.
pub fn agent_dir(cwd: &Path) -> PathBuf {
    cwd.join(".dorean").join("todos")
}

/// Per-agent todo file, `.dorean/todos/<agent>.md`.
pub fn agent_path(cwd: &Path, agent: &str) -> PathBuf {
    agent_dir(cwd).join(format!("{agent}.md"))
}

// --- Parsing / rendering -------------------------------------------------

/// Parse a todo file (master or per-agent). Items outside any `## <agent>`
/// section are skipped. Unknown checkbox markers are treated as pending.
pub fn parse(text: &str) -> Vec<TodoItem> {
    let mut items = Vec::new();
    let mut agent: Option<String> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            agent = Some(name.trim().to_string());
            continue;
        }
        let Some(agent) = &agent else { continue };
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("- [") {
            let Some(close) = rest.find(']') else {
                continue;
            };
            let marker = rest[..close].trim();
            let title = rest[close + 1..].trim().to_string();
            if title.is_empty() {
                continue;
            }
            let status = match marker {
                "x" | "X" => TodoStatus::Done,
                "~" => TodoStatus::InProgress,
                "!" => TodoStatus::Blocked,
                _ => TodoStatus::Pending,
            };
            items.push(TodoItem {
                id: item_id(agent, &title),
                agent: agent.clone(),
                title,
                status,
            });
        }
    }
    items
}

/// Render items grouped by agent (sorted) into the shared markdown format.
pub fn render(items: &[TodoItem]) -> String {
    let mut by_agent: BTreeMap<&str, Vec<&TodoItem>> = BTreeMap::new();
    for item in items {
        by_agent.entry(&item.agent).or_default().push(item);
    }

    let mut out = String::from("# dorean todos\n\n");
    for (agent, agent_items) in by_agent {
        out.push_str(&format!("## {agent}\n"));
        for item in agent_items {
            out.push_str(&format!("- [{}] {}\n", item.status.marker(), item.title));
        }
        out.push('\n');
    }
    out
}

// --- IO ------------------------------------------------------------------

fn ensure_dir(path: &Path) -> Result<(), DoreanError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn read_or_empty(path: &Path) -> Result<String, DoreanError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(DoreanError::Io(e)),
    }
}

/// Write the master todo list.
pub fn write_master(cwd: &Path, items: &[TodoItem]) -> Result<(), DoreanError> {
    let path = master_path(cwd);
    ensure_dir(&path)?;
    fs::write(&path, render(items))?;
    Ok(())
}

/// Read the master todo list (empty when absent).
pub fn read_master(cwd: &Path) -> Result<Vec<TodoItem>, DoreanError> {
    Ok(parse(&read_or_empty(&master_path(cwd))?))
}

/// Write one agent's todo file.
pub fn write_agent(cwd: &Path, agent: &str, items: &[TodoItem]) -> Result<(), DoreanError> {
    let path = agent_path(cwd, agent);
    ensure_dir(&path)?;
    let owned: Vec<TodoItem> = items.iter().filter(|i| i.agent == agent).cloned().collect();
    fs::write(&path, render(&owned))?;
    Ok(())
}

/// Read one agent's todo file.
pub fn read_agent(cwd: &Path, agent: &str) -> Result<Vec<TodoItem>, DoreanError> {
    let items = parse(&read_or_empty(&agent_path(cwd, agent))?);
    Ok(items.into_iter().filter(|i| i.agent == agent).collect())
}

/// Merge updates into a list. Existing items keep their position and take the
/// updated status; brand-new ids are appended. This is the orchestrator's
/// "review and merge" step — only the status of the update is trusted.
pub fn merge(updates: &[TodoItem], current: &[TodoItem]) -> Vec<TodoItem> {
    let mut merged = current.to_vec();
    for update in updates {
        if let Some(existing) = merged.iter_mut().find(|i| i.id == update.id) {
            existing.status = update.status;
        } else {
            merged.push(update.clone());
        }
    }
    merged
}

/// A sub-agent's items, reviewed and merged into the master list by the
/// orchestrator. Updates from a different agent are ignored.
pub fn review_agent(cwd: &Path, agent: &str) -> Result<Vec<TodoItem>, DoreanError> {
    let updates = read_agent(cwd, agent)?;
    let current = read_master(cwd)?;
    let reviewed: Vec<TodoItem> = updates.into_iter().filter(|u| u.agent == agent).collect();
    let merged = merge(&reviewed, &current);
    write_master(cwd, &merged)?;
    Ok(merged)
}

/// Count of done items out of total, for progress reporting.
pub fn progress(items: &[TodoItem]) -> (usize, usize) {
    let total = items.len();
    let done = items
        .iter()
        .filter(|i| i.status == TodoStatus::Done)
        .count();
    (done, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dorean-todos-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ids_are_deterministic_slugs() {
        assert_eq!(
            item_id("backend", "Set up the API server!"),
            "backend:set-up-the-api-server"
        );
        assert_eq!(
            item_id("db", "Write migrations"),
            item_id("db", "Write migrations")
        );
        assert!(item_id("a", "x").starts_with('a'));
    }

    #[test]
    fn parses_and_round_trips() {
        let items = vec![
            TodoItem::new("backend", "set up scaffold"),
            TodoItem::new("backend", "implement auth"),
            TodoItem::new("db", "migrations"),
        ];
        let text = render(&items);
        let parsed = parse(&text);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].agent, "backend");
        assert_eq!(parsed[2].agent, "db");
        assert_eq!(parsed[0].status, TodoStatus::Pending);
    }

    #[test]
    fn parses_status_markers() {
        let text = "# t\n\n## a\n- [ ] p\n- [~] ip\n- [x] d\n- [!] b\n";
        let parsed = parse(text);
        assert_eq!(parsed[0].status, TodoStatus::Pending);
        assert_eq!(parsed[1].status, TodoStatus::InProgress);
        assert_eq!(parsed[2].status, TodoStatus::Done);
        assert_eq!(parsed[3].status, TodoStatus::Blocked);
    }

    #[test]
    fn merges_updates_and_appends_new() {
        let current = vec![TodoItem::new("backend", "a"), TodoItem::new("backend", "b")];
        let mut done = current[0].clone();
        done.status = TodoStatus::Done;
        let updates = vec![done, TodoItem::new("backend", "c")];

        let merged = merge(&updates, &current);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].status, TodoStatus::Done);
        assert_eq!(merged[2].title, "c");
    }

    #[test]
    fn master_and_agent_files_round_trip() {
        let dir = temp_dir("files");
        let items = vec![TodoItem::new("backend", "a"), TodoItem::new("db", "b")];
        write_master(&dir, &items).unwrap();
        write_agent(&dir, "backend", &items).unwrap();

        assert_eq!(read_master(&dir).unwrap().len(), 2);
        let agent_items = read_agent(&dir, "backend").unwrap();
        assert_eq!(agent_items.len(), 1);
        assert_eq!(agent_items[0].agent, "backend");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_agent_merges_into_master() {
        let dir = temp_dir("review");
        let initial = vec![TodoItem::new("backend", "a")];
        write_master(&dir, &initial).unwrap();
        write_agent(&dir, "backend", &initial).unwrap();

        let mut done = initial[0].clone();
        done.status = TodoStatus::Done;
        write_agent(&dir, "backend", &[done]).unwrap();

        let merged = review_agent(&dir, "backend").unwrap();
        assert_eq!(merged[0].status, TodoStatus::Done);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn progress_counts_done() {
        let mut items = vec![TodoItem::new("a", "x"), TodoItem::new("a", "y")];
        items[0].status = TodoStatus::Done;
        assert_eq!(progress(&items), (1, 2));
    }
}
