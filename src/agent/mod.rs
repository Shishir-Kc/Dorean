//! Agent harness core: the model ↔ tool loop, prompts, tools, and the
//! sub-agent orchestration layer.

pub mod agent_loop;
pub mod context;
pub mod events;
pub mod manifest;
pub mod orchestrator;
pub mod prompts;
pub mod router;
pub mod spec;
pub mod stack;
pub mod sub_agent;
pub mod todos;
pub mod tools;

use std::path::PathBuf;

use crate::config::Config;
use crate::error::DoreanError;
use crate::history;

pub use agent_loop::{AbortHandle, AgentIdentity, AgentLoop, RunSummary};
pub use orchestrator::{OrchestrationSummary, Orchestrator};

/// Options for a single non-interactive run.
#[derive(Debug, Clone)]
pub struct RunOptions<'a> {
    pub message: &'a str,
    /// Stream assistant text to stdout as it arrives.
    pub print: bool,
    /// Continue the most recent session instead of starting fresh.
    pub resume: bool,
    /// Working directory for tools, session history, and prompt context.
    pub cwd: PathBuf,
    /// Handle that can stop the loop gracefully.
    pub abort: AbortHandle,
}

/// Run the agent once for a single non-interactive prompt and return the
/// final summary.
pub async fn run_once(
    config: &Config,
    options: &RunOptions<'_>,
) -> Result<RunSummary, DoreanError> {
    let mut agent = AgentLoop::with_cwd(config, &options.cwd)?;
    agent.set_print_stream(options.print);
    agent.set_abort(options.abort.clone());

    if options.resume
        && let Some(record) = history::load_latest(&options.cwd)?
    {
        agent.load_messages(record.messages);
    }

    let summary = agent.run(options.message).await?;

    let record = history::new_record(agent.model(), agent.messages.clone());
    history::append_session(&options.cwd, &record)?;

    Ok(summary)
}
