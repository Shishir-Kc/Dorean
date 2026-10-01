//! The chat TUI app: event loop, session state, streaming bubbles, overlays,
//! and keyboard-driven layout. Renders into a diffed [`Screen`] each frame.

use std::collections::{HashMap, VecDeque};
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;

use crate::agent::events::{AgentEvent, StreamEvent};
use crate::agent::manifest::AgentManifest;
use crate::agent::stack::STACKS;
use crate::agent::todos::TodoStatus;
use crate::config::{Config, Provider};
use crate::error::DoreanError;
use crate::history::SessionRecord;
use crate::permissions::PermissionMode;
use crate::providers::client::{Role, Usage};
use crate::providers::default_model;

use super::SessionCmd;
use super::components::message::draw_md_line;
use super::components::{Input, Message, MessageKind, SelectList, ToolStatus, ToolUi};
use super::events::TuiEvent;
use super::render::{Attrs, Screen};
use super::terminal::Terminal;
use super::theme::{THEME_AUTO, Theme};

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
    Provider,
    Stack,
    PermMode,
    Theme,
    Session,
}

/// Modal overlays layered above the chat view.
enum Overlay {
    /// A transient "fetching…" state (e.g. before the model list arrives).
    Loading(String),
    /// Filterable list of models (aggregated across providers).
    Model(SelectList),
    /// Provider picker for `/key` (and anywhere a provider is chosen).
    Provider(SelectList),
    /// Filterable list of stack presets for `/make`.
    Stack(SelectList),
    /// Tool-approval policy picker (`/permission`).
    PermMode(SelectList),
    /// Theme picker (`/theme`): auto/light/dark + custom JSON themes.
    Theme(SelectList),
    /// Saved-session picker (`/sessions`).
    Sessions(SelectList),
    /// Per-sub-agent model assignment: pick a brain for each agent, then start.
    AgentModels { selected: usize },
    /// A tool-approval question awaiting an answer on `reply`.
    Permission {
        prompt: String,
        reply: std::sync::mpsc::Sender<bool>,
    },
    /// Enter (or replace) the API key for the scoped provider; the buffer is
    /// masked. Picked from the provider picker (`/key`), never assumed.
    ApiKey { provider: Provider, input: String },
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
    /// Saved sessions backing the `/sessions` selector (agent, record),
    /// newest first, matching the selector's item order.
    sessions: Vec<(String, SessionRecord)>,
    /// Id of the latest model-list fetch. Responses echo it; superseded
    /// arrivals are ignored so a late fetch can't populate the wrong picker
    /// (or kill a newer spinner).
    model_fetch_id: u64,
    /// Current terminal size, updated on `Event::Resize` (and used by tests
    /// to render frames deterministically).
    term_size: (u16, u16),
    /// Owned `Terminal` (alt screen + raw mode), so the app can suspend it
    /// around an external `$EDITOR` and restore it afterwards.
    terminal: Option<Terminal>,
    /// Set while an external editor owns the terminal, so the event-reader
    /// thread stops consuming keys.
    suspend: Arc<AtomicBool>,
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
            sessions: Vec::new(),
            model_fetch_id: 0,
            term_size: Terminal::size(),
            terminal: None,
            suspend: Arc::new(AtomicBool::new(false)),
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

