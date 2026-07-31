# Dorean — Coding Harness from Scratch (Rust · Linux · Free/Local Models)

Build a pi/opencode-style coding agent from the ground up in Rust, Linux-only, using
free hosted AI models (OpenRouter free tier). No reuse of the old pi monorepo —
everything is new.

## Stack decisions
- **Language:** Rust (edition 2021+, single crate workspace: `dorean`)
- **Platform:** Linux-only (POSIX `/dev/tty`, crossterm raw mode, `SIGWINCH`)
- **Models:** one OpenAI-compatible client
  - **Hosted free:** OpenRouter free tier (`openrouter/free`, `:free` models) + NVIDIA hosted NIM (build.nvidia.com, `NVIDIA_API_KEY`)
- **UI:** chat TUI + non-interactive CLI mode
- **Key deps (subject to change):** `crossterm`, `tokio`, `serde`, `serde_json`,
  `reqwest` (streaming SSE), `clap`, `rayon` (search parallel), `syntect` or `tree-sitter` (highlighting)

## Target architecture

```
dorean/
  Cargo.toml
  src/
    main.rs              entry, CLI (clap), mode dispatch
    cli.rs               flags: -m/--message, -c/--continue, --model, -p/--print
    config.rs            ~/.dorean/config.json + env DOREAN_*
    providers/
      mod.rs             Provider trait: stream(), list_models(), is_available()
      openrouter.rs
      client.rs          shared OpenAI-compatible /chat/completions + SSE parser
    agent/
      mod.rs             harness core: AgentLoop
      agent_loop.rs      model <-> tool loop, turn budget, abort, retry
      orchestrator.rs    technical agent: stack prompt -> roster -> SPEC + master todos -> spawn
      sub_agent.rs       parallel per-agent loop (claim -> work -> verify -> done), owned-path enforcement
      manifest.rs        agent schema: name, role, responsibilities, tools, owned paths, model
      spec.rs            SPEC.md writer (orchestrator-only writes)
      todos.rs           per-agent todo files + master todos.md, submit/review/merge flow
      router.rs          @mention routing (user->agent, agent->agent) via tokio channels
      prompts.rs         system prompt builder (cwd, os, repo context)
      tools/
        mod.rs           Tool trait: name, schema, execute()
        read.rs          Read (file, line ranges)
        write.rs         Write (create/replace, apply edits)
        edit.rs          string/regex replace, apply patches
        bash.rs          shell exec with cwd, timeout, pty?/pipes
        glob.rs          glob file listing
        grep.rs          ripgrep wrapper / regex search
        list.rs          ls / tree
      context.rs         repo scanning (git status, file tree, ignore rules)
    tui/
      mod.rs             run() entry, session cmd channel, agent task wiring
      terminal.rs        raw mode, alt screen, kitty protocol, OSC 11
      render.rs          screen buffer + draw primitives, diffed flush
      diff.rs            unified-diff annotations (add/del) for tool output
      events.rs          TuiEvent stream (mirrors agent StreamEvent)
      app.rs             event loop, message state, streaming, overlays, layout
      markdown.rs        markdown → styled lines (headings, lists, tables, fences, code)
      highlight.rs       syntax highlighting for code blocks
      components/        message list, tool bubbles, input, select-list
      theme.rs           ANSI 256 + hex, light/dark
    history.rs           session log (JSONL), resume
    permissions.rs       tool approval policy (allow/deny/ask), safe-dir checks
    error.rs             error types, panic handler
  tests/
    integration/         mock HTTP server, golden snapshots (vt100)
```

## Phases

