//! The voice service shared by the HTTP API, MCP tools and the desktop app.
//!
//! Every billable call goes through the same pipeline:
//! resolve model → check token scopes → estimate cost → check budgets
//! (with an in-flight reservation so concurrent calls cannot overshoot) →
//! run the engine → log usage.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use chrono::Utc;
use serde::Serialize;
use x3_core::{
    audio, router::Resolved, AudioFormat, BudgetCheck, Budgets, Error, ModelId, ModelInfo,
    ModelRouter, Pricing, Result, SttRequest, Task, Transcript, TtsRequest, UsageRecord,
    UsageStatus, Voice,
};

use crate::{
    auth::Principal,
    config::{Paths, Settings},
    db::{month_start, Db},
};

/// Largest accepted TTS input, in characters.
pub const MAX_TTS_CHARS: usize = 10_000;
/// Largest accepted STT input, in bytes.
pub const MAX_STT_BYTES: usize = 100 * 1024 * 1024;

/// OpenAI voice names; treated as "engine default" so OpenAI clients work.
const OPENAI_VOICES: &[&str] = &[
    "alloy", "ash", "ballad", "coral", "echo", "fable", "nova", "onyx", "sage", "shimmer", "verse",
];

/// Audio file extensions the MCP `transcribe` tool will read.
pub const STT_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "m4a", "mp4", "aac", "aif", "aiff", "caf", "flac", "ogg", "oga", "opus", "webm",
];
/// Audio file extensions the MCP `play` tool will play.
pub const PLAY_EXTENSIONS: &[&str] = &["wav", "mp3", "m4a", "aac", "aif", "aiff", "caf"];

#[derive(Debug, Clone, Default)]
pub struct SpeakInput {
    pub text: String,
    pub model: Option<String>,
    pub voice: Option<String>,
    pub format: Option<AudioFormat>,
    pub sample_rate: Option<u32>,
    pub speed: Option<f32>,
    pub language: Option<String>,
    /// Write the audio to the outputs dir and return its path.
    pub save: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeakOutput {
    pub model: ModelId,
    pub format: AudioFormat,
    pub sample_rate: Option<u32>,
    pub duration_seconds: Option<f64>,
    pub estimated_cost_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip)]
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TranscribeInput {
    pub audio: Vec<u8>,
    pub filename: String,
    pub model: Option<String>,
    pub language: Option<String>,
    pub word_timestamps: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscribeOutput {
    pub model: ModelId,
    #[serde(flatten)]
    pub transcript: Transcript,
    pub estimated_cost_usd: f64,
}

/// In-flight cost reservations, so concurrent requests cannot jointly exceed
/// a budget that each would individually fit under.
#[derive(Default)]
struct Reservations {
    by_provider: HashMap<String, f64>,
    by_token: HashMap<String, f64>,
}

struct Reservation<'a> {
    svc: &'a VoiceService,
    provider: String,
    token: String,
    amount: f64,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.amount <= 0.0 {
            return;
        }
        if let Ok(mut r) = self.svc.reservations.lock() {
            if let Some(v) = r.by_provider.get_mut(&self.provider) {
                *v -= self.amount;
            }
            if let Some(v) = r.by_token.get_mut(&self.token) {
                *v -= self.amount;
            }
        }
    }
}

pub struct VoiceService {
    router: Arc<ModelRouter>,
    db: Db,
    paths: Paths,
    settings: RwLock<Settings>,
    reservations: Mutex<Reservations>,
    budget_lock: tokio::sync::Mutex<()>,
}

