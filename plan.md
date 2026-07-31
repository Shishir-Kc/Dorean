# Plan: Build a pi-style Chat TUI (Rust)

Status: **Live** — stack decision made and executed. **Rust**, single crate `dorean`, Linux-first,
hand-rolled diffed renderer on `crossterm` (no ratatui), **OpenRouter free + NVIDIA hosted NIM** models,
and a from-scratch agent harness (no pi/opencode code). Phases 0–6 are complete (the chat TUI ships);
Phases 7–8 are in progress; Phase 9 (distribution via cargo-dist) is set up and verified locally. The
checkboxes below track the original target inventory against what actually ships today, and `phase.md`
is the authoritative task list.
Deliverable: a `plan.md` + `phase.md` that a coding agent executed to reproduce the TUI experience.

---

## 0. Goals and Non-Goals

### Goals
- A terminal chat interface for an AI coding agent with:
  - Streaming assistant messages rendered as Markdown
  - Multi-line input editor with autocomplete and slash-commands
  - Live tool-execution status (bash, file edits, reads)
  - Model / session / theme selectors (overlays)
  - Working indicator, footer with keybinding hints, status line
  - Session resume, /slash commands, copy/share helpers
  - First-time setup and auth/login dialogs
  - Keyboard-first navigation (Esc cycles: editor → input → messages)

### Non-Goals (phase 1)
- The AI/LLM backend itself (assume a provider/agent core exists or is stubbed with a script that emits events)
- Plugin/extension SDK (can be added later)
- Native C addons for macOS/Windows (start with a portable key parser; add native modifiers later)

---

## 1. Reference Architecture (what pi actually did)

Two layers:

```
packages/tui          Low-level TUI library (terminal I/O, diff rendering, widgets)
packages/coding-agent/src/modes/interactive/
                      The chat UI: interactive-mode.ts + ~40 components
```

### Layer 1 — Terminal Library (assume or reuse an existing one)
- Raw terminal mode: enter/exit alternate screen, hide cursor, bracketed paste, mouse off
- **Differential rendering**: render components to lines, diff against previous frame, only emit changed cells (pi uses a custom renderer; in Go use `gocui`/`tview`, in Rust use `ratatui`)
- **Input**: ESC-sequence parser (arrows, modifiers, keys), Kitty keyboard protocol flags, text paste, IME support via cursor marker, ANSI OSC 11 background-color detection (dark/light)
- **Widgets needed**: Editor (multi-line, undo/redo, word nav, autocomplete popup), Input, Markdown renderer (inline code, fenced blocks, tables), SelectList (overlays), Loader/spinner, Box/Text/Spacer, TruncatedText, fuzzy matcher

### Layer 2 — Chat UI (the actual pi experience)
Core loop in `interactive-mode.ts`:
1. Subscribe to agent session events (message-updated, tool-started, session-started, compaction, usage/cost)
2. Render: header → chat history → pending/working status → editor + footer
3. Key handling with a focus stack (editor, then Esc → input navigation)
4. Overlays (selectors) swap the "above" area; editor persists below

---

## 2. Feature Inventory (from pi's components — your target checklist)

### Core rendering
- [x] Assistant message with streaming Markdown (code blocks, inline code, lists, links, tables)
- [x] User message bubble
- [x] Tool execution components: one per tool, collapsible output, status indicator, diff view
- [x] Tool/bash output (tool + sub-agent text streams live into the message list)
- [x] Pending/working container ("Working…" spinner, loading overlay)
- [ ] Compaction summary message (no compaction implemented yet)
- [ ] Branch summary message
- [x] **Sub-agent roster/status strip** (name, running/done/failed glyphs)
- [x] **Per-agent streaming bubbles** (parallel orchestration streams into the chat)
- [x] **Todo panel** rendering the master `.dorean/todos.md` live, grouped by agent
- [x] **SPEC/plan view** (read-only render of `.dorean/SPEC.md`, scrollable)

