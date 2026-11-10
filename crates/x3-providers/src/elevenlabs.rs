//! ElevenLabs adapter.
//!
//! Endpoints (checked against the official docs, Nov 2026):
//! - TTS: `POST /v1/text-to-speech/{voice_id}?output_format=...`, header
//!   `xi-api-key`, JSON `{text, model_id, voice_settings?, language_code?}`.
//!   <https://elevenlabs.io/docs/api-reference/text-to-speech/convert>
//! - STT: `POST /v1/speech-to-text`, multipart `file`, `model_id`
//!   (`scribe_v2`), `timestamps_granularity`, `language_code`. Response has
//!   `text`, `language_code`, `words[] {text, start, end, type}` where `type`
//!   is `word`, `spacing` or `audio_event`.
//!   <https://elevenlabs.io/docs/api-reference/speech-to-text/convert>
//! - Voices: `GET /v2/voices?page_size=..&next_page_token=..` returning
//!   `{voices[], has_more, next_page_token}`.
//!   <https://elevenlabs.io/docs/api-reference/voices/search>
//! - Default voice `JBFqnCBsd6RMkjVDRZzb` ("George") is the one used in the
//!   official quickstart: <https://elevenlabs.io/docs/quickstart>

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use x3_core::{
    audio, AudioFormat, Engine, Error, LoadedModel, ModelId, ModelInfo, Pricing, Result,
    SttRequest, Task, Transcript, TtsAudio, TtsRequest, Voice, Word,
};
use x3_vault::{ExposeSecret, SecretStore};

use crate::http::{key, map_error, mime_for_filename, transport_error};

pub const PROVIDER: &str = "elevenlabs";
pub const DEFAULT_BASE_URL: &str = "https://api.elevenlabs.io";
pub const DEFAULT_VOICE: &str = "JBFqnCBsd6RMkjVDRZzb";

/// Body substrings ElevenLabs uses for exhausted credits.
// TODO(verify): confirm the exact `detail.status` values for quota errors
// against a real over-quota account; we match loosely on substrings.
const QUOTA_MARKERS: &[&str] = &["quota_exceeded", "quota exceeded", "insufficient credits"];

/// Placeholder price estimates (USD). These are rough and plan-dependent;
/// the owner should confirm against current ElevenLabs pricing. They only
/// drive budget checks, never billing.
fn pricing_for(name: &str) -> Pricing {
    if name.starts_with("scribe") {
        Pricing::per_hour(0.40)
    } else if name.contains("flash") || name.contains("turbo") {
        Pricing::per_1k_chars(0.15)
    } else {
        Pricing::per_1k_chars(0.30)
    }
}

#[derive(Clone)]
pub struct ElevenLabsEngine {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    base_url: String,
    default_voice: String,
    store: Arc<dyn SecretStore>,
}

impl ElevenLabsEngine {
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self::with_base_url(store, DEFAULT_BASE_URL, crate::default_http_client())
    }

    pub fn with_base_url(
        store: Arc<dyn SecretStore>,
        base_url: &str,
        http: reqwest::Client,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                http,
                base_url: base_url.trim_end_matches('/').to_string(),
                default_voice: DEFAULT_VOICE.to_string(),
                store,
            }),
        }
    }

    fn has_key(&self) -> bool {
        matches!(self.inner.store.get(PROVIDER), Ok(Some(_)))
    }

    fn info(&self, name: &str, display: &str) -> ModelInfo {
        ModelInfo {
            id: ModelId::parse(&format!("{PROVIDER}/{name}")).expect("valid id"),
            task: if name.starts_with("scribe") {
                Task::Stt
            } else {
                Task::Tts
            },
            display_name: display.to_string(),
            cloud: true,
            available: self.has_key(),
            pricing: pricing_for(name),
        }
    }
}

