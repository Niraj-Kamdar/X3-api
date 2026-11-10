use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// What a model does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Task {
    Tts,
    Stt,
}

impl Task {
    pub fn as_str(self) -> &'static str {
        match self {
            Task::Tts => "tts",
            Task::Stt => "stt",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "tts" => Ok(Task::Tts),
            "stt" => Ok(Task::Stt),
            other => Err(Error::InvalidRequest(format!(
                "unknown task '{other}' (expected tts or stt)"
            ))),
        }
    }
}

impl fmt::Display for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Namespaced model identifier: `<provider>/<name>`, e.g. `apple/system`,
/// `elevenlabs/eleven_multilingual_v2`, `cartesia/sonic-3`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelId(String);

impl ModelId {
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        match s.split_once('/') {
            Some((p, n))
                if !p.is_empty()
                    && !n.is_empty()
                    && p.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') =>
            {
                Ok(Self(s.to_string()))
            }
            _ => Err(Error::UnknownModel(s.to_string())),
        }
    }

    pub fn provider(&self) -> &str {
        self.0.split_once('/').map(|(p, _)| p).unwrap_or("")
    }

    pub fn name(&self) -> &str {
        self.0.split_once('/').map(|(_, n)| n).unwrap_or("")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ModelId {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

impl From<ModelId> for String {
    fn from(value: ModelId) -> Self {
        value.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Billing unit used for cost estimates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingUnit {
    /// Input characters (TTS).
    Characters,
    /// Seconds of input audio (STT).
    Seconds,
}

impl PricingUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            PricingUnit::Characters => "characters",
            PricingUnit::Seconds => "seconds",
        }
    }
}

/// Estimated price, in USD per unit. These are *estimates* used for
/// budgets; the provider's invoice is authoritative.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub unit: PricingUnit,
    pub usd_per_unit: f64,
}

impl Pricing {
    pub const FREE_CHARS: Pricing = Pricing {
        unit: PricingUnit::Characters,
        usd_per_unit: 0.0,
    };
    pub const FREE_SECONDS: Pricing = Pricing {
        unit: PricingUnit::Seconds,
        usd_per_unit: 0.0,
    };

    pub fn per_1k_chars(usd: f64) -> Self {
        Self {
            unit: PricingUnit::Characters,
            usd_per_unit: usd / 1000.0,
        }
    }

    pub fn per_hour(usd: f64) -> Self {
        Self {
            unit: PricingUnit::Seconds,
            usd_per_unit: usd / 3600.0,
        }
    }

    pub fn estimate(&self, units: f64) -> f64 {
        self.usd_per_unit * units
    }
}

/// Catalog entry describing a model an engine can serve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: ModelId,
    pub task: Task,
    pub display_name: String,
    /// True when calls leave the machine and cost money.
    pub cloud: bool,
    /// False when the model cannot run here (e.g. Apple speech on Linux,
    /// or a cloud provider with no key configured).
    pub available: bool,
    pub pricing: Pricing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_ids() {
        let id = ModelId::parse("elevenlabs/eleven_multilingual_v2").unwrap();
        assert_eq!(id.provider(), "elevenlabs");
        assert_eq!(id.name(), "eleven_multilingual_v2");
        assert!(ModelId::parse("tts").is_err());
        assert!(ModelId::parse("/x").is_err());
        assert!(ModelId::parse("Apple/system").is_err());
    }

    #[test]
    fn pricing_estimates() {
        let p = Pricing::per_1k_chars(0.30);
        assert!((p.estimate(2000.0) - 0.60).abs() < 1e-9);
        let p = Pricing::per_hour(0.36);
        assert!((p.estimate(600.0) - 0.06).abs() < 1e-9);
    }
}
