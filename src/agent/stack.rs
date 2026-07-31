//! Stack selection and the deterministic roster builder.
//!
//! The orchestrator turns a stack name into a roster of [`AgentManifest`]s
//! with partitioned owned paths. Roster templates are deterministic; the
//! LLM-driven "prompt analysis" pass is a future enhancement — today the user
//! picks the stack (`--stack`, `--auto-stack`, or an interactive prompt) and
//! every agent's responsibilities reference the user's goal.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use crate::error::DoreanError;

use super::manifest::AgentManifest;

/// Known stack presets.
pub const STACKS: &[&str] = &["full-stack", "backend", "frontend", "cli"];

/// A template for one agent in a stack preset.
struct AgentTemplate {
    name: &'static str,
    role: &'static str,
    responsibilities: Vec<&'static str>,
    owned_paths: Vec<&'static str>,
    /// Restrict the tool set; `None` = full set.
    allowed_tools: Option<Vec<&'static str>>,
}

/// Build the roster for a stack, injecting the user's goal into each role's
/// responsibilities.
pub fn build_roster(stack: &str, goal: &str) -> Result<Vec<AgentManifest>, DoreanError> {
    let templates = templates(stack)?;
    Ok(templates
        .into_iter()
        .map(|t| AgentManifest {
            name: t.name.to_string(),
            role: t.role.to_string(),
            responsibilities: std::iter::once(format!("Work toward the user's goal: {goal}"))
                .chain(t.responsibilities.iter().map(|r| r.to_string()))
                .collect(),
            allowed_tools: t
                .allowed_tools
                .map(|v| v.iter().map(|s| s.to_string()).collect()),
            owned_paths: t.owned_paths.iter().map(PathBuf::from).collect(),
            model: None,
        })
        .collect())
}

fn templates(stack: &str) -> Result<Vec<AgentTemplate>, DoreanError> {
    let agents = match stack {
        "full-stack" => vec![
            AgentTemplate {
                name: "backend",
                role: "API and server",
                responsibilities: vec![
                    "Design and implement the backend server and REST API.",
                    "Own the server entrypoint, routes, handlers, and business logic.",
                    "Verify the server builds and its tests pass.",
                ],
                owned_paths: vec!["backend", "api", "server"],
                allowed_tools: None,
            },
            AgentTemplate {
                name: "db",
                role: "database schema and access",
                responsibilities: vec![
                    "Design the schema, migrations, and data-access layer.",
                    "Seed data if the app needs it.",
                    "Verify migrations apply cleanly.",
                ],
                owned_paths: vec!["db", "database", "migrations", "prisma"],
                allowed_tools: None,
            },
            AgentTemplate {
                name: "frontend-ui-ux",
                role: "UI/UX, styling, assets",
                responsibilities: vec![
                    "Build the visual layer: layout, styling, theme, responsive behavior.",
                    "Own styles, public assets, and design-system tokens.",
                    "Verify the UI builds.",
                ],
                owned_paths: vec![
                    "frontend/ui",
                    "frontend/styles",
                    "frontend/public",
                    "frontend/assets",
                ],
                allowed_tools: None,
            },
            AgentTemplate {
                name: "frontend-logic",
                role: "frontend app logic",
                responsibilities: vec![
                    "Implement the frontend application logic: state, data fetching, routing, wiring the UI to the API.",
                    "Own app source, components, pages, and config files.",
                    "Verify the frontend builds and tests pass.",
                ],
                owned_paths: vec![
                    "frontend/src",
                    "frontend/components",
                    "frontend/pages",
                    "frontend/app",
                    "frontend/package.json",
                    "frontend/package-lock.json",
                    "frontend/tsconfig.json",
                    "frontend/vite.config.ts",
                ],
                allowed_tools: None,
            },
        ],
        "backend" => vec![
            AgentTemplate {
                name: "backend",
                role: "API and server",
                responsibilities: vec![
                    "Design and implement the backend server and REST API.",
                    "Own the server entrypoint, routes, handlers, and business logic.",
                    "Verify the server builds and its tests pass.",
                ],
                owned_paths: vec!["backend", "api", "server"],
                allowed_tools: None,
            },
            AgentTemplate {
                name: "db",
                role: "database schema and access",
                responsibilities: vec![
                    "Design the schema, migrations, and data-access layer.",
                    "Verify migrations apply cleanly.",
                ],
                owned_paths: vec!["db", "database", "migrations", "prisma"],
                allowed_tools: None,
            },
        ],
        "frontend" => vec![
            AgentTemplate {
                name: "frontend-ui-ux",
                role: "UI/UX, styling, assets",
                responsibilities: vec![
                    "Build the visual layer: layout, styling, theme, responsive behavior.",
                    "Own styles, public assets, and design-system tokens.",
                ],
                owned_paths: vec![
                    "frontend/ui",
                    "frontend/styles",
                    "frontend/public",
                    "frontend/assets",
                ],
                allowed_tools: None,
            },
            AgentTemplate {
                name: "frontend-logic",
                role: "frontend app logic",
                responsibilities: vec![
                    "Implement the frontend application logic: state, data fetching, routing, wiring the UI to the API.",
                    "Own app source, components, pages, and config files.",
                ],
                owned_paths: vec![
                    "frontend/src",
                    "frontend/components",
                    "frontend/pages",
                    "frontend/app",
                    "frontend/package.json",
                    "frontend/package-lock.json",
                    "frontend/tsconfig.json",
                    "frontend/vite.config.ts",
                ],
                allowed_tools: None,
            },
        ],
        "cli" | "tool" => vec![AgentTemplate {
            name: "core",
            role: "single tool / CLI",
            responsibilities: vec![
                "Design and implement the CLI tool end to end.",
                "Own the whole repository and verify with a build/tests.",
            ],
            owned_paths: vec!["src", "tests", "bin", "Cargo.toml", "package.json"],
            allowed_tools: None,
        }],
        other => {
            return Err(DoreanError::Message(format!(
                "unknown stack `{other}` (expected one of {})",
                STACKS.join(", ")
            )));
        }
    };
    Ok(agents)
}

