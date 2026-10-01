//! Chat TUI: terminal core, renderer, app loop, components, theming.

pub mod app;
pub mod components;
pub mod diff;
pub mod events;
pub mod highlight;
pub mod markdown;
pub mod render;
pub mod terminal;
pub mod theme;

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

use crate::agent::agent_loop::AbortHandle;
use crate::config::Config;
use crate::error::DoreanError;

use self::events::TuiEvent;

/// Commands sent from the UI to the background agent task.
pub enum SessionCmd {
    /// Run the main agent loop on `text`.
    Send { text: String },
    /// Run an orchestrated build: goal + chosen stack + mention routing.
    Orchestrate {
        goal: String,
        stack: String,
        routing: crate::agent::router::Routing,
        roster: Vec<crate::agent::manifest::AgentManifest>,
    },
    /// Set the model for subsequent chat runs.
    SetModel(String),
    /// Switch the active provider and rebuild the agent (chat context is
    /// preserved across the rebuild).
    SetProvider(crate::config::Provider),
    /// Start a fresh agent loop (drops in-memory context).
    Clear,
    /// Abort the current run (via the shared [`AbortHandle`]).
    Abort,
    /// Fetch the model list from every provider in parallel for the model
    /// selector. Runs detached so it never queues behind chat: unreachables
    /// are skipped, never fatal. `id` echoes back in the response events.
    FetchModels { id: u64 },
    /// Persist a new API key for the given provider and rebuild the agent
    /// (chat context is preserved across the rebuild).
    SetApiKey {
        provider: crate::config::Provider,
        key: String,
    },
    /// Change the tool-approval policy and rebuild the agent.
    SetPermission(crate::permissions::PermissionMode),
    /// Load a saved conversation into the agent (the `/sessions` picker).
    LoadSession(Vec<crate::providers::client::Message>),
    /// Replay the last user message: truncate the agent's history back to the
    /// last user turn, then run it again.
    Regenerate { text: String },
    /// Stop the background task.
    Shutdown,
}

/// Launch the interactive chat TUI. Blocks until the user quits.
pub async fn run(config: &Config, resume: bool) -> Result<(), DoreanError> {
    if !io::stdin().is_terminal() {
        return Err(DoreanError::Message(
            "the chat TUI requires an interactive terminal".to_string(),
        ));
    }

    let terminal = terminal::Terminal::enter()?;
    let mut theme = theme::Theme::detect(config);
    if let Some(detected) = theme::Theme::detect_from_terminal() {
        theme = detected;
    }

    let cwd = std::env::current_dir()?;
    let abort = AbortHandle::new();

    // Approval bridge: the agent's permission policy asks the UI via this
    // channel; the UI answers on the oneshot sender, unblocking the agent.
    let (approve_tx, approve_rx) =
        mpsc::unbounded_channel::<(String, std::sync::mpsc::Sender<bool>)>();
    let approve_for_agent = Arc::new(move |prompt: &str| {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if approve_tx.send((prompt.to_string(), reply_tx)).is_err() {
            return false;
        }
        reply_rx.recv().unwrap_or(false)
    }) as crate::permissions::AskFn;

    // While an external $EDITOR owns the terminal the reader thread must not
    // consume keys (the editor reads stdin itself). The app toggles this flag
    // around `suspend()`/`resume()`.
    let suspend = Arc::new(AtomicBool::new(false));
    let (term_tx, mut term_rx) = mpsc::unbounded_channel::<crossterm::event::Event>();
    let suspend_for_reader = suspend.clone();
    std::thread::spawn(move || {
        loop {
            if suspend_for_reader.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(30));
                continue;
            }
            if crossterm::event::poll(std::time::Duration::from_millis(80)).unwrap_or(false)
                && let Ok(event) = crossterm::event::read()
                && term_tx.send(event).is_err()
            {
                break;
            }
        }
    });

    // Background agent task + its command channel.
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCmd>();
    let (events_tx, events_rx) = mpsc::unbounded_channel::<TuiEvent>();
    spawn_agent_task(
        config.clone(),
        cwd.clone(),
        resume,
        approve_for_agent,
        abort.clone(),
        cmd_rx,
        events_tx.clone(),
    );

    let mut app = app::App::new(
        config.clone(),
        cwd,
        theme,
        cmd_tx,
        events_rx,
        approve_rx,
        abort,
        resume,
    )?;
    app.attach_terminal(terminal, suspend);
    let result = app.run(&mut term_rx).await;
    app.shutdown();

    // The `Terminal` was moved into the app; its `Drop` restores the tty.
    let _ = io::stdout().flush();

    result?;
    Ok(())
}

