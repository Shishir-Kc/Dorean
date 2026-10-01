# Changelog

## [0.2.0] - Unreleased

### Fixed

- Hung streams no longer wedge the app: SSE reads have a 5-minute idle
  watchdog that fails the turn as a retryable error (transient stalls
  recover via backoff; persistent ones end loudly instead of spinning
  forever with chat dead and aborts unreachable)
- `/model` fetch runs detached from the command queue with a 45s watchdog,
  so it can never queue behind a long/wedged chat run; arrivals carry a
  fetch id and superseded responses are ignored instead of populating the
  wrong picker or killing a newer spinner
- Abort now lands within ~1s even on a fully stalled stream (short polls
  with accumulated silence instead of one unbounded await)
- New `dorean --list-models` diagnostic: prints per-provider model counts
  and errors without the TUI (ground truth for `/model` issues)

## [Unreleased]

### Added

- Providers: native DeepSeek (`--provider deepseek`, disk-cache-friendly
  prefix-stable prompts), local Ollama (`--provider local`), and generic
  OpenAI-compatible endpoints (`--provider generic` via `DOREAN_BASE_URL`);
  dual-model `executor_model` / `planner_model` (sub-agents default to the
  cheap executor unless explicitly picked)
- Speed: consecutive read-only tools (`read`/`glob`/`grep`/`list`/`bash_poll`)
  run concurrently; pooled HTTP keepalive; prompt prefix is byte-stable across
  turns (hour-rounded timestamps, sorted tool docs) for prefix/cache hits
- Efficiency: token meter with cache-hit tracking, stale tool-output pruning,
  and auto-compaction (`DOREAN_COMPACT_THRESHOLD`, `DOREAN_AUTO_COMPACT=off`
  to disable); project memory from `AGENTS.md`/`CLAUDE.md` plus
  `.dorean/skills/*/SKILL.md`
- Responsiveness: `bash_background` + `bash_poll`/`bash_kill` background tasks,
  per-turn file checkpoints (`checkpoints.rs`), Codex-style `suggest` /
  `auto-edit` / `full-auto` sandbox presets
- Extensibility: lifecycle hooks (`.dorean/hooks.json`, exit-2 denies),
  MCP server registry (`.dorean/mcp.json`, `mcp__<server>__<tool>`), git
  worktree helper for sub-agent isolation
- Perf regression gate: `tests/perf.rs` (prefix stability, parallel batch,
  compaction shrink)
- `/key` opens a provider picker (all 5 providers, key status shown) and the
  key entry is scoped to the picked provider; picking switches immediately
  (`/key <provider>` jumps straight there; local skips key entry)
- `/model` aggregates every provider's full catalog in parallel (free first),
  choosing a row switches provider + model together; unreachable providers
  are skipped with actionable hints (e.g. `ollama serve`) instead of an
  empty/failed list; DeepSeek lists its full catalog (was empty under
  free-only filtering); provider/key switches preserve chat context
- Model-fetch hardening: aggregation extracted to testable
  `providers::fetch_all_models` (wiremock-covered incl. total-outage
  messaging), live-catalog fixture locks the OpenRouter parser against the
  real 2026 response shape, and fetch errors name a custom `DOREAN_BASE_URL`
  explicitly since it reroutes hosted listings too

- `/copy [all]` copies the last assistant reply (or the whole session) via OSC 52
  with wl-copy/xclip/xsel fallbacks
- `/permission [mode]` + selector to switch allow/ask/deny on the fly (persisted)
- `/theme [auto|light|dark|<json>]` + selector; JSON themes loaded from
  `~/.dorean/themes/` (override with `DOREAN_THEME_DIR`)
- `/sessions` selector to load a saved session back into the chat
- `/regenerate` truncates the conversation to the last user message and re-runs
- Ctrl+Z / Ctrl+Y input undo/redo; Ctrl+E opens `$EDITOR` (TUI suspends)
- Header hint `⚠ no key (/key)` when the provider key is missing
- Golden `vt100` screen snapshots (`tests/fixtures/`, `DOREAN_BLESS=1` to
  regenerate), resize/SIGWINCH tests, malformed-SSE / huge-output / abort
  fuzz-ish tests

### Fixed

- Markdown wrapping dropped spaces across styled-span boundaries
  (`**bug** in` rendered as `bugin`)
- Scroll clamping test semantics (max scroll shows the top of the list, not the
  bottom)

## [0.1.1] - 2026-07-31

### Added

- NVIDIA hosted NIM provider: `--provider nvidia` talks to build.nvidia.com
  (OpenAI-compatible), configured via `DOREAN_NVIDIA_API_KEY` /
  `NVIDIA_API_KEY` / `"nvidia_api_key"` in config; default model
  `nvidia/nemotron-3-ultra-550b-a55b`
- `AnyProvider` dispatcher so the agent loop, `/key` overlay, and `/model`
  selector are provider-aware (the NVIDIA catalog is listed whole and free)
- README "Updating" section covering `dorean-update` and
  `AXOUPDATER_GITHUB_TOKEN` for the unauthenticated GitHub rate limit

## [0.1.0] - 2026-07-31

Initial release of Dorean, a pi/opencode-style coding agent for the terminal.

- Chat TUI + non-interactive CLI driven by OpenRouter
- Agent loop with builtin tools (read, write, edit, bash, glob, grep, list)
- Sub-agent orchestration with `/make`, per-agent model selection
- `/thinking` toggle for separated reasoning blocks