    /// Take ownership of the terminal (moved from `tui::run`) so the app can
    /// suspend it around an external `$EDITOR`.
    pub fn attach_terminal(&mut self, terminal: Terminal, suspend: Arc<AtomicBool>) {
        self.terminal = Some(terminal);
        self.suspend = suspend;
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
            Event::Resize(width, height) => self.term_size = (width, height),
            Event::Paste(text) => match self.overlay.take() {
                Some(Overlay::ApiKey {
                    provider,
                    mut input,
                }) => {
                    input.push_str(&text);
                    self.overlay = Some(Overlay::ApiKey { provider, input });
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
            Some(Overlay::Provider(list)) => {
                self.handle_select_key(key, list, SelectKind::Provider)
            }
            Some(Overlay::Stack(list)) => self.handle_select_key(key, list, SelectKind::Stack),
            Some(Overlay::PermMode(list)) => {
                self.handle_select_key(key, list, SelectKind::PermMode)
            }
            Some(Overlay::Theme(list)) => self.handle_select_key(key, list, SelectKind::Theme),
            Some(Overlay::Sessions(list)) => self.handle_select_key(key, list, SelectKind::Session),
            Some(Overlay::AgentModels { selected }) => self.handle_agent_models_key(key, selected),
            Some(Overlay::ApiKey { provider, input }) => {
                self.handle_api_key_key(key, provider, input)
            }
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
                    let (provider, id) = Self::parse_model_row(&value, self.config.provider);
                    if let Some(index) = self.pending_model_agent.take() {
                        let agent_name = if let Some(roster) = self.agent_models.as_mut()
                            && index < roster.len()
                        {
                            roster[index].model = Some(id.clone());
                            // Remember a cross-provider pick so the sub-agent
                            // runs against the right backend.
                            roster[index].provider =
                                (provider != self.config.provider).then_some(provider);
                            roster[index].name.clone()
                        } else {
                            String::new()
                        };
                        if agent_name.is_empty() {
                            self.apply_model_choice(provider, id);
                        } else {
                            self.overlay = Some(Overlay::AgentModels { selected: index });
                            self.toast(
                                format!("@{agent_name} brain → {provider}/{id}"),
                                ToastKind::Info,
                            );
                        }
                    } else {
                        self.apply_model_choice(provider, id);
                    }
                }
                SelectKind::Provider => self.choose_provider(&value),
                SelectKind::Stack => self.start_orchestration(value),
                SelectKind::PermMode => self.apply_permission(&value),
                SelectKind::Theme => self.apply_theme(&value),
                SelectKind::Session => self.load_session(&value),
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
                    SelectKind::Provider => Overlay::Provider(list),
                    SelectKind::Stack => Overlay::Stack(list),
                    SelectKind::PermMode => Overlay::PermMode(list),
                    SelectKind::Theme => Overlay::Theme(list),
                    SelectKind::Session => Overlay::Sessions(list),
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
                    self.request_models();
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

    fn handle_api_key_key(&mut self, key: KeyEvent, provider: Provider, mut input: String) {
        let close = matches!(key.code, KeyCode::Esc);
        match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                self.submit_api_key(provider, input.trim().to_string());
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
            self.overlay = Some(Overlay::ApiKey { provider, input });
        }
    }

    /// Persist the entered key for the scoped provider, switch to it, and jump
    /// straight into the aggregated model selector so the user can confirm the
    /// key works.
    fn submit_api_key(&mut self, provider: Provider, key: String) {
        if key.is_empty() {
            return;
        }
        match provider {
            crate::config::Provider::OpenRouter => {
                self.config.openrouter_api_key = Some(key.clone());
            }
            crate::config::Provider::Nvidia => {
                self.config.nvidia_api_key = Some(key.clone());
            }
            crate::config::Provider::DeepSeek => {
                self.config.deepseek_api_key = Some(key.clone());
            }
            crate::config::Provider::Generic => {
                self.config.generic_api_key = Some(key.clone());
            }
            crate::config::Provider::Local => {}
        }
        if let Err(e) = self.config.save() {
            self.toast(format!("failed to save config: {e}"), ToastKind::Error);
        }
        self.switch_provider(provider);
        let _ = self.cmd_tx.send(SessionCmd::SetApiKey { provider, key });
        self.request_models();
    }

    /// Switch the active provider immediately: persist config, tell the
    /// background agent to rebuild, and point the header at the new default
    /// model until the user picks one.
    fn switch_provider(&mut self, provider: Provider) {
        self.config.provider = provider;
        if let Err(e) = self.config.save() {
            self.toast(format!("failed to save config: {e}"), ToastKind::Error);
        }
        self.model = self
            .config
            .model
            .clone()
            .unwrap_or_else(|| default_model(provider).to_string());
        let _ = self.cmd_tx.send(SessionCmd::SetProvider(provider));
    }

    /// Apply a model choice from the aggregated picker: switch providers when
    /// the row belongs elsewhere, then set the model.
    fn apply_model_choice(&mut self, provider: Provider, id: String) {
        if provider != self.config.provider {
            self.switch_provider(provider);
        }
        self.model = id.clone();
        let _ = self.cmd_tx.send(SessionCmd::SetModel(id));
    }

    /// Rows for the provider picker: id + key status. The id is the first
    /// whitespace-delimited token so Enter can parse it back.
    fn provider_rows(&self) -> Vec<String> {
        [
            Provider::OpenRouter,
            Provider::Nvidia,
            Provider::DeepSeek,
            Provider::Local,
            Provider::Generic,
        ]
        .iter()
        .map(|p| {
            let status = match p {
                Provider::OpenRouter if self.config.openrouter_api_key.is_some() => "● key set",
                Provider::Nvidia if self.config.nvidia_api_key.is_some() => "● key set",
                Provider::DeepSeek if self.config.deepseek_api_key.is_some() => "● key set",
                Provider::Generic if self.config.generic_api_key.is_some() => "● key set",
                Provider::Local => "no key needed",
                _ => "○ no key",
            };
            let active = if *p == self.config.provider {
                " · active"
            } else {
                ""
            };
            format!("{p} — {status}{active}")
        })
        .collect()
    }

    /// Enter on a provider-picker row: switch immediately, then open that
    /// provider's key entry (local needs none — go straight to models).
    fn choose_provider(&mut self, row: &str) {
        let id = row.split_whitespace().next().unwrap_or_default();
        let Ok(provider) = id.parse::<Provider>() else {
            self.toast(format!("unknown provider `{id}`"), ToastKind::Error);
            return;
        };
        self.switch_provider(provider);
        self.open_api_key_for(provider);
    }

    /// Open the masked key entry for a provider (local skips to fetch).
    fn open_api_key_for(&mut self, provider: Provider) {
        if provider == Provider::Local {
            self.toast("local needs no key — fetching models…", ToastKind::Info);
            self.request_models();
            return;
        }
        self.overlay = Some(Overlay::ApiKey {
            provider,
            input: String::new(),
        });
    }

    /// Open the Loading overlay and ask the background task for a fresh model
    /// list. Each request gets a new id; arrivals echo it so superseded
    /// fetches are ignored instead of populating the wrong picker.
    fn request_models(&mut self) {
        self.model_fetch_id += 1;
        let id = self.model_fetch_id;
        self.overlay = Some(Overlay::Loading("fetching models…".to_string()));
        let _ = self.cmd_tx.send(SessionCmd::FetchModels { id });
    }

    /// Split an aggregated model-picker row back into (provider, id).
    /// Rows are `"provider  id[ · free]"` (two-space separator, optional free
    /// badge); bare ids without a separator belong to the active provider
    /// (older fixtures, ad-hoc rows).
    fn parse_model_row(row: &str, active: Provider) -> (Provider, String) {
        if let Some((provider, id)) = row.split_once("  ")
            && let Ok(provider) = provider.parse::<Provider>()
            && !id.is_empty()
        {
            let id = id.strip_suffix(" · free").unwrap_or(id);
            return (provider, id.to_string());
        }
        (active, row.to_string())
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
            KeyCode::Char('z') if ctrl => {
                self.mention = None;
                if !self.input.undo() {
                    self.toast("nothing to undo", ToastKind::Info);
                }
            }
            KeyCode::Char('y') if ctrl => {
                self.mention = None;
                if !self.input.redo() {
                    self.toast("nothing to redo", ToastKind::Info);
                }
            }
            KeyCode::Char('e') if ctrl => self.edit_in_editor(),
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
            "model" => self.request_models(),
            "key" => {
                if arg.is_empty() {
                    self.overlay = Some(Overlay::Provider(SelectList::new(
                        "provider — pick one, then enter its key",
                        self.provider_rows(),
                    )));
                } else if let Ok(provider) = arg.parse::<Provider>() {
                    // `/key nvidia`: switch immediately, jump to key entry.
                    self.switch_provider(provider);
                    self.open_api_key_for(provider);
                } else {
                    self.toast(
                        format!("unknown provider `{arg}` — try /key with no args"),
                        ToastKind::Error,
                    );
                }
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
            "copy" => self.copy_last_reply(arg),
            "permission" => {
                if arg.is_empty() {
                    let items = ["allow", "ask", "deny"]
                        .iter()
                        .map(|m| m.to_string())
                        .collect();
                    self.overlay =
                        Some(Overlay::PermMode(SelectList::new("permission mode", items)));
                } else {
                    self.apply_permission(arg);
                }
            }
            "theme" => {
                if arg.is_empty() {
                    let mut items = vec![
                        THEME_AUTO.to_string(),
                        "light".to_string(),
                        "dark".to_string(),
                    ];
                    items.extend(Theme::custom_theme_names());
                    self.overlay = Some(Overlay::Theme(SelectList::new("theme", items)));
                } else {
                    self.apply_theme(arg);
                }
            }
            "sessions" => self.open_sessions_selector(),
            "regenerate" => self.regenerate_last(),
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

    /// `/copy [all]`: copy the last assistant/agent reply (or the whole
    /// conversation) to the system clipboard.
    fn copy_last_reply(&mut self, arg: &str) {
        let text = if arg == "all" {
            self.messages
                .iter()
                .filter(|m| {
                    matches!(
                        m.kind,
                        MessageKind::User | MessageKind::Assistant | MessageKind::Agent { .. }
                    )
                })
                .map(|m| m.text.clone())
                .collect::<Vec<_>>()
                .join("\n\n")
        } else {
            self.messages
                .iter()
                .rev()
                .find(|m| matches!(m.kind, MessageKind::Assistant | MessageKind::Agent { .. }))
                .map(|m| m.text.clone())
                .unwrap_or_default()
        };
        if text.trim().is_empty() {
            self.toast("nothing to copy yet", ToastKind::Error);
            return;
        }
        match crate::clipboard::copy_text(&text) {
            Ok(()) => self.toast(
                format!("copied {} chars", text.chars().count()),
                ToastKind::Success,
            ),
            Err(e) => self.toast(e, ToastKind::Error),
        }
    }

    /// Apply a new tool-approval policy (`/permission` or its selector):
    /// persists to config and rebuilds the agent's gatekeeper.
    fn apply_permission(&mut self, raw: &str) {
        match raw.parse::<PermissionMode>() {
            Ok(mode) => {
                self.config.permission_mode = Some(mode);
                if let Err(e) = self.config.save() {
                    self.toast(format!("failed to save config: {e}"), ToastKind::Error);
                }
                let _ = self.cmd_tx.send(SessionCmd::SetPermission(mode));
                self.toast(format!("permission mode → {mode}"), ToastKind::Success);
            }
            Err(e) => self.toast(format!("{e}"), ToastKind::Error),
        }
    }

    /// Apply a theme choice (`/theme` or its selector): `auto` re-detects from
    /// the live terminal (OSC 11), `light`/`dark` are built in, anything else
    /// must be a JSON theme in `$DOREAN_THEME_DIR`.
    fn apply_theme(&mut self, raw: &str) {
        let name = raw.trim().to_ascii_lowercase();
        match name.as_str() {
            THEME_AUTO => {
                self.config.theme = None;
                self.theme = if io::stdin().is_terminal() {
                    Theme::detect_from_terminal().unwrap_or_else(|| Theme::detect(&self.config))
                } else {
                    Theme::detect(&self.config)
                };
            }
            "light" | "dark" => {
                self.config.theme = Some(name.clone());
                self.theme = if name == "light" {
                    Theme::light()
                } else {
                    Theme::dark()
                };
            }
            _ => {
                if Theme::load_custom(&name).is_none() {
                    self.toast(
                        format!(
                            "theme `{name}` not found in {}",
                            Theme::themes_dir().display()
                        ),
                        ToastKind::Error,
                    );
                    return;
                }
                self.config.theme = Some(name.clone());
                self.theme = Theme::load_custom(&name).unwrap_or_else(Theme::dark);
            }
        }
        if let Err(e) = self.config.save() {
            self.toast(format!("failed to save config: {e}"), ToastKind::Error);
        }
        self.toast(
            format!(
                "theme → {}",
                self.config.theme.as_deref().unwrap_or(THEME_AUTO)
            ),
            ToastKind::Success,
        );
    }

    /// `/sessions`: list every saved session (all agents' logs, newest first)
    /// and open the picker.
    fn open_sessions_selector(&mut self) {
        let mut records: Vec<(String, SessionRecord)> = Vec::new();
        let dir = crate::history::sessions_dir(&self.cwd);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "jsonl") {
                    let agent = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        for line in text.lines().filter(|l| !l.trim().is_empty()) {
                            if let Ok(record) = serde_json::from_str::<SessionRecord>(line) {
                                records.push((agent.clone(), record));
                            }
                        }
                    }
                }
            }
        }
        if records.is_empty() {
            self.toast("no saved sessions yet", ToastKind::Error);
            return;
        }
        records.sort_by_key(|(_agent, record)| std::cmp::Reverse(record.created_at));
        let items: Vec<String> = records
            .iter()
            .map(|(agent, record)| session_label(agent, record))
            .collect();
        self.sessions = records;
        self.overlay = Some(Overlay::Sessions(SelectList::new("sessions", items)));
    }

    /// A session was chosen in the `/sessions` picker: load its conversation
    /// into the chat and hand it to the background agent.
    fn load_session(&mut self, label: &str) {
        let Some((_agent, record)) = self
            .sessions
            .iter()
            .find(|(agent, record)| session_label(agent, record) == label)
        else {
            self.toast("session not found", ToastKind::Error);
            return;
        };
        let record = record.clone();
        self.messages.clear();
        for message in &record.messages {
            match message.role {
                Role::User => self.messages.push(Message::user(message.content.clone())),
                Role::Assistant => self
                    .messages
                    .push(Message::assistant(message.content.clone())),
                _ => {}
            }
        }
        self.scroll = 0;
        self.running = false;
        self.turns = 0;
        self.overlay = None;
        self.model = record.model.clone();
        let _ = self
            .cmd_tx
            .send(SessionCmd::LoadSession(record.messages.clone()));
        let _ = self.cmd_tx.send(SessionCmd::SetModel(record.model));
        self.toast("session loaded", ToastKind::Success);
    }

    /// `/regenerate`: drop everything after the last user message and replay
    /// it, so the model produces a fresh reply.
    fn regenerate_last(&mut self) {
        if self.running {
            self.toast(
                "agent is running — press esc twice to abort",
                ToastKind::Info,
            );
            return;
        }
        let Some(idx) = self
            .messages
            .iter()
            .rposition(|m| matches!(m.kind, MessageKind::User))
        else {
            self.toast("no previous message to regenerate", ToastKind::Error);
            return;
        };
        let text = self.messages[idx].text.clone();
        self.messages.truncate(idx + 1);
        self.running = true;
        self.turns = 0;
        let _ = self.cmd_tx.send(SessionCmd::Regenerate { text });
    }

    /// Ctrl+E: hand the current input to `$EDITOR` (default `vi`). The TUI
    /// suspends the alternate screen, runs the editor, then resumes with the
    /// edited text in the input.
    fn edit_in_editor(&mut self) {
        if self.running {
            self.toast(
                "agent is running — press esc twice to abort",
                ToastKind::Info,
            );
            return;
        }
        let Some(mut terminal) = self.terminal.take() else {
            self.toast("terminal unavailable", ToastKind::Error);
            return;
        };
        if let Err(e) = terminal.leave() {
            self.toast(format!("failed to suspend terminal: {e}"), ToastKind::Error);
            self.terminal = Some(terminal);
            return;
        }
        self.suspend.store(true, Ordering::SeqCst);
        // Give the reader thread a beat to stop polling the tty.
        std::thread::sleep(Duration::from_millis(60));

        let result = run_external_editor(self.input.text());
        let mut fatal = false;

        self.suspend.store(false, Ordering::SeqCst);
        match Terminal::enter() {
            Ok(t) => self.terminal = Some(t),
            Err(e) => {
                // Cannot restore the TUI; bail out gracefully.
                self.toast(
                    format!("failed to re-enter terminal: {e}"),
                    ToastKind::Error,
                );
                fatal = true;
            }
        }
        match result {
            Ok(Some(text)) => {
                self.input.set_text(text);
                self.mention = None;
                self.toast("input edited", ToastKind::Success);
            }
            Ok(None) => self.toast("no changes", ToastKind::Info),
            Err(e) => self.toast(e, ToastKind::Error),
        }
        if fatal {
            self.quit = true;
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
            TuiEvent::ModelList { id, models } => {
                if id != self.model_fetch_id {
                    return; // superseded fetch — a newer spinner owns the overlay
                }
                if models.is_empty() {
                    if matches!(self.overlay, Some(Overlay::Loading(_))) {
                        self.overlay = None;
                    }
                    self.toast("no models returned by any provider", ToastKind::Error);
                    return;
                }
                let items: Vec<String> = models
                    .into_iter()
                    .map(|(provider, m)| {
                        let free = if m.is_free { " · free" } else { "" };
                        format!("{provider}  {}{free}", m.id)
                    })
                    .collect();
                if matches!(self.overlay, Some(Overlay::Loading(_))) {
                    self.overlay = Some(Overlay::Model(SelectList::new("model", items)));
                }
            }
            TuiEvent::ModelListFailed { id, message } => {
                if id != self.model_fetch_id {
                    return; // stale failure must not kill a newer spinner
                }
                if matches!(self.overlay, Some(Overlay::Loading(_))) {
                    if self.pending_model_agent.is_some() && self.agent_models.is_some() {
                        let selected = self.pending_model_agent.take().unwrap_or(0);
                        self.overlay = Some(Overlay::AgentModels { selected });
                    } else {
                        self.overlay = None;
                    }
                }
                self.toast(message, ToastKind::Error);
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
        let (cols, _) = self.term_size;
        (cols as usize).saturating_sub(2).max(1)
    }

    // --- Drawing -----------------------------------------------------------

    fn draw(&mut self) -> Result<(), DoreanError> {
        let (cols, rows) = self.term_size;
        let screen = self.render_frame(cols as usize, rows as usize)?;
        let mut stdout = io::stdout();
        screen.flush(&mut stdout, &mut self.prev)?;
        Ok(())
    }

    /// Draw the full frame into a fresh [`Screen`]. Split from [`App::draw`]
    /// so tests can render frames at a chosen size without a terminal.
    fn render_frame(&mut self, width: usize, height: usize) -> Result<Screen, DoreanError> {
        let theme = self.theme;
        self.expire_toasts();
        if width < 20 || height < 6 {
            return Ok(Screen::new(width.max(1), height.max(1), theme.bg));
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

        Ok(screen)
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
        // Provider status hint: warn when no API key is configured for the
        // active provider (chat would fail; `/key` fixes it).
        let key_missing = match self.config.provider {
            Provider::OpenRouter => self.config.openrouter_api_key.is_none(),
            Provider::Nvidia => self.config.nvidia_api_key.is_none(),
            Provider::DeepSeek => self.config.deepseek_api_key.is_none(),
            Provider::Generic => false,
            Provider::Local => false,
        };
        let hint = if key_missing {
            " ⚠ no key (/key)"
        } else {
            ""
        };

        let clipped = Screen::clip(
            &line,
            width
                .saturating_sub(14)
                .saturating_sub(hint.chars().count()),
        );
        screen.put_str(0, 0, &clipped, theme.fg, theme.status_bg, Attrs::none());
        if key_missing {
            let x = clipped.chars().count();
            screen.put_str(x, 0, hint, theme.warning, theme.status_bg, Attrs::bold());
        }

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
            Overlay::Provider(mut list) => {
                Self::draw_list(screen, &theme, &mut list, width, height);
                self.overlay = Some(Overlay::Provider(list));
            }
            Overlay::Stack(mut list) => {
                Self::draw_list(screen, &theme, &mut list, width, height);
                self.overlay = Some(Overlay::Stack(list));
            }
            Overlay::PermMode(mut list) => {
                Self::draw_list(screen, &theme, &mut list, width, height);
                self.overlay = Some(Overlay::PermMode(list));
            }
            Overlay::Theme(mut list) => {
                Self::draw_list(screen, &theme, &mut list, width, height);
                self.overlay = Some(Overlay::Theme(list));
            }
            Overlay::Sessions(mut list) => {
                Self::draw_list(screen, &theme, &mut list, width, height);
                self.overlay = Some(Overlay::Sessions(list));
            }
            Overlay::AgentModels { selected } => {
                self.draw_agent_models(screen, &theme, width, height, selected);
                self.overlay = Some(Overlay::AgentModels { selected });
            }
            Overlay::ApiKey { provider, input } => {
                Self::draw_api_key(screen, &theme, width, height, &input, provider);
                self.overlay = Some(Overlay::ApiKey { provider, input });
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
            Provider::DeepSeek => " deepseek api key",
            Provider::Generic => " generic api key",
            Provider::Local => " local (no key needed)",
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
        let bw = 66usize.min(width);
        let bh = 24usize.min(height);
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
            ("ctrl+z/y", "undo / redo input edits"),
            ("ctrl+e", "edit input in $EDITOR"),
            ("↑/↓", "history or cursor · select in menus"),
            ("pgup/pgdn", "scroll the chat"),
            ("tab", "complete @agent mention"),
            ("/model", "switch model (all providers)"),
            ("/key", "pick provider + set api key"),
            ("/permission", "allow / ask / deny tool approval"),
            ("/theme", "auto · light · dark · JSON themes"),
            ("/sessions", "resume a saved session"),
            ("/copy", "copy last reply (all = whole chat)"),
            ("/regenerate", "replay the last message"),
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

/// Run `$EDITOR` (fallback `vi`) on a temp file seeded with `initial`.
/// Returns `Ok(Some(text))` when the file changed, `Ok(None)` when untouched.
/// The caller must have suspended the TUI's terminal first.
fn run_external_editor(initial: &str) -> Result<Option<String>, String> {
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());
    let path = std::env::temp_dir().join(format!("dorean-edit-{}.txt", std::process::id()));
    std::fs::write(&path, initial).map_err(|e| format!("cannot write temp file: {e}"))?;

    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi").to_string();
    let args: Vec<&str> = parts.collect();
    let status = std::process::Command::new(&program)
        .args(&args)
        .arg(&path)
        .status()
        .map_err(|e| format!("cannot start {program}: {e}"))?;

    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read temp file: {e}"))?;
    let _ = std::fs::remove_file(&path);
    if !status.success() {
        return Err(format!("editor exited with {status}"));
    }
    Ok((text != initial).then_some(text))
}

/// A one-line label for a saved session in the `/sessions` picker:
/// `2026-08-02 14:32 · <agent> · 5 msgs · "first user line…"`.
fn session_label(agent: &str, record: &SessionRecord) -> String {
    let stamp = format_timestamp(record.created_at);
    let first = record
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .map(|m| {
            let line = m.content.lines().next().unwrap_or_default().trim();
            Screen::clip(line, 36)
        })
        .unwrap_or_default();
    format!(
        "{stamp} · {agent} · {} msgs · \"{first}\"",
        record.messages.len()
    )
}

/// Civil date/time for a unix timestamp (UTC, Howard Hinnant's algorithm).
fn format_timestamp(ts: u64) -> String {
    let days = (ts / 86_400) as i64;
    let seconds = ts % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    let (h, mi, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    format!("{year:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_civil_timestamps() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00:00");
        assert_eq!(format_timestamp(1_700_000_000), "2023-11-14 22:13:20");
        assert_eq!(
            format_timestamp(1_700_000_000 + 86_400),
            "2023-11-15 22:13:20"
        );
        assert_eq!(format_timestamp(86_400 - 1), "1970-01-01 23:59:59");
    }
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use crate::agent::AbortHandle;
    use serial_test::serial;

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

    // --- Golden screen snapshots (vt100) -----------------------------------

    /// Render the app's current frame to plain text via a `vt100` parser.
    /// Colors are exercised separately by unit tests; these snapshots pin the
    /// text layout of the whole frame.
    fn frame_text(app: &mut App) -> String {
        let (cols, rows) = app.term_size;
        let width = cols as usize;
        let height = rows as usize;
        let screen = app.render_frame(width, height).unwrap();
        let mut bytes = Vec::new();
        let mut prev = Screen::default_blank(1, 1);
        screen.flush(&mut bytes, &mut prev).unwrap();
        let mut parser = vt100::Parser::new(height as u16, width as u16, 0);
        parser.process(&bytes);
        let contents = parser.screen().contents();
        contents
            .lines()
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end_matches('\n')
            .to_string()
    }

    /// Compare a rendered frame against `tests/fixtures/<name>.txt`. Run with
    /// `DOREAN_BLESS=1` to (re)generate the fixture.
    fn assert_golden(app: &mut App, name: &str) {
        app.term_size = (80, 24);
        app.animate = false;
        let text = frame_text(app);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(format!("{name}.txt"));
        if std::env::var("DOREAN_BLESS").is_ok() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("{text}\n")).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "golden fixture missing: {} (run with DOREAN_BLESS=1 to write it)",
                path.display()
            )
        });
        assert_eq!(
            text,
            expected.trim_end_matches('\n'),
            "golden snapshot `{name}` drifted — run with DOREAN_BLESS=1 to bless the new frame"
        );
    }

    #[test]
    fn golden_idle_chat() {
        let mut app = test_app();
        app.model = "meta-llama/llama-3.3-70b-instruct:free".to_string();
        app.config.openrouter_api_key = Some("sk-test".to_string());
        app.messages
            .push(Message::user("fix the **bug** in `main.rs`"));
        app.messages.push(Message::assistant(
            "Found it — the parser dropped trailing spaces:\n\n- added a `trim`\n- verified with tests\n\n```rust\nlet x = 1;\n```",
        ));
        app.messages.push(Message {
            kind: MessageKind::Tool(ToolUi {
                id: "call_1".to_string(),
                name: "edit".to_string(),
                summary: "src/main.rs".to_string(),
                status: ToolStatus::Ok,
                output: "patched src/main.rs".to_string(),
                expanded: true,
                max_lines: 8,
            }),
            text: String::new(),
            reasoning: String::new(),
            streaming: false,
        });
        assert_golden(&mut app, "idle_chat");
    }

    #[test]
    fn golden_streaming_assistant() {
        let mut app = test_app();
        app.running = true;
        app.animate = true;
        app.model = "deepseek/deepseek-r1:free".to_string();
        app.config.openrouter_api_key = Some("sk-test".to_string());
        app.usage.total_tokens = 1234;
        app.messages.push(Message::user("summarize the diff"));
        let mut streaming = Message::assistant("the diff touches ");
        streaming.streaming = true;
        app.messages.push(streaming);
        assert_golden(&mut app, "streaming_assistant");
    }

    #[test]
    fn golden_model_selector() {
        let mut app = test_app();
        app.overlay = Some(Overlay::Model(SelectList::new(
            "model",
            vec![
                "meta-llama/llama-3.3-70b-instruct:free".to_string(),
                "deepseek/deepseek-r1:free".to_string(),
                "google/gemini-2.0-flash-exp:free".to_string(),
                "nvidia/llama-3.3-nemotron-super-49b-v1".to_string(),
            ],
        )));
        assert_golden(&mut app, "model_selector");
    }

    #[test]
    fn golden_help_overlay() {
        let mut app = test_app();
        app.run_command("help");
        assert_golden(&mut app, "help_overlay");
    }

    #[test]
    fn golden_roster_and_todos() {
        let mut app = test_app();
        app.model = "m1".to_string();
        app.roster = vec![
            AgentManifest {
                name: "backend".to_string(),
                role: "API".to_string(),
                responsibilities: vec!["endpoints".to_string()],
                allowed_tools: Some(vec!["bash".to_string()]),
                owned_paths: vec![PathBuf::from("src")],
                model: None,
                provider: None,
            },
            AgentManifest {
                name: "frontend".to_string(),
                role: "UI".to_string(),
                responsibilities: vec!["pages".to_string()],
                allowed_tools: Some(vec!["bash".to_string()]),
                owned_paths: vec![PathBuf::from("ui")],
                model: None,
                provider: None,
            },
        ];
        app.agent_status
            .insert("backend".to_string(), AgentStatus::Running);
        app.agent_status
            .insert("frontend".to_string(), AgentStatus::Done);
        app.show_todos = true;
        app.todos = vec![
            crate::agent::todos::TodoItem {
                id: "1".to_string(),
                agent: "backend".to_string(),
                title: "scaffold the API".to_string(),
                status: crate::agent::todos::TodoStatus::InProgress,
            },
            crate::agent::todos::TodoItem {
                id: "2".to_string(),
                agent: "frontend".to_string(),
                title: "wire the login page".to_string(),
                status: crate::agent::todos::TodoStatus::Done,
            },
        ];
        app.running = true;
        assert_golden(&mut app, "roster_todos");
    }

    #[test]
    fn golden_no_key_hint() {
        let mut app = test_app();
        app.model = "m1".to_string();
        app.config.openrouter_api_key = None;
        assert_golden(&mut app, "no_key_hint");
    }

    // --- Resize / SIGWINCH -------------------------------------------------

    #[test]
    fn resize_sigwinch_shrinks_and_renders() {
        let mut app = test_app();
        app.messages.push(Message::assistant("a".repeat(400)));
        // SIGWINCH arrives as Event::Resize (crossterm translates it).
        app.handle_term_event(Event::Resize(60, 16));
        assert_eq!(app.term_size, (60, 16));
        let text = frame_text(&mut app);
        // The frame must be exactly the new size.
        assert_eq!(text.lines().count(), 16);
        assert!(text.lines().all(|l| l.chars().count() <= 60));
        // A very small terminal renders (no panic) and yields no frame.
        app.handle_term_event(Event::Resize(10, 3));
        assert_eq!(frame_text(&mut app), "");
    }

    #[test]
    fn resize_grows_and_reflows() {
        let mut app = test_app();
        app.messages.push(Message::assistant("hello world"));
        app.handle_term_event(Event::Resize(40, 12));
        let small = frame_text(&mut app);
        app.handle_term_event(Event::Resize(120, 40));
        let large = frame_text(&mut app);
        assert!(small.contains("hello world"));
        assert!(large.contains("hello world"));
        assert_eq!(large.lines().count(), 40);
    }

    #[test]
    fn scroll_is_clamped_after_shrink() {
        let mut app = test_app();
        for i in 0..30 {
            app.messages
                .push(Message::assistant(format!("message {i}")));
        }
        app.handle_term_event(Event::Resize(80, 12));
        app.scroll = 1_000_000;
        // Rendering clamps the scroll so no panic and a sane frame: the top of
        // the list is shown, not the scrolled-off bottom.
        let text = frame_text(&mut app);
        assert!(text.contains("message 0"));
        assert!(!text.contains("message 29"));
        // At the bottom of the list (scroll 0) the newest message is visible.
        app.scroll = 0;
        let text = frame_text(&mut app);
        assert!(text.contains("message 29"));
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
        app.overlay = Some(Overlay::Loading("fetching models…".to_string()));
        app.handle_tui_event(TuiEvent::ModelListFailed {
            id: app.model_fetch_id,
            message: "no API key".to_string(),
        });
        assert!(app.overlay.is_none());
        assert_eq!(app.toasts.len(), 1);
        assert!(
            matches!(app.toasts.front(), Some((text, ToastKind::Error, _)) if text == "no API key")
        );
    }

    #[test]
    fn stale_model_events_are_ignored() {
        let mut app = test_app();
        // A newer fetch is in flight (Loading); a late failure from the
        // previous fetch must not kill the spinner nor toast.
        app.request_models();
        let stale = app.model_fetch_id - 1;
        app.handle_tui_event(TuiEvent::ModelListFailed {
            id: stale,
            message: "old news".to_string(),
        });
        assert!(matches!(app.overlay, Some(Overlay::Loading(_))));
        assert!(app.toasts.is_empty());
        // Same for a stale success: no picker hijack.
        app.handle_tui_event(TuiEvent::ModelList {
            id: stale,
            models: vec![],
        });
        assert!(matches!(app.overlay, Some(Overlay::Loading(_))));
    }

    #[test]
    fn esc_dismisses_loading_overlay() {
        let mut app = test_app();
        app.overlay = Some(Overlay::Loading("fetching free models…".to_string()));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.overlay.is_none());
    }

    #[test]
    fn key_command_opens_provider_picker_then_api_key() {
        let mut app = test_app();
        app.run_command("key");
        assert!(matches!(app.overlay, Some(Overlay::Provider(_))));
        // Enter on the first row (openrouter) switches and opens key entry.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            app.overlay,
            Some(Overlay::ApiKey { provider, .. }) if provider == Provider::OpenRouter
        ));
        assert_eq!(app.config.provider, Provider::OpenRouter);
        // Typing and backspace edit the masked buffer.
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert!(matches!(
            app.overlay,
            Some(Overlay::ApiKey { ref input, .. }) if input == "sk"
        ));
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(matches!(
            app.overlay,
            Some(Overlay::ApiKey { ref input, .. }) if input == "s"
        ));
    }

    #[test]
    fn key_command_with_provider_arg_jumps_to_key_entry() {
        let mut app = test_app();
        app.run_command("key nvidia");
        assert_eq!(app.config.provider, Provider::Nvidia);
        assert!(matches!(
            app.overlay,
            Some(Overlay::ApiKey { provider, .. }) if provider == Provider::Nvidia
        ));
    }

    #[test]
    fn key_command_with_bogus_provider_toasts() {
        let mut app = test_app();
        app.run_command("key bogus");
        assert!(app.overlay.is_none());
        assert_eq!(app.toasts.len(), 1);
    }

    #[test]
    fn key_picker_local_skips_to_fetch() {
        let mut app = test_app();
        app.run_command("key");
        // Filter to the local row, then Enter.
        if let Some(Overlay::Provider(mut list)) = app.overlay.take() {
            list.set_filter("local".to_string());
            app.overlay = Some(Overlay::Provider(list));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.config.provider, Provider::Local);
        assert!(matches!(app.overlay, Some(Overlay::Loading(_))));
    }

    #[test]
    fn parse_model_row_handles_tagged_bare_and_badged() {
        assert_eq!(
            App::parse_model_row(
                "nvidia  nvidia/nemotron-3-ultra-550b-a55b",
                Provider::OpenRouter
            ),
            (
                Provider::Nvidia,
                "nvidia/nemotron-3-ultra-550b-a55b".to_string()
            )
        );
        // Free badge is display-only and stripped on parse.
        assert_eq!(
            App::parse_model_row(
                "openrouter  meta-llama/llama-3.3-70b-instruct:free · free",
                Provider::Nvidia
            ),
            (
                Provider::OpenRouter,
                "meta-llama/llama-3.3-70b-instruct:free".to_string()
            )
        );
        // Bare ids fall back to the active provider (back-compat).
        assert_eq!(
            App::parse_model_row("m1", Provider::DeepSeek),
            (Provider::DeepSeek, "m1".to_string())
        );
    }

    #[test]
    fn provider_rows_cover_all_providers() {
        let app = test_app();
        let rows = app.provider_rows();
        assert_eq!(rows.len(), 5);
        for id in ["openrouter", "nvidia", "deepseek", "local", "generic"] {
            assert!(rows.iter().any(|r| r.starts_with(id)), "missing {id}");
        }
        assert!(rows.iter().any(|r| r.contains("active")));
    }

    #[test]
    fn model_choice_on_other_provider_switches_first() {
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
        assert_eq!(app.config.provider, Provider::OpenRouter);
        app.apply_model_choice(Provider::Nvidia, "nvidia/nemotron-3-x".to_string());
        assert_eq!(app.config.provider, Provider::Nvidia);
        assert_eq!(app.model, "nvidia/nemotron-3-x");
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(SessionCmd::SetProvider(Provider::Nvidia))
        ));
        assert!(matches!(cmd_rx.try_recv(), Ok(SessionCmd::SetModel(_))));
    }

    #[test]
    fn tagged_model_list_opens_picker_and_chooses_with_provider() {
        let mut app = test_app();
        app.overlay = Some(Overlay::Loading("fetching models…".to_string()));
        app.handle_tui_event(TuiEvent::ModelList {
            id: app.model_fetch_id,
            models: vec![(
                Provider::DeepSeek,
                crate::providers::client::ModelInfo {
                    id: "deepseek-chat".to_string(),
                    name: String::new(),
                    description: String::new(),
                    context_length: None,
                    is_free: false,
                    prompt_price: 0.0,
                    completion_price: 0.0,
                },
            )],
        });
        assert!(matches!(app.overlay, Some(Overlay::Model(_))));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.config.provider, Provider::DeepSeek);
        assert_eq!(app.model, "deepseek-chat");
    }

    #[test]
    fn empty_model_list_dismisses_with_error() {
        let mut app = test_app();
        app.overlay = Some(Overlay::Loading("fetching models…".to_string()));
        app.handle_tui_event(TuiEvent::ModelList {
            id: app.model_fetch_id,
            models: vec![],
        });
        assert!(app.overlay.is_none());
        assert!(matches!(app.toasts.front(), Some((_, ToastKind::Error, _))));
    }

    #[test]
    fn paste_into_api_key_overlay() {
        let mut app = test_app();
        app.overlay = Some(Overlay::ApiKey {
            provider: Provider::OpenRouter,
            input: String::new(),
        });
        app.handle_term_event(Event::Paste("sk-live-key".to_string()));
        assert!(matches!(
            app.overlay,
            Some(Overlay::ApiKey { ref input, .. }) if input == "sk-live-key"
        ));
    }

    #[test]
    fn esc_closes_api_key_overlay() {
        let mut app = test_app();
        app.overlay = Some(Overlay::ApiKey {
            provider: Provider::Nvidia,
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
    fn permission_command_opens_selector_and_applies() {
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

        app.run_command("permission");
        assert!(matches!(app.overlay, Some(Overlay::PermMode(_))));
        // Choose "deny" in the selector.
        app.overlay = Some(Overlay::PermMode(SelectList::new(
            "permission mode",
            vec!["allow".to_string(), "ask".to_string(), "deny".to_string()],
        )));
        for _ in 0..2 {
            app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.config.permission_mode, Some(PermissionMode::Deny));
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(SessionCmd::SetPermission(PermissionMode::Deny))
        ));
    }

    #[test]
    fn permission_command_with_arg_applies_directly() {
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
        app.run_command("permission allow");
        assert_eq!(app.config.permission_mode, Some(PermissionMode::Allow));
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(SessionCmd::SetPermission(PermissionMode::Allow))
        ));
        app.run_command("permission bogus");
        assert_eq!(app.toasts.len(), 2); // error toast
        assert!(matches!(app.toasts.back(), Some((_, ToastKind::Error, _))));
    }

    #[serial]
    #[test]
    fn theme_command_switches_theme_and_persists() {
        unsafe {
            std::env::set_var("COLORFGBG", "15;0");
        }
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
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
        app.run_command("theme light");
        assert_eq!(app.config.theme.as_deref(), Some("light"));
        assert_eq!(app.theme.bg, Theme::light().bg);

        app.run_command("theme auto");
        assert_eq!(app.config.theme, None);
        // stdin is not a terminal in tests, so detection uses the heuristic.
        assert_eq!(app.theme.bg, Theme::dark().bg);
    }

    #[serial]
    #[test]
    fn theme_selector_lists_builtins_and_customs() {
        unsafe {
            std::env::set_var("DOREAN_THEME_DIR", theme_test_dir());
        }
        std::fs::create_dir_all(Theme::themes_dir()).unwrap();
        std::fs::write(Theme::themes_dir().join("gruvbox.json"), "{}").unwrap();
        let mut app = test_app();
        app.run_command("theme");
        let Some(Overlay::Theme(list)) = app.overlay.as_ref() else {
            panic!("expected theme selector");
        };
        assert!(list.items.iter().any(|i| i == "auto"));
        assert!(list.items.iter().any(|i| i == "light"));
        assert!(list.items.iter().any(|i| i == "dark"));
        assert!(list.items.iter().any(|i| i == "gruvbox"));
    }

    fn theme_test_dir() -> String {
        std::env::temp_dir()
            .join(format!("dorean-app-themes-{}", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn regenerate_truncates_and_resends() {
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
        app.messages.push(Message::user("fix the bug"));
        app.messages.push(Message::assistant("old answer"));
        app.messages.push(Message::system("tool ran"));

        app.run_command("regenerate");
        assert_eq!(app.messages.len(), 1); // only the user message remains
        assert!(app.running);
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(SessionCmd::Regenerate { text }) if text == "fix the bug"
        ));
    }

    #[test]
    fn regenerate_without_history_toasts() {
        let mut app = test_app();
        app.run_command("regenerate");
        assert!(!app.running);
        assert!(matches!(app.toasts.back(), Some((_, ToastKind::Error, _))));
    }

    #[test]
    fn sessions_selector_lists_and_loads() {
        let dir = std::env::temp_dir().join(format!("dorean-sess-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".dorean/sessions")).unwrap();
        let record = crate::history::SessionRecord {
            id: "s1".to_string(),
            created_at: 1_700_000_000,
            model: "m1".to_string(),
            messages: vec![
                crate::providers::client::Message::user("first ask"),
                crate::providers::client::Message::assistant(
                    "first answer".to_string(),
                    Vec::new(),
                ),
            ],
        };
        crate::history::append_session(&dir, &record).unwrap();

        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let (_events_tx, events_rx) = mpsc::unbounded_channel();
        let (_approve_tx, approve_rx) = mpsc::unbounded_channel();
        let mut app = App::new(
            Config::default(),
            dir.clone(),
            Theme::dark(),
            cmd_tx,
            events_rx,
            approve_rx,
            AbortHandle::new(),
            false,
        )
        .unwrap();

        app.run_command("sessions");
        assert!(matches!(app.overlay, Some(Overlay::Sessions(_))));
        assert_eq!(app.sessions.len(), 1);

        // Enter picks the (only) session.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.model, "m1");
        assert!(matches!(cmd_rx.try_recv(), Ok(SessionCmd::LoadSession(_))));
        assert!(matches!(cmd_rx.try_recv(), Ok(SessionCmd::SetModel(m)) if m == "m1"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sessions_without_history_toasts() {
        let mut app = test_app();
        app.run_command("sessions");
        assert!(app.overlay.is_none());
        assert!(matches!(app.toasts.back(), Some((_, ToastKind::Error, _))));
    }

    #[test]
    fn copy_command_uses_last_assistant_reply() {
        let mut app = test_app();
        app.messages.push(Message::user("hello"));
        app.messages.push(Message::assistant("hi **there**"));
        app.run_command("copy");
        assert!(
            matches!(app.toasts.back(), Some((text, ToastKind::Success, _)) if text.starts_with("copied"))
        );
        // copy all joins the conversation
        app.run_command("copy all");
        assert!(
            matches!(app.toasts.back(), Some((text, ToastKind::Success, _)) if text.starts_with("copied"))
        );
    }

    #[test]
    fn copy_without_reply_toasts_error() {
        let mut app = test_app();
        app.run_command("copy");
        assert!(matches!(app.toasts.back(), Some((_, ToastKind::Error, _))));
    }

    #[test]
    fn undo_redo_keys_edit_input() {
        let mut app = test_app();
        for c in "ab".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.input.text(), "ab");
        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(app.input.text(), "a");
        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(app.input.text(), "");
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(app.input.text(), "a");
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(app.input.text(), "ab");
    }

    #[test]
    fn resize_event_updates_term_size() {
        let mut app = test_app();
        app.handle_term_event(Event::Resize(120, 40));
        assert_eq!(app.term_size, (120, 40));
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
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(SessionCmd::FetchModels { .. })
        ));

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
