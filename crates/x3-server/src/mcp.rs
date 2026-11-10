//! MCP server (Streamable HTTP at `/mcp`, built on the official `rmcp` SDK).
//!
//! Tools return file paths and small JSON documents, never base64 audio.
//! No tool can read, set or list provider keys: the service layer has no
//! such operation to call.

use std::{path::PathBuf, sync::Arc};

use rmcp::{
    handler::server::{router::tool::ToolRouter, tool::Extension, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use x3_core::{AudioFormat, Task};

use crate::{
    auth::Principal,
    service::{check_extension, SpeakInput, VoiceService, PLAY_EXTENSIONS},
};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpeakArgs {
    /// Text to speak.
    pub text: String,
    /// Voice ID from list_voices. Omit for the model's default voice.
    #[serde(default)]
    pub voice: Option<String>,
    /// Model ID or alias (default: alias "tts", the free Apple system voice).
    #[serde(default)]
    pub model: Option<String>,
    /// Output format: wav, mp3, opus or pcm. Default: wav for local models,
    /// mp3 for cloud models.
    #[serde(default)]
    pub format: Option<String>,
    /// Play the audio on the speakers after synthesis (default true).
    #[serde(default)]
    pub play: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TranscribeArgs {
    /// Absolute path to a local audio file (wav, mp3, m4a, flac, ogg, ...).
    pub path: String,
    /// Model ID or alias (default: alias "stt", on-device Apple
    /// SpeechTranscriber).
    #[serde(default)]
    pub model: Option<String>,
    /// Include per-word timestamps in the result.
    #[serde(default)]
    pub word_timestamps: Option<bool>,
    /// Language hint, e.g. "en" or "en-US".
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlayArgs {
    /// Absolute path to an audio file (wav, mp3, m4a, aiff, caf).
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListModelsArgs {
    /// Filter by task: "tts" or "stt".
    #[serde(default)]
    pub task: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListVoicesArgs {
    /// Filter by provider: "apple", "elevenlabs" or "cartesia".
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Clone)]
pub struct VoiceMcp {
    svc: Arc<VoiceService>,
    playback: Arc<tokio::sync::Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

fn principal(parts: &http::request::Parts) -> Result<Principal, McpError> {
    parts
        .extensions
        .get::<Principal>()
        .cloned()
        .ok_or_else(|| McpError::invalid_request("unauthenticated", None))
}

fn ok_json(v: serde_json::Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string_pretty(&v).unwrap_or_default(),
    )]))
}

fn tool_error(e: impl std::fmt::Display) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![ContentBlock::text(
        e.to_string(),
    )]))
}

