# Dorean

A pi/opencode-style coding agent for the terminal, written from scratch in Rust
for Linux. Chat TUI + non-interactive CLI, driven by OpenRouter's free tier.

Status: Phase 3 (harness core — agent loop + tools). See `phase.md` for the roadmap.

## Install

Linux (x86_64 or ARM64):

```
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Shishir-Kc/Dorean/releases/latest/download/dorean-installer.sh | sh
```

Or grab the `dorean-<arch>-unknown-linux-gnu.tar.xz` archive from the latest
[GitHub Release](https://github.com/Shishir-Kc/Dorean/releases/latest) and add
the binary to your `PATH`.

## Build & test

```
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

## Usage

```
dorean                # opens the chat TUI (Phase 6)
dorean -m "msg"       # one-shot prompt: agent loop with tools, prints the final answer
dorean -m "msg" -p    # stream assistant text live, no decorations
dorean -c -m "msg"    # continue the most recent session from .dorean/sessions/
dorean --provider openrouter
dorean --provider nvidia --model nvidia/nemotron-3-super-120b-a12b
dorean --provider deepseek --model deepseek-chat
dorean --provider local --model qwen2.5-coder:7b
dorean --provider generic  # any OpenAI-compatible endpoint via DOREAN_BASE_URL
dorean --model meta-llama/llama-3.3-70b-instruct:free
dorean --version
dorean --help
```

Ctrl+C aborts a `-m` run. Sessions are persisted per-repo under `.dorean/sessions/main.jsonl`.

Read-only tools (`read`, `glob`, `grep`, `list`) run concurrently; long
commands use `bash_background` + `bash_poll`/`bash_kill` so the loop stays
responsive. Old tool output is pruned and long conversations auto-compact
(threshold `DOREAN_COMPACT_THRESHOLD`, off with `DOREAN_AUTO_COMPACT=off`).

## Configuration

Loaded from `~/.dorean/config.json`; environment overrides are `DOREAN_PROVIDER`,
`DOREAN_MODEL`, `DOREAN_BASE_URL`, `DOREAN_OPENROUTER_API_KEY`,
`DOREAN_NVIDIA_API_KEY`, `DOREAN_DEEPSEEK_API_KEY`, `DOREAN_GENERIC_API_KEY`,
`DOREAN_EXECUTOR_MODEL`, `DOREAN_PLANNER_MODEL`, `DOREAN_COMPACT_THRESHOLD`,
`DOREAN_AUTO_COMPACT`, `DOREAN_TELEMETRY`, `DOREAN_THEME`, `DOREAN_MAX_TOKENS`,
`DOREAN_MAX_TURNS`.

### OpenRouter

- Set an API key via `DOREAN_OPENROUTER_API_KEY`, `OPENROUTER_API_KEY`, or
  `"openrouter_api_key"` in `~/.dorean/config.json`.
- Default model is `openrouter/free` (routes to free models); pick any `:free`
  model with `--model`.
- `DOREAN_REFERER` / `DOREAN_TITLE` set the optional `HTTP-Referer` /
  `X-OpenRouter-Title` headers.

### NVIDIA (hosted NIM, build.nvidia.com)

- Get a free API key at <https://build.nvidia.com/settings/api-keys>, then set
  it via `DOREAN_NVIDIA_API_KEY`, `NVIDIA_API_KEY`, or `"nvidia_api_key"` in
  `~/.dorean/config.json`.
- Select the provider with `--provider nvidia` (or `"provider": "nvidia"` in
  config).
- Default model is `nvidia/nemotron-3-ultra-550b-a55b`; a solid
  alternative is `nvidia/nemotron-3-super-120b-a12b`. Pick any catalog model
  with `--model`.
- The whole NVIDIA catalog is free (rate-limited); the model selector lists
  every model.

### DeepSeek (native API)

- Set a key via `DOREAN_DEEPSEEK_API_KEY`, `DEEPSEEK_API_KEY`, or
  `"deepseek_api_key"` in `~/.dorean/config.json`.
- Select with `--provider deepseek` (default `deepseek-chat`; also
  `deepseek-reasoner`). System prompts are prefix-stable with hour-rounded
  timestamps so DeepSeek disk context-caching hits across turns.

### Local (Ollama) and generic endpoints

- `--provider local` talks to `http://localhost:11434/v1` (no key needed;
  override with `DOREAN_BASE_URL`). Default `qwen2.5-coder:7b`.
- `--provider generic` talks to any OpenAI-compatible server at
  `DOREAN_BASE_URL` with optional `DOREAN_GENERIC_API_KEY`.

### Memory, hooks, MCP, sandbox

- Project memory: `AGENTS.md` / `CLAUDE.md` (root → cwd chain) plus
  `.dorean/skills/<name>/SKILL.md` are injected into the system prompt.
- Hooks: `.dorean/hooks.json` (`PreToolUse` / `PostToolUse` / `Stop` /
  `SessionStart`); exit 2 denies a tool call, stdout becomes context.
- MCP: `.dorean/mcp.json` server registry (`mcp__<server>__<tool>` naming).
- Approvals: `/permission` switches `suggest` / `auto-edit` / `full-auto`
  sandbox presets over the `allow` / `ask` / `deny` policy.
- Keys: `/key` opens a provider picker (key status per provider), then the
  masked key entry for the provider you pick — picking also switches to it
  immediately (`/key nvidia` jumps straight there). `/model` lists every
  provider's catalog at once (free models first); unreachable providers are
  skipped with a hint instead of failing the list.
- Troubleshooting `/model`: if every provider fails, the error names each
  reason — no network and a dead custom `DOREAN_BASE_URL` are the usual
  culprits (it reroutes hosted listings too, so unset it when using
  OpenRouter/NVIDIA/DeepSeek directly). `local` needs `ollama serve`;
  `generic` needs `DOREAN_BASE_URL`. For ground truth without the TUI, run
  `dorean --list-models`: it prints per-provider counts and errors.

## Updating

`dorean-update` (installed next to `dorean` by the installer) checks the
GitHub Releases API and can update itself and the binary:

```
dorean-update
```

The update check calls the GitHub API unauthenticated, which is rate-limited to
60 requests/hour per IP (a 403 can look like "no update available"). If you hit
that, set a fine-grained personal access token (no scopes needed) so the check
runs under its own budget:

```
export AXOUPDATER_GITHUB_TOKEN=github_pat_...
```

or re-run the installer to fetch the latest release directly:

```
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Shishir-Kc/Dorean/releases/latest/download/dorean-installer.sh | sh
```

