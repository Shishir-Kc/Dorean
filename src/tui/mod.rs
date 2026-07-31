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
    /// Start a fresh agent loop (drops in-memory context).
    Clear,
    /// Abort the current run (via the shared [`AbortHandle`]).
    Abort,
    /// Fetch the free model list for the model selector.
    FetchModels,
    /// Persist a new API key (for the active provider) and rebuild the agent.
    SetApiKey(String),
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

    let mut terminal = terminal::Terminal::enter()?;
    let mut theme = theme::Theme::detect(config);
    if let Some(detected) = theme::Theme::detect_from_terminal() {
        theme = detected;
    }
    let _ = &mut terminal;
    let _ = &mut theme;

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

    // Terminal event reader thread.
    let (term_tx, mut term_rx) = mpsc::unbounded_channel::<crossterm::event::Event>();
    std::thread::spawn(move || {
        loop {
            if let Ok(event) = crossterm::event::read()
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
    let result = app.run(&mut term_rx).await;
    app.shutdown();

    let _ = terminal.leave();
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
                SessionCmd::SetModel(model) => {
                    agent.set_model(Some(model.clone()));
                    let _ = events_tx.send(TuiEvent::Notice(format!("model → {model}")));
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
                SessionCmd::SetApiKey(key) => {
                    match config.provider {
                        crate::config::Provider::OpenRouter => {
                            config.openrouter_api_key = Some(key.clone())
                        }
                        crate::config::Provider::Nvidia => config.nvidia_api_key = Some(key),
                    }
                    match crate::agent::AgentLoop::with_cwd_and_ask(&config, &cwd, approve.clone())
                    {
                        Ok(fresh) => {
                            agent = fresh;
                            agent.set_stream_sink(Some(sink_tx.clone()));
                            let _ = events_tx.send(TuiEvent::Notice("API key saved".to_string()));
                        }
                        Err(e) => {
                            let _ = events_tx.send(TuiEvent::Error(format!("{e}")));
                        }
                    }
                }
                SessionCmd::FetchModels => {
                    // Model listing works even without a key on OpenRouter, and
                    // is attempted keyless on NVIDIA too, so /model always
                    // opens. Chat itself still needs /key.
                    let provider = crate::providers::AnyProvider::for_model_listing(&config);
                    match provider.list_free_models().await {
                        Ok(models) => {
                            let _ = events_tx.send(TuiEvent::ModelList(models));
                        }
                        Err(e) => {
                            let _ = events_tx.send(TuiEvent::ModelListFailed(format!("{e}")));
                        }
                    }
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
