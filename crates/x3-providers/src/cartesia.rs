//! Cartesia adapter.
//!
//! Endpoints (checked against the official docs, Nov 2026; API version
//! header `Cartesia-Version: 2026-08-14`, the only value the current spec
//! accepts; auth is `Authorization: Bearer <sk_car_...>`):
//! - TTS: `POST /tts/bytes`, JSON `{model_id, transcript, voice: {id},
//!   output_format: {container, encoding?, sample_rate, bit_rate?},
//!   language?}`. <https://docs.cartesia.ai/api-reference/tts/bytes>
//! - STT: `POST /stt`, multipart `file`, `model` (`ink-whisper`),
//!   `language`, `timestamp_granularities[]=word`; response
//!   `{text, language, duration, words[] {word, start, end}}`.
//!   <https://docs.cartesia.ai/api-reference/stt/transcribe>
//! - Voices: `GET /voices?limit=..&starting_after=..` returning
//!   `{data[], has_more, next_page}`.
//!   <https://docs.cartesia.ai/api-reference/voices/list>

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use x3_core::{
    audio, AudioFormat, Engine, Error, LoadedModel, ModelId, ModelInfo, Pricing, Result,
    SttRequest, Task, Transcript, TtsAudio, TtsRequest, Voice, Word,
};
use x3_vault::{ExposeSecret, SecretStore};

use crate::http::{key, map_error, mime_for_filename, transport_error};

pub const PROVIDER: &str = "cartesia";
pub const DEFAULT_BASE_URL: &str = "https://api.cartesia.ai";
pub const API_VERSION: &str = "2026-08-14";

// TODO(verify): confirm Cartesia's error body for exhausted credits.
const QUOTA_MARKERS: &[&str] = &["credit", "quota", "insufficient"];

const SAMPLE_RATES: &[u32] = &[8000, 16000, 22050, 24000, 44100, 48000];

/// Placeholder price estimates (USD), plan-dependent; confirm against
/// current Cartesia pricing. Used only for budget checks.
fn pricing_for(name: &str) -> Pricing {
    if name.starts_with("ink") {
        Pricing::per_hour(0.15)
    } else {
        Pricing::per_1k_chars(0.05)
    }
}

#[derive(Clone)]
pub struct CartesiaEngine {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    base_url: String,
    store: Arc<dyn SecretStore>,
}

impl CartesiaEngine {
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
            task: if name.starts_with("ink") {
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

    fn request(&self, method: reqwest::Method, path: &str) -> Result<reqwest::RequestBuilder> {
        let k = key(&self.inner.store, PROVIDER)?;
        Ok(self
            .inner
            .http
            .request(method, format!("{}{path}", self.inner.base_url))
            .header("Cartesia-Version", API_VERSION)
            .bearer_auth(k.expose_secret()))
    }
}

fn output_format(
    format: AudioFormat,
    sample_rate: Option<u32>,
) -> Result<(serde_json::Value, u32)> {
    let sr = sample_rate.unwrap_or(44100);
    if !SAMPLE_RATES.contains(&sr) {
        return Err(Error::InvalidRequest(format!(
            "cartesia does not support sample rate {sr}"
        )));
    }
    let v = match format {
        AudioFormat::Wav => serde_json::json!({
            "container": "wav", "encoding": "pcm_s16le", "sample_rate": sr
        }),
        AudioFormat::Pcm => serde_json::json!({
            "container": "raw", "encoding": "pcm_s16le", "sample_rate": sr
        }),
        AudioFormat::Mp3 => serde_json::json!({
            "container": "mp3", "sample_rate": sr, "bit_rate": 128000
        }),
        AudioFormat::Opus => {
            return Err(Error::Unsupported(
                "cartesia /tts/bytes does not offer opus; use wav, mp3 or pcm".into(),
            ))
        }
    };
    Ok((v, sr))
}

#[async_trait]
impl Engine for CartesiaEngine {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![
            self.info("sonic-3", "Cartesia Sonic 3"),
            self.info("sonic-latest", "Cartesia Sonic (latest)"),
            self.info("ink-whisper", "Cartesia Ink Whisper"),
        ]
    }

    fn model_info(&self, id: &ModelId) -> Option<ModelInfo> {
        (id.provider() == PROVIDER).then(|| self.info(id.name(), id.name()))
    }

    async fn load(&self, model: &ModelId) -> Result<Arc<dyn LoadedModel>> {
        Ok(Arc::new(CartesiaModel {
            id: model.clone(),
            engine: self.clone(),
        }))
    }

