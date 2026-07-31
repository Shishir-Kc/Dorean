//! Tool approval policy (allow/ask/deny), safe-dir checks, deny list.
//!
//! The policy gates what the model may do:
//! - the **deny list** always blocks matching paths, for every tool;
//! - `write`/`edit` must target a location the agent owns (see [`PermissionPolicy::owned_paths`]);
//! - `bash` is governed by the [`PermissionMode`] (Allow runs it, Ask prompts,
//!   Deny blocks it).
//!
//! In non-interactive contexts (no TTY) `Ask` degrades to `Deny`, so a prompt
//! never hangs a scripted run.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use glob::Pattern;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::error::DoreanError;

/// How strictly tool calls are gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    /// Auto-approve every tool call that clears the deny list.
    Allow,
    /// Prompt before risky operations (writes outside the owned area, `bash`).
    /// Without a TTY this degrades to [`PermissionMode::Deny`].
    Ask,
    /// Block risky operations outright.
    Deny,
}

impl std::str::FromStr for PermissionMode {
    type Err = DoreanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "allow" => Ok(PermissionMode::Allow),
            "ask" => Ok(PermissionMode::Ask),
            "deny" => Ok(PermissionMode::Deny),
            other => Err(DoreanError::Config(format!(
                "unknown permission mode `{other}` (expected allow|ask|deny)"
            ))),
        }
    }
}

/// The approval callback: given a human-readable prompt, returns whether the
/// user approved the operation.
pub type AskFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// The gatekeeper for tool execution. Cheap to build, cloned per agent loop.
pub struct PermissionPolicy {
    pub mode: PermissionMode,
    /// Compiled glob patterns of paths that may never be read or written.
    deny_list: Vec<Pattern>,
    /// Roots an agent may write inside. Defaults to the working directory.
    safe_dirs: Vec<PathBuf>,
    /// When set, writes must land inside one of these roots instead of
    /// `safe_dirs` — used to confine sub-agents to their owned paths.
    owned_paths: Option<Vec<PathBuf>>,
    ask: Option<AskFn>,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        PermissionPolicy {
            mode: PermissionMode::Allow,
            deny_list: Vec::new(),
            safe_dirs: Vec::new(),
            owned_paths: None,
            ask: None,
        }
    }
}

impl fmt::Debug for PermissionPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PermissionPolicy")
            .field("mode", &self.mode)
            .field("deny_list", &self.deny_list.len())
            .field("safe_dirs", &self.safe_dirs)
            .field("owned_paths", &self.owned_paths)
            .field("ask", &self.ask.is_some())
            .finish()
    }
}

impl Clone for PermissionPolicy {
    fn clone(&self) -> Self {
        PermissionPolicy {
            mode: self.mode,
            deny_list: self.deny_list.clone(),
            safe_dirs: self.safe_dirs.clone(),
            owned_paths: self.owned_paths.clone(),
            ask: self.ask.clone(),
        }
    }
}

impl PermissionPolicy {
    /// Everything allowed (within the deny list). Used by tests.
    pub fn allow_all() -> Self {
        PermissionPolicy::default()
    }

    /// Policy for the primary agent from config, rooted at `cwd`.
    ///
    /// When the mode is `Ask` and stdin is a TTY, approvals are read from the
    /// terminal; otherwise `Ask` degrades to `Deny`.
    pub fn from_config(config: &Config, cwd: &Path) -> Self {
        let mode = config.permission_mode.unwrap_or(PermissionMode::Ask);
        let ask = (mode == PermissionMode::Ask && is_tty())
            .then(|| Arc::new(ask_via_stdin) as Arc<dyn Fn(&str) -> bool + Send + Sync>);
        Self::build(config, cwd, mode, ask)
    }

    /// Policy for the primary agent that routes approvals to an external
    /// callback (used by the TUI, which shows an overlay instead of reading
    /// stdin). `Ask` with a callback that always denies degrades safely.
    pub fn interactive(config: &Config, cwd: &Path, ask: AskFn) -> Self {
        let mode = config.permission_mode.unwrap_or(PermissionMode::Ask);
        Self::build(config, cwd, mode, Some(ask))
    }

    fn build(config: &Config, cwd: &Path, mode: PermissionMode, ask: Option<AskFn>) -> Self {
        let deny_list = config
            .permission_deny
            .iter()
            .filter_map(|p| Pattern::new(p).ok())
            .collect();
        let safe_dirs = if config.safe_dirs.is_empty() {
            vec![cwd.to_path_buf()]
        } else {
            config.safe_dirs.clone()
        };
        PermissionPolicy {
            mode,
            deny_list,
            safe_dirs,
            owned_paths: None,
            ask,
        }
    }

