//! The chat TUI app: event loop, session state, streaming bubbles, overlays,
//! and keyboard-driven layout. Renders into a diffed [`Screen`] each frame.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;

use crate::agent::events::{AgentEvent, StreamEvent};
use crate::agent::manifest::AgentManifest;
use crate::agent::stack::STACKS;
use crate::agent::todos::TodoStatus;
use crate::config::{Config, Provider};
use crate::error::DoreanError;
use crate::providers::client::Usage;
use crate::providers::default_model;

use super::SessionCmd;
use super::components::message::draw_md_line;
use super::components::{Input, Message, MessageKind, SelectList, ToolStatus, ToolUi};
use super::events::TuiEvent;
use super::render::{Attrs, Screen};
use super::theme::Theme;

/// Lifecycle of a sub-agent in the roster strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentStatus {
    Running,
    Done,
    Failed,
}

/// What kind of selector an overlay is driving.
#[derive(Debug, Clone, Copy)]
enum SelectKind {
    Model,
    Stack,
}

/// Modal overlays layered above the chat view.
enum Overlay {
    /// A transient "fetching…" state (e.g. before the model list arrives).
    Loading(String),
    /// Filterable list of free models.
    Model(SelectList),
    /// Filterable list of stack presets for `/make`.
    Stack(SelectList),
    /// Per-sub-agent model assignment: pick a brain for each agent, then start.
    AgentModels { selected: usize },
    /// A tool-approval question awaiting an answer on `reply`.
    Permission {
        prompt: String,
        reply: std::sync::mpsc::Sender<bool>,
    },
    /// Enter (or replace) the OpenRouter API key; the buffer is masked.
    ApiKey { input: String },
    /// The `.dorean/SPEC.md` plan viewer.
    Spec,
    /// Keybinding reference.
    Help,
    /// "Quit dorean?" confirm dialog.
    Quit,
}

/// The `@agent` mention completion popup.
struct MentionPopup {
    candidates: Vec<String>,
    selected: usize,
}

#[derive(Debug, Clone, Copy)]
enum ToastKind {
    Info,
    Success,
    Error,
}

