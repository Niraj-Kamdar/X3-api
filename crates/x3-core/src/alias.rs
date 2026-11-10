use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{model::ModelId, Error, Result};

/// User-editable mapping from short aliases (`tts`, `stt`, `realtime-stt`,
/// ...) to concrete model IDs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AliasTable(BTreeMap<String, ModelId>);

pub const APPLE_TTS: &str = "apple/system";
pub const APPLE_STT: &str = "apple/speech-transcriber";

impl Default for AliasTable {
    fn default() -> Self {
        let mut m = BTreeMap::new();
        let tts = ModelId::parse(APPLE_TTS).expect("valid");
        let stt = ModelId::parse(APPLE_STT).expect("valid");
        m.insert("tts".to_string(), tts);
        m.insert("stt".to_string(), stt.clone());
        m.insert("realtime-stt".to_string(), stt);
        Self(m)
    }
}

impl AliasTable {
    pub fn empty() -> Self {
        Self(BTreeMap::new())
    }

    pub fn get(&self, alias: &str) -> Option<&ModelId> {
        self.0.get(alias)
    }

    /// Set an alias. Alias names may not contain `/` (that is reserved for
    /// model IDs) and must be non-empty.
    pub fn set(&mut self, alias: &str, model: ModelId) -> Result<()> {
        let alias = alias.trim();
        if alias.is_empty() || alias.contains('/') {
            return Err(Error::InvalidRequest(format!(
                "invalid alias name '{alias}'"
            )));
        }
        self.0.insert(alias.to_string(), model);
        Ok(())
    }

    pub fn remove(&mut self, alias: &str) -> Option<ModelId> {
        self.0.remove(alias)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &ModelId)> {
        self.0.iter()
    }

    /// Aliases that point at `model`.
    pub fn aliases_for(&self, model: &ModelId) -> Vec<String> {
        self.0
            .iter()
            .filter(|(_, m)| *m == model)
            .map(|(a, _)| a.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_point_at_apple() {
        let t = AliasTable::default();
        assert_eq!(t.get("tts").unwrap().as_str(), APPLE_TTS);
        assert_eq!(t.get("stt").unwrap().as_str(), APPLE_STT);
        assert_eq!(t.get("realtime-stt").unwrap().as_str(), APPLE_STT);
    }

    #[test]
    fn json_roundtrip_and_validation() {
        let mut t = AliasTable::default();
        t.set(
            "tts",
            ModelId::parse("elevenlabs/eleven_flash_v2_5").unwrap(),
        )
        .unwrap();
        assert!(t
            .set("a/b", ModelId::parse("apple/system").unwrap())
            .is_err());
        let json = serde_json::to_string(&t).unwrap();
        let back: AliasTable = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }
}
