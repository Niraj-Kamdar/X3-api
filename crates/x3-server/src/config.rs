//! Non-secret settings, persisted as JSON. Provider keys never live here.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use x3_core::{AliasTable, Budgets, ModelId, Pricing};

pub const DEFAULT_PORT: u16 = 47800;

/// macOS bundle identifier; also the Keychain service name and data dir
/// name. Placeholder until the owner picks the final ID.
pub const BUNDLE_ID: &str = "dev.x3.voicestudio";

/// Platform data dir: `~/Library/Application Support/<bundle id>` on macOS,
/// `$XDG_DATA_HOME/x3` (or `~/.local/share/x3`) elsewhere.
pub fn default_data_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support").join(BUNDLE_ID)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("x3")
    }
}

/// User-editable settings (`settings.json` in the data dir).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub aliases: AliasTable,
    #[serde(default)]
    pub budgets: Budgets,
    /// Per-model price overrides for cost estimates.
    #[serde(default)]
    pub pricing_overrides: BTreeMap<ModelId, Pricing>,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            aliases: AliasTable::default(),
            budgets: Budgets::default(),
            pricing_overrides: BTreeMap::new(),
        }
    }
}

/// Filesystem layout under the data dir
/// (`~/Library/Application Support/<bundle id>` on macOS).
#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub outputs_dir: PathBuf,
    pub db_path: PathBuf,
    pub settings_path: PathBuf,
}

impl Paths {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        Self {
            outputs_dir: data_dir.join("outputs"),
            db_path: data_dir.join("x3.sqlite"),
            settings_path: data_dir.join("settings.json"),
            data_dir,
        }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.outputs_dir)?;
        Ok(())
    }
}

impl Settings {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("x3-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        assert_eq!(Settings::load(&p).unwrap(), Settings::default());
        let mut s = Settings::default();
        s.budgets
            .provider_monthly_usd
            .insert("elevenlabs".into(), 5.0);
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p).unwrap(), s);
        // Partial files fill in defaults.
        std::fs::write(&p, b"{\"port\": 1234}").unwrap();
        let s = Settings::load(&p).unwrap();
        assert_eq!(s.port, 1234);
        assert_eq!(s.aliases, AliasTable::default());
        std::fs::remove_dir_all(dir).ok();
    }
}