/// The main TUI application.
pub struct App {
    config: Config,
    cwd: PathBuf,
    theme: Theme,
    cmd_tx: mpsc::UnboundedSender<SessionCmd>,
    events_rx: mpsc::UnboundedReceiver<TuiEvent>,
    approve_rx: mpsc::UnboundedReceiver<(String, std::sync::mpsc::Sender<bool>)>,
    abort: crate::agent::AbortHandle,
    messages: Vec<Message>,
    /// Rows scrolled up from the bottom of the message list.
    scroll: usize,
    input: Input,
    model: String,
    turns: usize,
    usage: Usage,
    running: bool,
    /// Toggles every ticker tick, driving the spinner + streaming cursor.
    animate: bool,
    roster: Vec<crate::agent::manifest::AgentManifest>,
    agent_status: HashMap<String, AgentStatus>,
    todos: Vec<crate::agent::todos::TodoItem>,
    show_todos: bool,
    overlay: Option<Overlay>,
    /// Goal captured by `/make <goal>`, waiting for the stack pick.
    pending_goal: Option<String>,
    spec_text: String,
    overlay_scroll: usize,
    mention: Option<MentionPopup>,
    toasts: VecDeque<(String, ToastKind, Instant)>,
    prev: Screen,
    quit: bool,
    /// When the user pressed Esc during a run: armed for a double-Esc abort.
    esc_armed: Option<Instant>,
    /// Whether the reasoning ("thinking") block is rendered (`/thinking`).
    show_thinking: bool,
    /// Stack chosen in the `/make` picker, held while assigning models.
    pending_stack: Option<String>,
    /// The roster being configured by the per-agent model picker.
    agent_models: Option<Vec<AgentManifest>>,
    /// The roster index whose model the model selector is currently picking.
    pending_model_agent: Option<usize>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        cwd: PathBuf,
        theme: Theme,
        cmd_tx: mpsc::UnboundedSender<SessionCmd>,
        events_rx: mpsc::UnboundedReceiver<TuiEvent>,
        approve_rx: mpsc::UnboundedReceiver<(String, std::sync::mpsc::Sender<bool>)>,
        abort: crate::agent::AbortHandle,
        resume: bool,
    ) -> Result<Self, DoreanError> {
        let mut app = App {
            model: config
                .model
                .clone()
                .unwrap_or_else(|| default_model(config.provider).to_string()),
            roster: Vec::new(),
            agent_status: HashMap::new(),
            todos: Vec::new(),
            show_todos: false,
            overlay: None,
            pending_goal: None,
            spec_text: String::new(),
            overlay_scroll: 0,
            mention: None,
            toasts: VecDeque::new(),
            prev: Screen::default_blank(1, 1),
            quit: false,
            esc_armed: None,
            show_thinking: true,
            pending_stack: None,
            agent_models: None,
            pending_model_agent: None,
            config,
            cwd,
            theme,
            cmd_tx,
            events_rx,
            approve_rx,
            abort,
            messages: Vec::new(),
            scroll: 0,
            input: Input::default(),
            turns: 0,
            usage: Usage::default(),
            running: false,
            animate: false,
        };

        // Restore a previous roster so `@agent` completion works even before
        // this session's first orchestration.
        if let Ok(Some(roster)) = crate::agent::manifest::load_roster(&app.cwd) {
            for agent in &roster {
                app.agent_status
                    .insert(agent.name.clone(), AgentStatus::Done);
            }
            app.roster = roster;
        }

        if resume && let Ok(Some(record)) = crate::history::load_latest(&app.cwd) {
            app.model = record.model;
            for message in record.messages {
                match message.role {
                    crate::providers::client::Role::User => {
                        app.messages.push(Message::user(message.content));
                    }
                    crate::providers::client::Role::Assistant => {
                        app.messages.push(Message::assistant(message.content));
                    }
                    _ => {}
                }
            }
        }
        Ok(app)
    }

    /// Stop the background agent task.
    pub fn shutdown(&mut self) {
        self.abort.abort();
        let _ = self.cmd_tx.send(SessionCmd::Shutdown);
    }

    /// Run the event loop until the user quits.
    pub async fn run(
        &mut self,
        term_rx: &mut mpsc::UnboundedReceiver<crossterm::event::Event>,
    ) -> Result<(), DoreanError> {
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        self.draw()?;
        loop {
            let mut dirty = true;
            tokio::select! {
                maybe = term_rx.recv() => {
                    match maybe {
                        Some(event) => self.handle_term_event(event),
                        None => self.quit = true,
                    }
                }
                maybe = self.events_rx.recv() => {
                    match maybe {
                        Some(event) => self.handle_tui_event(event),
                        None => break,
                    }
                }
                maybe = self.approve_rx.recv() => {
                    if let Some((prompt, reply)) = maybe {
                        self.overlay = Some(Overlay::Permission { prompt, reply });
                    }
                }
                _ = tick.tick() => {
                    self.animate = !self.animate;
                    dirty = self.running || self.has_streaming();
                }
            }
            if dirty || self.quit {
                self.draw()?;
            }
            if self.quit {
                break;
            }
        }
        Ok(())
    }

    // --- Input handling ----------------------------------------------------

    fn handle_term_event(&mut self, event: Event) {
        match event {
            Event::Resize(_, _) => {}
            Event::Paste(text) => match self.overlay.take() {
                Some(Overlay::ApiKey { mut input }) => {
                    input.push_str(&text);
                    self.overlay = Some(Overlay::ApiKey { input });
                }
                Some(other) => self.overlay = Some(other),
                None => {
                    self.input.insert_str(&text);
                    self.recompute_mention();
                }
            },
            Event::Key(key) => self.handle_key(key),
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // The kitty keyboard protocol (REPORT_EVENT_TYPES) reports both press
        // and release; only act on presses (and OS repeats) to avoid every
        // keystroke registering twice.
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        // 1. A pending permission prompt blocks everything else.
        if matches!(self.overlay, Some(Overlay::Permission { .. })) {
            if let Some(Overlay::Permission { prompt, reply }) = self.overlay.take() {
                let approved = match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => true,
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => false,
                    _ => {
                        self.overlay = Some(Overlay::Permission { prompt, reply });
                        return;
                    }
                };
                let _ = reply.send(approved);
            }
            return;
        }

        // 2. Modal overlays consume keys.
        match self.overlay.take() {
            Some(Overlay::Help) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {}
                _ => self.overlay = Some(Overlay::Help),
            },
            Some(Overlay::Quit) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.quit = true,
                _ => {}
            },
            Some(Overlay::Spec) => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {}
                    KeyCode::Up => self.overlay_scroll = self.overlay_scroll.saturating_sub(1),
                    KeyCode::Down => self.overlay_scroll += 1,
                    KeyCode::PageUp => self.overlay_scroll = self.overlay_scroll.saturating_sub(10),
                    KeyCode::PageDown => self.overlay_scroll += 10,
                    KeyCode::Home => self.overlay_scroll = 0,
                    KeyCode::End => self.overlay_scroll = usize::MAX,
                    _ => {}
                }
                self.overlay = Some(Overlay::Spec);
            }
            Some(Overlay::Loading(text)) => {
                if matches!(key.code, KeyCode::Esc) {
                    // Never trap the user on a hung fetch; Esc bails out.
                } else {
                    self.overlay = Some(Overlay::Loading(text));
                }
            }
            Some(Overlay::Model(list)) => self.handle_select_key(key, list, SelectKind::Model),
            Some(Overlay::Stack(list)) => self.handle_select_key(key, list, SelectKind::Stack),
            Some(Overlay::AgentModels { selected }) => self.handle_agent_models_key(key, selected),
            Some(Overlay::ApiKey { input }) => self.handle_api_key_key(key, input),
            // Handled above: a pending permission prompt blocks all input.
            Some(Overlay::Permission { .. }) => unreachable!(),
            None => self.handle_input_key(key),
        }
    }

    fn handle_select_key(&mut self, key: KeyEvent, mut list: SelectList, kind: SelectKind) {
        let mut chosen: Option<String> = None;
        match key.code {
            KeyCode::Up => list.select_prev(),
            KeyCode::Down => list.select_next(),
            KeyCode::PageUp => list.page_prev(),
            KeyCode::PageDown => list.page_next(),
            KeyCode::Char(c) => list.set_filter(format!("{}{c}", list.filter)),
            KeyCode::Backspace => {
                let mut filter = list.filter.clone();
                filter.pop();
                list.set_filter(filter);
            }
            KeyCode::Esc => {}
            KeyCode::Enter => chosen = list.selected_value().map(str::to_string),
            _ => {}
        }
        match chosen {
            Some(value) => match kind {
                SelectKind::Model => {
                    if let Some(index) = self.pending_model_agent.take() {
                        let agent_name = if let Some(roster) = self.agent_models.as_mut()
                            && index < roster.len()
                        {
                            roster[index].model = Some(value.clone());
                            roster[index].name.clone()
                        } else {
                            String::new()
                        };
                        if agent_name.is_empty() {
                            self.model = value.clone();
                            let _ = self.cmd_tx.send(SessionCmd::SetModel(value));
                        } else {
                            self.overlay = Some(Overlay::AgentModels { selected: index });
                            self.toast(format!("@{agent_name} brain → {value}"), ToastKind::Info);
                        }
                    } else {
                        self.model = value.clone();
                        let _ = self.cmd_tx.send(SessionCmd::SetModel(value));
                    }
                }
                SelectKind::Stack => self.start_orchestration(value),
            },
            None if matches!(key.code, KeyCode::Esc) => {
                // Esc in the model picker during an assignment returns to the
                // per-agent list instead of dismissing it.
                if let Some(selected) = self.pending_model_agent.take()
                    && self.agent_models.is_some()
                {
                    self.overlay = Some(Overlay::AgentModels { selected });
                }
            }
            None => {
                self.overlay = Some(match kind {
                    SelectKind::Model => Overlay::Model(list),
                    SelectKind::Stack => Overlay::Stack(list),
                });
            }
        }
    }

    /// The per-sub-agent model assignment list (`/make`). Up/Down select an
    /// agent, Enter opens its model picker, Esc starts the build.
    fn handle_agent_models_key(&mut self, key: KeyEvent, selected: usize) {
        let len = self.agent_models.as_ref().map_or(0, |r| r.len());
        let mut selected = selected;
        match key.code {
            KeyCode::Up => selected = selected.saturating_sub(1),
            KeyCode::Down => selected = selected.saturating_add(1).min(len),
            KeyCode::Enter => {
                if selected >= len {
                    self.begin_orchestration();
                } else {
                    self.pending_model_agent = Some(selected);
                    self.overlay = Some(Overlay::Loading("fetching free models…".to_string()));
                    let _ = self.cmd_tx.send(SessionCmd::FetchModels);
                }
                return;
            }
            KeyCode::Esc => {
                self.begin_orchestration();
                return;
            }
            _ => {}
        }
        self.overlay = Some(Overlay::AgentModels { selected });
    }

    fn handle_api_key_key(&mut self, key: KeyEvent, mut input: String) {
        let close = matches!(key.code, KeyCode::Esc);
        match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                self.submit_api_key(input.trim().to_string());
                return;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => input.clear(),
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char(c) => input.push(c),
            _ => {}
        }
        if !close {
            self.overlay = Some(Overlay::ApiKey { input });
        }
    }

    /// Persist the entered key, rebuild the background agent, and jump straight
    /// into the model selector so the user can confirm the key works.
    fn submit_api_key(&mut self, key: String) {
        if key.is_empty() {
            return;
        }
        match self.config.provider {
            crate::config::Provider::OpenRouter => {
                self.config.openrouter_api_key = Some(key);
            }
            crate::config::Provider::Nvidia => {
                self.config.nvidia_api_key = Some(key);
            }
        }
        if let Err(e) = self.config.save() {
            self.toast(format!("failed to save config: {e}"), ToastKind::Error);
        }
        let key = match self.config.provider {
            crate::config::Provider::OpenRouter => self.config.openrouter_api_key.clone(),
            crate::config::Provider::Nvidia => self.config.nvidia_api_key.clone(),
        }
        .unwrap_or_default();
        let _ = self.cmd_tx.send(SessionCmd::SetApiKey(key));
        self.overlay = Some(Overlay::Loading("fetching free models…".to_string()));
        let _ = self.cmd_tx.send(SessionCmd::FetchModels);
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        // Any key other than Esc disarms a pending double-Esc abort.
        if !matches!(key.code, KeyCode::Esc) {
            self.esc_armed = None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => {
                if self.mention.is_some() {
                    self.accept_mention();
                } else if ctrl {
                    self.input.newline();
                } else {
                    self.submit_input();
                }
            }
            KeyCode::Char('m') if ctrl => self.input.newline(),
            KeyCode::Char('j') if ctrl => self.submit_input(),
            KeyCode::Char('c') if ctrl => {
                if self.running {
                    let _ = self.cmd_tx.send(SessionCmd::Abort);
                } else {
                    self.overlay = Some(Overlay::Quit);
                }
            }
            KeyCode::Char('u') if ctrl => {
                self.input.clear();
                self.mention = None;
            }
            KeyCode::Char('w') if ctrl => self.input.delete_word_left(),
            KeyCode::Esc => {
                if self.mention.is_some() {
                    self.mention = None;
                } else if self.running {
                    let armed = matches!(self.esc_armed, Some(t) if t.elapsed() < Duration::from_millis(2000));
                    if armed {
                        self.esc_armed = None;
                        let _ = self.cmd_tx.send(SessionCmd::Abort);
                        self.toast("aborting…", ToastKind::Info);
                    } else {
                        self.esc_armed = Some(Instant::now());
                        self.toast("press esc again to abort", ToastKind::Info);
                    }
                } else {
                    self.overlay = Some(Overlay::Quit);
                }
            }
            KeyCode::Char(c) => self.input.push_char(c),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete_forward(),
            KeyCode::Tab => {
                if self.mention.is_some() {
                    self.accept_mention();
                }
            }
            KeyCode::BackTab => self.mention_prev(),
            KeyCode::Up => {
                if self.mention.is_some() {
                    self.mention_prev();
                } else {
                    self.input.move_up(self.input_width());
                }
            }
            KeyCode::Down => {
                if self.mention.is_some() {
                    self.mention_next();
                } else {
                    self.input.move_down(self.input_width());
                }
            }
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::Home => self.input.move_home(),
            KeyCode::End => self.input.move_end(),
            KeyCode::PageUp => self.scroll += 5,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(5),
            _ => {}
        }
        self.recompute_mention();
    }

    fn submit_input(&mut self) {
        if self.running {
            self.toast("agent is running — press esc to abort", ToastKind::Info);
            return;
        }
        let text = self.input.submit();
        if text.trim().is_empty() {
            return;
        }
        if let Some(command) = text.strip_prefix('/') {
            self.run_command(command.trim());
            return;
        }
        self.messages.push(Message::user(text.clone()));
        self.running = true;
        self.turns = 0;
        let _ = self.cmd_tx.send(SessionCmd::Send { text });
    }

    fn run_command(&mut self, command: &str) {
        let mut parts = command.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or_default();
        let arg = parts.next().unwrap_or("").trim();
        match name {
            "help" => self.overlay = Some(Overlay::Help),
            "model" => {
                self.overlay = Some(Overlay::Loading("fetching free models…".to_string()));
                let _ = self.cmd_tx.send(SessionCmd::FetchModels);
            }
            "key" => {
                self.overlay = Some(Overlay::ApiKey {
                    input: String::new(),
                })
            }
            "stack" => {
                let items = STACKS.iter().map(|s| s.to_string()).collect();
                self.overlay = Some(Overlay::Stack(SelectList::new("stack", items)));
            }
            "todo" => {
                self.show_todos = !self.show_todos;
                if self.show_todos
                    && let Ok(items) = crate::agent::todos::read_master(&self.cwd)
                {
                    self.todos = items;
                }
            }
            "spec" => match std::fs::read_to_string(self.cwd.join(".dorean").join("SPEC.md")) {
                Ok(text) => {
                    self.spec_text = text;
                    self.overlay_scroll = 0;
                    self.overlay = Some(Overlay::Spec);
                }
                Err(e) => self.toast(
                    format!("no plan yet ({}): run /make first", e.kind()),
                    ToastKind::Error,
                ),
            },
            "clear" => {
                self.messages.clear();
                self.scroll = 0;
                let _ = self.cmd_tx.send(SessionCmd::Clear);
            }
            "quit" => self.overlay = Some(Overlay::Quit),
            "abort" => {
                let _ = self.cmd_tx.send(SessionCmd::Abort);
            }
            "thinking" => {
                self.show_thinking = !self.show_thinking;
                self.toast(
                    if self.show_thinking {
                        "showing thinking"
                    } else {
                        "thinking hidden"
                    },
                    ToastKind::Info,
                );
            }
            "make" => {
                if arg.is_empty() {
                    self.toast("usage: /make <goal>", ToastKind::Error);
                } else if self.running {
                    self.toast(
                        "agent is running — press esc twice to abort",
                        ToastKind::Info,
                    );
                } else {
                    self.pending_goal = Some(arg.to_string());
                    self.pending_stack = None;
                    self.agent_models = None;
                    let items = STACKS.iter().map(|s| s.to_string()).collect();
                    self.overlay = Some(Overlay::Stack(SelectList::new("stack", items)));
                }
            }
            other => self.toast(
                format!("unknown command /{other} — try /help"),
                ToastKind::Error,
            ),
        }
    }

    /// A stack was chosen in `/make`: build the roster, then open the
    /// per-sub-agent model picker before anything runs.
    fn start_orchestration(&mut self, stack: String) {
        let Some(goal) = self.pending_goal.clone() else {
            return;
        };
        match crate::agent::stack::build_roster(&stack, &goal) {
            Ok(mut roster) => {
                crate::agent::orchestrator::apply_agent_models(
                    &mut roster,
                    &self.config.agent_models,
                );
                self.pending_stack = Some(stack);
                self.agent_models = Some(roster);
                self.overlay = Some(Overlay::AgentModels { selected: 0 });
            }
            Err(e) => self.toast(format!("{e}"), ToastKind::Error),
        }
    }

    /// The user confirmed the roster in the model picker: kick off the build.
    fn begin_orchestration(&mut self) {
        let Some(goal) = self.pending_goal.take() else {
            return;
        };
        let Some(stack) = self.pending_stack.take() else {
            return;
        };
        let Some(roster) = self.agent_models.take() else {
            return;
        };
        self.pending_model_agent = None;
        let routing = crate::agent::router::Routing::new(&roster);
        self.roster = roster.clone();
        self.agent_status.clear();
        for agent in &roster {
            self.agent_status
                .insert(agent.name.clone(), AgentStatus::Running);
        }
        let names: Vec<&str> = roster.iter().map(|a| a.name.as_str()).collect();
        self.messages.push(Message::system(format!(
            "orchestrating `{stack}` with {} agent(s): @{}",
            roster.len(),
            names.join(", @")
        )));
        self.running = true;
        self.overlay = None;
        let _ = self.cmd_tx.send(SessionCmd::Orchestrate {
            goal,
            stack,
            routing,
            roster,
        });
    }

    // --- Background events --------------------------------------------------

    fn handle_tui_event(&mut self, event: TuiEvent) {
        match event {
            TuiEvent::Stream(stream) => self.handle_stream(stream),
            TuiEvent::Agent(agent) => self.handle_agent_event(agent),
            TuiEvent::ChatFinished { turns, aborted } => {
                self.running = false;
                self.esc_armed = None;
                self.turns = turns;
                if let Some(last) = self.messages.last_mut() {
                    last.streaming = false;
                }
                if aborted {
                    self.messages
                        .push(Message::system("run aborted".to_string()));
                    self.toast("aborted", ToastKind::Info);
                } else {
                    self.toast("done", ToastKind::Success);
                }
            }
            TuiEvent::OrchestrationFinished(text) => {
                self.running = false;
                self.esc_armed = None;
                self.messages.push(Message::assistant(text));
                if let Ok(items) = crate::agent::todos::read_master(&self.cwd) {
                    self.todos = items;
                }
                for status in self.agent_status.values_mut() {
                    if *status == AgentStatus::Running {
                        *status = AgentStatus::Done;
                    }
                }
                self.toast("orchestration finished", ToastKind::Success);
            }
            TuiEvent::ModelList(models) => {
                let items: Vec<String> = models.into_iter().map(|m| m.id).collect();
                if matches!(self.overlay, Some(Overlay::Loading(_))) {
                    self.overlay = Some(Overlay::Model(SelectList::new("model", items)));
                }
            }
            TuiEvent::ModelListFailed(text) => {
                if matches!(self.overlay, Some(Overlay::Loading(_))) {
                    if self.pending_model_agent.is_some() && self.agent_models.is_some() {
                        let selected = self.pending_model_agent.take().unwrap_or(0);
                        self.overlay = Some(Overlay::AgentModels { selected });
                    } else {
                        self.overlay = None;
                    }
                }
                self.toast(text, ToastKind::Error);
            }
            TuiEvent::Notice(text) => self.toast(text, ToastKind::Info),
            TuiEvent::Error(text) => {
                self.running = false;
                if let Some(last) = self.messages.last_mut() {
                    last.streaming = false;
                }
                self.toast(text, ToastKind::Error);
            }
        }
    }

    fn handle_stream(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Delta(delta) => {
                let streaming = matches!(
                    self.messages.last(),
                    Some(Message {
                        kind: MessageKind::Assistant | MessageKind::Agent { .. },
                        streaming: true,
                        ..
                    })
                );
                if streaming {
                    if let Some(last) = self.messages.last_mut() {
                        last.text.push_str(&delta);
                    }
                } else {
                    let mut message = Message::assistant(delta);
                    message.streaming = true;
                    self.messages.push(message);
                }
            }
            StreamEvent::Reasoning(text) => {
                let streaming = matches!(
                    self.messages.last(),
                    Some(Message {
                        kind: MessageKind::Assistant | MessageKind::Agent { .. },
                        streaming: true,
                        ..
                    })
                );
                if streaming {
                    if let Some(last) = self.messages.last_mut() {
                        last.reasoning.push_str(&text);
                    }
                } else {
                    let mut message = Message::assistant(String::new());
                    message.streaming = true;
                    message.reasoning = text;
                    self.messages.push(message);
                }
            }
            StreamEvent::ToolCall { id, name, summary } => {
                if let Some(last) = self.messages.last_mut() {
                    last.streaming = false;
                }
                let tool = ToolUi {
                    id,
                    name,
                    summary,
                    status: ToolStatus::Running,
                    output: String::new(),
                    expanded: false,
                    max_lines: 8,
                };
                self.messages.push(Message {
                    kind: MessageKind::Tool(tool),
                    text: String::new(),
                    reasoning: String::new(),
                    streaming: false,
                });
            }
            StreamEvent::ToolResult { id, name, output } => {
                let status = if output.starts_with("Error")
                    || output.starts_with("Permission denied")
                    || output.trim().is_empty()
                {
                    ToolStatus::Error
                } else {
                    ToolStatus::Ok
                };
                let mut found = false;
                for message in self.messages.iter_mut().rev() {
                    if let MessageKind::Tool(tool) = &mut message.kind
                        && tool.id == id
                    {
                        tool.status = status;
                        tool.output = output.clone();
                        tool.expanded = output.lines().count() <= 6;
                        found = true;
                        break;
                    }
                }
                if !found {
                    let tool = ToolUi {
                        id,
                        name,
                        summary: String::new(),
                        status,
                        output,
                        expanded: false,
                        max_lines: 8,
                    };
                    self.messages.push(Message {
                        kind: MessageKind::Tool(tool),
                        text: String::new(),
                        reasoning: String::new(),
                        streaming: false,
                    });
                }
            }
            StreamEvent::Usage(usage) => self.usage = usage,
            StreamEvent::Done => {
                if let Some(last) = self.messages.last_mut() {
                    last.streaming = false;
                }
            }
        }
    }

    fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::StackPrompt { stack } => {
                self.messages
                    .push(Message::system(format!("stack selected: {stack}")));
            }
            AgentEvent::PlanCreated { path } => {
                self.messages
                    .push(Message::system(format!("plan written: {}", path.display())));
            }
            AgentEvent::AgentStarted { name, role } => {
                self.agent_status.insert(name.clone(), AgentStatus::Running);
                self.messages
                    .push(Message::system(format!("@{} started ({role})", name)));
            }
            AgentEvent::AgentMessage { agent, text } => {
                let streaming = matches!(
                    self.messages.last(),
                    Some(Message {
                        kind: MessageKind::Agent { name },
                        streaming: true,
                        ..
                    }) if *name == agent
                );
                if streaming {
                    if let Some(last) = self.messages.last_mut() {
                        last.text.push_str(&text);
                    }
                } else {
                    let mut message = Message::agent(agent, text);
                    message.streaming = true;
                    self.messages.push(message);
                }
            }
            AgentEvent::TodoUpdated { .. } => {
                if let Ok(items) = crate::agent::todos::read_master(&self.cwd) {
                    self.todos = items;
                }
            }
            AgentEvent::AgentFinished { name, status } => {
                let done = !status.contains("error");
                self.agent_status.insert(
                    name.clone(),
                    if done {
                        AgentStatus::Done
                    } else {
                        AgentStatus::Failed
                    },
                );
                if let Some(last) = self.messages.last_mut() {
                    last.streaming = false;
                }
                self.messages
                    .push(Message::system(format!("@{} finished ({status})", name)));
            }
            AgentEvent::MentionRouted { to, from, text } => {
                let brief = text.split('\n').next().unwrap_or_default();
                self.messages
                    .push(Message::system(format!("{from} → @{to}: {brief}")));
            }
        }
    }

    // --- Mention completion ------------------------------------------------

    fn recompute_mention(&mut self) {
        let text = self.input.text();
        let cursor = self.input.cursor();
        if cursor == 0 || cursor > text.len() {
            self.mention = None;
            return;
        }
        let before = &text[..cursor];
        let token_start = before
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        let token = &before[token_start..];
        if !token.starts_with('@') {
            self.mention = None;
            return;
        }
        let after = &text[cursor..];
        if after
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            self.mention = None;
            return;
        }
        let query = &token[1..];
        let candidates: Vec<String> = self
            .roster
            .iter()
            .map(|agent| agent.name.clone())
            .filter(|name| name.starts_with(query))
            .collect();
        if candidates.is_empty() {
            self.mention = None;
            return;
        }
        let selected = self
            .mention
            .as_ref()
            .filter(|_| query.is_empty())
            .map(|m| m.selected)
            .unwrap_or(0);
        self.mention = Some(MentionPopup {
            candidates,
            selected,
        });
    }

    fn accept_mention(&mut self) {
        let Some(popup) = self.mention.take() else {
            return;
        };
        let Some(candidate) = popup.candidates.get(popup.selected) else {
            return;
        };
        let text = self.input.text().to_string();
        let cursor = self.input.cursor();
        let token_start = text[..cursor]
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        let mut new_text = String::new();
        new_text.push_str(&text[..token_start]);
        new_text.push_str(&format!("@{candidate} "));
        new_text.push_str(&text[cursor..]);
        self.input.set_text(new_text);
        self.input.set_cursor(token_start + candidate.len() + 2);
    }

    fn mention_prev(&mut self) {
        if let Some(popup) = self.mention.as_mut()
            && !popup.candidates.is_empty()
        {
            popup.selected = (popup.selected + popup.candidates.len() - 1) % popup.candidates.len();
        }
    }

    fn mention_next(&mut self) {
        if let Some(popup) = self.mention.as_mut()
            && !popup.candidates.is_empty()
        {
            popup.selected = (popup.selected + 1) % popup.candidates.len();
        }
    }

    // --- Toasts ------------------------------------------------------------

    fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toasts.push_back((text.into(), kind, Instant::now()));
        if self.toasts.len() > 5 {
            self.toasts.pop_front();
        }
    }

    fn expire_toasts(&mut self) {
        let now = Instant::now();
        while let Some(front) = self.toasts.front() {
            if now.duration_since(front.2) > Duration::from_secs(6) {
                self.toasts.pop_front();
            } else {
                break;
            }
        }
    }

    fn has_streaming(&self) -> bool {
        self.messages.last().is_some_and(|m| m.streaming)
    }

    fn input_width(&self) -> usize {
        // The app calls this with the current terminal width inside draw(); for
        // key handling we approximate with the terminal size.
        let (cols, _) = super::terminal::Terminal::size();
        (cols as usize).saturating_sub(2).max(1)
    }

    // --- Drawing -----------------------------------------------------------

    fn draw(&mut self) -> Result<(), DoreanError> {
        let theme = self.theme;
        self.expire_toasts();
        let (cols, rows) = super::terminal::Terminal::size();
        let width = cols as usize;
        let height = rows as usize;
        if width < 20 || height < 6 {
            return Ok(());
        }

        let mut screen = Screen::new(width, height, theme.bg);

        let footer_h = 1usize;
        let input_lines = self
            .input
            .visual_lines(width.saturating_sub(2))
            .len()
            .clamp(1, 5);
        let input_h = input_lines + 1;
        let roster_h = if self.roster.is_empty() { 0 } else { 1 };
        let input_top = height - footer_h - input_h;
        let roster_top = input_top - roster_h;
        let content_bottom = roster_top;

        let panel_w = if self.show_todos {
            (width * 3) / 4
        } else {
            width
        };
        self.draw_messages(&mut screen, 1, content_bottom, panel_w);
        if self.show_todos && panel_w < width {
            self.draw_todos(&mut screen, panel_w, 1, width - panel_w, content_bottom - 1);
        }
        if roster_h > 0 {
            self.draw_roster(&mut screen, width, roster_top);
        }
        self.draw_header(&mut screen, width);
        self.draw_input(&mut screen, width, input_top, input_h);
        self.draw_footer(&mut screen, width, height - 1);
        self.draw_overlays(&mut screen, width, height);
        self.draw_toasts(&mut screen, width);

        let mut stdout = io::stdout();
        screen.flush(&mut stdout, &mut self.prev)?;
        Ok(())
    }

    fn draw_header(&mut self, screen: &mut Screen, width: usize) {
        let theme = self.theme;
        screen.fill(0, 0, width, 1, Screen::blank_cell(theme.status_bg));
        let provider = self.config.provider.to_string();
        let mut line = format!(" {}  ·  {}", self.model, provider);
        if self.turns > 0 {
            line.push_str(&format!("  ·  turn {}", self.turns));
        }
        if self.usage.total_tokens > 0 {
            line.push_str(&format!("  ·  {} tok", self.usage.total_tokens));
        }
        let clipped = Screen::clip(&line, width.saturating_sub(14));
        screen.put_str(0, 0, &clipped, theme.fg, theme.status_bg, Attrs::none());

        let (glyph, color) = if self.running {
            let spin = ['◐', '◓', '◑', '◒'][self.animate as usize % 4];
            (format!("{spin} running"), theme.warning)
        } else {
            ("○ idle".to_string(), theme.dim)
        };
        let x = width.saturating_sub(glyph.len());
        screen.put_str(x, 0, &glyph, color, theme.status_bg, Attrs::bold());
    }

    fn draw_messages(&mut self, screen: &mut Screen, top: usize, bottom: usize, width: usize) {
        let theme = self.theme;
        let content_h = bottom.saturating_sub(top);
        if content_h == 0 {
            return;
        }
        let heights: Vec<usize> = self
            .messages
            .iter()
            .map(|m| m.measure(width, &theme, self.show_thinking))
            .collect();
        let total: usize = heights.iter().sum();
        self.scroll = self.scroll.min(total.saturating_sub(content_h));
        let hidden = total.saturating_sub(self.scroll).saturating_sub(content_h);

        let mut index = 0usize;
        let mut skip = 0usize;
        if hidden > 0 {
            let mut acc = 0usize;
            for (idx, h) in heights.iter().enumerate() {
                if acc + h > hidden {
                    index = idx;
                    skip = hidden - acc;
                    break;
                }
                acc += h;
            }
        }

        // Draw from above the viewport so the screen clips the skipped top
        // rows; anything spilling past the bottom is painted over by the
        // header/input/footer drawn afterwards.
        let show = self.show_thinking;
        let mut y = top.saturating_sub(skip);
        while index < self.messages.len() && y < bottom {
            let h = heights[index];
            self.messages[index].draw_region(screen, 0, y, width, &theme, skip, show);
            y += h;
            index += 1;
            skip = 0;
        }
    }

    fn draw_input(&mut self, screen: &mut Screen, width: usize, top: usize, height: usize) {
        let theme = self.theme;
        let editable_w = width.saturating_sub(2);
        screen.hline(0, top, width, '─', theme.border, theme.bg);

        let text = self.input.text();
        let lines = self.input.visual_lines(editable_w);
        for (i, (start, len)) in lines.iter().enumerate().take(height.saturating_sub(1)) {
            let row = top + 1 + i;
            let line_text: String = text.chars().skip(*start).take(*len).collect();
            screen.put_str(1, row, &line_text, theme.fg, theme.bg, Attrs::none());
            screen.fill(
                1 + *len,
                row,
                width.saturating_sub(2 + *len),
                1,
                Screen::blank_cell(theme.bg),
            );
        }
        if text.is_empty() {
            screen.put_str(
                1,
                top + 1,
                "type a message · /make <goal> · /help",
                theme.dim,
                theme.bg,
                Attrs::none(),
            );
        }

        // Reverse-video block cursor on the current visual line.
        let (cline, ccol) = self.input.cursor_line_col(editable_w);
        if cline < height.saturating_sub(1) {
            let cx = 1 + ccol;
            let cy = top + 1 + cline;
            let ch = text
                .chars()
                .nth(text[..self.input.cursor()].chars().count())
                .unwrap_or(' ');
            screen.put_char(cx, cy, ch, theme.fg, theme.bg, Attrs::reversed());
        }
    }

    fn draw_footer(&mut self, screen: &mut Screen, width: usize, y: usize) {
        let theme = self.theme;
        screen.fill(0, y, width, 1, Screen::blank_cell(theme.bg));
        let hint = "⏎ send · ctrl+j newline · esc esc abort · pgup/pgdn scroll · /help";
        screen.put_str(
            1,
            y,
            &Screen::clip(hint, width.saturating_sub(2)),
            theme.dim,
            theme.bg,
            Attrs::none(),
        );
    }

    fn draw_roster(&mut self, screen: &mut Screen, width: usize, y: usize) {
        let theme = self.theme;
        screen.fill(0, y, width, 1, Screen::blank_cell(theme.bg));
        let mut parts: Vec<String> = Vec::new();
        for agent in &self.roster {
            let status = self
                .agent_status
                .get(&agent.name)
                .copied()
                .unwrap_or(AgentStatus::Done);
            let glyph = match status {
                AgentStatus::Running => "●",
                AgentStatus::Done => "✓",
                AgentStatus::Failed => "✗",
            };
            parts.push(format!("{glyph} @{}", agent.name));
        }
        let line = parts.join("   ");
        screen.put_str(
            1,
            y,
            &Screen::clip(&line, width.saturating_sub(2)),
            theme.agent,
            theme.bg,
            Attrs::none(),
        );
    }

    fn draw_todos(&mut self, screen: &mut Screen, x: usize, y: usize, w: usize, h: usize) {
        let theme = self.theme;
        screen.fill(x, y, w, h, Screen::blank_cell(theme.bg));
        screen.vline(x, y, h, '│', theme.border, theme.bg);
        screen.put_str(x + 2, y, "todos", theme.accent, theme.bg, Attrs::bold());

        let mut row = y + 2;
        let mut truncated = false;
        for agent in &self.roster {
            let items: Vec<_> = self
                .todos
                .iter()
                .filter(|t| t.agent == agent.name)
                .collect();
            if items.is_empty() {
                continue;
            }
            if row >= y + h - 1 {
                truncated = true;
                break;
            }
            screen.put_str(
                x + 2,
                row,
                &format!("@{}", agent.name),
                theme.agent,
                theme.bg,
                Attrs::bold(),
            );
            row += 1;
            for item in items {
                if row >= y + h - 1 {
                    truncated = true;
                    break;
                }
                let (marker, color) = match item.status {
                    TodoStatus::Pending => ("[ ]", theme.dim),
                    TodoStatus::InProgress => ("[~]", theme.warning),
                    TodoStatus::Done => ("[x]", theme.success),
                    TodoStatus::Blocked => ("[!]", theme.error),
                };
                let text = format!("{marker} {}", item.title);
                screen.put_str(
                    x + 2,
                    row,
                    &Screen::clip(&text, w.saturating_sub(3)),
                    color,
                    theme.bg,
                    Attrs::none(),
                );
                row += 1;
            }
        }
        if truncated {
            screen.put_str(x + 2, row, "▾", theme.dim, theme.bg, Attrs::none());
        }
    }

    fn draw_overlays(&mut self, screen: &mut Screen, width: usize, height: usize) {
        let Some(overlay) = self.overlay.take() else {
            return;
        };
        let theme = self.theme;
        match overlay {
            Overlay::Loading(text) => {
                let (bx, by, bw, _bh) = centered_box(width, height, 46, 5);
                screen.box_border(bx, by, bw, 5, &theme);
                screen.put_str(
                    bx + 2,
                    by + 2,
                    &Screen::clip(&text, bw.saturating_sub(4)),
                    theme.dim,
                    theme.bg,
                    Attrs::none(),
                );
                self.overlay = Some(Overlay::Loading(text));
            }
            Overlay::Help => {
                Self::draw_help(screen, &theme, width, height);
                self.overlay = Some(Overlay::Help);
            }
            Overlay::Quit => {
                Self::draw_quit(screen, &theme, width, height);
                self.overlay = Some(Overlay::Quit);
            }
            Overlay::Model(mut list) => {
                Self::draw_list(screen, &theme, &mut list, width, height);
                self.overlay = Some(Overlay::Model(list));
            }
            Overlay::Stack(mut list) => {
                Self::draw_list(screen, &theme, &mut list, width, height);
                self.overlay = Some(Overlay::Stack(list));
            }
            Overlay::AgentModels { selected } => {
                self.draw_agent_models(screen, &theme, width, height, selected);
                self.overlay = Some(Overlay::AgentModels { selected });
            }
            Overlay::ApiKey { input } => {
                Self::draw_api_key(screen, &theme, width, height, &input, self.config.provider);
                self.overlay = Some(Overlay::ApiKey { input });
            }
            Overlay::Permission { prompt, reply } => {
                Self::draw_permission(screen, &theme, width, height, &prompt);
                self.overlay = Some(Overlay::Permission { prompt, reply });
            }
            Overlay::Spec => {
                self.draw_spec(screen, width, height);
                self.overlay = Some(Overlay::Spec);
            }
        }
    }

    /// The per-sub-agent model assignment overlay: one row per agent showing
    /// its current brain, plus a "start build" row. The selected row is
    /// highlighted; Enter on an agent opens the model picker.
    fn draw_agent_models(
        &mut self,
        screen: &mut Screen,
        theme: &Theme,
        width: usize,
        height: usize,
        selected: usize,
    ) {
        let Some(roster) = self.agent_models.as_ref() else {
            return;
        };
        let rows = roster.len() + 1;
        let bw = 66usize.min(width);
        let bh = (rows + 4).min(height);
        let (bx, by, bw, bh) = centered_box(width, height, bw, bh);
        screen.box_border(bx, by, bw, bh, theme);
        screen.put_str(
            bx + 2,
            by,
            " sub-agent brains — pick a model per agent",
            theme.accent,
            theme.bg,
            Attrs::bold(),
        );
        let mut row = by + 2;
        for (i, agent) in roster.iter().enumerate() {
            if row >= by + bh - 1 {
                break;
            }
            let is_sel = i == selected;
            let bg = if is_sel { theme.selection_bg } else { theme.bg };
            screen.fill(bx + 1, row, bw.saturating_sub(2), 1, Screen::blank_cell(bg));
            screen.put_str(
                bx + 2,
                row,
                &format!("@{}", agent.name),
                theme.agent,
                bg,
                Attrs::bold(),
            );
            let model = agent
                .model
                .clone()
                .unwrap_or_else(|| "default (main model)".to_string());
            screen.put_str(
                bx + 4 + agent.name.len(),
                row,
                &Screen::clip(&model, bw.saturating_sub(agent.name.len() + 6)),
                if is_sel { theme.fg } else { theme.dim },
                bg,
                Attrs::none(),
            );
            row += 1;
        }
        if row < by + bh - 1 {
            let is_sel = roster.len() == selected;
            let bg = if is_sel { theme.selection_bg } else { theme.bg };
            screen.fill(bx + 1, row, bw.saturating_sub(2), 1, Screen::blank_cell(bg));
            screen.put_str(
                bx + 2,
                row,
                "▶ start build",
                if is_sel { theme.accent } else { theme.success },
                bg,
                Attrs::bold(),
            );
            row += 1;
        }
        if row < by + bh - 1 {
            screen.put_str(
                bx + 2,
                row,
                "↑/↓ select · enter: pick brain · esc: start",
                theme.dim,
                theme.bg,
                Attrs::none(),
            );
        }
    }

    fn draw_api_key(
        screen: &mut Screen,
        theme: &Theme,
        width: usize,
        height: usize,
        input: &str,
        provider: Provider,
    ) {
        let (bx, by, bw, _bh) = centered_box(width, height, 58, 7);
        screen.box_border(bx, by, bw, 7, theme);
        let label = match provider {
            Provider::OpenRouter => " openrouter api key",
            Provider::Nvidia => " nvidia api key",
        };
        screen.put_str(bx + 2, by, label, theme.accent, theme.bg, Attrs::bold());
        screen.put_str(
            bx + 2,
            by + 2,
            &Screen::clip(
                "paste your key (ctrl+shift+v or middle-click)",
                bw.saturating_sub(4),
            ),
            theme.dim,
            theme.bg,
            Attrs::none(),
        );
        let masked: String = "*".repeat(input.chars().count());
        screen.put_str(
            bx + 2,
            by + 3,
            &Screen::clip(&masked, bw.saturating_sub(4)),
            theme.fg,
            theme.bg,
            Attrs::none(),
        );
        screen.put_str(
            bx + 2,
            by + 5,
            &Screen::clip(
                "stored in ~/.dorean/config.json · enter save · esc cancel",
                bw.saturating_sub(4),
            ),
            theme.dim,
            theme.bg,
            Attrs::none(),
        );
    }

    fn draw_spec(&mut self, screen: &mut Screen, width: usize, height: usize) {
        let theme = self.theme;
        let content_x = 2usize;
        let content_y = 2usize;
        let content_w = width.saturating_sub(4);
        let content_h = height.saturating_sub(4);
        screen.box_border(
            1,
            1,
            width.saturating_sub(2),
            height.saturating_sub(2),
            &theme,
        );
        screen.put_str(
            3,
            2,
            &Screen::clip(".dorean/SPEC.md", content_w),
            theme.accent,
            theme.bg,
            Attrs::bold(),
        );

        let lines = crate::tui::markdown::render_markdown(&self.spec_text, content_w);
        let total = lines.len();
        let max_scroll = total.saturating_sub(content_h.saturating_sub(1));
        self.overlay_scroll = self.overlay_scroll.min(max_scroll);

        for (y, line) in (content_y + 1..).zip(lines.iter().skip(self.overlay_scroll)) {
            if y >= content_y + content_h {
                break;
            }
            draw_md_line(
                screen, content_x, y, content_w, &theme, line, theme.bg, None, false,
            );
        }
    }

    fn draw_help(screen: &mut Screen, theme: &Theme, width: usize, height: usize) {
        let bw = 62usize.min(width);
        let bh = 17usize.min(height);
        let (bx, by, bw, bh) = centered_box(width, height, bw, bh);
        screen.box_border(bx, by, bw, bh, theme);
        screen.put_str(
            bx + 2,
            by,
            " dorean help",
            theme.accent,
            theme.bg,
            Attrs::bold(),
        );
        let entries: &[(&str, &str)] = &[
            ("⏎", "send · ctrl+j / shift+⏎ newline"),
            ("esc", "abort run (esc twice) · close overlay"),
            ("ctrl+c", "quit"),
            ("ctrl+u", "clear input · ctrl+w delete word"),
            ("↑/↓", "history or cursor · select in menus"),
            ("pgup/pgdn", "scroll the chat"),
            ("tab", "complete @agent mention"),
            ("/model", "switch model"),
            ("/key", "set openrouter api key"),
            ("/make <goal>", "parallel sub-agent build (+ model picker)"),
            ("/thinking", "show / hide the reasoning block"),
            ("/todo /spec", "todo panel · plan view"),
            ("/clear /quit", "reset chat · leave"),
        ];
        for (row, (key, desc)) in (by + 2..).zip(entries.iter()) {
            if row >= by + bh - 1 {
                break;
            }
            screen.put_str(bx + 2, row, key, theme.accent, theme.bg, Attrs::none());
            screen.put_str(
                bx + 26,
                row,
                &Screen::clip(desc, bw.saturating_sub(30)),
                theme.fg,
                theme.bg,
                Attrs::none(),
            );
        }
    }

    fn draw_quit(screen: &mut Screen, theme: &Theme, width: usize, height: usize) {
        let (bx, by, bw, _bh) = centered_box(width, height, 42, 5);
        screen.box_border(bx, by, bw, 5, theme);
        screen.put_str(
            bx + 2,
            by + 2,
            "Quit dorean?",
            theme.fg,
            theme.bg,
            Attrs::bold(),
        );
        screen.put_str(
            bx + 2,
            by + 3,
            "[y]es · [n]o · esc",
            theme.dim,
            theme.bg,
            Attrs::none(),
        );
    }

    fn draw_permission(
        screen: &mut Screen,
        theme: &Theme,
        width: usize,
        height: usize,
        prompt: &str,
    ) {
        let bw = 60usize.min(width);
        let bh = 8usize.min(height);
        let (bx, by, bw, _bh) = centered_box(width, height, bw, bh);
        screen.box_border(bx, by, bw, bh, theme);
        screen.put_str(
            bx + 2,
            by,
            " tool approval",
            theme.warning,
            theme.bg,
            Attrs::bold(),
        );
        let wrapped = crate::tui::render::word_wrap(prompt, bw.saturating_sub(6));
        for (row, line) in (by + 2..).zip(wrapped.iter().take(bh.saturating_sub(4))) {
            screen.put_str(
                bx + 2,
                row,
                &Screen::clip(line, bw.saturating_sub(6)),
                theme.fg,
                theme.bg,
                Attrs::none(),
            );
        }
        screen.put_str(
            bx + 2,
            by + bh - 2,
            "[y]es · [n]o · esc",
            theme.dim,
            theme.bg,
            Attrs::none(),
        );
    }

    fn draw_list(
        screen: &mut Screen,
        theme: &Theme,
        list: &mut SelectList,
        width: usize,
        height: usize,
    ) {
        let bw = 52usize.min(width);
        let bh = 14usize.min(height);
        let (bx, by, bw, _bh) = centered_box(width, height, bw, bh);
        list.viewport = bh.saturating_sub(4);
        screen.box_border(bx, by, bw, bh, theme);
        screen.put_str(
            bx + 2,
            by,
            &format!(" {}", list.title),
            theme.accent,
            theme.bg,
            Attrs::bold(),
        );
        screen.put_str(
            bx + 2,
            by + 2,
            &format!(
                "filter: {}",
                Screen::clip(&list.filter, bw.saturating_sub(12))
            ),
            theme.fg,
            theme.bg,
            Attrs::none(),
        );
        let filtered = list.filtered();
        for (row, (i, item)) in (by + 3..).zip(
            filtered
                .iter()
                .enumerate()
                .skip(list.scroll)
                .take(list.viewport),
        ) {
            if row >= by + bh - 1 {
                break;
            }
            let selected = i == list.selected;
            let bg = if selected {
                theme.selection_bg
            } else {
                theme.bg
            };
            let marker = if selected { "›" } else { " " };
            screen.fill(bx + 1, row, bw.saturating_sub(2), 1, Screen::blank_cell(bg));
            let label = format!("{marker} {item}");
            screen.put_str(
                bx + 2,
                row,
                &Screen::clip(&label, bw.saturating_sub(4)),
                theme.fg,
                bg,
                Attrs::none(),
            );
        }
    }

    fn draw_toasts(&mut self, screen: &mut Screen, width: usize) {
        let theme = self.theme;
        let mut cx = width;
        for (text, kind, _created) in self.toasts.iter() {
            let color = match kind {
                ToastKind::Info => theme.dim,
                ToastKind::Success => theme.success,
                ToastKind::Error => theme.error,
            };
            let label = format!(" {text} ");
            let len = label.chars().count();
            let x = cx.saturating_sub(len);
            screen.put_str(x, 1, &label, color, theme.bg, Attrs::none());
            if x < 2 {
                break;
            }
            cx = x.saturating_sub(1);
        }
    }
}