/// The background task: owns the persistent [`AgentLoop`] and the orchestration
/// pipeline, bridging streaming events back to the UI.
#[allow(clippy::too_many_arguments)]
fn spawn_agent_task(
    mut config: Config,
    cwd: PathBuf,
    resume: bool,
    approve: crate::permissions::AskFn,
    abort: AbortHandle,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCmd>,
    events_tx: mpsc::UnboundedSender<TuiEvent>,
) {
    tokio::spawn(async move {
        let mut agent =
            match crate::agent::AgentLoop::with_cwd_and_ask(&config, &cwd, approve.clone()) {
                Ok(agent) => agent,
                Err(e) => {
                    let _ = events_tx.send(TuiEvent::Error(format!("{e}")));
                    return;
                }
            };

        let (sink_tx, mut sink_rx) = mpsc::unbounded_channel();
        agent.set_stream_sink(Some(sink_tx.clone()));
        let stream_events = events_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = sink_rx.recv().await {
                let _ = stream_events.send(TuiEvent::Stream(event));
            }
        });

        if resume && let Ok(Some(record)) = crate::history::load_latest(&cwd) {
            let initial = record.messages.clone();
            agent.load_messages(initial);
            let _ = events_tx.send(TuiEvent::Notice("resumed most recent session".to_string()));
        }

        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                SessionCmd::Send { text } => {
                    abort.reset();
                    let summary = match agent.run(&text).await {
                        Ok(summary) => summary,
                        Err(e) => {
                            let _ = events_tx.send(TuiEvent::Error(format!("{e}")));
                            continue;
                        }
                    };
                    let record = crate::history::new_record(agent.model(), agent.messages.clone());
                    let _ = crate::history::append_session(&cwd, &record);
                    let _ = events_tx.send(TuiEvent::ChatFinished {
                        turns: summary.turns,
                        aborted: summary.aborted,
                    });
                }
                SessionCmd::Regenerate { text } => {
                    abort.reset();
                    // Drop the old reply (and any tool calls it made) from the
                    // agent's context, then replay the user message.
                    let _ = agent.truncate_to_last_user();
                    let summary = match agent.run(&text).await {
                        Ok(summary) => summary,
                        Err(e) => {
                            let _ = events_tx.send(TuiEvent::Error(format!("{e}")));
                            continue;
                        }
                    };
                    let record = crate::history::new_record(agent.model(), agent.messages.clone());
                    let _ = crate::history::append_session(&cwd, &record);
                    let _ = events_tx.send(TuiEvent::ChatFinished {
                        turns: summary.turns,
                        aborted: summary.aborted,
                    });
                }
                SessionCmd::LoadSession(messages) => {
                    agent.load_messages(messages);
                    let _ = events_tx.send(TuiEvent::Notice("session loaded".to_string()));
                }
                SessionCmd::SetModel(model) => {
                    agent.set_model(Some(model.clone()));
                    let _ = events_tx.send(TuiEvent::Notice(format!("model → {model}")));
                }
                SessionCmd::SetProvider(provider) => {
                    // Preserve chat context across the rebuild (same as /key).
                    let history = agent.messages.clone();
                    config.provider = provider;
                    match crate::agent::AgentLoop::with_cwd_and_ask(&config, &cwd, approve.clone())
                    {
                        Ok(fresh) => {
                            agent = fresh;
                            agent.load_messages(history);
                            agent.set_stream_sink(Some(sink_tx.clone()));
                            let _ =
                                events_tx.send(TuiEvent::Notice(format!("provider → {provider}")));
                        }
                        Err(e) => {
                            let _ = events_tx.send(TuiEvent::Error(format!("{e}")));
                        }
                    }
                }
                SessionCmd::Clear => {
                    abort.reset();
                    match crate::agent::AgentLoop::with_cwd_and_ask(&config, &cwd, approve.clone())
                    {
                        Ok(fresh) => {
                            agent = fresh;
                            agent.set_stream_sink(Some(sink_tx.clone()));
                        }
                        Err(e) => {
                            let _ = events_tx.send(TuiEvent::Error(format!("{e}")));
                        }
                    }
                }
                SessionCmd::Abort => {
                    abort.abort();
                }
                SessionCmd::SetApiKey { provider, key } => {
                    // Preserve chat context across the rebuild.
                    let history = agent.messages.clone();
                    config.provider = provider;
                    match provider {
                        crate::config::Provider::OpenRouter => {
                            config.openrouter_api_key = Some(key.clone())
                        }
                        crate::config::Provider::Nvidia => {
                            config.nvidia_api_key = Some(key.clone())
                        }
                        crate::config::Provider::DeepSeek => {
                            config.deepseek_api_key = Some(key.clone())
                        }
                        crate::config::Provider::Generic => {
                            config.generic_api_key = Some(key.clone())
                        }
                        crate::config::Provider::Local => {}
                    }
                    match crate::agent::AgentLoop::with_cwd_and_ask(&config, &cwd, approve.clone())
                    {
                        Ok(fresh) => {
                            agent = fresh;
                            agent.load_messages(history);
                            agent.set_stream_sink(Some(sink_tx.clone()));
                            let _ = events_tx.send(TuiEvent::Notice("API key saved".to_string()));
                        }
                        Err(e) => {
                            let _ = events_tx.send(TuiEvent::Error(format!("{e}")));
                        }
                    }
                }
                SessionCmd::SetPermission(mode) => {
                    config.permission_mode = Some(mode);
                    match crate::agent::AgentLoop::with_cwd_and_ask(&config, &cwd, approve.clone())
                    {
                        Ok(fresh) => {
                            agent = fresh;
                            agent.set_stream_sink(Some(sink_tx.clone()));
                            let _ = events_tx
                                .send(TuiEvent::Notice(format!("permission mode → {mode}")));
                        }
                        Err(e) => {
                            let _ = events_tx.send(TuiEvent::Error(format!("{e}")));
                        }
                    }
                }
                SessionCmd::FetchModels { id } => {
                    // Detached: model listing is read-only, so it must never
                    // queue behind a long chat/orchestration run (a wedged
                    // queue is exactly the forever-spinner). Hard watchdog on
                    // top of fetch_all_models' per-provider caps.
                    let cfg = config.clone();
                    let tx = events_tx.clone();
                    tokio::spawn(async move {
                        let fetched = tokio::time::timeout(
                            std::time::Duration::from_secs(45),
                            crate::providers::fetch_all_models(&cfg),
                        )
                        .await;
                        match fetched {
                            Ok(f) if !f.tagged.is_empty() => {
                                if !f.problems.is_empty() {
                                    let _ = tx.send(TuiEvent::Notice(format!(
                                        "skipped: {}",
                                        f.problems.join("; ")
                                    )));
                                }
                                let _ = tx.send(TuiEvent::ModelList {
                                    id,
                                    models: f.tagged,
                                });
                            }
                            Ok(f) => {
                                let base = cfg
                                    .base_url
                                    .as_deref()
                                    .map(|u| format!(" DOREAN_BASE_URL={u};"))
                                    .unwrap_or_default();
                                let _ = tx.send(TuiEvent::ModelListFailed {
                                    id,
                                    message: format!(
                                        "couldn't fetch models from any provider ({}).{base} check network access, then keys via /key",
                                        f.problems.join("; ")
                                    ),
                                });
                            }
                            Err(_) => {
                                let _ = tx.send(TuiEvent::ModelListFailed {
                                    id,
                                    message: "fetching models timed out after 45s (check network / proxy)"
                                        .to_string(),
                                });
                            }
                        }
                    });
                }
                SessionCmd::Orchestrate {
                    goal,
                    stack,
                    routing,
                    roster,
                } => {
                    abort.reset();
                    let mut orch_config = config.clone();
                    // Sub-agents never prompt interactively in the TUI.
                    orch_config.permission_mode = Some(crate::permissions::PermissionMode::Allow);
                    let (orch_tx, mut orch_rx) =
                        tokio::sync::broadcast::channel::<crate::agent::events::AgentEvent>(1024);
                    let events_forward = events_tx.clone();
                    tokio::spawn(async move {
                        while let Ok(event) = orch_rx.recv().await {
                            let _ = events_forward.send(TuiEvent::Agent(event));
                        }
                    });
                    let orchestrator = crate::agent::Orchestrator::new(
                        orch_config,
                        cwd.clone(),
                        abort.clone(),
                        orch_tx,
                        false,
                        config.sub_agent_rounds,
                    )
                    .with_routing(routing);
                    match orchestrator
                        .run(
                            &goal,
                            &crate::agent::stack::StackChoice::Named(stack),
                            Some(roster),
                        )
                        .await
                    {
                        Ok(summary) => {
                            let text = crate::agent::orchestrator::render_summary(&summary);
                            let _ = events_tx.send(TuiEvent::OrchestrationFinished(text));
                        }
                        Err(e) => {
                            let _ = events_tx.send(TuiEvent::Error(format!("{e}")));
                        }
                    }
                }
                SessionCmd::Shutdown => break,
            }
        }
    });
}