### Phase 0 — Scaffold (foundation)
- [x] `cargo init dorean`, set edition 2021, module skeleton
- [x] CLI parsing (`clap`): `dorean`, `dorean -m "fix the bug"`, `--model`, `--continue`
- [x] `config.rs`: load `~/.dorean/config.json`, env overrides `DOREAN_*`
- [x] error types + friendly panic handler (clear raw mode on panic)
- [x] `dorean --version`, `dorean --help`
- [x] CI: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` on Linux

**Exit:** clean build, CLI skeleton, config loads, panic-safe.

### Phase 1 — Model providers
- [x] `providers/client.rs`: OpenAI-compatible `/v1/chat/completions` with `stream: true`
- [x] SSE frame parser (`data:`, `[DONE]`, error frames)
- [x] `openrouter.rs`: `:free` models list, key from config/env
- [x] `nvidia.rs`: hosted NIM adapter — `https://integrate.api.nvidia.com/v1`, key from config/`NVIDIA_API_KEY`, OpenAI-shaped `GET /models` (whole catalog free), default `nvidia/nemotron-3-ultra-550b-a55b`
- [x] `AnyProvider` dispatcher (`providers/mod.rs`): `from_config` / `for_model_listing` / provider-aware default model; `--provider nvidia`, `/key` + model selector are provider-aware
- [x] `Provider::list_models()` grouped: Free (OpenRouter) — `list_models()`/`list_free_models()` on OpenRouterProvider
- [x] streaming progress: token count, partial content callbacks
- [x] tool-call message support in client (tool calls round-trip): `delta.tool_calls` streaming parsed; `ToolCallDelta` accumulated by index into OpenAI-shaped `tool_calls`; assistant/tool messages round-trip

**Exit:** can stream a full response from OpenRouter free. (Done: `dorean -m "hi" -p` streams live, verified against the real API. Ollama removed — OpenRouter only. NVIDIA verified via mock-server integration tests in `tests/nvidia.rs` and live: `GET /v1/models` on `integrate.api.nvidia.com` returns the full free catalog — 102 models — without a key.)

### Phase 3 — Harness core (agent loop)
- [x] `agent_loop.rs`: the loop — user msg → model → tool_calls → execute → observe → repeat
- [x] turn budget (`max_turns`, default 20), abort (`AbortHandle`; Ctrl+C wired in `-m` mode), retry on transient provider errors (429/5xx/network, backoff)
- [x] `prompts.rs`: system prompt (Linux, cwd, OS, timestamp, coding rules, tool docs with JSON schemas)
- [x] tool schema generation (JSON Schema per tool → OpenAI `tools` param)
- [x] non-interactive mode: `dorean -m "task"` runs the loop, prints the final response, returns exit code
- [x] tool results fed back as structured `tool` messages
- [x] continue/resume: `--continue` loads the last session from `.dorean/sessions/main.jsonl` (JSONL)
- [x] core tools implemented early (needed for a real end-to-end task): `read`, `write`, `edit`, `bash`, `glob`, `grep`, `list` — see Phase 4 for the remaining hardening

**Exit:** harness completes a real task end-to-end non-interactively. (Verified via mock-provider integration tests: tool-call round-trip, resume, abort. Live run pending an API key.)

### Phase 4 — Tools
- [x] `read.rs`: file read with line ranges, binary safety
- [x] `write.rs`: write files (create/overwrite, parent dirs)
- [x] `edit.rs`: exact-string replace (nth occurrence), atomic write via temp+rename
- [x] `bash.rs`: shell exec via `sh -c`, cwd, timeout, combined output capture
- [x] `glob.rs` + `grep.rs`: file discovery + regex content search (rayon parallel)
- [x] `list.rs`: directory tree
- [x] permissions: allow/deny/ask policy, safe-dir checks, deny-list
- [x] git-aware context: status, branch, recent diff
- [x] write review step, regex edit, per-file output caps tuned

**Exit:** all tools work + permission policy enforced.