impl VoiceService {
    pub fn new(router: Arc<ModelRouter>, db: Db, paths: Paths, settings: Settings) -> Self {
        router.set_aliases(settings.aliases.clone());
        Self {
            router,
            db,
            paths,
            settings: RwLock::new(settings),
            reservations: Mutex::new(Reservations::default()),
            budget_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn router(&self) -> &Arc<ModelRouter> {
        &self.router
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().expect("settings lock").clone()
    }

    /// Replace and persist settings (aliases take effect immediately).
    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) -> anyhow::Result<Settings> {
        let mut s = self.settings.write().expect("settings lock");
        let mut next = s.clone();
        f(&mut next);
        next.save(&self.paths.settings_path)?;
        self.router.set_aliases(next.aliases.clone());
        *s = next.clone();
        Ok(next)
    }

    fn pricing(&self, info: &ModelInfo) -> Pricing {
        self.settings
            .read()
            .expect("settings lock")
            .pricing_overrides
            .get(&info.id)
            .copied()
            .unwrap_or(info.pricing)
    }

    fn budgets(&self) -> Budgets {
        self.settings.read().expect("settings lock").budgets.clone()
    }

    /// Check budgets and reserve `estimate` until the guard drops.
    async fn reserve(
        &self,
        who: &Principal,
        provider: &str,
        estimate: f64,
    ) -> Result<Reservation<'_>> {
        let mut guard = Reservation {
            svc: self,
            provider: provider.to_string(),
            token: who.token_id.clone(),
            amount: 0.0,
        };
        if estimate <= 0.0 {
            return Ok(guard);
        }
        let _serial = self.budget_lock.lock().await;
        let internal = |e: anyhow::Error| Error::Internal(format!("usage db: {e:#}"));
        let provider_spent = self
            .db
            .provider_spend_since(provider, month_start(Utc::now()))
            .await
            .map_err(internal)?;
        let token_spent = self.db.token_spend(&who.token_id).await.map_err(internal)?;
        let (pending_p, pending_t) = {
            let r = self.reservations.lock().expect("reservations lock");
            (
                r.by_provider.get(provider).copied().unwrap_or(0.0),
                r.by_token.get(&who.token_id).copied().unwrap_or(0.0),
            )
        };
        self.budgets().check(&BudgetCheck {
            provider,
            provider_spent_this_month: provider_spent + pending_p,
            token_label: &who.label,
            token_cap: who.spend_cap_usd,
            token_spent: token_spent + pending_t,
            estimate,
        })?;
        let mut r = self.reservations.lock().expect("reservations lock");
        *r.by_provider.entry(provider.to_string()).or_default() += estimate;
        *r.by_token.entry(who.token_id.clone()).or_default() += estimate;
        guard.amount = estimate;
        Ok(guard)
    }

    async fn log(&self, who: &Principal, info: &ModelInfo, units: f64, cost: f64, ok: bool) {
        let rec = UsageRecord {
            ts: Utc::now(),
            token_id: who.token_id.clone(),
            token_label: who.label.clone(),
            model: info.id.to_string(),
            provider: info.id.provider().to_string(),
            task: info.task,
            units,
            unit: self.pricing(info).unit,
            est_cost_usd: cost,
            status: if ok {
                UsageStatus::Ok
            } else {
                UsageStatus::Error
            },
        };
        if let Err(e) = self.db.record_usage(&rec).await {
            tracing::error!("failed to record usage: {e:#}");
        }
        tracing::info!(
            token = %rec.token_label,
            model = %rec.model,
            units = rec.units,
            est_cost_usd = rec.est_cost_usd,
            status = rec.status.as_str(),
            "voice call"
        );
    }

    fn resolve(&self, who: &Principal, model: Option<&str>, task: Task) -> Result<Resolved> {
        let r = self.router.resolve(model, task)?;
        who.authorize(r.info.id.provider(), task)?;
        Ok(r)
    }

    /// Model-native default output format when the caller does not ask.
    fn default_format(info: &ModelInfo) -> AudioFormat {
        if info.cloud {
            AudioFormat::Mp3
        } else {
            AudioFormat::Wav
        }
    }

    pub async fn speak(&self, who: &Principal, input: SpeakInput) -> Result<SpeakOutput> {
        let text = input.text.trim().to_string();
        if text.is_empty() {
            return Err(Error::InvalidRequest("input text is empty".into()));
        }
        let chars = text.chars().count();
        if chars > MAX_TTS_CHARS {
            return Err(Error::InvalidRequest(format!(
                "input is {chars} characters; the limit is {MAX_TTS_CHARS}"
            )));
        }
        let Resolved { engine, info } = self.resolve(who, input.model.as_deref(), Task::Tts)?;
        let estimate = self.pricing(&info).estimate(chars as f64);
        let format = input.format.unwrap_or_else(|| Self::default_format(&info));
        let voice = input
            .voice
            .filter(|v| !v.trim().is_empty())
            .filter(|v| !OPENAI_VOICES.contains(&v.to_ascii_lowercase().as_str()));

        let reservation = self.reserve(who, info.id.provider(), estimate).await?;
        let result = async {
            let model = engine.load(&info.id).await?;
            model
                .tts(TtsRequest {
                    text,
                    voice,
                    format,
                    sample_rate: input.sample_rate,
                    speed: input.speed,
                    language: input.language,
                })
                .await
        }
        .await;
        let audio = match result {
            Ok(a) => {
                self.log(who, &info, chars as f64, estimate, true).await;
                drop(reservation);
                a
            }
            Err(e) => {
                self.log(who, &info, chars as f64, 0.0, false).await;
                return Err(e);
            }
        };

        let path = if input.save {
            Some(self.save_output(&info.id, audio.format, &audio.bytes)?)
        } else {
            None
        };
        Ok(SpeakOutput {
            model: info.id,
            format: audio.format,
            sample_rate: audio.sample_rate,
            duration_seconds: audio.duration_secs,
            estimated_cost_usd: estimate,
            path,
            bytes: audio.bytes,
        })
    }

