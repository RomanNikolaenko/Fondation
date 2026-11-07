//! Імена записів у Windows Credential Manager.

use serde::{Deserialize, Serialize};

/// Назва сервісу в сховищі секретів.
pub const KEYRING_SERVICE: &str = "Fond";

/// Секрет, що зберігається в Credential Manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum SecretKey {
    AlpacaKeyId,
    AlpacaSecret,
    AnthropicApiKey,
    FinnhubApiKey,
    EngineApiToken,
}

impl SecretKey {
    /// Ім'я облікового запису (user) в Credential Manager.
    pub fn account(self) -> &'static str {
        match self {
            Self::AlpacaKeyId => "alpaca-key-id",
            Self::AlpacaSecret => "alpaca-secret",
            Self::AnthropicApiKey => "anthropic-api-key",
            Self::FinnhubApiKey => "finnhub-api-key",
            Self::EngineApiToken => "engine-api-token",
        }
    }

    pub fn all() -> [SecretKey; 5] {
        [
            Self::AlpacaKeyId,
            Self::AlpacaSecret,
            Self::AnthropicApiKey,
            Self::FinnhubApiKey,
            Self::EngineApiToken,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_are_exact() {
        let names: Vec<_> = SecretKey::all().iter().map(|k| k.account()).collect();
        assert_eq!(
            names,
            [
                "alpaca-key-id",
                "alpaca-secret",
                "anthropic-api-key",
                "finnhub-api-key",
                "engine-api-token"
            ]
        );
        assert_eq!(KEYRING_SERVICE, "Fond");
    }

    #[test]
    fn serde_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&SecretKey::AlpacaKeyId).unwrap(),
            "\"alpaca_key_id\""
        );
    }
}