### Phase 5 — Sub-agent orchestration
- [x] `manifest.rs`: agent schema — name, role, responsibilities, allowed tools, **owned paths**, model override → `agents.json`
- [x] `spec.rs`: technical agent writes `.dorean/SPEC.md` (goal, stack, architecture, per-agent breakdown, acceptance criteria); **read-only for sub-agents**
- [x] **stack selection:** always ask the user first (stdin prompt; `--stack`/`--auto-stack` in `-m` mode)
- [x] `orchestrator.rs`: stack → roster (full-stack ⇒ `backend` / `db` / `frontend-ui-ux` / `frontend-logic`) → SPEC + master todos → **spawn all sub-agents in parallel** (one tokio task each)
- [x] `sub_agent.rs`: concurrent loop — claim todo → work (model+tools, confined to owned paths) → verify → mark done (via `todo` tool); rounds capped by `sub_agent_rounds` with a stall guard
- [x] `todos.rs`: per-agent todo files (`.dorean/todos/<agent>.md`, writeable by that agent only); sub-agents submit updates → orchestrator **reviews and merges** into master `.dorean/todos.md`
- [x] conflict handling: owned-path enforcement in tools; cross-path access only via an orchestrator grant (mention/request)
- [x] `router.rs`: `@agentname` mentions in both directions (user→agent, agent→agent); agent names feed TUI autocomplete
- [x] per-agent sessions `.dorean/sessions/<agent>.jsonl`; `--continue` restores the roster and todo state
- [x] non-interactive: `dorean -m "build a full stack app" --orchestrate` asks for stack, runs the full parallel pipeline and prints a summary
- [x] events for the TUI (Phase 6): `agent-started`/`agent-finished`, `agent-message`, `todo-updated`, `plan-created`, `stack-prompt`

**Exit:** `-m "build a full stack app"` scaffolds a working app via multiple specialized agents running in parallel; `@backend <msg>` and agent→agent mentions route correctly; todo reviews are enforced. (Verified via mock-provider integration test: 4 parallel agents, todo merge, roster/SPEC/sessions written.)

### Phase 6 — Chat TUI
- [x] terminal core: raw mode, alt screen, resize, kitty protocol (`REPORT_EVENT_TYPES`), OSC 11 background detection
- [x] renderer: `Screen` buffer + draw primitives, incremental diffed redraw
- [x] message list: virtualized (skip-based `draw_region`), streaming cursor, reasoning + tool toasts
- [x] code blocks: syntax highlighting (rust/python/sql/…), scroll in the virtualized list (copy deferred to Phase 7)
- [x] diff renderer (pi parity) for edit/tool output (`diff.rs`: unified-diff annotations, add/del colors)
- [x] markdown renderer (headings, lists, bold/italic/strike, links, tables, fenced code)
- [x] text input: multiline, hard wrap, history (200-entry, dedup), word nav, bracketed paste, Ctrl+W/U/M
- [x] header: model, provider, turn, token usage; footer: keybinding hints
- [x] abort (Esc), `/clear`, `/quit`; permission overlay for tool approval (allow/ask/deny, deny-list)
- [x] agent roster/status strip + per-agent streaming bubbles (consume `agent-started`/`agent-finished`/`agent-message`)
- [x] todo panel: master `.dorean/todos.md` rendered live, grouped by agent
- [x] SPEC/plan view (`/spec`) + `@agentname` mention input with agent-name autocomplete