    pub async fn transcribe(
        &self,
        who: &Principal,
        input: TranscribeInput,
    ) -> Result<TranscribeOutput> {
        if input.audio.is_empty() {
            return Err(Error::InvalidRequest("audio is empty".into()));
        }
        if input.audio.len() > MAX_STT_BYTES {
            return Err(Error::InvalidRequest("audio file too large".into()));
        }
        let Resolved { engine, info } = self.resolve(who, input.model.as_deref(), Task::Stt)?;
        let pricing = self.pricing(&info);
        let est_secs = audio::estimate_input_duration_secs(&input.audio);
        let estimate = pricing.estimate(est_secs);

        let reservation = self.reserve(who, info.id.provider(), estimate).await?;
        let result = async {
            let model = engine.load(&info.id).await?;
            model
                .stt(SttRequest {
                    audio: input.audio,
                    filename: input.filename,
                    language: input.language,
                    word_timestamps: input.word_timestamps,
                })
                .await
        }
        .await;
        match result {
            Ok(t) => {
                let secs = t.duration_secs.unwrap_or(est_secs);
                let cost = pricing.estimate(secs);
                self.log(who, &info, secs, cost, true).await;
                drop(reservation);
                Ok(TranscribeOutput {
                    model: info.id,
                    transcript: t,
                    estimated_cost_usd: cost,
                })
            }
            Err(e) => {
                self.log(who, &info, est_secs, 0.0, false).await;
                Err(e)
            }
        }
    }

    /// Transcribe a local file (MCP `transcribe`).
    pub async fn transcribe_path(
        &self,
        who: &Principal,
        path: &Path,
        model: Option<String>,
        language: Option<String>,
        word_timestamps: bool,
    ) -> Result<TranscribeOutput> {
        check_extension(path, STT_EXTENSIONS)?;
        let meta = tokio::fs::metadata(path)
            .await
            .map_err(|e| Error::InvalidRequest(format!("{}: {e}", path.display())))?;
        if !meta.is_file() {
            return Err(Error::InvalidRequest(format!(
                "{} is not a file",
                path.display()
            )));
        }
        if meta.len() as usize > MAX_STT_BYTES {
            return Err(Error::InvalidRequest("audio file too large".into()));
        }
        let audio = tokio::fs::read(path).await?;
        let filename = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "audio.wav".into());
        self.transcribe(
            who,
            TranscribeInput {
                audio,
                filename,
                model,
                language,
                word_timestamps,
            },
        )
        .await
    }

    pub fn list_models(&self, task: Option<Task>) -> Vec<(ModelInfo, Vec<String>, Pricing)> {
        let aliases = self.router.aliases();
        self.router
            .list_models(task)
            .into_iter()
            .map(|m| {
                let a = aliases.aliases_for(&m.id);
                let p = self.pricing(&m);
                (m, a, p)
            })
            .collect()
    }

    /// Voices for one provider, or all providers (errors reported per
    /// provider instead of failing the whole call).
    pub async fn list_voices(
        &self,
        provider: Option<&str>,
    ) -> Result<(Vec<Voice>, HashMap<String, String>)> {
        let engines: Vec<_> = match provider {
            Some(p) => vec![self
                .router
                .engine(p)
                .ok_or_else(|| Error::InvalidRequest(format!("unknown provider '{p}'")))?],
            None => self.router.engines().cloned().collect(),
        };
        let mut voices = Vec::new();
        let mut errors = HashMap::new();
        for e in engines {
            match e.list_voices().await {
                Ok(v) => voices.extend(v),
                Err(err) if provider.is_some() => return Err(err),
                Err(err) => {
                    errors.insert(e.provider().to_string(), err.to_string());
                }
            }
        }
        Ok((voices, errors))
    }

    fn save_output(&self, model: &ModelId, format: AudioFormat, bytes: &[u8]) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.paths.outputs_dir)?;
        let stamp = Utc::now().format("%Y%m%d-%H%M%S%.3f");
        let safe: String = model
            .as_str()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = self
            .paths
            .outputs_dir
            .join(format!("{stamp}-{safe}.{}", format.extension()));
        std::fs::write(&path, bytes)?;
        Ok(path)
    }
}

pub fn check_extension(path: &Path, allowed: &[&str]) -> Result<()> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if allowed.contains(&ext.as_str()) {
        Ok(())
    } else {
        Err(Error::InvalidRequest(format!(
            "unsupported audio file extension '{ext}' (allowed: {})",
            allowed.join(", ")
        )))
    }
}
