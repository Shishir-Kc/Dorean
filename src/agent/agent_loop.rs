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
use crate::agent::token_meter::TokenMeter;
use crate::agent::tools::{Tool, ToolContext, ToolRegistry};
use crate::agent::{compact, memory, prompts};
use crate::config::Config;
use crate::error::DoreanError;
use crate::permissions::PermissionPolicy;
use crate::providers::client::{
    ChatRequest, CompletionEvent, Message, ToolCall, ToolCallDelta, Usage, accumulate_tool_calls,
};
use crate::providers::{AnyProvider, default_model};

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
    provider: AnyProvider,
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
    /// Running token totals (provider usage folded in per turn).
    meter: TokenMeter,
    /// Project memory (AGENTS.md/CLAUDE.md/skills), loaded once per loop.
    project_memory: String,
    /// Lifecycle hooks from `.dorean/hooks.json`, loaded once per loop.
    hooks: Vec<crate::agent::hooks::Hook>,
    /// Max silence between SSE events before a turn is failed as stalled.
    /// Without this a blackholed stream pends forever, wedging the whole
    /// background task (chat dead, queued commands never run, aborts
    /// unreachable). Expiry is retryable, so transient stalls recover.
    stream_idle_timeout: Duration,
}

/// Silence allowance between stream events (generous: reasoning models can
/// legitimately pause a minute+ between deltas).
pub const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

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
        let project_memory = memory::ProjectMemory::load(cwd).text;
        let hooks = crate::agent::hooks::load_hooks(cwd);
        Ok(AgentLoop {
            provider: AnyProvider::from_config(config)?,
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
            meter: TokenMeter::new(),
            project_memory,
            hooks,
            stream_idle_timeout: DEFAULT_STREAM_IDLE_TIMEOUT,
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

    /// The configured model id, the override, or the provider's default.
    pub fn model(&self) -> String {
        self.model_override
            .clone()
            .or_else(|| self.config.model.clone())
            .unwrap_or_else(|| default_model(self.config.provider).to_string())
    }

    /// Restore a prior conversation (for `--continue`).
    pub fn load_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }

    /// Override the idle watchdog (tests use milliseconds).
    pub fn set_stream_idle_timeout(&mut self, timeout: Duration) {
        self.stream_idle_timeout = timeout;
    }

    /// Token totals for this loop (provider usage folded in per turn).
    pub fn meter(&self) -> &TokenMeter {
        &self.meter
    }

    /// One-line token/cost status for headers and footers.
    pub fn token_summary(&self) -> String {
        self.meter.summary()
    }

    /// Discard everything after the last user message (for `/regenerate`) and
    /// return that message's text, or `None` when the log has no user message.
    pub fn truncate_to_last_user(&mut self) -> Option<String> {
        let idx = self
            .messages
            .iter()
            .rposition(|m| m.role == crate::providers::client::Role::User)?;
        let text = self.messages[idx].content.clone();
        self.messages.truncate(idx);
        Some(text)
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

            // Context maintenance first: prune stale tool output, then compact
            // the head when estimates exceed the threshold (unless disabled).
            // This keeps long runs fast and cache-friendly instead of dying at
            // the turn budget with a blown context.
            compact::prune_tool_results(&mut self.messages);
            if self.config.auto_compact && self.config.compact_threshold > 0 {
                let est = crate::agent::token_meter::estimate_messages(&self.messages);
                if compact::needs_compaction(est, self.config.compact_threshold) {
                    compact::compact_messages(&mut self.messages, compact::KEEP_TAIL_MESSAGES);
                }
            }

            let tools = self.tools.specs();
            let mut system = match &self.agent {
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
            if !self.project_memory.trim().is_empty() {
                system.push_str("\n## Project memory\n");
                system.push_str(&self.project_memory);
            }
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
            usage = result.usage.clone();
            self.meter.add_usage(&result.usage);
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
            // so the message log never holds dangling calls. Consecutive
            // read-only calls (read/glob/grep/list) run concurrently via
            // join_all — typically 3-4x faster than serial on investigation
            // turns — while write/edit/bash stay serial to preserve ordering
            // semantics. Abort is honored between batches.
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
            }
            let mut idx = 0;
            while idx < result.calls.len() {
                if self.abort.is_aborted() {
                    break;
                }
                if is_read_only(&result.calls[idx].name) {
                    let mut end = idx;
                    while end < result.calls.len() && is_read_only(&result.calls[end].name) {
                        end += 1;
                    }
                    let batch = &result.calls[idx..end];
                    let outputs = futures_util::future::join_all(batch.iter().map(|c| {
                        run_tool_with_hooks(&self.tools, &ctx, &self.hooks, &self.cwd, c)
                    }))
                    .await;
                    for (call, output) in batch.iter().zip(outputs) {
                        self.emit(StreamEvent::ToolResult {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            output: output.clone(),
                        });
                        self.messages.push(Message::tool(call.id.clone(), output));
                    }
                    idx = end;
                } else {
                    let call = &result.calls[idx];
                    let output =
                        run_tool_with_hooks(&self.tools, &ctx, &self.hooks, &self.cwd, call).await;
                    self.emit(StreamEvent::ToolResult {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        output: output.clone(),
                    });
                    self.messages.push(Message::tool(call.id.clone(), output));
                    idx += 1;
                }
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
        // Accumulated silence since the last stream event. Polls are short so
        // aborts land within ~1s even on a fully dead stream; only sustained
        // silence up to the idle timeout counts as a stall.
        let mut silent = Duration::ZERO;
        let poll = self.stream_idle_timeout.min(Duration::from_secs(1));

        loop {
            if self.abort.is_aborted() {
                return Ok(TurnResult {
                    text,
                    calls: accumulate_tool_calls(&deltas),
                    usage,
                    aborted: true,
                });
            }
            // Idle watchdog: each event resets the clock. A stream that goes
            // silent (blackholed route, dead proxy, hung server) fails as a
            // retryable error instead of wedging the task forever.
            let next = match tokio::time::timeout(poll, stream.next()).await {
                Ok(next) => {
                    silent = Duration::ZERO;
                    next
                }
                Err(_) => {
                    silent += poll;
                    if self.abort.is_aborted() {
                        return Ok(TurnResult {
                            text,
                            calls: accumulate_tool_calls(&deltas),
                            usage,
                            aborted: true,
                        });
                    }
                    if silent >= self.stream_idle_timeout {
                        return Err(DoreanError::Stream(format!(
                            "stream stalled: no data for {}s (check network / proxy)",
                            self.stream_idle_timeout.as_secs()
                        )));
                    }
                    continue;
                }
            };
            match next {
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

/// Read-only tools are safe to run concurrently: no side effects, no ordering
/// constraints. Everything else (write/edit/bash/todo/…) runs serially.
fn is_read_only(name: &str) -> bool {
    matches!(name, "read" | "glob" | "grep" | "list" | "bash_poll")
}

/// Run one tool with Pre/Post hooks. A Pre-hook exit-2 denial returns denial
/// text without executing the tool; Post-hook stdout is appended as context.
async fn run_tool_with_hooks(
    registry: &ToolRegistry,
    ctx: &ToolContext,
    hooks: &[crate::agent::hooks::Hook],
    cwd: &std::path::Path,
    call: &ToolCall,
) -> String {
    use crate::agent::hooks::{HookEvent, run_hooks};
    if !hooks.is_empty() {
        match run_hooks(
            hooks,
            HookEvent::PreToolUse,
            &call.name,
            &call.arguments,
            cwd,
        ) {
            crate::agent::hooks::HookOutcome::Deny { reason } => {
                return format!("Hook denied tool `{}`: {reason}", call.name);
            }
            crate::agent::hooks::HookOutcome::Allow { extra_context } => {
                if !extra_context.is_empty() {
                    let out = registry.run(ctx, call).await;
                    let post = run_hooks(
                        hooks,
                        HookEvent::PostToolUse,
                        &call.name,
                        &call.arguments,
                        cwd,
                    );
                    let suffix = match post {
                        crate::agent::hooks::HookOutcome::Allow { extra_context: p }
                            if !p.is_empty() =>
                        {
                            format!("\n[hook context]\n{p}")
                        }
                        _ => String::new(),
                    };
                    return format!("{out}\n[pre-hook context]\n{extra_context}{suffix}");
                }
            }
        }
    }
    let out = registry.run(ctx, call).await;
    if hooks.is_empty() {
        return out;
    }
    match run_hooks(
        hooks,
        HookEvent::PostToolUse,
        &call.name,
        &call.arguments,
        cwd,
    ) {
        crate::agent::hooks::HookOutcome::Allow { extra_context } if !extra_context.is_empty() => {
            format!("{out}\n[hook context]\n{extra_context}")
        }
        _ => out,
    }
}