fn centered_box(width: usize, height: usize, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let x = width.saturating_sub(w) / 2;
    let y = height.saturating_sub(h) / 2;
    (x, y, w.min(width), h.min(height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AbortHandle;

    fn test_app() -> App {
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let (_events_tx, events_rx) = mpsc::unbounded_channel();
        let (_approve_tx, approve_rx) = mpsc::unbounded_channel();
        App::new(
            Config::default(),
            PathBuf::from("/tmp"),
            Theme::dark(),
            cmd_tx,
            events_rx,
            approve_rx,
            AbortHandle::new(),
            false,
        )
        .unwrap()
    }

    #[test]
    fn release_events_do_not_type() {
        let mut app = test_app();
        app.handle_term_event(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('g'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )));
        assert_eq!(app.input.text(), "");
        app.handle_term_event(Event::Key(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::NONE,
        )));
        assert_eq!(app.input.text(), "g");
    }

    #[test]
    fn repeat_events_still_type() {
        let mut app = test_app();
        app.handle_term_event(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('g'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )));
        assert_eq!(app.input.text(), "g");
    }

    #[test]
    fn model_list_failed_dismisses_loading_overlay() {
        let mut app = test_app();
        app.overlay = Some(Overlay::Loading("fetching free models…".to_string()));
        app.handle_tui_event(TuiEvent::ModelListFailed("no API key".to_string()));
        assert!(app.overlay.is_none());
        assert_eq!(app.toasts.len(), 1);
        assert!(
            matches!(app.toasts.front(), Some((text, ToastKind::Error, _)) if text == "no API key")
        );
    }

    #[test]
    fn esc_dismisses_loading_overlay() {
        let mut app = test_app();
        app.overlay = Some(Overlay::Loading("fetching free models…".to_string()));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.overlay.is_none());
    }

    #[test]
    fn key_command_opens_api_key_overlay_and_edits() {
        let mut app = test_app();
        app.run_command("key");
        assert!(matches!(app.overlay, Some(Overlay::ApiKey { .. })));
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert!(matches!(
            app.overlay,
            Some(Overlay::ApiKey { ref input }) if input == "sk"
        ));
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(matches!(
            app.overlay,
            Some(Overlay::ApiKey { ref input }) if input == "s"
        ));
    }

    #[test]
    fn paste_into_api_key_overlay() {
        let mut app = test_app();
        app.overlay = Some(Overlay::ApiKey {
            input: String::new(),
        });
        app.handle_term_event(Event::Paste("sk-live-key".to_string()));
        assert!(matches!(
            app.overlay,
            Some(Overlay::ApiKey { ref input }) if input == "sk-live-key"
        ));
    }

    #[test]
    fn esc_closes_api_key_overlay() {
        let mut app = test_app();
        app.overlay = Some(Overlay::ApiKey {
            input: "abc".to_string(),
        });
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.overlay.is_none());
    }

    fn running_app() -> (App, mpsc::UnboundedReceiver<SessionCmd>) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (_events_tx, events_rx) = mpsc::unbounded_channel();
        let (_approve_tx, approve_rx) = mpsc::unbounded_channel();
        let mut app = App::new(
            Config::default(),
            PathBuf::from("/tmp"),
            Theme::dark(),
            cmd_tx,
            events_rx,
            approve_rx,
            AbortHandle::new(),
            false,
        )
        .unwrap();
        app.running = true;
        (app, cmd_rx)
    }

    #[test]
    fn first_esc_arms_without_aborting() {
        let (mut app, mut cmd_rx) = running_app();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.esc_armed.is_some());
        assert!(cmd_rx.try_recv().is_err());
    }

    #[test]
    fn second_esc_aborts() {
        let (mut app, mut cmd_rx) = running_app();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.esc_armed.is_none());
        assert!(matches!(cmd_rx.try_recv(), Ok(SessionCmd::Abort)));
    }

    #[test]
    fn esc_arm_expires_after_window() {
        let (mut app, mut cmd_rx) = running_app();
        app.esc_armed = Some(Instant::now() - Duration::from_millis(3000));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        // Re-armed, not aborted.
        assert!(app.esc_armed.is_some());
        assert!(cmd_rx.try_recv().is_err());
    }

    #[test]
    fn other_key_disarms_esc() {
        let (mut app, mut cmd_rx) = running_app();
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.esc_armed.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert!(app.esc_armed.is_none());
        assert!(cmd_rx.try_recv().is_err());
    }

    #[test]
    fn thinking_command_toggles_display() {
        let mut app = test_app();
        assert!(app.show_thinking);
        app.run_command("thinking");
        assert!(!app.show_thinking);
        app.run_command("thinking");
        assert!(app.show_thinking);
    }

    #[test]
    fn make_opens_stack_picker() {
        let mut app = test_app();
        app.run_command("make build a notes app");
        assert!(matches!(app.overlay, Some(Overlay::Stack(_))));
        assert_eq!(app.pending_goal.as_deref(), Some("build a notes app"));
    }

    #[test]
    fn make_flow_assigns_models_then_starts() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let (_events_tx, events_rx) = mpsc::unbounded_channel();
        let (_approve_tx, approve_rx) = mpsc::unbounded_channel();
        let mut app = App::new(
            Config::default(),
            PathBuf::from("/tmp"),
            Theme::dark(),
            cmd_tx,
            events_rx,
            approve_rx,
            AbortHandle::new(),
            false,
        )
        .unwrap();
        app.run_command("make build it");
        app.start_orchestration("full-stack".to_string());
        assert!(app.agent_models.is_some());
        assert!(matches!(app.overlay, Some(Overlay::AgentModels { .. })));

        // Enter on the first agent opens its model picker.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.pending_model_agent, Some(0));
        assert!(matches!(app.overlay, Some(Overlay::Loading(_))));
        assert!(matches!(cmd_rx.try_recv(), Ok(SessionCmd::FetchModels)));

        // The model list arrives; choosing one assigns it to the agent.
        app.overlay = Some(Overlay::Model(SelectList::new(
            "model",
            vec!["meta-llama/llama-3.3-70b-instruct:free".to_string()],
        )));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let roster = app.agent_models.as_ref().unwrap();
        assert_eq!(
            roster[0].model.as_deref(),
            Some("meta-llama/llama-3.3-70b-instruct:free")
        );
        assert!(app.pending_model_agent.is_none());
        assert!(matches!(app.overlay, Some(Overlay::AgentModels { .. })));

        // Esc starts the build with the configured roster.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.running);
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(SessionCmd::Orchestrate { .. })
        ));
    }

    #[test]
    fn make_flow_esc_from_model_picker_returns_to_list() {
        let mut app = test_app();
        app.run_command("make build it");
        app.start_orchestration("full-stack".to_string());
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // Simulate the list arriving, then Esc out of the picker.
        app.overlay = Some(Overlay::Model(SelectList::new(
            "model",
            vec!["m1".to_string()],
        )));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.overlay, Some(Overlay::AgentModels { .. })));
        assert!(app.pending_model_agent.is_none());
    }
}
