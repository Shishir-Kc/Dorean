//! The orchestrator: stack selection → roster → SPEC + master todos → spawn
//! parallel sub-agents → review and merge todos.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::sync::broadcast;

use crate::agent::events::AgentEvent;
use crate::agent::manifest;
use crate::agent::router::Routing;
use crate::agent::spec;
use crate::agent::stack::{self, StackChoice};
use crate::agent::sub_agent::{self, SubAgentResult};
use crate::agent::todos::{TodoItem, merge, progress, write_agent, write_master};
use crate::config::Config;
use crate::error::DoreanError;

use super::agent_loop::AbortHandle;

/// Orchestrator handle for one orchestrated run.
pub struct Orchestrator {
    pub config: Config,
    pub cwd: PathBuf,
    pub abort: AbortHandle,
    /// Broadcast channel for orchestration events (TUI subscribes).
    pub events: broadcast::Sender<AgentEvent>,
    /// Restore per-agent sessions (`--continue`).
    pub resume: bool,
    /// Per-sub-agent round cap.
    pub max_rounds: usize,
    /// External mention routing (the TUI routes `@agent` mentions into the
    /// run). `None` creates a private router.
    routing: Option<Routing>,
}

/// Result of an orchestrated run.
#[derive(Debug, Clone)]
pub struct OrchestrationSummary {
    pub stack: String,
    pub agents: Vec<SubAgentResult>,
    pub todos: Vec<TodoItem>,
}

impl Orchestrator {
    /// Build an orchestrator for one run. External mention routing can be
    /// attached afterwards with [`Orchestrator::with_routing`].
    pub fn new(
        config: Config,
        cwd: PathBuf,
        abort: AbortHandle,
        events: broadcast::Sender<AgentEvent>,
        resume: bool,
        max_rounds: usize,
    ) -> Self {
        Orchestrator {
            config,
            cwd,
            abort,
            events,
            resume,
            max_rounds,
            routing: None,
        }
    }

    /// Attach external mention routing (see [`Routing`]).
    pub fn with_routing(mut self, routing: Routing) -> Self {
        self.routing = Some(routing);
        self
    }

    /// Run the full pipeline: pick stack, write the plan, spawn sub-agents in
    /// parallel, then merge and review todos. `roster` optionally supplies the
    /// exact roster (with per-agent model picks from the `/make` picker);
    /// `None` rebuilds the deterministic roster from the stack.
    pub async fn run(
        self,
        goal: &str,
        choice: &StackChoice,
        roster: Option<Vec<manifest::AgentManifest>>,
    ) -> Result<OrchestrationSummary, DoreanError> {
        // 1. Stack selection (asked up front, before any agent starts).
        let stack = stack::select_stack(choice)?;
        let _ = self.events.send(AgentEvent::StackPrompt {
            stack: stack.clone(),
        });

        // 2. Roster + plan. An explicit roster (per-agent models chosen in the
        //    TUI) wins; otherwise build from the stack and layer on the
        //    configured `agent_models` for any agent without a pick.
        let roster = match roster {
            Some(mut roster) => {
                apply_agent_models(&mut roster, &self.config.agent_models);
                roster
            }
            None => {
                let mut roster = stack::build_roster(&stack, goal)?;
                apply_agent_models(&mut roster, &self.config.agent_models);
                roster
            }
        };
        manifest::write_roster(&self.cwd, &roster)?;

        let plan_path = spec::write(&self.cwd, goal, &stack, &roster)?;
        let _ = self
            .events
            .send(AgentEvent::PlanCreated { path: plan_path });

        // 3. Seed todos: one item per agent from its first responsibility.
        let mut todos: Vec<TodoItem> = roster
            .iter()
            .map(|agent| {
                let title = agent
                    .responsibilities
                    .first()
                    .cloned()
                    .unwrap_or_else(|| format!("implement the {} role", agent.name));
                TodoItem::new(&agent.name, title)
            })
            .collect();
        write_master(&self.cwd, &todos)?;
        for agent in &roster {
            let owned: Vec<TodoItem> = todos
                .iter()
                .filter(|t| t.agent == agent.name)
                .cloned()
                .collect();
            write_agent(&self.cwd, &agent.name, &owned)?;
        }

        // 4. Run all sub-agents in parallel.
        let results = sub_agent::run_parallel(
            &self.config,
            self.cwd.clone(),
            roster,
            self.abort.clone(),
            self.events.clone(),
            self.resume,
            self.max_rounds,
            self.routing,
        )
        .await?;

        // 5. Orchestrator review & merge: sub-agent files → master.
        for result in &results {
            let updates = crate::agent::todos::read_agent(&self.cwd, &result.name)?;
            todos = merge(&updates, &todos);
        }
        write_master(&self.cwd, &todos)?;

        Ok(OrchestrationSummary {
            stack,
            agents: results,
            todos,
        })
    }
}