### Input & editing
- [x] Multi-line input (hard wrap, 200-entry deduped history, word nav, bracketed paste)
- [ ] Multi-line editor with syntax awareness / undo stack / kill-ring (deferred)
- [x] Slash-commands shipped: `/help /model /key /stack /make /thinking /todo /spec /clear /abort /quit`
- [ ] Slash-commands remaining: `/copy /permission /theme /sessions /regenerate`
- [x] Autocomplete for **`@agentname`** mentions (Tab accept, Up/Down cycle); model selector has type-to-filter
- [ ] Autocomplete for slash commands / files (deferred)
- [x] **`@agentname` mention routing** — user→agent and agent→agent (`router.rs`)
- [ ] External editor (`$EDITOR`)
- [x] Clipboard paste (bracketed paste)

### Overlays / selectors (Esc or /command opens these)
- [x] Model selector (grouped Free/OpenRouter, type-to-filter; also lists the whole free NVIDIA catalog)
- [x] Trust/tool-approval selector (allow/ask/deny, deny-list)
- [x] Stack selector (orchestration stack prompt)
- [ ] Session selector (resume via `--continue` only)
- [ ] Theme selector (OSC 11 bg detected; no theme switching yet)
- [ ] Settings selector
- [x] Login/auth selector (`/key` masked-input overlay, stored to `~/.dorean/config.json`; also env `DOREAN_OPENROUTER_API_KEY` / `OPENROUTER_API_KEY`)
- [ ] Show-images selector

### Status & chrome
- [x] Footer with keybinding hints
- [x] Status line / header: model, provider, turn, token usage
- [x] Hidden thinking label (reasoning rendered as a dim italic line)
- [x] Header (provider, model, cwd-driven context)
- [ ] Update check + first-time setup screen (telemetry consent)
- [ ] Terminal title updates

### Keybindings (implemented subset of pi's defaults)
- `Enter` send / `Ctrl+J` newline / `Ctrl+M` newline, `Ctrl+C` abort (or quit when idle), `Esc` abort/quit
- `Ctrl+W` delete word, `Ctrl+U` clear line, `PageUp/PageDown` scroll history
- `Tab` autocomplete (mentions), `Up/Down` history in input, `Ctrl+V` paste (bracketed)
- Remaining: `Alt+Left/Right` word nav, `Ctrl+L` clear, `Ctrl+R` reload, double `Ctrl+C` exit

### Backend contract (agent events the UI consumes)
- [x] `message-updated` → streamed deltas (`StreamEvent::Delta`)
- [x] `tool-started` / `tool-finished` (`ToolCall`/`ToolResult`)
- [x] `agent-started` / `agent-finished` / `agent-message` (roster + per-agent bubbles)
- [x] `usage-updated` (token usage in header)
- [x] `stack-prompt` (stack selector)
- [x] `plan-created` (SPEC view)
- [x] `todo-updated` (todo panel)
- [x] `mention-routed` (`@agentname` delivery)
- [ ] `session-started` / `session-ended`, `compaction-needed`, `auth-prompt`, `package-update-available`

---

## 3. Recommended Phases

### Phase 0 — Decide stack & scaffold ✅
- Chosen: **Rust** (not ratatui — a hand-rolled diffed `Screen` renderer on `crossterm` for parity and control). Go was not pursued.
- CLI (`clap`), config (`~/.dorean/config.json` + `DOREAN_*` env), error types + panic-safe terminal restore, CI (fmt/clippy/test).
- **Exit met**: clean build, CLI skeleton, config loads, panic-safe.

### Phase 1 — Terminal core ✅
- Raw mode, alternate screen, resize, bracketed paste, OSC 11 background query (kitty `REPORT_EVENT_TYPES` for modifiers).
- `Screen` buffer + draw primitives + diffed flush (only changed cells emitted).
- **Exit met**: flicker-free diffed rendering; unit tests for the screen/diff engine.

### Phase 2 — Widgets ✅
- Input (multiline, history, word nav), Markdown renderer (headings, bold/italic/strike, inline + fenced code, lists, links, tables), SelectList overlay, spinner/loader, message/tool components.
- **Exit met**: widgets tested; select-list overlays open/close via Esc.

### Phase 3 — Chat layout & event loop ✅
- Layout: header (model/provider/turn/tokens) / roster / chat (virtualized) / input / footer.
- Streaming: assistant deltas, reasoning label, tool call/result bubbles, per-agent text, todo updates, usage.
- **Exit met**: live conversation renders; streaming deltas update in place.

