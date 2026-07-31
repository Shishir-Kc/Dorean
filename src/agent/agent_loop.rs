//! The agent loop: user message → model → tool calls → execute → observe → repeat.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::agent::context::RepoContext;
use crate::agent::events::StreamEvent;
use crate::agent::prompts;
use crate::agent::tools::{Tool, ToolContext, ToolRegistry};
use crate::config::Config;
use crate::error::DoreanError;
use crate::permissions::PermissionPolicy;
use crate::providers::client::{
    ChatRequest, CompletionEvent, Message, ToolCall, ToolCallDelta, Usage, accumulate_tool_calls,
};
use crate::providers::{DEFAULT_FREE_MODEL, OpenRouterProvider};

/// Handle for requesting a graceful stop of the agent loop. Checked between
/// turns and between stream events, so an abort takes effect promptly.
#[derive(Debug, Clone, Default)]
pub struct AbortHandle(Arc<AtomicBool>);

impl AbortHandle {
    pub fn new() -> Self {
        AbortHandle(Arc::new(AtomicBool::new(false)))
    }

    /// Request an abort. Idempotent.
    pub fn abort(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Clear a previously requested abort (the TUI reuses one handle across
    /// runs).
    pub fn reset(&self) {
        self.0.store(false, Ordering::Relaxed);
    }

    pub fn is_aborted(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// How a run ended.
#[derive(Debug, Clone)]
pub struct RunSummary {
    /// The final assistant text (the last turn with no tool calls).
    pub response: String,
    /// Usage reported by the last turn.
    pub usage: Usage,
    /// Number of model turns executed.
    pub turns: usize,
    pub aborted: bool,
}

/// Identity injected into a sub-agent's prompt (Phase 5).
#[derive(Debug, Clone)]
pub struct AgentIdentity {
    pub name: String,
    pub role: String,
    pub responsibilities: Vec<String>,
    pub owned_paths: Vec<PathBuf>,
}

/// The model ↔ tool loop. Owns the conversation (excluding the system prompt,
/// which is rebuilt per request to reflect current tools) and the tool set.
pub struct AgentLoop {
    config: Config,
    provider: OpenRouterProvider,
    cwd: PathBuf,
    repo: RepoContext,
    tools: ToolRegistry,
    permissions: PermissionPolicy,
    /// Conversation excluding the system prompt.
    pub messages: Vec<Message>,
    max_turns: usize,
    retries: u32,
    print_stream: bool,
    abort: AbortHandle,
    /// Live streaming events for the TUI (None = no streaming).
    stream_tx: Option<mpsc::UnboundedSender<StreamEvent>>,
    /// When set, the loop runs as a confined sub-agent.
    agent: Option<AgentIdentity>,
    model_override: Option<String>,
}

/// Result of one model turn.
struct TurnResult {
    text: String,
    calls: Vec<ToolCall>,
    usage: Usage,
    aborted: bool,
}

impl AgentLoop {
    /// Build a loop for the given config, operating in the current directory.
    pub fn new(config: &Config) -> Result<Self, DoreanError> {
        Self::with_cwd(config, &std::env::current_dir()?)
    }

    /// Build a loop pinned to an explicit working directory (used by tests).
    pub fn with_cwd(config: &Config, cwd: &Path) -> Result<Self, DoreanError> {
        let permissions = PermissionPolicy::from_config(config, cwd);
        Self::custom(config, cwd, ToolRegistry::builtin(), permissions)
    }

    /// Build a loop rooted at `cwd` whose approval prompts go through an
    /// external callback (the TUI). See [`PermissionPolicy::interactive`].
    pub fn with_cwd_and_ask(
        config: &Config,
        cwd: &Path,
        ask: crate::permissions::AskFn,
    ) -> Result<Self, DoreanError> {
        let permissions = PermissionPolicy::interactive(config, cwd, ask);
        Self::custom(config, cwd, ToolRegistry::builtin(), permissions)
    }

    /// Build a loop with a custom tool set and permission policy (sub-agents).
    pub fn custom(
        config: &Config,
        cwd: &Path,
        tools: ToolRegistry,
        permissions: PermissionPolicy,
    ) -> Result<Self, DoreanError> {
        Ok(AgentLoop {
            provider: OpenRouterProvider::from_config(config)?,
            config: config.clone(),
            cwd: cwd.to_path_buf(),
            repo: RepoContext::snapshot(cwd),
            tools,
            permissions,
            messages: Vec::new(),
            max_turns: config.max_turns as usize,
            retries: 3,
            print_stream: false,
            abort: AbortHandle::new(),
            stream_tx: None,
            agent: None,
            model_override: None,
        })
    }

    /// Clone of the abort handle; call [`AbortHandle::abort`] to stop.
    pub fn abort_handle(&self) -> AbortHandle {
        self.abort.clone()
    }

    /// Install an external abort handle (e.g. from the CLI's Ctrl+C handler).
    pub fn set_abort(&mut self, handle: AbortHandle) {
        self.abort = handle;
    }

    /// Echo streamed text deltas to stdout as they arrive.
    pub fn set_print_stream(&mut self, print: bool) {
        self.print_stream = print;
    }

    /// Install a sink for live streaming events (text deltas, tool calls,
    /// results, usage). None disables streaming.
    pub fn set_stream_sink(&mut self, sink: Option<mpsc::UnboundedSender<StreamEvent>>) {
        self.stream_tx = sink;
    }

    fn emit(&self, event: StreamEvent) {
        if let Some(tx) = &self.stream_tx {
            let _ = tx.send(event);
        }
    }

    /// Configure the loop to run as a confined sub-agent.
    pub fn set_agent_identity(&mut self, identity: AgentIdentity) {
        self.agent = Some(identity);
    }

    /// Override the model (from a sub-agent's manifest).
    pub fn set_model(&mut self, model: Option<String>) {
        self.model_override = model;
    }

    /// Register an extra tool (e.g. the sub-agent `todo` tool).
    pub fn add_tool(&mut self, tool: impl Tool + 'static) {
        self.tools.register(tool);
    }

    /// The configured model id, the override, or the default free model.
    pub fn model(&self) -> String {
        self.model_override
            .clone()
            .or_else(|| self.config.model.clone())
            .unwrap_or_else(|| DEFAULT_FREE_MODEL.to_string())
    }

    /// Restore a prior conversation (for `--continue`).
    pub fn load_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }

    /// Run the loop with a fresh user message, returning the final summary.
    pub async fn run(&mut self, user_message: &str) -> Result<RunSummary, DoreanError> {
        self.messages.push(Message::user(user_message));
        self.run_loop().await
    }

    async fn run_loop(&mut self) -> Result<RunSummary, DoreanError> {
        let mut usage = Usage::default();
        let mut last_text = String::new();

        for turn in 0..self.max_turns {
            if self.abort.is_aborted() {
                return Ok(RunSummary {
                    response: last_text,
                    usage,
                    turns: turn,
                    aborted: true,
                });
            }

            let tools = self.tools.specs();
            let system = match &self.agent {
                Some(id) => prompts::agent_prompt(
                    &self.cwd,
                    &self.repo,
                    &tools,
                    &id.name,
                    &id.role,
                    &id.responsibilities,
                    &id.owned_paths,
                ),
                None => prompts::system_prompt(&self.cwd, &self.repo, &tools),
            };
            let mut messages = vec![Message::system(system)];
            messages.extend(self.messages.clone());

            let request = ChatRequest {
                model: self.model(),
                messages,
                max_tokens: self.config.max_tokens,
                temperature: None,
                tools,
            };

            let result = self.stream_turn(request).await?;
            usage = result.usage;
            last_text = result.text.clone();

            if result.aborted {
                if !result.text.is_empty() {
                    self.messages
                        .push(Message::assistant(result.text, Vec::new()));
                }
                return Ok(RunSummary {
                    response: last_text,
                    usage,
                    turns: turn + 1,
                    aborted: true,
                });
            }

            if result.calls.is_empty() {
                if !result.text.is_empty() {
                    self.messages
                        .push(Message::assistant(result.text, Vec::new()));
                }
                return Ok(RunSummary {
                    response: last_text,
                    usage,
                    turns: turn + 1,
                    aborted: false,
                });
            }

            // The model wants tools. Record its request, then run every call
            // (tool execution is local and fast; abort is honored on the next
            // turn boundary) so the message log never holds dangling calls.
            self.messages
                .push(Message::assistant(result.text, result.calls.clone()));

            let ctx = ToolContext::new(self.cwd.clone()).with_permissions(self.permissions.clone());
            for call in &result.calls {
                if self.print_stream {
                    eprintln!("  [dorean] {}", tool_brief(call));
                }
                self.emit(StreamEvent::ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    summary: tool_brief(call),
                });
                let output = self.tools.run(&ctx, call).await;
                self.emit(StreamEvent::ToolResult {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    output: output.clone(),
                });
                self.messages.push(Message::tool(call.id.clone(), output));
            }
        }

        Err(DoreanError::Message(format!(
            "turn budget exhausted after {max} turns",
            max = self.max_turns
        )))
    }

    /// Stream one model turn, retrying transient failures with backoff.
    async fn stream_turn(&mut self, request: ChatRequest) -> Result<TurnResult, DoreanError> {
        let mut last_error = None;
        for attempt in 0..=self.retries {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt - 1))).await;
            }
            match self.stream_once(&request).await {
                Ok(result) => return Ok(result),
                Err(e) if e.is_retryable() && attempt < self.retries => {
                    last_error = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_error
            .unwrap_or_else(|| DoreanError::Stream("request failed after retries".to_string())))
    }