/// Render a human-readable summary of an orchestrated run.
pub fn render_summary(summary: &OrchestrationSummary) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Stack `{}` with {} sub-agent(s) finished.\n\n",
        summary.stack,
        summary.agents.len()
    ));
    for result in &summary.agents {
        out.push_str(&format!(
            "- {}: {} (todos {}/{}, {} rounds, {})\n",
            result.name,
            result.status(),
            result.todos_done,
            result.todos_total,
            result.rounds,
            if result.aborted { "aborted" } else { "ok" }
        ));
        if !result.response.is_empty() {
            out.push_str(&indent(&result.response, "    "));
            out.push('\n');
        }
    }
    let (done, total) = progress(&summary.todos);
    out.push_str(&format!("\nMaster todos: {done}/{total} done.\n"));
    out
}

fn indent(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|l| format!("{prefix}{l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The standard `.dorean` layout inside a working repo.
pub fn dorean_dir(cwd: &Path) -> PathBuf {
    cwd.join(".dorean")
}

/// Layer configured per-agent model overrides onto a roster. Only agents that
/// have no explicit model yet are filled, so a `/make` pick always wins.
pub fn apply_agent_models(
    roster: &mut [manifest::AgentManifest],
    overrides: &HashMap<String, String>,
) {
    for agent in roster.iter_mut() {
        if agent.model.is_none()
            && let Some(model) = overrides.get(&agent.name)
        {
            agent.model = Some(model.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::todos::TodoStatus;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dorean-orch-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn renders_summary() {
        let mut todo = TodoItem::new("backend", "build api");
        todo.status = TodoStatus::Done;
        let summary = OrchestrationSummary {
            stack: "full-stack".to_string(),
            agents: vec![SubAgentResult {
                name: "backend".to_string(),
                response: "Built the API".to_string(),
                rounds: 2,
                todos_done: 1,
                todos_total: 1,
                aborted: false,
            }],
            todos: vec![todo],
        };
        let text = render_summary(&summary);
        assert!(text.contains("backend"));
        assert!(text.contains("Built the API"));
        assert!(text.contains("1/1 done"));
    }

    #[test]
    fn dorean_dir_path() {
        assert_eq!(
            dorean_dir(Path::new("/repo")),
            PathBuf::from("/repo/.dorean")
        );
    }

    #[test]
    fn empty_roster_is_valid_but_pointless() {
        // Guards against a stack that produces no agents.
        let dir = temp_dir("empty");
        assert!(write_master(&dir, &[]).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn applies_configured_models_only_when_unset() {
        let mut roster = stack::build_roster("full-stack", "x").unwrap();
        roster[0].model = Some("picked".to_string());
        let overrides: HashMap<String, String> = [
            ("backend".to_string(), "cfg-backend".to_string()),
            ("db".to_string(), "cfg-db".to_string()),
        ]
        .into_iter()
        .collect();
        apply_agent_models(&mut roster, &overrides);
        // An explicit pick wins over the configured override.
        assert_eq!(roster[0].model.as_deref(), Some("picked"));
        assert_eq!(
            roster
                .iter()
                .find(|a| a.name == "db")
                .unwrap()
                .model
                .as_deref(),
            Some("cfg-db")
        );
        // Agents with no configured model stay None.
        assert!(
            roster
                .iter()
                .find(|a| a.name == "frontend-ui-ux")
                .unwrap()
                .model
                .is_none()
        );
    }
}
