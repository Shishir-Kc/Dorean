//! The unified event stream the TUI consumes.
//!
//! Background agent tasks (the main chat loop and the orchestrator) publish
//! [`TuiEvent`]s; the app loop merges them with terminal input events.

use crate::agent::events::{AgentEvent, StreamEvent};
use crate::config::Provider;

/// One event from a background run, tagged so the app can update the right
/// part of its state.
#[derive(Debug, Clone)]
pub enum TuiEvent {
    /// A streaming event from the main agent loop.
    Stream(StreamEvent),
    /// An orchestration event (roster, todos, mentions, agent messages).
    Agent(AgentEvent),
    /// The main chat run finished.
    ChatFinished { turns: usize, aborted: bool },
    /// An orchestrated run finished, with a rendered summary.
    OrchestrationFinished(String),
    /// The aggregated model list for the model selector: one entry per
    /// (provider, model), free models first. Fetched from all providers in
    /// parallel; unreachable providers are skipped with a notice. `id`
    /// echoes the requesting fetch so superseded requests are ignored.
    ModelList {
        id: u64,
        models: Vec<(Provider, crate::providers::client::ModelInfo)>,
    },
    /// The model list fetch failed (missing key, network, provider error).
    /// The app closes any loading overlay and shows the message. `id`
    /// echoes the requesting fetch so stale failures can't kill a newer
    /// spinner.
    ModelListFailed { id: u64, message: String },
    /// A transient notice (e.g. "no API key configured").
    Notice(String),
    /// A hard error that ends the current run (shown, then the UI returns to
    /// idle).
    Error(String),
}
