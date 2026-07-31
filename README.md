# Dorean

A pi/opencode-style coding agent for the terminal, written from scratch in Rust
for Linux. Chat TUI + non-interactive CLI, driven by OpenRouter's free tier.

Status: Phase 3 (harness core — agent loop + tools). See `phase.md` for the roadmap.

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
dorean --model meta-llama/llama-3.3-70b-instruct:free
dorean --version
dorean --help
```

Ctrl+C aborts a `-m` run. Sessions are persisted per-repo under `.dorean/sessions/main.jsonl`.

## Configuration

Loaded from `~/.dorean/config.json`; environment overrides are `DOREAN_PROVIDER`,
`DOREAN_MODEL`, `DOREAN_BASE_URL`, `DOREAN_OPENROUTER_API_KEY`,
`DOREAN_TELEMETRY`, `DOREAN_THEME`, `DOREAN_MAX_TOKENS`, `DOREAN_MAX_TURNS`.

### OpenRouter

- Set an API key via `DOREAN_OPENROUTER_API_KEY`, `OPENROUTER_API_KEY`, or
  `"openrouter_api_key"` in `~/.dorean/config.json`.
- Default model is `openrouter/free` (routes to free models); pick any `:free`
  model with `--model`.
- `DOREAN_REFERER` / `DOREAN_TITLE` set the optional `HTTP-Referer` /
  `X-OpenRouter-Title` headers.

