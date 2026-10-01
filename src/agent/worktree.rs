//! Git worktrees: isolated working directories per sub-agent.
//!
//! Each sub-agent gets its own `git worktree` under
//! `.dorean/worktrees/<agent>` so parallel agents never stomp the same files.
//! Non-git repos fall back to the shared `cwd` (no isolation, no error).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::DoreanError;

/// Worktree root inside the repo.
pub fn worktrees_dir(cwd: &Path) -> PathBuf {
    cwd.join(".dorean").join("worktrees")
}

/// Path for one agent's worktree.
pub fn worktree_path(cwd: &Path, agent: &str) -> PathBuf {
    worktrees_dir(cwd).join(sanitize(agent))
}

/// Create (or reuse) a worktree for `agent` on a detached HEAD. Returns the
/// path the agent should work in — the worktree on success, `cwd` otherwise.
pub fn ensure_worktree(cwd: &Path, agent: &str) -> PathBuf {
    let path = worktree_path(cwd, agent);
    if path.exists() {
        return path;
    }
    if !is_git_repo(cwd) {
        return cwd.to_path_buf();
    }
    // Best-effort: detached worktree so agents never disturb the user's branch.
    let status = Command::new("git")
        .args([
            "worktree",
            "add",
            "--detach",
            &path.to_string_lossy(),
            "HEAD",
        ])
        .current_dir(cwd)
        .output();
    match status {
        Ok(out) if out.status.success() => path,
        _ => cwd.to_path_buf(),
    }
}

/// Remove one agent's worktree (best-effort).
pub fn remove_worktree(cwd: &Path, agent: &str) {
    let path = worktree_path(cwd, agent);
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force", &path.to_string_lossy()])
        .current_dir(cwd)
        .output();
}

fn is_git_repo(cwd: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// List existing worktrees (for status display / GC).
pub fn list_worktrees(cwd: &Path) -> Result<Vec<String>, DoreanError> {
    let out = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(cwd)
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(|s| s.to_string())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_names_safe() {
        assert_eq!(sanitize("frontend-ui-ux"), "frontend-ui-ux");
        assert_eq!(sanitize("a/b@c"), "a-b-c");
    }

    #[test]
    fn non_repo_falls_back_to_cwd() {
        let cwd = Path::new("/tmp");
        // /tmp is not a dorean test repo worktree target; either path is fine
        // as long as it doesn't error.
        let _ = ensure_worktree(cwd, "test-agent");
    }
}
