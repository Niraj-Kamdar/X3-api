use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    model::{ModelId, ModelInfo},
    types::{SttRequest, Transcript, TtsAudio, TtsRequest, Voice},
    Error, Result,
};

/// A backend that can serve one or more models: Apple built-in speech,
/// a cloud provider, or (later) a local runtime such as FluidAudio.
#[async_trait]
pub trait Engine: Send + Sync {
    /// Provider namespace, e.g. `apple`, `elevenlabs`, `cartesia`.
    fn provider(&self) -> &str;

    /// Static catalog of models this engine knows about.
    fn models(&self) -> Vec<ModelInfo>;

    /// Describe a model by ID. The default looks it up in
    /// [`Engine::models`]; cloud engines override this to accept model names
    /// that are not in the static catalog (e.g. a newly released model).
    fn model_info(&self, id: &ModelId) -> Option<ModelInfo> {
        self.models().into_iter().find(|m| &m.id == id)
    }

    /// Prepare a model for use. Cloud engines return a cheap handle; local
    /// engines may load weights here (M1+).
    async fn load(&self, model: &ModelId) -> Result<Arc<dyn LoadedModel>>;

    /// Voices this engine offers.
    async fn list_voices(&self) -> Result<Vec<Voice>>;
}

/// A model ready to run. Methods default to [`Error::Unsupported`].
#[async_trait]
pub trait LoadedModel: Send + Sync {
    fn id(&self) -> &ModelId;

    async fn tts(&self, _req: TtsRequest) -> Result<TtsAudio> {
        Err(Error::Unsupported(format!(
            "model '{}' does not do text-to-speech",
            self.id()
        )))
    }

    async fn stt(&self, _req: SttRequest) -> Result<Transcript> {
        Err(Error::Unsupported(format!(
            "model '{}' does not do speech-to-text",
            self.id()
        )))
    }
}
