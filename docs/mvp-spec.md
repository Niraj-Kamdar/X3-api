# Voice Studio — MVP spec

Working name: Voice Studio. Repository: `x3`, crate prefix `x3-`.

## 1. Scope

A signed macOS app for Apple Silicon, macOS 26+. It:

- speaks and listens out of the box with Apple's built-in speech (no download);
- runs downloadable local models on the Neural Engine and GPU;
- proxies ElevenLabs and Cartesia, with provider keys stored in the Keychain;
- exposes everything through MCP, WebRTC and an OpenAI-compatible HTTP API.

### Later (not MVP)

- Windows, Linux, Intel Macs, CUDA
- Local sound effects (SFX) and music
- Open Hugging Face model discovery
- sherpa-onnx, candle, PyTorch backends
- WebSocket realtime, gRPC, async jobs

## 2. Built-in Apple speech (zero download)

| Task | API | Availability | Notes |
| --- | --- | --- | --- |
| TTS | `AVSpeechSynthesizer` | any supported macOS | PCM captured via `write(_:toBufferCallback:)` so audio can be saved, not only played |
| STT | `SpeechAnalyzer` + `SpeechTranscriber` | macOS 26+ | Swift-only API; OS-managed models; ~10 languages; streaming partials |

Caveats:

- Which Siri voices third-party apps may use must be tested on macOS 26.
- Personal Voice is usable only with the user's permission.
- Never use `SFSpeechRecognizer` (it can send audio to Apple servers).
- No voice cloning with Apple speech.

## 3. Mac acceleration

| Runtime | Hardware | Models |
| --- | --- | --- |
| FluidAudio | Core ML / Neural Engine | Parakeet TDT v3, Parakeet streaming EOU, Silero VAD, Kokoro, PocketTTS |
| whisper.cpp | Metal + Core ML encoder | Whisper large-v3-turbo |
| mlx-audio (Python sidecar) | Metal via MLX | Chatterbox-Turbo |

Scheduler:

- Two queues, `ane` and `gpu`, running in parallel.
- One unified-memory budget (default 40% of RAM) with LRU eviction.
- Core ML models are compiled right after download.

## 4. Model catalog and aliases

| Alias | Default model |
| --- | --- |
| `tts` | Apple system voice (`apple/system`) |
| `stt` | Apple SpeechTranscriber (`apple/speech-transcriber`) |
| `realtime-stt` | Apple SpeechTranscriber (`apple/speech-transcriber`) |
| `kokoro` | Kokoro |
| `pocket-tts` | PocketTTS |
| `tts-clone` | Chatterbox-Turbo |
| `parakeet` | Parakeet TDT v3 |
| `parakeet-eou` | Parakeet streaming EOU |
| `whisper` | Whisper large-v3-turbo |
| `vad` | Silero VAD |
| `elevenlabs/*` | ElevenLabs models |
| `cartesia/*` | Cartesia models |

Aliases are user-editable. Before Silero is installed, barge-in uses the
transcriber's first partial result as the speech-start signal.

## 5. WebRTC realtime sessions

- `POST /v1/realtime/webrtc` with an SDP offer plus `{stt, tts}` returns an SDP
  answer and a session ID.
- Audio path: mic Opus 48 kHz → decode → resample to 16 kHz → VAD + STT.
- Data-channel events:
  - client → app: `session.update`, `tts.say`, `tts.cancel`
  - app → client: `speech.started`, `speech.stopped`, `transcript.partial`,
    `transcript.final`, `tts.started`, `tts.done`, `tts.interrupted`, `error`
- Barge-in is on by default.
- Stack: `str0m` + `audiopus` + `rubato`. Localhost host candidates only; no
  STUN/TURN.
- Latency targets (to be measured): first partial < 300 ms and first TTS audio
  < 300 ms on an M1.
- The playground ships a WebRTC test page.

## 6. MCP

Tools: `speak`, `listen` (record until end of utterance; the menu-bar mic
indicator is shown while listening), `transcribe`, `play`, `list_models`,
`list_voices`.

- Transports: a stdio shim (`x3 mcp`) and Streamable HTTP (`/mcp`).
- Results carry file paths, not base64.
- No tool reads, sets or lists provider keys.

## 7. HTTP API

| Method | Path |
| --- | --- |
| GET | `/v1/models` |
| POST | `/v1/audio/speech` |
| POST | `/v1/audio/transcriptions` |
| GET | `/v1/voices` |
| POST | `/v1/realtime/webrtc` |
| * | `/mcp` |

- Bearer tokens minted by the app, with spend caps.
- CORS off; Host header checks (DNS-rebinding guard).

## 8. Security model

- Provider keys live only in the Keychain (item ACL bound to the signed app).
- Local tokens are scoped, capped and revocable.
- Budgets, and optional approval prompts.
- Model recipes pin commit SHAs; safetensors/ONNX only; no `trust_remote_code`.
- Sidecars never receive keys.
- Every call is logged with token label, model, units and estimated cost.
- A provider key exists in memory only as a `SecretString` on the request path.

## 9. Packaging

- Developer ID signed and notarized DMG (not the Mac App Store).
- `tauri-plugin-updater` for updates.
- Microphone entitlement.
- Menu-bar app with launch at login.
- Models stored under `~/Library/Application Support/<bundle id>/models`.
- The Python sidecar is downloaded on first MLX use.

## 10. Milestones

| Milestone | Contents | Done when |
| --- | --- | --- |
| M0 | Vault, proxy, MCP, and built-in Apple TTS/STT | Claude Code speaks on a fresh Mac with nothing downloaded, and speaks through ElevenLabs with no API key on disk |
| M1 | FluidAudio bridge, whisper.cpp, catalog downloads, two-queue scheduler, MCP `listen` | Claude Code holds a spoken back-and-forth fully offline |
| M2 | WebRTC | The test page holds a duplex conversation with barge-in, on local models and on Cartesia |
| M3 | MLX sidecar with Chatterbox; signed, notarized DMG with auto-update | A clean Mac installs and runs everything without Terminal |

## 11. Open questions

- `fluidaudio-rs` maturity vs. our own Swift bridge.
- Python in the MVP or not.
- 8 GB vs. 16 GB RAM minimum.
- Cloud SFX/music passthrough in the MVP.
- Native WebRTC clients.
- Siri voice access on macOS 26.
- Require macOS 26, or support macOS 14–15 with Parakeet as the default STT.
