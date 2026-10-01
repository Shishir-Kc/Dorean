//! Agent manifest: name, role, responsibilities, allowed tools, owned paths.
//!
//! The roster of sub-agents is persisted as `.dorean/agents.json` in the
//! working repo so `--continue` can restore it.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Provider;
use crate::error::DoreanError;

/// Schema for one sub-agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentManifest {
    /// Unique, lowercase-with-hyphens name (`backend`, `frontend-ui-ux`).
    pub name: String,
    /// Short role description used in the roster strip.
    pub role: String,
    /// What this agent is responsible for (fed into its system prompt).
    pub responsibilities: Vec<String>,
    /// Restrict the tool set; `None` means the full set.
    pub allowed_tools: Option<Vec<String>>,
    /// Paths (relative to the working directory) the agent may write inside.
    /// Everything else is read-only.
    pub owned_paths: Vec<PathBuf>,
    /// Optional model override (e.g. a cheaper model for a big role).
    pub model: Option<String>,
    /// Optional provider override (set by the `/make` model picker when the
    /// chosen model lives on another provider). `None` = main provider.
    pub provider: Option<Provider>,
}

/// Resolve an owned path against the working directory.
pub fn resolve_owned(cwd: &Path, owned: &Path) -> PathBuf {
    if owned.is_absolute() {
        owned.to_path_buf()
    } else {
        cwd.join(owned)
    }
}

/// Path to the roster file, `.dorean/agents.json`.
pub fn roster_path(cwd: &Path) -> PathBuf {
    cwd.join(".dorean").join("agents.json")
}

/// Persist the roster.
pub fn write_roster(cwd: &Path, agents: &[AgentManifest]) -> Result<(), DoreanError> {
    let path = roster_path(cwd);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(agents)?;
    fs::write(&path, json)?;
    Ok(())
}

/// Load the roster, or `None` when no run has been orchestrated yet.
pub fn load_roster(cwd: &Path) -> Result<Option<Vec<AgentManifest>>, DoreanError> {
    let path = roster_path(cwd);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(DoreanError::Io(e)),
    };
    let agents = serde_json::from_str(&text)?;
    Ok(Some(agents))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_round_trips() {
        let dir = std::env::temp_dir().join(format!("dorean-roster-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let agents = vec![AgentManifest {
            name: "backend".to_string(),
            role: "API & server".to_string(),
            responsibilities: vec!["build the API".to_string()],
            allowed_tools: Some(vec!["read".to_string(), "write".to_string()]),
            owned_paths: vec![PathBuf::from("backend")],
            model: Some("meta-llama/llama-3.3-70b-instruct:free".to_string()),
            provider: None,
        }];

        write_roster(&dir, &agents).unwrap();
        let loaded = load_roster(&dir).unwrap().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "backend");
        assert_eq!(loaded[0].allowed_tools.as_ref().unwrap().len(), 2);
        assert_eq!(loaded[0].owned_paths[0], PathBuf::from("backend"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_roster_is_none() {
        let dir = std::env::temp_dir().join(format!("dorean-noroster-{}", std::process::id()));
        assert!(load_roster(&dir).unwrap().is_none());
    }

    #[test]
    fn resolves_owned_paths() {
        assert_eq!(
            resolve_owned(Path::new("/repo"), Path::new("backend")),
            PathBuf::from("/repo/backend")
        );
        assert_eq!(
            resolve_owned(Path::new("/repo"), Path::new("/abs/path")),
            PathBuf::from("/abs/path")
        );
    }
}
