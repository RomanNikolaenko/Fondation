//! Сховища секретів: Windows Credential Manager та пам'ять (для тестів).

use crate::http::Secret;
use fond_core::keys::{KEYRING_SERVICE, SecretKey};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secret backend: {0}")]
    Backend(String),
}

pub trait SecretStore: Send + Sync {
    fn get(&self, k: SecretKey) -> Result<Option<Secret>, SecretError>;
    fn set(&self, k: SecretKey, v: &str) -> Result<(), SecretError>;
    fn delete(&self, k: SecretKey) -> Result<(), SecretError>;
}

/// Windows Credential Manager через `keyring`.
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    pub fn new() -> Self {
        Self::with_service(KEYRING_SERVICE)
    }

    /// Інший service (для тестів, що торкаються реального сховища).
    pub fn with_service(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }

    fn entry(&self, k: SecretKey) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, k.account()).map_err(backend)
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

fn backend(e: keyring::Error) -> SecretError {
    SecretError::Backend(e.to_string())
}

impl SecretStore for KeyringStore {
    fn get(&self, k: SecretKey) -> Result<Option<Secret>, SecretError> {
        match self.entry(k)?.get_password() {
            Ok(v) => Ok(Some(Secret::new(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(backend(e)),
        }
    }

    fn set(&self, k: SecretKey, v: &str) -> Result<(), SecretError> {
        self.entry(k)?.set_password(v).map_err(backend)
    }

    fn delete(&self, k: SecretKey) -> Result<(), SecretError> {
        match self.entry(k)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(backend(e)),
        }
    }
}

/// Сховище в пам'яті (тести).
#[derive(Default)]
pub struct MemorySecretStore {
    map: Mutex<HashMap<SecretKey, String>>,
}

impl MemorySecretStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SecretKey, String>> {
        self.map.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, k: SecretKey) -> Result<Option<Secret>, SecretError> {
        Ok(self.lock().get(&k).cloned().map(Secret::new))
    }

    fn set(&self, k: SecretKey, v: &str) -> Result<(), SecretError> {
        self.lock().insert(k, v.to_string());
        Ok(())
    }

    fn delete(&self, k: SecretKey) -> Result<(), SecretError> {
        self.lock().remove(&k);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_roundtrip() {
        let s = MemorySecretStore::default();
        assert!(s.get(SecretKey::FinnhubApiKey).unwrap().is_none());
        s.set(SecretKey::FinnhubApiKey, "abc123456").unwrap();
        assert_eq!(
            s.get(SecretKey::FinnhubApiKey).unwrap().unwrap().expose(),
            "abc123456"
        );
        assert!(s.get(SecretKey::AlpacaSecret).unwrap().is_none());
        s.set(SecretKey::FinnhubApiKey, "new").unwrap();
        assert_eq!(
            s.get(SecretKey::FinnhubApiKey).unwrap().unwrap().expose(),
            "new"
        );
        s.delete(SecretKey::FinnhubApiKey).unwrap();
        assert!(s.get(SecretKey::FinnhubApiKey).unwrap().is_none());
        s.delete(SecretKey::FinnhubApiKey).unwrap(); // повторне видалення — Ok
    }

    #[test]
    #[ignore = "touches Windows Credential Manager"]
    fn keyring_roundtrip() {
        let store = KeyringStore::with_service("Fond-test");
        let k = SecretKey::EngineApiToken;
        store.delete(k).unwrap();
        assert!(store.get(k).unwrap().is_none());
        store.set(k, "token-1234").unwrap();
        assert_eq!(store.get(k).unwrap().unwrap().expose(), "token-1234");
        store.delete(k).unwrap();
        assert!(store.get(k).unwrap().is_none());
        store.delete(k).unwrap();
    }
}
