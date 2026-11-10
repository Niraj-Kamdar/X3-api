//! Apple built-in speech (zero download).
//!
//! - TTS: `AVSpeechSynthesizer`, capturing PCM with
//!   `write(_:toBufferCallback:)` so audio can be saved, not only played.
//! - STT: macOS 26 `SpeechAnalyzer` + `SpeechTranscriber` through a small
//!   Swift package (`swift/`), because that API is Swift-only.
//!   `SFSpeechRecognizer` is deliberately never used (it can send audio to
//!   Apple servers).
//!
//! Everything macOS-specific is behind `#[cfg(target_os = "macos")]`. On other
//! platforms [`AppleEngine`] still registers its models (marked unavailable)
//! and every call returns [`x3_core::Error::Unsupported`], so the rest of the
//! workspace builds and tests on Linux.

use std::sync::Arc;

use async_trait::async_trait;
use x3_core::{
    alias::{APPLE_STT, APPLE_TTS},
    audio, AudioFormat, Engine, Error, LoadedModel, ModelId, ModelInfo, Pricing, Result,
    SttRequest, Task, Transcript, TtsAudio, TtsRequest, Voice,
};

pub mod playback;
mod stt;
mod tts;

pub const PROVIDER: &str = "apple";

/// Apple system speech engine. Free and on-device.
#[derive(Clone, Default)]
pub struct AppleEngine;

impl AppleEngine {
    pub fn new() -> Self {
        Self
    }

    /// True when Apple TTS can run in this build.
    pub fn tts_available() -> bool {
        tts::available()
    }

    /// True when Apple STT (SpeechAnalyzer) can run in this build.
    pub fn stt_available() -> bool {
        stt::available()
    }
}

#[async_trait]
impl Engine for AppleEngine {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: ModelId::parse(APPLE_TTS).expect("valid"),
                task: Task::Tts,
                display_name: "Apple system voice (AVSpeechSynthesizer)".into(),
                cloud: false,
                available: tts::available(),
                pricing: Pricing::FREE_CHARS,
            },
            ModelInfo {
                id: ModelId::parse(APPLE_STT).expect("valid"),
                task: Task::Stt,
                display_name: "Apple SpeechTranscriber (on-device)".into(),
                cloud: false,
                available: stt::available(),
                pricing: Pricing::FREE_SECONDS,
            },
        ]
    }

    async fn load(&self, model: &ModelId) -> Result<Arc<dyn LoadedModel>> {
        if self.model_info(model).is_none() {
            return Err(Error::UnknownModel(model.to_string()));
        }
        Ok(Arc::new(AppleModel { id: model.clone() }))
    }

    async fn list_voices(&self) -> Result<Vec<Voice>> {
        tokio::task::spawn_blocking(tts::voices)
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
    }
}

struct AppleModel {
    id: ModelId,
}

#[async_trait]
impl LoadedModel for AppleModel {
    fn id(&self) -> &ModelId {
        &self.id
    }

    async fn tts(&self, req: TtsRequest) -> Result<TtsAudio> {
        if self.id.as_str() != APPLE_TTS {
            return Err(Error::Unsupported(format!("{} is speech-to-text", self.id)));
        }
        if !matches!(req.format, AudioFormat::Wav | AudioFormat::Pcm) {
            return Err(Error::Unsupported(format!(
                "{APPLE_TTS} produces wav or pcm, not {}",
                req.format.as_str()
            )));
        }
        let text = req.text.clone();
        let voice = req.voice.clone();
        let speed = req.speed;
        let (samples, native_rate) =
            tokio::task::spawn_blocking(move || tts::synthesize(&text, voice.as_deref(), speed))
                .await
                .map_err(|e| Error::Internal(e.to_string()))??;
        let rate = req.sample_rate.unwrap_or(native_rate);
        let samples = if rate != native_rate {
            resample_linear(&samples, native_rate, rate)
        } else {
            samples
        };
        let duration_secs = Some(samples.len() as f64 / rate as f64);
        let bytes = match req.format {
            AudioFormat::Wav => audio::wav_from_f32(&samples, rate)?,
            _ => audio::pcm_s16le_from_f32(&samples),
        };
        Ok(TtsAudio {
            bytes,
            format: req.format,
            sample_rate: Some(rate),
            duration_secs,
        })
    }

    async fn stt(&self, req: SttRequest) -> Result<Transcript> {
        if self.id.as_str() != APPLE_STT {
            return Err(Error::Unsupported(format!("{} is text-to-speech", self.id)));
        }
        // SpeechAnalyzer reads from a file, so spill the upload to a temp
        // file that keeps the original extension (AVAudioFile sniffs it).
        let ext = std::path::Path::new(&req.filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("wav")
            .to_string();
        let tmp = tempfile::Builder::new()
            .prefix("x3-stt-")
            .suffix(&format!(".{ext}"))
            .tempfile()?;
        std::fs::write(tmp.path(), &req.audio)?;
        let path = tmp.path().to_path_buf();
        let lang = req.language.clone();
        let words = req.word_timestamps;
        let out = tokio::task::spawn_blocking(move || {
            stt::transcribe_file(&path, lang.as_deref(), words)
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))?;
        drop(tmp);
        out
    }
}

/// Simple linear resampler (adequate for speech at M0; replace with rubato
/// when the realtime pipeline lands in M2).
pub fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() || from == 0 || to == 0 {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos.floor() as usize;
            let frac = (pos - idx as f64) as f32;
            let a = input[idx.min(input.len() - 1)];
            let b = input[(idx + 1).min(input.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_lengths() {
        let x = vec![0.0f32; 22050];
        assert_eq!(resample_linear(&x, 22050, 16000).len(), 16000);
        assert_eq!(resample_linear(&x, 22050, 22050).len(), 22050);
    }

    #[test]
    fn catalog_has_default_alias_targets() {
        let e = AppleEngine::new();
        let ids: Vec<_> = e.models().into_iter().map(|m| m.id.to_string()).collect();
        assert!(ids.contains(&APPLE_TTS.to_string()));
        assert!(ids.contains(&APPLE_STT.to_string()));
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn non_macos_is_unsupported() {
        let e = AppleEngine::new();
        assert!(e.models().iter().all(|m| !m.available));
        let m = e.load(&ModelId::parse(APPLE_TTS).unwrap()).await.unwrap();
        let r = m
            .tts(TtsRequest {
                text: "hi".into(),
                voice: None,
                format: AudioFormat::Wav,
                sample_rate: None,
                speed: None,
                language: None,
            })
            .await;
        assert!(matches!(r, Err(Error::Unsupported(_))));
        assert!(e.list_voices().await.unwrap().is_empty());
    }
}