    /// Policy that confines writes to an agent's owned paths (Phase 5).
    pub fn for_owned_paths(
        mode: PermissionMode,
        cwd: &Path,
        owned_paths: &[PathBuf],
        deny_list: &[String],
    ) -> Self {
        PermissionPolicy {
            mode,
            deny_list: deny_list
                .iter()
                .filter_map(|p| Pattern::new(p).ok())
                .collect(),
            safe_dirs: vec![cwd.to_path_buf()],
            owned_paths: Some(owned_paths.to_vec()),
            ask: (mode == PermissionMode::Ask && is_tty())
                .then(|| Arc::new(ask_via_stdin) as Arc<dyn Fn(&str) -> bool + Send + Sync>),
        }
    }

    /// Root directories a write may target: the owned paths when set,
    /// otherwise the safe dirs.
    pub fn roots(&self) -> &[PathBuf] {
        match &self.owned_paths {
            Some(paths) => paths,
            None => &self.safe_dirs,
        }
    }

    /// Whether `path` is matched by a deny-list glob. Both the absolute path
    /// and the path relative to `cwd` are checked, so patterns like
    /// `secrets/**` work against absolute paths.
    pub fn is_denied(&self, cwd: &Path, path: &Path) -> bool {
        let mut candidates = vec![path.to_path_buf()];
        if let Ok(rel) = path.strip_prefix(cwd) {
            candidates.push(rel.to_path_buf());
        }
        self.deny_list
            .iter()
            .any(|p| candidates.iter().any(|c| p.matches_path(c)))
    }

    /// Whether `path` lies inside one of the write roots.
    pub fn within_owned(&self, path: &Path) -> bool {
        self.roots().iter().any(|root| path.starts_with(root))
    }

    /// Gate a tool call before dispatch. Returns `Ok(())` when it may run.
    pub fn check_tool(&self, name: &str, args: &Value, cwd: &Path) -> Result<(), String> {
        // The deny list applies to every path-bearing argument.
        for key in ["path", "cwd"] {
            if let Some(p) = args.get(key).and_then(Value::as_str) {
                let resolved = resolve(cwd, p);
                if self.is_denied(cwd, &resolved) {
                    return Err(format!(
                        "{name}: path is on the deny list: {}",
                        resolved.display()
                    ));
                }
            }
        }

        match name {
            "write" | "edit" => {
                let Some(raw) = args.get("path").and_then(Value::as_str) else {
                    return Ok(());
                };
                let resolved = resolve(cwd, raw);
                if self.within_owned(&resolved) {
                    return Ok(());
                }
                match self.mode {
                    PermissionMode::Allow => Ok(()),
                    PermissionMode::Ask => self.ask_or_deny(&format!(
                        "write {} (outside owned area)",
                        resolved.display()
                    )),
                    PermissionMode::Deny => Err(format!(
                        "{name} {}: outside the owned area and writes are denied",
                        resolved.display()
                    )),
                }
            }
            "bash" => match self.mode {
                PermissionMode::Allow => Ok(()),
                PermissionMode::Ask => self.ask_or_deny("run a bash command"),
                PermissionMode::Deny => {
                    Err("bash is disabled by the permission policy".to_string())
                }
            },
            // Read-only discovery tools are always permitted (modulo the
            // deny list), so sub-agents can inspect the whole repo.
            _ => Ok(()),
        }
    }

    fn ask_or_deny(&self, prompt: &str) -> Result<(), String> {
        match &self.ask {
            Some(ask) if ask(prompt) => Ok(()),
            Some(_) => Err(format!("`{prompt}` was not approved")),
            None => Err(format!(
                "`{prompt}` requires approval but this is a non-interactive run"
            )),
        }
    }
}

/// Resolve a possibly-relative path against the working directory.
fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Whether stdin is a terminal (interactive).
fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

