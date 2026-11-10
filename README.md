# x3 — Voice Studio (working name)

An "LM Studio for voice" for macOS. Voice Studio runs local speech models
(text-to-speech and speech-to-text) on Apple Silicon and proxies cloud voice
providers (ElevenLabs, Cartesia). Everything is exposed through an
OpenAI-compatible HTTP API on localhost and an MCP server, so AI agents such as
Claude Code can speak and listen.

Provider API keys live only in the macOS Keychain, inside the app. Agents get
scoped, revocable, spend-capped local tokens instead of keys.

- Platform: macOS 26+, Apple Silicon (Rust + Tauri 2, with a small Swift bridge)
- Spec: [docs/mvp-spec.md](docs/mvp-spec.md)

Status: pre-alpha. See the milestones in the spec.

## Layout

| Path | What |
| --- | --- |
| `crates/x3-core` | Shared types, `Engine`/`LoadedModel` traits, aliases, router, usage, budgets |
| `crates/x3-vault` | Provider key storage (Keychain on macOS, in-memory elsewhere) |
| `crates/x3-providers` | ElevenLabs and Cartesia adapters |
| `crates/x3-apple` | Apple built-in TTS (AVSpeechSynthesizer) and STT (SpeechAnalyzer via `swift/`) — macOS only |
| `crates/x3-server` | Localhost HTTP API (OpenAI-compatible) + MCP over Streamable HTTP |
| `crates/x3-cli` | `x3` binary: `x3 mcp` stdio shim, `x3 serve`, `x3 token` |
| `apps/desktop` | Tauri 2 menu-bar app (Svelte UI) |

## Development

```sh
cargo test --workspace                 # Linux or macOS
cargo clippy --workspace --all-targets -- -D warnings

# Headless server (no desktop app); prints a token once
cargo run -p x3-cli -- serve --mint-token dev

# Desktop app (macOS)
cd apps/desktop && npm install && npx tauri dev
```

Claude Code MCP config (token minted in the app's Server screen; `x3` on PATH):

```json
{ "mcpServers": { "voice": { "command": "x3", "args": ["mcp"], "env": { "X3_TOKEN": "x3_live_…" } } } }
```
