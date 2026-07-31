//! `.dorean/SPEC.md` — the orchestrator's project plan.
//!
//! Written by the orchestrator before spawning sub-agents and read-only for
//! sub-agents (their owned-path policies never include this file). Describes
//! the goal, the stack, the architecture, and the per-agent breakdown.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::DoreanError;

use super::manifest::AgentManifest;

/// Path to the plan, `.dorean/SPEC.md`.
pub fn spec_path(cwd: &Path) -> PathBuf {
    cwd.join(".dorean").join("SPEC.md")
}

/// Render the plan markdown for a stack and roster.
pub fn render(goal: &str, stack: &str, agents: &[AgentManifest]) -> String {
    let mut out = String::new();
    out.push_str("# SPEC\n\n");
    out.push_str("_Written by the Dorean orchestrator. Sub-agents must follow this plan; they may not modify it._\n\n");
    out.push_str(&format!("## Goal\n\n{goal}\n\n"));
    out.push_str(&format!("## Stack\n\n`{stack}`\n\n"));
    out.push_str("## Architecture\n\n");
    out.push_str(&format!("{} parallel sub-agents.\n\n", agents.len()));
    for agent in agents {
        out.push_str(&format!("### `{}` — {}\n", agent.name, agent.role));
        out.push_str("\nResponsibilities:\n");
        for r in &agent.responsibilities {
            out.push_str(&format!("- {r}\n"));
        }
        out.push_str("\nOwned paths (writable):\n");
        for p in &agent.owned_paths {
            out.push_str(&format!("- `{}`\n", p.display()));
        }
        if let Some(model) = &agent.model {
            out.push_str(&format!("\nModel: `{model}`\n"));
        }
        out.push('\n');
    }
    out.push_str("## Acceptance criteria\n\n");
    out.push_str(
        "- Each sub-agent completes its todos and verifies its work (build/tests where available).\n\
         - The orchestrator merges per-agent todo updates into `.dorean/todos.md`.\n\
         - No agent writes outside its owned paths; cross-path changes require the orchestrator.\n",
    );
    out
}

/// Write the plan. Fails if a file already exists unless `overwrite`.
pub fn write(
    cwd: &Path,
    goal: &str,
    stack: &str,
    agents: &[AgentManifest],
) -> Result<PathBuf, DoreanError> {
    let path = spec_path(cwd);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, render(goal, stack, agents))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn renders_plan_with_agents() {
        let agents = vec![AgentManifest {
            name: "backend".to_string(),
            role: "API".to_string(),
            responsibilities: vec!["build the REST API".to_string()],
            owned_paths: vec![PathBuf::from("backend")],
            ..AgentManifest::default()
        }];
        let text = render("Build a notes app", "full-stack", &agents);
        assert!(text.contains("Build a notes app"));
        assert!(text.contains("`backend` — API"));
        assert!(text.contains("build the REST API"));
        assert!(text.contains("`backend`"));
        assert!(text.contains("Acceptance criteria"));
    }

    #[test]
    fn writes_spec_file() {
        let dir = std::env::temp_dir().join(format!("dorean-spec-{}", std::process::id()));
        let path = write(&dir, "goal", "stack", &[]).unwrap();
        assert!(path.ends_with("SPEC.md"));
        assert!(fs::read_to_string(&path).unwrap().contains("goal"));
        let _ = fs::remove_dir_all(&dir);
    }
}