/// Read an approval from the terminal. Blocking, but only invoked on the rare
/// interactive permission prompt.
fn ask_via_stdin(prompt: &str) -> bool {
    use std::io::Write;
    eprint!("[dorean] approve `{prompt}`? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_ok() {
        matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned_policy(mode: PermissionMode) -> PermissionPolicy {
        PermissionPolicy::for_owned_paths(
            mode,
            Path::new("/repo"),
            &[PathBuf::from("/repo/src")],
            &["*.lock".to_string(), "secrets/**".to_string()],
        )
    }

    #[test]
    fn deny_list_blocks_every_tool() {
        let policy = owned_policy(PermissionMode::Allow);
        let args = serde_json::json!({ "path": "secrets/keys.json" });
        let err = policy
            .check_tool("read", &args, Path::new("/repo"))
            .unwrap_err();
        assert!(err.contains("deny list"));
    }

    #[test]
    fn deny_list_matches_globs() {
        let policy = owned_policy(PermissionMode::Allow);
        let cwd = Path::new("/repo");
        assert!(policy.is_denied(cwd, Path::new("/repo/Cargo.lock")));
        assert!(policy.is_denied(cwd, Path::new("/repo/secrets/keys.json")));
        assert!(!policy.is_denied(cwd, Path::new("/repo/src/main.rs")));
    }

    #[test]
    fn write_within_owned_path_is_allowed() {
        let policy = owned_policy(PermissionMode::Deny);
        let args = serde_json::json!({ "path": "src/main.rs" });
        assert!(
            policy
                .check_tool("write", &args, Path::new("/repo"))
                .is_ok()
        );
    }

    #[test]
    fn write_outside_owned_path_is_denied_in_deny_mode() {
        let policy = owned_policy(PermissionMode::Deny);
        let args = serde_json::json!({ "path": "elsewhere/main.rs" });
        let err = policy
            .check_tool("edit", &args, Path::new("/repo"))
            .unwrap_err();
        assert!(err.contains("outside the owned area"));
    }

    #[test]
    fn write_outside_owned_path_allowed_in_allow_mode() {
        let policy = owned_policy(PermissionMode::Allow);
        let args = serde_json::json!({ "path": "elsewhere/main.rs" });
        assert!(
            policy
                .check_tool("write", &args, Path::new("/repo"))
                .is_ok()
        );
    }

    #[test]
    fn write_outside_owned_path_denied_in_ask_without_tty() {
        let policy = owned_policy(PermissionMode::Ask);
        let args = serde_json::json!({ "path": "elsewhere/main.rs" });
        let err = policy
            .check_tool("write", &args, Path::new("/repo"))
            .unwrap_err();
        assert!(err.contains("requires approval"));
    }

    #[test]
    fn bash_gated_by_mode() {
        let args = serde_json::json!({ "command": "ls" });
        let policy = owned_policy(PermissionMode::Deny);
        assert!(
            policy
                .check_tool("bash", &args, Path::new("/repo"))
                .is_err()
        );

        let policy = owned_policy(PermissionMode::Allow);
        assert!(policy.check_tool("bash", &args, Path::new("/repo")).is_ok());
    }

    #[test]
    fn read_only_tools_pass_in_deny_mode() {
        let policy = owned_policy(PermissionMode::Deny);
        let args = serde_json::json!({ "pattern": "*.rs", "path": "src" });
        assert!(policy.check_tool("grep", &args, Path::new("/repo")).is_ok());
    }

    #[test]
    fn parses_modes() {
        assert_eq!(
            "allow".parse::<PermissionMode>().unwrap(),
            PermissionMode::Allow
        );
        assert_eq!(
            "ASK".parse::<PermissionMode>().unwrap(),
            PermissionMode::Ask
        );
        assert_eq!(
            "deny".parse::<PermissionMode>().unwrap(),
            PermissionMode::Deny
        );
        assert!("bogus".parse::<PermissionMode>().is_err());
    }

    #[test]
    fn from_config_defaults_to_ask_and_cwd() {
        let config = Config::default();
        let policy = PermissionPolicy::from_config(&config, Path::new("/repo"));
        assert_eq!(policy.mode, PermissionMode::Ask);
        assert_eq!(policy.safe_dirs, vec![PathBuf::from("/repo")]);
    }

    #[test]
    fn from_config_honors_explicit_mode_and_deny() {
        let config = Config {
            permission_mode: Some(PermissionMode::Deny),
            permission_deny: vec!["*.lock".to_string()],
            ..Config::default()
        };
        let policy = PermissionPolicy::from_config(&config, Path::new("/repo"));
        assert_eq!(policy.mode, PermissionMode::Deny);
        assert!(policy.is_denied(Path::new("/repo"), Path::new("/repo/Cargo.lock")));
    }
}
