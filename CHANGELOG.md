# Changelog

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
