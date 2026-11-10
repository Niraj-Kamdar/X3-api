//! Secret storage for provider API keys.
//!
//! Rules enforced by the rest of the workspace (and by the API of this
//! crate):
//! - keys are held as [`SecretString`] and are only exposed on the request
//!   path that sends them to the provider;
//! - keys are never written to config files, SQLite, logs, HTTP responses or
//!   MCP results;
//! - UIs may show only a [`hint`] (last 4 characters).
//!
//! On macOS the [`KeychainStore`] keeps keys in the login Keychain. Other
//! platforms only have [`MemoryStore`] (tests and headless development).

use std::{collections::HashMap, sync::Mutex};

pub use secrecy::{ExposeSecret, SecretString};

#[cfg(target_os = "macos")]
mod keychain;
#[cfg(target_os = "macos")]
pub use keychain::KeychainStore;

/// Providers whose keys the vault manages. Keeping this a closed list means
/// no caller can use the vault as a general-purpose secret oracle.
pub const PROVIDERS: &[&str] = &["elevenlabs", "cartesia"];

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("unknown provider '{0}'")]
    UnknownProvider(String),
    #[error("empty key")]
    EmptyKey,
    #[error("secure storage error: {0}")]
    Backend(String),
}

pub type Result<T> = std::result::Result<T, VaultError>;

/// Storage for provider API keys.
pub trait SecretStore: Send + Sync {
    /// Fetch a key. `Ok(None)` when no key is stored.
    fn get(&self, provider: &str) -> Result<Option<SecretString>>;
    /// Store or replace a key.
    fn set(&self, provider: &str, key: SecretString) -> Result<()>;
    /// Remove a key. Removing a missing key is not an error.
    fn delete(&self, provider: &str) -> Result<()>;

    /// Last-4 hint for UIs, e.g. `…a1b2`. Never returns more.
    fn hint(&self, provider: &str) -> Result<Option<String>> {
        Ok(self.get(provider)?.map(|k| hint(&k)))
    }
}

/// Last four characters of a key, prefixed with an ellipsis. Keys shorter
/// than 12 characters show nothing but the ellipsis.
pub fn hint(key: &SecretString) -> String {
    let k = key.expose_secret();
    let n = k.chars().count();
    if n < 12 {
        return "…".to_string();
    }
    let last4: String = k.chars().skip(n - 4).collect();
    format!("…{last4}")
}

pub(crate) fn validate(provider: &str) -> Result<()> {
    if PROVIDERS.contains(&provider) {
        Ok(())
    } else {
        Err(VaultError::UnknownProvider(provider.to_string()))
    }
}

pub(crate) fn validate_key(key: &SecretString) -> Result<()> {
    if key.expose_secret().trim().is_empty() {
        Err(VaultError::EmptyKey)
    } else {
        Ok(())
    }
}

/// In-memory store for tests and headless development. Nothing touches disk.
#[derive(Default)]
pub struct MemoryStore {
    keys: Mutex<HashMap<String, SecretString>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, provider: &str) -> Result<Option<SecretString>> {
        validate(provider)?;
        Ok(self
            .keys
            .lock()
            .map_err(|_| VaultError::Backend("poisoned".into()))?
            .get(provider)
            .cloned())
    }

    fn set(&self, provider: &str, key: SecretString) -> Result<()> {
        validate(provider)?;
        validate_key(&key)?;
        let trimmed = SecretString::from(key.expose_secret().trim().to_string());
        self.keys
            .lock()
            .map_err(|_| VaultError::Backend("poisoned".into()))?
            .insert(provider.to_string(), trimmed);
        Ok(())
    }

    fn delete(&self, provider: &str) -> Result<()> {
        validate(provider)?;
        self.keys
            .lock()
            .map_err(|_| VaultError::Backend("poisoned".into()))?
            .remove(provider);
        Ok(())
    }
}

/// The platform's default store: Keychain on macOS, memory elsewhere.
pub fn default_store(service: &str) -> Box<dyn SecretStore> {
    #[cfg(target_os = "macos")]
    {
        Box::new(KeychainStore::new(service))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = service;
        Box::new(MemoryStore::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_roundtrip() {
        let s = MemoryStore::new();
        assert!(s.get("elevenlabs").unwrap().is_none());
        s.set(
            "elevenlabs",
            SecretString::from(" sk_test_1234567890abcd ".to_string()),
        )
        .unwrap();
        assert_eq!(
            s.get("elevenlabs").unwrap().unwrap().expose_secret(),
            "sk_test_1234567890abcd"
        );
        assert_eq!(s.hint("elevenlabs").unwrap().unwrap(), "…abcd");
        s.delete("elevenlabs").unwrap();
        assert!(s.get("elevenlabs").unwrap().is_none());
    }

    #[test]
    fn rejects_unknown_provider_and_empty_key() {
        let s = MemoryStore::new();
        assert!(matches!(
            s.set("openai", SecretString::from("x".to_string())),
            Err(VaultError::UnknownProvider(_))
        ));
        assert!(matches!(
            s.set("cartesia", SecretString::from("  ".to_string())),
            Err(VaultError::EmptyKey)
        ));
    }

    #[test]
    fn short_keys_reveal_nothing() {
        assert_eq!(hint(&SecretString::from("abcdef".to_string())), "…");
    }

    #[test]
    fn debug_does_not_leak() {
        let k = SecretString::from("sk_super_secret_value".to_string());
        assert!(!format!("{k:?}").contains("super_secret"));
    }
}