#[tool_router]
impl VoiceMcp {
    pub fn new(svc: Arc<VoiceService>, playback: Arc<tokio::sync::Mutex<()>>) -> Self {
        Self {
            svc,
            playback,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Speak text aloud and save it to an audio file. Returns the file path, \
        duration and estimated cost. The default model (alias \"tts\") is the free on-device \
        Apple system voice. Cloud models (elevenlabs/*, cartesia/*) COST MONEY, billed by the \
        provider per character; the result includes estimated_cost_usd. Playback is queued so \
        consecutive calls do not overlap."
    )]
    async fn speak(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(args): Parameters<SpeakArgs>,
    ) -> Result<CallToolResult, McpError> {
        let who = principal(&parts)?;
        let format = match args.format.as_deref().map(AudioFormat::parse).transpose() {
            Ok(f) => f,
            Err(e) => return tool_error(e),
        };
        let out = match self
            .svc
            .speak(
                &who,
                SpeakInput {
                    text: args.text,
                    model: args.model,
                    voice: args.voice,
                    format,
                    save: true,
                    ..Default::default()
                },
            )
            .await
        {
            Ok(o) => o,
            Err(e) => return tool_error(e),
        };
        let path = out.path.clone().unwrap_or_default();
        let want_play = args.play.unwrap_or(true);
        let playable = check_extension(&path, PLAY_EXTENSIONS).is_ok();
        let played = want_play && playable;
        if played {
            let lock = self.playback.clone();
            let p = path.clone();
            tokio::spawn(async move {
                let _g = lock.lock().await;
                if let Err(e) = x3_apple::playback::play_file(&p).await {
                    tracing::warn!("playback failed: {e}");
                }
            });
        }
        ok_json(serde_json::json!({
            "path": path,
            "duration_seconds": out.duration_seconds,
            "model": out.model,
            "format": out.format,
            "estimated_cost_usd": out.estimated_cost_usd,
            "playing": played,
        }))
    }

    #[tool(
        description = "Transcribe a local audio file to text. Returns text and timed segments \
        (and per-word timestamps if requested). The default model (alias \"stt\") is free \
        on-device Apple SpeechTranscriber. Cloud models (elevenlabs/scribe_*, \
        cartesia/ink-whisper) COST MONEY, billed per second of audio, and upload the file to the \
        provider; the result includes estimated_cost_usd."
    )]
    async fn transcribe(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(args): Parameters<TranscribeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let who = principal(&parts)?;
        match self
            .svc
            .transcribe_path(
                &who,
                &PathBuf::from(&args.path),
                args.model,
                args.language,
                args.word_timestamps.unwrap_or(false),
            )
            .await
        {
            Ok(out) => ok_json(serde_json::to_value(&out).unwrap_or_default()),
            Err(e) => tool_error(e),
        }
    }

    #[tool(
        description = "Play a local audio file (wav, mp3, m4a, aiff, caf) on the speakers. \
        Free. Waits until playback finishes."
    )]
    async fn play(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(args): Parameters<PlayArgs>,
    ) -> Result<CallToolResult, McpError> {
        principal(&parts)?;
        let path = PathBuf::from(&args.path);
        if let Err(e) = check_extension(&path, PLAY_EXTENSIONS) {
            return tool_error(e);
        }
        let _g = self.playback.lock().await;
        match x3_apple::playback::play_file(&path).await {
            Ok(()) => ok_json(serde_json::json!({ "played": path })),
            Err(e) => tool_error(e),
        }
    }

    #[tool(
        description = "List available speech models and aliases, with whether each is \
        local (free) or cloud (paid) and its estimated price."
    )]
    async fn list_models(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(args): Parameters<ListModelsArgs>,
    ) -> Result<CallToolResult, McpError> {
        principal(&parts)?;
        let task = match args.task.as_deref().map(Task::parse).transpose() {
            Ok(t) => t,
            Err(e) => return tool_error(e),
        };
        let models: Vec<_> = self
            .svc
            .list_models(task)
            .into_iter()
            .map(|(m, aliases, pricing)| {
                serde_json::json!({
                    "id": m.id,
                    "task": m.task,
                    "name": m.display_name,
                    "cloud": m.cloud,
                    "available": m.available,
                    "aliases": aliases,
                    "pricing": pricing,
                })
            })
            .collect();
        ok_json(serde_json::json!({
            "models": models,
            "aliases": self.svc.router().aliases(),
        }))
    }

    #[tool(
        description = "List voices, optionally for one provider (apple, elevenlabs, \
        cartesia). Free. Pass a voice id to speak's `voice`."
    )]
    async fn list_voices(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(args): Parameters<ListVoicesArgs>,
    ) -> Result<CallToolResult, McpError> {
        principal(&parts)?;
        match self.svc.list_voices(args.provider.as_deref()).await {
            Ok((voices, errors)) => ok_json(serde_json::json!({
                "voices": voices,
                "errors": errors,
            })),
            Err(e) => tool_error(e),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for VoiceMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "voice-studio",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Voice Studio: text-to-speech and speech-to-text on this Mac. \
                 Use `speak` to talk to the user (free Apple voice by default) and \
                 `transcribe` for audio files. Cloud models cost money; check \
                 estimated_cost_usd in results.",
            )
    }
}
