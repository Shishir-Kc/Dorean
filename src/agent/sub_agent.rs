//! The parallel sub-agent loop: claim → work → verify → mark done.
//!
//! Each sub-agent runs its own [`crate::agent::AgentLoop`] confined to its
//! owned paths, with a restricted tool set plus a `todo` tool that updates its
//! per-agent todo file. It loops through rounds until all of its todos are
//! done, the abort fires, or a stall is detected.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;

use crate::agent::agent_loop::{AbortHandle, AgentIdentity, AgentLoop};
use crate::agent::events::{AgentEvent, StreamEvent};
use crate::agent::manifest::{AgentManifest, resolve_owned};
use crate::agent::router::{Mention, Routing};
use crate::agent::todos::{TodoItem, TodoStatus, agent_path, progress, read_agent, write_agent};
use crate::agent::tools::{Tool, ToolContext, ToolOutput, ToolRegistry};
use crate::config::Config;
use crate::error::DoreanError;
use crate::history;
use crate::permissions::PermissionPolicy;

/// Everything a sub-agent needs to run.
pub struct SubAgentSettings {
    pub config: Config,
    pub cwd: PathBuf,
    pub manifest: AgentManifest,
    pub abort: AbortHandle,
    /// Inbound `@agent` mentions routed to this agent.
    pub inbox: mpsc::UnboundedReceiver<Mention>,
    /// Where to publish orchestration events.
    pub events: broadcast::Sender<AgentEvent>,
    /// Restore this agent's session (`--continue`).
    pub resume: bool,
    /// Hard cap on work rounds (the TUI lets the user interrupt instead).
    pub max_rounds: usize,
    /// Consecutive rounds with no todo change before giving up.
    pub stall_limit: usize,
}

/// Outcome of one sub-agent run.
#[derive(Debug, Clone)]
pub struct SubAgentResult {
    pub name: String,
    pub response: String,
    pub rounds: usize,
    pub todos_done: usize,
    pub todos_total: usize,
    pub aborted: bool,
}

impl SubAgentResult {
    pub fn status(&self) -> &'static str {
        if self.aborted {
            "aborted"
        } else if self.todos_total > 0 && self.todos_done == self.todos_total {
            "done"
        } else {
            "incomplete"
        }
    }
}

/// The `todo` tool: lets a sub-agent mark its own todo items done.
pub struct TodoTool {
    cwd: PathBuf,
    agent: String,
}

impl TodoTool {
    pub fn new(cwd: PathBuf, agent: String) -> Self {
        TodoTool { cwd, agent }
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str {
        "todo"
    }

    fn description(&self) -> &str {
        "Update one of your todo items. Provide `title` (or `id`) and the new `status` \
         (pending | in_progress | done | blocked). Call this with status \"done\" when you \
         finish a task, so the orchestrator can track progress."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "The todo id (from the todo list)." },
                "title": { "type": "string", "description": "The todo title, when you don't know its id." },
                "status": { "type": "string", "enum": ["pending", "in_progress", "done", "blocked"] }
            },
            "required": ["status"],
            "additionalProperties": false
        })
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, DoreanError> {
        let status = parse_status(&args)?;
        let mut items = read_agent(&self.cwd, &self.agent)?;

        let title = args
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_string);
        let id = args.get("id").and_then(Value::as_str).map(str::to_string);

        let target = items
            .iter_mut()
            .find(|i| Some(&i.id) == id.as_ref() || Some(&i.title) == title.as_ref());

        let message = match target {
            Some(item) => {
                let old = item.status;
                item.status = status;
                format!("todo {}: {} -> {}", item.id, old, item.status)
            }
            None => match title {
                Some(t) => {
                    let mut item = TodoItem::new(&self.agent, t);
                    item.status = status;
                    items.push(item.clone());
                    write_agent(&self.cwd, &self.agent, &items)?;
                    format!("todo {}: created as {}", item.id, item.status)
                }
                None => {
                    return Ok(ToolOutput::new(
                        "todo: no matching item; provide `title` or `id`".to_string(),
                    ));
                }
            },
        };

        write_agent(&self.cwd, &self.agent, &items)?;
        Ok(ToolOutput::new(message))
    }
}

fn parse_status(args: &Value) -> Result<TodoStatus, DoreanError> {
    let raw = args
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| DoreanError::Message("todo: missing string `status`".to_string()))?;
    match raw {
        "pending" => Ok(TodoStatus::Pending),
        "in_progress" | "in-progress" | "in progress" => Ok(TodoStatus::InProgress),
        "done" | "completed" | "complete" => Ok(TodoStatus::Done),
        "blocked" | "stuck" => Ok(TodoStatus::Blocked),
        other => Err(DoreanError::Message(format!(
            "todo: unknown status `{other}` (pending|in_progress|done|blocked)"
        ))),
    }
}

