//! macOS Keychain backend (UNVERIFIED on Linux CI: compiled only on macOS).
//!
//! Uses `keyring` 3.x with the `apple-native` feature, which stores generic
//! passwords in the user's login keychain via `security-framework`.
//! Items are created by the app process, so the default Keychain ACL lets the
//! signed app read them without a prompt while other apps (including a
//! terminal running an agent) trigger a user prompt.
//!
//! TODO(mac): verify on a signed build that a second process (e.g.
//! `security find-generic-password -s <service> -a elevenlabs -w`) is
//! prompted rather than silently given the key, and decide whether to move
//! to the data-protection keychain (requires the keychain-access-groups
//! entitlement) for stronger binding to the app's signature.

use keyring::Entry;
use secrecy::{ExposeSecret, SecretString};

use crate::{validate, validate_key, Result, SecretStore, VaultError};

pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    /// `service` is the Keychain service name, normally the app bundle ID.
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }

    fn entry(&self, provider: &str) -> Result<Entry> {
        validate(provider)?;
        Entry::new(&self.service, provider).map_err(|e| VaultError::Backend(e.to_string()))
    }
}

impl SecretStore for KeychainStore {
    fn get(&self, provider: &str) -> Result<Option<SecretString>> {
        match self.entry(provider)?.get_password() {
            Ok(p) => Ok(Some(SecretString::from(p))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(VaultError::Backend(e.to_string())),
        }
    }

    fn set(&self, provider: &str, key: SecretString) -> Result<()> {
        validate_key(&key)?;
        self.entry(provider)?
            .set_password(key.expose_secret().trim())
            .map_err(|e| VaultError::Backend(e.to_string()))
    }

    fn delete(&self, provider: &str) -> Result<()> {
        match self.entry(provider)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(VaultError::Backend(e.to_string())),
        }
    }
}
