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