/// Run one sub-agent to completion.
pub async fn run(settings: SubAgentSettings) -> Result<SubAgentResult, DoreanError> {
    let SubAgentSettings {
        config,
        cwd,
        manifest,
        abort,
        mut inbox,
        events,
        resume,
        max_rounds,
        stall_limit,
    } = settings;

    let name = manifest.name.clone();

    // Confine the agent: restricted tools + owned-path write policy.
    let allowed: Vec<&str> = manifest
        .allowed_tools
        .as_ref()
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_else(|| {
            vec![
                "read",
                "write",
                "edit",
                "bash",
                "bash_background",
                "bash_poll",
                "bash_kill",
                "glob",
                "grep",
                "list",
            ]
        });
    let mut tools = ToolRegistry::builtin_filtered(&allowed);
    tools.register(TodoTool::new(cwd.clone(), name.clone()));

    let mut owned: Vec<PathBuf> = manifest
        .owned_paths
        .iter()
        .map(|p| resolve_owned(&cwd, p))
        .collect();
    owned.push(agent_path(&cwd, &name));

    let mode = config
        .permission_mode
        .unwrap_or(crate::permissions::PermissionMode::Allow);
    let policy = PermissionPolicy::for_owned_paths(mode, &cwd, &owned, &config.permission_deny);

    let mut agent_config = config.clone();
    // Cross-provider pick from the `/make` model picker: run this sub-agent
    // against its own provider instead of the main one.
    if let Some(provider) = &manifest.provider {
        agent_config.provider = *provider;
    }
    let mut agent = AgentLoop::custom(&agent_config, &cwd, tools, policy)?;
    agent.set_abort(abort.clone());
    // Dual-model: explicit manifest pick wins, then executor_model (cheap),
    // then the loop's own config.model/provider default.
    agent.set_model(
        manifest
            .model
            .clone()
            .or_else(|| config.executor_model.clone()),
    );
    agent.set_agent_identity(AgentIdentity {
        name: name.clone(),
        role: manifest.role.clone(),
        responsibilities: manifest.responsibilities.clone(),
        owned_paths: owned,
    });

    // Stream text deltas out as `AgentMessage` events so the TUI can render
    // per-agent bubbles live. A dedicated forwarding task keeps the broadcast
    // channel from ever blocking the agent loop.
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<StreamEvent>();
    let events_for_stream = events.clone();
    let agent_name = name.clone();
    tokio::spawn(async move {
        while let Some(event) = stream_rx.recv().await {
            if let StreamEvent::Delta(text) = event {
                let _ = events_for_stream.send(AgentEvent::AgentMessage {
                    agent: agent_name.clone(),
                    text,
                });
            }
        }
    });
    agent.set_stream_sink(Some(stream_tx));

    if resume && let Some(record) = history::load_latest_named(&cwd, &name)? {
        agent.load_messages(record.messages);
    }

    let _ = events.send(AgentEvent::AgentStarted {
        name: name.clone(),
        role: manifest.role.clone(),
    });

    let mut rounds = 0usize;
    let mut aborted = false;
    let mut last_response = String::new();
    let mut stalls = 0usize;

    loop {
        if abort.is_aborted() {
            aborted = true;
            break;
        }
        if rounds >= max_rounds {
            break;
        }

        let current = read_agent(&cwd, &name)?;
        let (done, total) = progress(&current);
        if total > 0 && done == total {
            break;
        }
        if total == 0 {
            break;
        }

        // Claim: promote the first pending item to in-progress.
        let claimed = claim_first(&current);
        if claimed != current {
            write_agent(&cwd, &name, &claimed)?;
        }

        let mentions = drain_inbox(&mut inbox);
        let prompt = build_prompt(&manifest, &claimed, &mentions);

        let summary = match agent.run(&prompt).await {
            Ok(summary) => summary,
            Err(e) => return Err(e),
        };
        rounds += 1;
        if !summary.response.is_empty() {
            last_response = summary.response.clone();
        }

        let after = read_agent(&cwd, &name)?;
        emit_todo_updates(&events, &name, &claimed, &after);

        let (done_after, _) = progress(&after);
        let made_progress = done_after > done;
        if made_progress {
            stalls = 0;
        } else {
            stalls += 1;
        }

        if summary.aborted {
            aborted = true;
            break;
        }
        if stalls >= stall_limit {
            break;
        }
    }

    let record = history::new_record(agent.model(), agent.messages.clone());
    history::append_session_named(&cwd, &name, &record)?;

    let current = read_agent(&cwd, &name)?;
    let (todos_done, todos_total) = progress(&current);
    let _ = events.send(AgentEvent::AgentFinished {
        name: name.clone(),
        status: SubAgentResult {
            name: name.clone(),
            response: String::new(),
            rounds,
            todos_done,
            todos_total,
            aborted,
        }
        .status()
        .to_string(),
    });

    Ok(SubAgentResult {
        name,
        response: last_response,
        rounds,
        todos_done,
        todos_total,
        aborted,
    })
}