### Phase 4 — Slash commands & selectors ◐ (partial)
- Shipped: `/help /model /stack /todo /spec /clear /abort /orchestrate /quit`; model + stack + tool-approval selectors.
- Remaining: `/copy /permission /theme /sessions /regenerate`, session/theme/settings/auth selectors (→ Phase 7).

### Phase 5 — Polish & integration ◐ (partial)
- Done: real agent harness wired (no stub — `agent_loop.rs` + tools + orchestration drive the UI); cost/token display; Ctrl+C abort.
- Remaining: regenerate, copy/share/export, undo, update check, terminal title, double-Ctrl+C exit (→ Phase 7).

---

## 4. File/Module Layout (stack-agnostic)

```
src/
  terminal/        raw mode, input parser, kitty protocol, virtual-terminal tests
  render/          diff engine, buffer/line model
  widgets/         editor, input, markdown, select-list, loader, box, text, spacer, truncated-text
  chat/            layout, focus stack, message/tool streaming components
  orchestration/   roster/status strip, per-agent bubbles, todo panel, SPEC view, mention routing UI
  commands/        slash-command registry + handlers
  selectors/       model, session, theme, settings, auth, trust overlays
  backend/         agent event client (JSON stream), session store
  theme/           theme model (dark/light JSON), color resolver
  main.go (or main.rs)
tests/
  fixtures/        captured ESC streams, golden rendered frames
```

Project artifacts the orchestration layer reads/writes (in the working repo):

```
.dorean/
  SPEC.md                  orchestrator-only plan (goal, stack, architecture, per-agent work)
  todos.md                 master todo list merged after orchestrator review
  todos/<agent>.md         per-agent todo files (writeable by that agent)
  agents.json              generated roster (name, role, responsibilities, owned paths, model)
  sessions/<agent>.jsonl   per-agent message history for direct chat + resume
```

---

## 5. Testing Strategy
- **Virtual terminal**: feed bytes in, capture rendered frame (like pi's `virtual-terminal.ts`). Use for diff-engine and key-handler tests.
- **Golden frames**: snapshot expected renderings for selectors, messages, tool states.
- **Scripted sessions**: replay a recorded event stream through the UI and assert frame output at checkpoints.
- **Orchestration fixtures**: scripted prompt → roster → parallel sub-agent event streams → merged todos, asserted as golden frames.
- **Manual smoke script**: a shell script that drives the real TUI through the happy path.

---

## 6. Decisions Made (previously open questions)
1. **Go or Rust?** → **Rust**, single crate `dorean`. (Renderer is hand-rolled, so no ratatui dependency.)
2. **Reuse a widget library or hand-roll?** → **Hand-rolled** `Screen` buffer + incremental diff flush, like pi. Full control over streaming/virtualized rendering.
3. **Which AI backend?** → **Own Rust harness** (agent loop + tools + orchestration) driving **OpenRouter free** and **NVIDIA hosted NIM** (build.nvidia.com, `NVIDIA_API_KEY`) models via one OpenAI-compatible client.
4. **macOS/Windows?** → **Linux-first**; no native modifier support for other platforms in v1.
5. **Modal overlays or full-screen dialogs?** → **Modal overlays** that swap the area above the input, pi-style.

---

## 7. Next Actions (current state — see `phase.md`)
1. **Done:** stack decision, scaffold, terminal core, widgets, chat layout/event loop, real harness integration (Phases 0–3) — the TUI ships and is fully tested.
2. **Done:** distribution — cargo-dist pipeline (Linux x86_64 + ARM64, shell installer + self-updater), `CHANGELOG.md`, README install docs; verified locally (Phase 9).
3. **Doing:** Phase 7 polish — remaining slash commands (`/copy /permission /theme /sessions /regenerate`), session/theme/settings selectors, regenerate, clipboard copy, provider status hint.
4. **Then:** Phase 8 hardening — golden `vt100` screen snapshots, resize/SIGWINCH tests, fuzz-ish malformed-SSE/huge-output/abort tests.
5. **Then:** cut `v0.1.0` GitHub Release. **Done:** v0.1.0 released (tag → cargo-dist pipeline built both arches). Next cut is `v0.1.1` (NVIDIA provider): bump `Cargo.toml` to 0.1.1, add `## [0.1.1]` Keep-a-Changelog section, then tag `v0.1.1` at the release commit.