**Exit:** full chat UI showing a streaming agent run. (Done: `cargo run` opens the TUI; streaming agent runs, roster/todos/SPEC, mention routing, permission overlay. Kitty-protocol release events are filtered so keystrokes don't double — regression-tested. 153 unit + 10 integration tests green, `cargo clippy --all-targets -D warnings` clean.)

### Phase 7 — Commands, selectors, polish
- [x] command palette so far: `/help`, `/model`, `/key`, `/stack`, `/make`, `/thinking`, `/todo`, `/spec`, `/clear`, `/abort`, `/quit`
- [x] API key entry: `/key` masked-input overlay, persisted to `~/.dorean/config.json`, rebuilds the agent and jumps straight into the model selector
- [x] `/model` actually works against the live API: `deserialize_price` tolerates the `"request": null`/cache-null pricing shape OpenRouter emits on every model (previously the first model failed to parse and the list was always empty); fetch has connect + per-request timeouts so the loading overlay can never hang; Esc dismisses it; `GET /models` is public so the selector works even before a key is set
- [ ] command palette remaining: `/copy`, `/permission`, `/theme`, `/sessions`, `/regenerate`
- [x] model selector: grouped Free (OpenRouter), type-to-filter
- [x] selector for tool-approval in interactive mode (permission overlay)
- [ ] theme: light/dark auto-detect (OSC 11 bg is queried) + JSON themes
- [x] toasts/notifications (errors, permission prompts)
- [x] status bar: provider, model, token usage, turn counter (header)
- [x] session history persistence + resume (`--continue`)
- [ ] provider status hint (OpenRouter reachability / API key missing)
- [x] `@agentname` direct-message UX polish: per-agent brains — `/make` opens a per-sub-agent model picker before spawning (each agent runs on its own chosen model; config `agent_models` / `DOREAN_AGENT_MODELS` for the non-interactive path); `/thinking` toggles the reasoning block
- [ ] regenerate last reply, clipboard copy, external `$EDITOR`, undo stack

**Exit:** polished, keyboard-driven agent UI.

### Phase 8 — Tests & hardening
- [x] unit: SSE parser, markdown→cells, diff→annotations, config, tools, input, terminal (OSC 11), app key handling (release-event filter)
- [x] unit: manifest/todos parsing, owned-path enforcement, router (`@mention` parsing)
- [x] integration: mock HTTP server for providers (`tests/openrouter.rs`), agent-loop fixtures (`tests/agent_loop.rs`)
- [x] integration: orchestration fixture — scripted prompt → roster → parallel sub-agents → todos merged (`tests/orchestration.rs`)
- [ ] golden screen snapshots via `vt100`
- [ ] resize + SIGWINCH handling tests
- [ ] fuzz-ish: malformed SSE, huge output, abort mid-stream
- [x] install path superseded by the cargo-dist release pipeline (Phase 9) — local binary verified via packaged archive

**Exit:** CI green, stable on real tasks.

### Phase 9 — Distribution
- [x] `cargo-dist` pipeline: `dist-workspace.toml` (targets `x86_64` + `aarch64`-unknown-linux-gnu, `install-path = CARGO_HOME`), generated `.github/workflows/release.yml` (tag-push trigger, `pr-run-mode = "plan"`)
- [x] shell installer (`dorean-installer.sh`) → `~/.cargo/bin` + PATH via `.profile`; self-updater binary (`dorean-update`, `install-updater = true`)
- [x] `repository` field in `Cargo.toml`, `[profile.dist]` (inherits release, thin LTO)
- [x] `CHANGELOG.md` (Keep-a-Changelog: `[Unreleased]` + `[0.1.0]` sections → auto GitHub Release notes); README "Install" section (curl one-liner + manual archive)
- [x] local verification: `cargo dist build` produced working `dorean` archive + installer (binary runs, `dorean 0.1.0`); 190 tests / clippy `-D warnings` / fmt green
- [x] cut `v0.1.0`: tag pushed → CI built both Linux arches, GitHub Release published
- [x] cut `v0.1.1`: NVIDIA provider release — bump `Cargo.toml` to 0.1.1, `## [0.1.1]` changelog section, `git tag v0.1.1` at the release commit (the tagged commit's `Cargo.toml` must match the tag version or `dist host --steps=create` refuses)

**Exit:** `curl ... dorean-installer.sh | sh` installs dorean on any Linux x86_64/ARM64.

## Milestones
- **M0** scaffold+config (Phase 0) ✅
- **M1** provider streaming, OpenRouter (Phase 1) ✅
- **M2** agent loop + core tools, non-interactive works (Phases 3–4) ✅
- **M3** sub-agent orchestration: parallel multi-agent build + @mention routing (Phase 5) ✅
- **M4** chat TUI live (Phase 6) ✅
- **M5** selectors/polish + hardening (Phases 7–8)
- **M6** distribution: cargo-dist release pipeline (Phase 9) ✅ (v0.1.0 and v0.1.1 released)

## Not in scope (phase 0 decisions)
- Windows / macOS terminal handling
- Native C addons
- Reusing pi/opencode code — harness is authored from scratch
