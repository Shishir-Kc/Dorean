//! The unified event stream the TUI consumes.
//!
//! Background agent tasks (the main chat loop and the orchestrator) publish
//! [`TuiEvent`]s; the app loop merges them with terminal input events.

use crate::agent::events::{AgentEvent, StreamEvent};

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
    /// The free model list for the model selector.
    ModelList(Vec<crate::providers::client::ModelInfo>),
    /// The model list fetch failed (missing key, network, provider error).
    /// The app closes any loading overlay and shows the message.
    ModelListFailed(String),
    /// A transient notice (e.g. "no API key configured").
    Notice(String),
    /// A hard error that ends the current run (shown, then the UI returns to
    /// idle).
    Error(String),
}