/// How the stack is chosen for a run.
#[derive(Debug, Clone, Default)]
pub enum StackChoice {
    /// Ask the user on stdin (interactive).
    #[default]
    Prompt,
    /// Pick the default preset (`full-stack`) without asking.
    Auto,
    /// Use an explicit stack name.
    Named(String),
}

/// Resolve the stack for a run: explicit choice, auto default, or prompt.
pub fn select_stack(choice: &StackChoice) -> Result<String, DoreanError> {
    match choice {
        StackChoice::Named(name) => {
            // Validate the name now so typos fail fast.
            templates(name)?;
            Ok(name.clone())
        }
        StackChoice::Auto => Ok("full-stack".to_string()),
        StackChoice::Prompt => prompt_stack(),
    }
}

fn prompt_stack() -> Result<String, DoreanError> {
    if !std::io::stdin().is_terminal() {
        return Err(DoreanError::Message(
            "no stack given: pass --stack <name> or --auto-stack (non-interactive run)".to_string(),
        ));
    }
    eprintln!("[dorean] which stack should the agents build?");
    for (i, stack) in STACKS.iter().enumerate() {
        eprintln!("  {}) {stack}", i + 1);
    }
    eprint!("choice [1]: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Ok(STACKS[0].to_string());
    }
    let input = line.trim();
    if input.is_empty() {
        return Ok(STACKS[0].to_string());
    }
    if let Ok(n) = input.parse::<usize>()
        && (1..=STACKS.len()).contains(&n)
    {
        return Ok(STACKS[n - 1].to_string());
    }
    if STACKS.contains(&input) {
        return Ok(input.to_string());
    }
    Ok(STACKS[0].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_stack_roster_partitions_paths() {
        let roster = build_roster("full-stack", "Build a notes app").unwrap();
        assert_eq!(roster.len(), 4);
        let names: Vec<&str> = roster.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"backend"));
        assert!(names.contains(&"db"));
        assert!(names.contains(&"frontend-ui-ux"));
        assert!(names.contains(&"frontend-logic"));
        assert!(roster[0].responsibilities[0].contains("Build a notes app"));
        assert!(roster[0].allowed_tools.is_none());
    }

    #[test]
    fn cli_stack_yields_single_agent() {
        let roster = build_roster("cli", "make a linter").unwrap();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].name, "core");
    }

    #[test]
    fn owned_paths_do_not_overlap_between_agents() {
        let roster = build_roster("full-stack", "x").unwrap();
        for i in 0..roster.len() {
            for j in (i + 1)..roster.len() {
                let a = &roster[i].owned_paths;
                let b = &roster[j].owned_paths;
                for pa in a {
                    for pb in b {
                        assert!(
                            !pa.starts_with(pb) && !pb.starts_with(pa),
                            "overlapping owned paths {} and {}",
                            pa.display(),
                            pb.display()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_stack_is_rejected() {
        assert!(build_roster("bogus", "x").is_err());
        assert!(select_stack(&StackChoice::Named("bogus".to_string())).is_err());
    }

    #[test]
    fn auto_chooses_full_stack() {
        assert_eq!(select_stack(&StackChoice::Auto).unwrap(), "full-stack");
    }
}
