//! Events emitted by the orchestration layer, consumed by the TUI (Phase 6)
//! or printed as a compact progress stream in non-interactive mode.

use std::path::PathBuf;

use serde::Serialize;

use crate::providers::client::Usage;

use super::todos::TodoStatus;

/// A live event from an agent run (the main chat loop or a sub-agent),
/// consumed by the TUI to render streaming output. Emitted on an unbounded
/// channel installed via [`crate::agent::AgentLoop::set_stream_sink`].
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A fragment of the assistant's answer text.
    Delta(String),
    /// A fragment of reasoning (reasoning models only).
    Reasoning(String),
    /// The model requested a tool call.
    ToolCall {
        id: String,
        name: String,
        /// Short human-readable arguments summary.
        summary: String,
    },
    /// A tool call completed with its output text.
    ToolResult {
        id: String,
        name: String,
        output: String,
    },
    /// Token usage reported for the turn.
    Usage(Usage),
    /// The turn finished without an abort.
    Done,
}

/// A structured event from an orchestrated run.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The orchestrator is asking the user which stack to build.
    StackPrompt { stack: String },
    /// The orchestrator wrote the project plan.
    PlanCreated { path: PathBuf },
    /// A sub-agent began its loop.
    AgentStarted { name: String, role: String },
    /// A sub-agent produced text (streamed).
    AgentMessage { agent: String, text: String },
    /// A sub-agent updated one of its todos.
    TodoUpdated {
        agent: String,
        id: String,
        status: TodoStatus,
    },
    /// A sub-agent finished its loop.
    AgentFinished { name: String, status: String },
    /// An `@agentname` mention was routed to its target.
    MentionRouted {
        to: String,
        from: String,
        text: String,
    },
}

impl AgentEvent {
    /// Compact one-line rendering for non-interactive progress output.
    pub fn render(&self) -> String {
        match self {
            AgentEvent::StackPrompt { stack } => format!("[stack] prompt: {stack}"),
            AgentEvent::PlanCreated { path } => format!("[plan] {}", path.display()),
            AgentEvent::AgentStarted { name, role } => format!("[agent] {name} started ({role})"),
            AgentEvent::AgentMessage { agent, text } => {
                let brief = text.split('\n').next().unwrap_or_default();
                format!("[agent] {agent}: {brief}")
            }
            AgentEvent::TodoUpdated { agent, id, status } => {
                format!("[todo] {agent} {id} -> {status}")
            }
            AgentEvent::AgentFinished { name, status } => {
                format!("[agent] {name} finished ({status})")
            }
            AgentEvent::MentionRouted { to, from, text } => {
                let brief = text.split('\n').next().unwrap_or_default();
                format!("[mention] {from} -> {to}: {brief}")
            }
        }
    }
}
