use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use crate::{
    alias::AliasTable,
    engine::Engine,
    model::{ModelId, ModelInfo, Task},
    Error, Result,
};

/// OpenAI model names accepted for drop-in compatibility, mapped to our
/// default aliases.
const OPENAI_COMPAT: &[(&str, &str)] = &[
    ("tts-1", "tts"),
    ("tts-1-hd", "tts"),
    ("gpt-4o-mini-tts", "tts"),
    ("whisper-1", "stt"),
    ("gpt-4o-transcribe", "stt"),
    ("gpt-4o-mini-transcribe", "stt"),
];

/// Resolves aliases and model IDs to the engine that serves them.
pub struct ModelRouter {
    engines: BTreeMap<String, Arc<dyn Engine>>,
    aliases: RwLock<AliasTable>,
}

/// Result of resolving a model name.
#[derive(Clone)]
pub struct Resolved {
    pub engine: Arc<dyn Engine>,
    pub info: ModelInfo,
}

impl ModelRouter {
    pub fn new(aliases: AliasTable) -> Self {
        Self {
            engines: BTreeMap::new(),
            aliases: RwLock::new(aliases),
        }
    }

    pub fn register(&mut self, engine: Arc<dyn Engine>) {
        self.engines.insert(engine.provider().to_string(), engine);
    }

    pub fn engine(&self, provider: &str) -> Option<Arc<dyn Engine>> {
        self.engines.get(provider).cloned()
    }

    pub fn engines(&self) -> impl Iterator<Item = &Arc<dyn Engine>> {
        self.engines.values()
    }

    pub fn aliases(&self) -> AliasTable {
        self.aliases.read().expect("alias lock").clone()
    }

    pub fn set_aliases(&self, table: AliasTable) {
        *self.aliases.write().expect("alias lock") = table;
    }

    /// Resolve `name` (alias, OpenAI-compatible name, or `<provider>/<model>`)
    /// for `task`. `None` means "the default for this task" (alias `tts` or
    /// `stt`).
    pub fn resolve(&self, name: Option<&str>, task: Task) -> Result<Resolved> {
        let name = name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(task.as_str());
        let name = OPENAI_COMPAT
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| *v)
            .unwrap_or(name);

        let id = match self.aliases.read().expect("alias lock").get(name) {
            Some(id) => id.clone(),
            None => ModelId::parse(name)?,
        };

        let engine = self
            .engines
            .get(id.provider())
            .cloned()
            .ok_or_else(|| Error::UnknownModel(id.to_string()))?;
        let info = engine
            .model_info(&id)
            .ok_or_else(|| Error::UnknownModel(id.to_string()))?;
        if info.task != task {
            return Err(Error::InvalidRequest(format!(
                "model '{id}' is a {} model, not {task}",
                info.task
            )));
        }
        Ok(Resolved { engine, info })
    }

    /// Every model from every engine.
    pub fn list_models(&self, task: Option<Task>) -> Vec<ModelInfo> {
        self.engines
            .values()
            .flat_map(|e| e.models())
            .filter(|m| task.is_none_or(|t| m.task == t))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::{
        engine::LoadedModel,
        model::Pricing,
        types::{TtsAudio, TtsRequest, Voice},
        AudioFormat,
    };

    struct Fake;
    struct FakeModel(ModelId);

    #[async_trait]
    impl LoadedModel for FakeModel {
        fn id(&self) -> &ModelId {
            &self.0
        }
        async fn tts(&self, req: TtsRequest) -> Result<TtsAudio> {
            Ok(TtsAudio {
                bytes: req.text.into_bytes(),
                format: AudioFormat::Pcm,
                sample_rate: Some(16_000),
                duration_secs: None,
            })
        }
    }

    #[async_trait]
    impl Engine for Fake {
        fn provider(&self) -> &str {
            "fake"
        }
        fn models(&self) -> Vec<ModelInfo> {
            vec![
                ModelInfo {
                    id: ModelId::parse("fake/speak").unwrap(),
                    task: Task::Tts,
                    display_name: "Fake TTS".into(),
                    cloud: false,
                    available: true,
                    pricing: Pricing::FREE_CHARS,
                },
                ModelInfo {
                    id: ModelId::parse("fake/hear").unwrap(),
                    task: Task::Stt,
                    display_name: "Fake STT".into(),
                    cloud: false,
                    available: true,
                    pricing: Pricing::FREE_SECONDS,
                },
            ]
        }
        async fn load(&self, model: &ModelId) -> Result<Arc<dyn LoadedModel>> {
            Ok(Arc::new(FakeModel(model.clone())))
        }
        async fn list_voices(&self) -> Result<Vec<Voice>> {
            Ok(vec![])
        }
    }

    fn router() -> ModelRouter {
        let mut aliases = AliasTable::empty();
        aliases
            .set("tts", ModelId::parse("fake/speak").unwrap())
            .unwrap();
        aliases
            .set("stt", ModelId::parse("fake/hear").unwrap())
            .unwrap();
        let mut r = ModelRouter::new(aliases);
        r.register(Arc::new(Fake));
        r
    }

    #[test]
    fn resolves_alias_default_and_openai_names() {
        let r = router();
        assert_eq!(
            r.resolve(None, Task::Tts).unwrap().info.id.as_str(),
            "fake/speak"
        );
        assert_eq!(
            r.resolve(Some("tts-1"), Task::Tts)
                .unwrap()
                .info
                .id
                .as_str(),
            "fake/speak"
        );
        assert_eq!(
            r.resolve(Some("whisper-1"), Task::Stt)
                .unwrap()
                .info
                .id
                .as_str(),
            "fake/hear"
        );
        assert_eq!(
            r.resolve(Some("fake/hear"), Task::Stt)
                .unwrap()
                .info
                .id
                .as_str(),
            "fake/hear"
        );
    }

    #[test]
    fn rejects_unknown_and_wrong_task() {
        let r = router();
        assert!(matches!(
            r.resolve(Some("nope/x"), Task::Tts),
            Err(Error::UnknownModel(_))
        ));
        assert!(matches!(
            r.resolve(Some("fake/hear"), Task::Tts),
            Err(Error::InvalidRequest(_))
        ));
        assert!(matches!(
            r.resolve(Some("fake/unlisted"), Task::Tts),
            Err(Error::UnknownModel(_))
        ));
    }

    #[test]
    fn alias_edits_take_effect() {
        let r = router();
        let mut t = r.aliases();
        t.set("tts", ModelId::parse("fake/hear").unwrap()).unwrap();
        r.set_aliases(t);
        assert!(r.resolve(None, Task::Tts).is_err());
    }

    #[tokio::test]
    async fn loads_and_runs() {
        let r = router();
        let res = r.resolve(None, Task::Tts).unwrap();
        let m = res.engine.load(&res.info.id).await.unwrap();
        let out = m
            .tts(TtsRequest {
                text: "hi".into(),
                voice: None,
                format: AudioFormat::Pcm,
                sample_rate: None,
                speed: None,
                language: None,
            })
            .await
            .unwrap();
        assert_eq!(out.bytes, b"hi");
        assert!(m
            .stt(crate::SttRequest {
                audio: vec![],
                filename: "a.wav".into(),
                language: None,
                word_timestamps: false
            })
            .await
            .is_err());
    }
}