/// Map our format to an ElevenLabs `output_format` value, returning the
/// format and sample rate actually produced.
fn output_format(format: AudioFormat, sample_rate: Option<u32>) -> Result<(String, u32)> {
    const PCM_RATES: &[u32] = &[8000, 16000, 22050, 24000, 32000, 44100, 48000];
    let pick = |default: u32| -> Result<u32> {
        let sr = sample_rate.unwrap_or(default);
        if PCM_RATES.contains(&sr) {
            Ok(sr)
        } else {
            Err(Error::InvalidRequest(format!(
                "elevenlabs does not support sample rate {sr}"
            )))
        }
    };
    Ok(match format {
        AudioFormat::Wav => {
            let sr = pick(44100)?;
            (format!("wav_{sr}"), sr)
        }
        AudioFormat::Pcm => {
            let sr = pick(24000)?;
            (format!("pcm_{sr}"), sr)
        }
        AudioFormat::Mp3 => ("mp3_44100_128".to_string(), 44100),
        // TODO(verify): confirm the container of `opus_48000_*` (Ogg Opus
        // expected) before advertising audio/ogg for it.
        AudioFormat::Opus => ("opus_48000_64".to_string(), 48000),
    })
}

#[async_trait]
impl Engine for ElevenLabsEngine {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![
            self.info("eleven_multilingual_v2", "ElevenLabs Multilingual v2"),
            self.info("eleven_v4", "ElevenLabs v4"),
            self.info("scribe_v2", "ElevenLabs Scribe v2"),
        ]
    }

    fn model_info(&self, id: &ModelId) -> Option<ModelInfo> {
        // Any `elevenlabs/<name>` is passed through so new models work
        // without an app update. Task is inferred from the name.
        (id.provider() == PROVIDER).then(|| self.info(id.name(), id.name()))
    }

    async fn load(&self, model: &ModelId) -> Result<Arc<dyn LoadedModel>> {
        Ok(Arc::new(ElevenLabsModel {
            id: model.clone(),
            engine: self.clone(),
        }))
    }

    async fn list_voices(&self) -> Result<Vec<Voice>> {
        let inner = &self.inner;
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        // Cap pagination to keep the call bounded.
        for _ in 0..10 {
            let k = key(&inner.store, PROVIDER)?;
            let mut req = inner
                .http
                .get(format!("{}/v2/voices", inner.base_url))
                .query(&[("page_size", "100")]);
            if let Some(t) = &token {
                req = req.query(&[("next_page_token", t.as_str())]);
            }
            let resp = req
                .header("xi-api-key", k.expose_secret())
                .send()
                .await
                .map_err(|e| transport_error(PROVIDER, e))?;
            if !resp.status().is_success() {
                return Err(map_error(PROVIDER, resp, QUOTA_MARKERS).await);
            }
            let page: VoicesPage = resp
                .json()
                .await
                .map_err(|e| transport_error(PROVIDER, e))?;
            out.extend(page.voices.into_iter().map(|v| {
                Voice {
                    id: v.voice_id,
                    name: v.name.unwrap_or_default(),
                    provider: PROVIDER.into(),
                    language: v
                        .labels
                        .as_ref()
                        .and_then(|l| l.get("language").or_else(|| l.get("accent")))
                        .and_then(|s| s.as_str().map(str::to_string)),
                    quality: v.category,
                    description: v.description,
                }
            }));
            match (page.has_more, page.next_page_token) {
                (true, Some(t)) => token = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }
}

#[derive(Deserialize)]
struct VoicesPage {
    #[serde(default)]
    voices: Vec<VoiceItem>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct VoiceItem {
    voice_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    labels: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Deserialize)]
struct SttResponse {
    #[serde(default)]
    text: String,
    #[serde(default)]
    language_code: Option<String>,
    #[serde(default)]
    words: Vec<SttWord>,
}

#[derive(Deserialize)]
struct SttWord {
    text: String,
    #[serde(default)]
    start: Option<f64>,
    #[serde(default)]
    end: Option<f64>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

struct ElevenLabsModel {
    id: ModelId,
    engine: ElevenLabsEngine,
}

#[async_trait]
impl LoadedModel for ElevenLabsModel {
    fn id(&self) -> &ModelId {
        &self.id
    }

    async fn tts(&self, req: TtsRequest) -> Result<TtsAudio> {
        if self.id.name().starts_with("scribe") {
            return Err(Error::Unsupported(format!("{} is speech-to-text", self.id)));
        }
        let inner = &self.engine.inner;
        let voice = req
            .voice
            .clone()
            .unwrap_or_else(|| inner.default_voice.clone());
        if voice.is_empty() || !voice.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(Error::InvalidRequest(format!(
                "invalid ElevenLabs voice id '{voice}'"
            )));
        }
        let (fmt, sr) = output_format(req.format, req.sample_rate)?;
        let mut body = serde_json::json!({
            "text": req.text,
            "model_id": self.id.name(),
        });
        if let Some(speed) = req.speed {
            body["voice_settings"] = serde_json::json!({ "speed": speed });
        }
        // `language_code` is not supported by multilingual_v2 per the docs.
        if let Some(lang) = &req.language {
            if !self.id.name().contains("multilingual_v2") {
                body["language_code"] = serde_json::json!(lang);
            }
        }
        let k = key(&inner.store, PROVIDER)?;
        let resp = inner
            .http
            .post(format!("{}/v1/text-to-speech/{voice}", inner.base_url))
            .query(&[("output_format", fmt.as_str())])
            .header("xi-api-key", k.expose_secret())
            .json(&body)
            .send()
            .await
            .map_err(|e| transport_error(PROVIDER, e))?;
        drop(k);
        if !resp.status().is_success() {
            return Err(map_error(PROVIDER, resp, QUOTA_MARKERS).await);
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| transport_error(PROVIDER, e))?
            .to_vec();
        let duration_secs = match req.format {
            AudioFormat::Wav => audio::wav_duration_secs(&bytes),
            AudioFormat::Pcm => audio::pcm_duration_secs(bytes.len(), sr),
            AudioFormat::Mp3 => Some(audio::estimate_duration_from_bitrate(bytes.len(), 128)),
            AudioFormat::Opus => Some(audio::estimate_duration_from_bitrate(bytes.len(), 64)),
        };
        Ok(TtsAudio {
            bytes,
            format: req.format,
            sample_rate: Some(sr),
            duration_secs,
        })
    }

    async fn stt(&self, req: SttRequest) -> Result<Transcript> {
        if !self.id.name().starts_with("scribe") {
            return Err(Error::Unsupported(format!("{} is text-to-speech", self.id)));
        }
        let inner = &self.engine.inner;
        let input_duration = audio::wav_duration_secs(&req.audio);
        let part = reqwest::multipart::Part::bytes(req.audio)
            .file_name(req.filename.clone())
            .mime_str(mime_for_filename(&req.filename))
            .map_err(|e| Error::Internal(e.to_string()))?;
        let mut form = reqwest::multipart::Form::new()
            .text("model_id", self.id.name().to_string())
            // Word timings are always requested so we can build segments.
            .text("timestamps_granularity", "word")
            .part("file", part);
        if let Some(lang) = req.language.clone() {
            form = form.text("language_code", lang);
        }
        let k = key(&inner.store, PROVIDER)?;
        let resp = inner
            .http
            .post(format!("{}/v1/speech-to-text", inner.base_url))
            .header("xi-api-key", k.expose_secret())
            .multipart(form)
            .send()
            .await
            .map_err(|e| transport_error(PROVIDER, e))?;
        drop(k);
        if !resp.status().is_success() {
            return Err(map_error(PROVIDER, resp, QUOTA_MARKERS).await);
        }
        let r: SttResponse = resp
            .json()
            .await
            .map_err(|e| transport_error(PROVIDER, e))?;
        let words: Vec<Word> = r
            .words
            .into_iter()
            .filter(|w| w.kind.as_deref().unwrap_or("word") == "word")
            .filter_map(|w| {
                Some(Word {
                    word: w.text,
                    start: w.start?,
                    end: w.end?,
                })
            })
            .collect();
        let segments = Transcript::segments_from_words(&words, 1.0);
        let duration_secs = input_duration.or_else(|| words.last().map(|w| w.end));
        Ok(Transcript {
            text: r.text.trim().to_string(),
            language: r.language_code,
            duration_secs,
            segments,
            words: req.word_timestamps.then_some(words),
        })
    }
}