/// Mark the first pending item as in-progress.
fn claim_first(items: &[TodoItem]) -> Vec<TodoItem> {
    let mut claimed = items.to_vec();
    if let Some(item) = claimed.iter_mut().find(|i| i.status == TodoStatus::Pending) {
        item.status = TodoStatus::InProgress;
    }
    claimed
}

/// Collect all pending inbound mentions into a readable block.
fn drain_inbox(inbox: &mut mpsc::UnboundedReceiver<Mention>) -> String {
    let mut mentions = Vec::new();
    while let Ok(mention) = inbox.try_recv() {
        mentions.push(mention);
    }
    if mentions.is_empty() {
        return String::new();
    }
    let mut text = String::from("Inbound messages routed to you from other agents:\n");
    for m in mentions {
        text.push_str(&format!("- @{}: {}\n", m.from, m.text));
    }
    text.push('\n');
    text
}

/// Compose the next-round user prompt.
fn build_prompt(manifest: &AgentManifest, items: &[TodoItem], mentions: &str) -> String {
    let mut list = String::new();
    for item in items {
        list.push_str(&format!(
            "- [{}] {} (id: {})\n",
            item.status.marker(),
            item.title,
            item.id
        ));
    }
    format!(
        "Continue working as `{}`.{}\n\
         Your current todos (in `.dorean/todos/{}.md`):\n{list}\n\
         Work on the next item, then mark it done via the `todo` tool. \
         Verify your work (build/tests) before marking items done. \
         When every item is done, give a concise final summary of what you built.",
        manifest.name, mentions, manifest.name
    )
}

/// Publish `TodoUpdated` events for any status change between two snapshots.
fn emit_todo_updates(
    events: &broadcast::Sender<AgentEvent>,
    agent: &str,
    before: &[TodoItem],
    after: &[TodoItem],
) {
    let before_by_id: std::collections::HashMap<&str, &TodoItem> =
        before.iter().map(|i| (i.id.as_str(), i)).collect();
    for item in after {
        match before_by_id.get(item.id.as_str()) {
            Some(prev) if prev.status != item.status => {
                let _ = events.send(AgentEvent::TodoUpdated {
                    agent: agent.to_string(),
                    id: item.id.clone(),
                    status: item.status,
                });
            }
            None => {
                let _ = events.send(AgentEvent::TodoUpdated {
                    agent: agent.to_string(),
                    id: item.id.clone(),
                    status: item.status,
                });
            }
            _ => {}
        }
    }
}

/// Run several sub-agents in parallel, collecting results in completion order.
///
/// `routing` optionally provides a pre-built [`Router`] (plus its receivers)
/// so an external caller — the TUI — can route `@agent` mentions into a live
/// run. When `None`, a private router is created.
#[allow(clippy::too_many_arguments)]
pub async fn run_parallel(
    config: &Config,
    cwd: PathBuf,
    manifests: Vec<AgentManifest>,
    abort: AbortHandle,
    events: broadcast::Sender<AgentEvent>,
    resume: bool,
    max_rounds: usize,
    routing: Option<Routing>,
) -> Result<Vec<SubAgentResult>, DoreanError> {
    let (router, mut receivers) = match routing {
        Some(routing) => (routing.router, routing.receivers),
        None => {
            let (router, receivers) = super::router::Router::new(&manifests);
            (router, receivers)
        }
    };
    let mut set: JoinSet<Result<SubAgentResult, DoreanError>> = JoinSet::new();

    for manifest in manifests {
        let Some(inbox) = receivers.remove(&manifest.name) else {
            continue;
        };
        let settings = SubAgentSettings {
            config: config.clone(),
            cwd: cwd.clone(),
            manifest,
            abort: abort.clone(),
            inbox,
            events: events.clone(),
            resume,
            max_rounds,
            stall_limit: 2,
        };
        set.spawn(async move { run(settings).await });
    }

    let mut results = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(result)) => {
                // Route agent→agent mentions found in final responses.
                let names = router.agent_names();
                let known: std::collections::HashSet<&str> =
                    names.iter().map(|s| s.as_str()).collect();
                for mention in super::router::parse_mentions(&result.response, &known) {
                    if router
                        .route(&result.name, &mention, &result.response)
                        .is_ok()
                    {
                        let _ = events.send(AgentEvent::MentionRouted {
                            to: mention,
                            from: result.name.clone(),
                            text: result.response.clone(),
                        });
                    }
                }
                results.push(result);
            }
            Ok(Err(e)) => return Err(e),
            Err(e) => {
                return Err(DoreanError::Message(format!(
                    "sub-agent task panicked: {e}"
                )));
            }
        }
    }
    Ok(results)
}
