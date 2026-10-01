//! Project memory: AGENTS.md / CLAUDE.md loader plus skills.
//!
//! Parity with Codex (`AGENTS.md`) and Claude Code (`CLAUDE.md` + skills):
//! walk from the filesystem root down to `cwd`, loading each memory file
//! found (capped), so deeper files override higher ones. Files over the cap
//! are truncated with a marker rather than failing.

use std::path::{Path, PathBuf};

/// Memory filenames checked at each directory level, in priority order.
pub const MEMORY_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md"];
/// Skills directory inside the repo (`.dorean/skills/<name>/SKILL.md`).
pub const SKILLS_DIR: &str = ".dorean/skills";
/// Per-file cap so a giant memory file can't eat the context window.
pub const MAX_MEMORY_CHARS: usize = 12_000;

/// Loaded project memory.
#[derive(Debug, Clone, Default)]
pub struct ProjectMemory {
    /// Concatenated memory files, root-first.
    pub text: String,
    /// Files that contributed, for debugging / status display.
    pub sources: Vec<PathBuf>,
}

impl ProjectMemory {
    /// Load memory for `cwd`: root → cwd chain for AGENTS.md/CLAUDE.md, plus
    /// repo skills. Never errors — missing files yield empty memory.
    pub fn load(cwd: &Path) -> Self {
        let mut text = String::new();
        let mut sources = Vec::new();
        for dir in ancestors(cwd) {
            for name in MEMORY_FILES {
                let path = dir.join(name);
                if let Ok(content) = std::fs::read_to_string(&path) {
                    sources.push(path.clone());
                    text.push_str(&format!("\n## Project memory ({})\n", path.display()));
                    text.push_str(&truncate(&content, MAX_MEMORY_CHARS));
                    text.push('\n');
                }
            }
        }
        // Repo skills: each SKILL.md contributes its head.
        let skills = cwd.join(SKILLS_DIR);
        if let Ok(entries) = std::fs::read_dir(&skills) {
            let mut names: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            names.sort_by_key(|e| e.file_name());
            for entry in names {
                let skill = entry.path().join("SKILL.md");
                if let Ok(content) = std::fs::read_to_string(&skill) {
                    sources.push(skill.clone());
                    text.push_str(&format!("\n## Skill ({})\n", skill.display()));
                    text.push_str(&truncate(&content, MAX_MEMORY_CHARS / 2));
                    text.push('\n');
                }
            }
        }
        ProjectMemory { text, sources }
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

fn ancestors(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut cur = Some(cwd.to_path_buf());
    while let Some(dir) = cur {
        dirs.push(dir.clone());
        cur = dir.parent().map(|p| p.to_path_buf());
        if dirs.len() > 32 {
            break;
        }
    }
    dirs.reverse();
    dirs
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}\n… [memory truncated]", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn loads_agents_md_chain() {
        let dir = std::env::temp_dir().join(format!("dorean-mem-{}", std::process::id()));
        let sub = dir.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.join("AGENTS.md"), "use pnpm, not npm").unwrap();
        fs::write(sub.join("CLAUDE.md"), "run tests with pytest").unwrap();
        let mem = ProjectMemory::load(&sub);
        assert!(mem.text.contains("pnpm"));
        assert!(mem.text.contains("pytest"));
        assert_eq!(mem.sources.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_files_yield_empty_memory() {
        let mem = ProjectMemory::load(Path::new("/nonexistent-dorean-xyz"));
        assert!(mem.is_empty());
    }
}