    async fn list_voices(&self) -> Result<Vec<Voice>> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..10 {
            let mut req = self
                .request(reqwest::Method::GET, "/voices")?
                .query(&[("limit", "100")]);
            if let Some(a) = &after {
                req = req.query(&[("starting_after", a.as_str())]);
            }
            let resp = req.send().await.map_err(|e| transport_error(PROVIDER, e))?;
            if !resp.status().is_success() {
                return Err(map_error(PROVIDER, resp, QUOTA_MARKERS).await);
            }
            let page: VoicesPage = resp
                .json()
                .await
                .map_err(|e| transport_error(PROVIDER, e))?;
            out.extend(page.data.into_iter().map(|v| Voice {
                id: v.id,
                name: v.name.unwrap_or_default(),
                provider: PROVIDER.into(),
                language: v.language,
                quality: None,
                description: v.description,
            }));
            match (page.has_more, page.next_page) {
                (true, Some(n)) => after = Some(n),
                _ => break,
            }
        }
        Ok(out)
    }
}

#[derive(Deserialize)]
struct VoicesPage {
    #[serde(default)]
    data: Vec<VoiceItem>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    next_page: Option<String>,
}

#[derive(Deserialize)]
struct VoiceItem {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    language: Option<String>,
}

#[derive(Deserialize)]
struct SttResponse {
    #[serde(default)]
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    words: Option<Vec<SttWord>>,
}

#[derive(Deserialize)]
struct SttWord {
    word: String,
    start: f64,
    end: f64,
}

struct CartesiaModel {
    id: ModelId,
    engine: CartesiaEngine,
}

#[async_trait]
impl LoadedModel for CartesiaModel {
    fn id(&self) -> &ModelId {
        &self.id
    }

    async fn tts(&self, req: TtsRequest) -> Result<TtsAudio> {
        if self.id.name().starts_with("ink") {
            return Err(Error::Unsupported(format!("{} is speech-to-text", self.id)));
        }
        // Cartesia has no documented "default voice"; require one rather
        // than guess an ID.
        let voice = req
            .voice
            .clone()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| {
                Error::InvalidRequest(
                    "cartesia requires a voice id; call list_voices(provider=\"cartesia\")".into(),
                )
            })?;
        let (fmt, sr) = output_format(req.format, req.sample_rate)?;
        let mut body = serde_json::json!({
            "model_id": self.id.name(),
            "transcript": req.text,
            "voice": { "id": voice },
            "output_format": fmt,
        });
        if let Some(lang) = &req.language {
            body["language"] = serde_json::json!(lang);
        }
        // TODO(verify): map `speed` onto `generation_config.speed` once the
        // accepted range is confirmed in the docs.
        let resp = self
            .engine
            .request(reqwest::Method::POST, "/tts/bytes")?
            .json(&body)
            .send()
            .await
            .map_err(|e| transport_error(PROVIDER, e))?;
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
            _ => Some(audio::estimate_duration_from_bitrate(bytes.len(), 128)),
        };
        Ok(TtsAudio {
            bytes,
            format: req.format,
            sample_rate: Some(sr),
            duration_secs,
        })
    }

    async fn stt(&self, req: SttRequest) -> Result<Transcript> {
        if !self.id.name().starts_with("ink") {
            return Err(Error::Unsupported(format!("{} is text-to-speech", self.id)));
        }
        let part = reqwest::multipart::Part::bytes(req.audio)
            .file_name(req.filename.clone())
            .mime_str(mime_for_filename(&req.filename))
            .map_err(|e| Error::Internal(e.to_string()))?;
        let mut form = reqwest::multipart::Form::new()
            .text("model", self.id.name().to_string())
            .text("timestamp_granularities[]", "word")
            .part("file", part);
        if let Some(lang) = req.language.clone() {
            form = form.text("language", lang);
        }
        let resp = self
            .engine
            .request(reqwest::Method::POST, "/stt")?
            .multipart(form)
            .send()
            .await
            .map_err(|e| transport_error(PROVIDER, e))?;
        if !resp.status().is_success() {
            return Err(map_error(PROVIDER, resp, QUOTA_MARKERS).await);
        }
        let r: SttResponse = resp
            .json()
            .await
            .map_err(|e| transport_error(PROVIDER, e))?;
        let words: Vec<Word> = r
            .words
            .unwrap_or_default()
            .into_iter()
            .map(|w| Word {
                word: w.word,
                start: w.start,
                end: w.end,
            })
            .collect();
        Ok(Transcript {
            text: r.text.trim().to_string(),
            language: r.language,
            duration_secs: r.duration,
            segments: Transcript::segments_from_words(&words, 1.0),
            words: req.word_timestamps.then_some(words),
        })
    }
}
