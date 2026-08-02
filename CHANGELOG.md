# Changelog

## [Unreleased]

### Added

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