    async fn stream_once(&self, request: &ChatRequest) -> Result<TurnResult, DoreanError> {
        let mut stream = self.provider.stream_chat(request.clone());
        let mut text = String::new();
        let mut deltas: Vec<ToolCallDelta> = Vec::new();
        let mut usage = Usage::default();

        loop {
            if self.abort.is_aborted() {
                return Ok(TurnResult {
                    text,
                    calls: accumulate_tool_calls(&deltas),
                    usage,
                    aborted: true,
                });
            }
            match stream.next().await {
                Some(Ok(CompletionEvent::TextDelta(delta))) => {
                    if self.print_stream {
                        print!("{delta}");
                        let _ = std::io::stdout().flush();
                    }
                    self.emit(StreamEvent::Delta(delta.clone()));
                    text.push_str(&delta);
                }
                Some(Ok(CompletionEvent::ToolCallDelta(delta))) => deltas.push(delta),
                Some(Ok(CompletionEvent::Usage(u))) => {
                    usage = u.clone();
                    self.emit(StreamEvent::Usage(u));
                }
                Some(Ok(CompletionEvent::ReasoningDelta(r))) => {
                    self.emit(StreamEvent::Reasoning(r));
                }
                Some(Ok(CompletionEvent::Done)) | None => break,
                Some(Err(e)) => return Err(e),
            }
        }

        self.emit(StreamEvent::Done);
        Ok(TurnResult {
            text,
            calls: accumulate_tool_calls(&deltas),
            usage,
            aborted: false,
        })
    }
}

/// A short human-readable summary of a tool call, for progress output.
fn tool_brief(call: &ToolCall) -> String {
    let args = serde_json::to_string(&call.arguments).unwrap_or_default();
    if args.len() > 80 {
        format!("{} {}", call.name, &args[..80])
    } else {
        format!("{} {args}", call.name)
    }
}
