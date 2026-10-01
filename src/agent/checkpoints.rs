//! Per-turn checkpoints: snapshot edited files so `/rewind` can restore.
//!
//! Checkpoints cover the agent's file edits (write/edit tools). They never
//! cover user edits or `bash` side-effects — same rule as Claude Code.

use std::path::{Path, PathBuf};

use crate::error::DoreanError;

/// A single checkpoint: id + captured file contents.
#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub id: String,
    pub files: Vec<(PathBuf, Option<String>)>,
}

impl Checkpoint {
    /// Capture the current contents of `paths` (None = file did not exist).
    pub fn capture(id: impl Into<String>, cwd: &Path, paths: &[PathBuf]) -> Self {
        let mut files = Vec::new();
        for p in paths {
            let full = if p.is_absolute() {
                p.clone()
            } else {
                cwd.join(p)
            };
            let content = std::fs::read_to_string(&full).ok();
            files.push((full, content));
        }
        Checkpoint {
            id: id.into(),
            files,
        }
    }

    /// Restore captured contents.
    pub fn restore(&self) -> Result<(), DoreanError> {
        for (path, content) in &self.files {
            match content {
                Some(text) => {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).map_err(DoreanError::Io)?;
                    }
                    std::fs::write(path, text).map_err(DoreanError::Io)?;
                }
                None => {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        Ok(())
    }
}

/// Checkpoint directory for a repo: `.dorean/checkpoints`.
pub fn checkpoints_dir(cwd: &Path) -> PathBuf {
    cwd.join(".dorean").join("checkpoints")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_and_restore_round_trips() {
        let dir = std::env::temp_dir().join(format!("dorean-ckpt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "v1").unwrap();
        let ckpt = Checkpoint::capture("t1", &dir, &[PathBuf::from("a.txt")]);
        std::fs::write(&file, "v2").unwrap();
        ckpt.restore().unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v1");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
