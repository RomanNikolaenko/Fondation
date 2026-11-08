//! Залежності кроків циклу: порти, сховище, годинник, шина подій — і фабрика, що збирає
//! їх із секретів і конфігурації.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use fond_adapters::alpaca::{AlpacaClient, AlpacaConfig};
use fond_adapters::claude::{ClaudeConfig, ClaudeJudge};
use fond_adapters::finnhub::FinnhubClient;
use fond_adapters::http::Secret;
use fond_adapters::secrets::SecretStore;
use fond_api::config::EngineConfig;
use fond_api::dto::{CheckDto, VerifyResultDto};
use fond_core::error::{BrokerError, DataError, JudgeError};
use fond_core::keys::SecretKey;
use fond_core::traits::{
    Broker, Clock, EarningsCalendar, MarketData, NewsJudge, NewsJudgeRequest, NewsJudgment,
    NewsSource,
};
use fond_core::types::Symbol;
use fond_store::Store;
use url::Url;

use crate::events::EventBus;
use crate::health::BrokerHealth;

/// Галузь компанії (сектор для тікерів, доданих вручну).
#[async_trait]
pub trait IndustryLookup: Send + Sync {
    async fn industry(&self, s: &Symbol) -> Result<Option<String>, DataError>;
}

#[derive(Clone)]
pub struct Deps {
    pub broker: Arc<dyn Broker>,
    pub data: Arc<dyn MarketData>,
    pub news: Arc<dyn NewsSource>,
    pub earnings: Arc<dyn EarningsCalendar>,
    pub judge: Arc<dyn NewsJudge>,
    pub industry: Arc<dyn IndustryLookup>,
    pub store: Store,
    pub clock: Arc<dyn Clock>,
    pub bus: EventBus,
    pub health: Arc<BrokerHealth>,
}

/// Чому залежності не зібрано.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetupError {
    /// Потрібних секретів немає у сховищі (порядок — як у `SecretKey::all()`).
    #[error("не задано секрети: {}", names(.0))]
    MissingSecrets(Vec<SecretKey>),
    #[error("{0}")]
    Other(String),
}

fn names(keys: &[SecretKey]) -> String {
    keys.iter()
        .map(|k| k.account())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Збирає `Deps` для поточної конфігурації.
pub trait DepsFactory: Send + Sync {
    fn build(&self, cfg: &EngineConfig) -> Result<Deps, SetupError>;
}

/// Справжні клієнти: Alpaca (paper) — брокер, ринкові дані й новини; Finnhub — звітність і
/// галузь (лише з увімкненим фільтром звітності); Claude — суддя новин (лише з увімкненим
/// фільтром новин). Вимкнений фільтр отримує заглушку, яка завжди повертає помилку.
pub struct LiveDepsFactory {
    pub secrets: Arc<dyn SecretStore>,
    pub store: Store,
    pub clock: Arc<dyn Clock>,
    pub bus: EventBus,
    pub health: Arc<BrokerHealth>,
}

const FINNHUB_BASE: &str = "https://finnhub.io/api/v1";
const ANTHROPIC_BASE: &str = "https://api.anthropic.com";

fn static_url(s: &str) -> Url {
    Url::parse(s).expect("статична адреса")
}

impl LiveDepsFactory {
    /// Секрет `k`, якщо він потрібен (`needed`); відсутній чи порожній додається в `missing`.
    fn secret(
        &self,
        k: SecretKey,
        needed: bool,
        missing: &mut Vec<SecretKey>,
    ) -> Result<Option<Secret>, SetupError> {
        if !needed {
            return Ok(None);
        }
        match self.secrets.get(k) {
            Ok(Some(v)) if !v.expose().trim().is_empty() => Ok(Some(v)),
            Ok(_) => {
                missing.push(k);
                Ok(None)
            }
            Err(e) => Err(SetupError::Other(format!(
                "не вдалося прочитати секрет {}: {e}",
                k.account()
            ))),
        }
    }
}

impl DepsFactory for LiveDepsFactory {
    fn build(&self, cfg: &EngineConfig) -> Result<Deps, SetupError> {
        let mut missing = Vec::new();
        let key_id = self.secret(SecretKey::AlpacaKeyId, true, &mut missing)?;
        let secret = self.secret(SecretKey::AlpacaSecret, true, &mut missing)?;
        let anthropic = self.secret(
            SecretKey::AnthropicApiKey,
            cfg.news_filter.enabled,
            &mut missing,
        )?;
        let finnhub = self.secret(
            SecretKey::FinnhubApiKey,
            cfg.earnings_filter.enabled,
            &mut missing,
        )?;
        let (Some(key_id), Some(secret), true) = (key_id, secret, missing.is_empty()) else {
            return Err(SetupError::MissingSecrets(missing));
        };

        let alpaca = Arc::new(AlpacaClient::new(
            AlpacaConfig::paper(key_id, secret),
            self.clock.clone(),
        ));
        let (earnings, industry): (Arc<dyn EarningsCalendar>, Arc<dyn IndustryLookup>) =
            match finnhub {
                Some(key) => {
                    let f = Arc::new(FinnhubClient::new(static_url(FINNHUB_BASE), key));
                    (f.clone(), f)
                }
                None => (Arc::new(EarningsOff), Arc::new(EarningsOff)),
            };
        let judge: Arc<dyn NewsJudge> = match anthropic {
            Some(api_key) => Arc::new(ClaudeJudge::new(ClaudeConfig {
                base: static_url(ANTHROPIC_BASE),
                api_key,
                model: cfg.news_filter.model.clone(),
                effort: cfg.news_filter.effort.clone(),
            })),
            None => Arc::new(NewsOff),
        };
        Ok(Deps {
            broker: alpaca.clone(),
            data: alpaca.clone(),
            news: alpaca,
            earnings,
            judge,
            industry,
            store: self.store.clone(),
            clock: self.clock.clone(),
            bus: self.bus.clone(),
            health: self.health.clone(),
        })
    }
}

#[async_trait]
impl IndustryLookup for FinnhubClient {
    async fn industry(&self, s: &Symbol) -> Result<Option<String>, DataError> {
        FinnhubClient::industry(self, s).await
    }
}

/// Заглушка Finnhub, коли фільтр звітності вимкнено (аналіз її тоді не викликає).
struct EarningsOff;

const EARNINGS_OFF: &str = "фільтр звітності вимкнено, Finnhub не використовується";

#[async_trait]
impl EarningsCalendar for EarningsOff {
    async fn next_earnings(
        &self,
        _symbol: &Symbol,
        _from: NaiveDate,
        _to: NaiveDate,
    ) -> Result<Option<NaiveDate>, DataError> {
        Err(DataError::NotAvailable(EARNINGS_OFF.into()))
    }
}

#[async_trait]
impl IndustryLookup for EarningsOff {
    async fn industry(&self, _s: &Symbol) -> Result<Option<String>, DataError> {
        Err(DataError::NotAvailable(EARNINGS_OFF.into()))
    }
}

/// Заглушка судді, коли фільтр новин вимкнено (аналіз її тоді не викликає).
struct NewsOff;

#[async_trait]
impl NewsJudge for NewsOff {
    async fn judge(&self, _req: &NewsJudgeRequest) -> Result<NewsJudgment, JudgeError> {
        Err(JudgeError::Other("фільтр новин вимкнено".into()))
    }
}

const NO_KEY: &str = "немає ключа";
const KEY_REJECTED: &str = "ключ відхилено (401)";

fn passed() -> CheckDto {
    CheckDto {
        ok: true,
        message: None,
    }
}

fn failed(message: impl Into<String>) -> CheckDto {
    CheckDto {
        ok: false,
        message: Some(message.into()),
    }
}

/// Непорожній ключ або `None`; збій сховища — текст без значення ключа.
fn stored_key(secrets: &dyn SecretStore, k: SecretKey) -> Result<Option<Secret>, String> {
    match secrets.get(k) {
        Ok(Some(v)) if !v.expose().trim().is_empty() => Ok(Some(v)),
        Ok(_) => Ok(None),
        Err(e) => Err(format!("не вдалося прочитати ключ: {e}")),
    }
}

/// Перевірка збережених ключів: Alpaca (paper) — `account()`, Finnhub — `industry("AAPL")`,
/// Claude — `verify_key()`. Сервіс без ключа не викликається («немає ключа»). Повідомлення
/// описують лише тип помилки, значень ключів у них немає.
pub async fn verify_secrets(
    secrets: &dyn SecretStore,
    clock: Arc<dyn Clock>,
    cfg: &EngineConfig,
) -> VerifyResultDto {
    let alpaca = match (
        stored_key(secrets, SecretKey::AlpacaKeyId),
        stored_key(secrets, SecretKey::AlpacaSecret),
    ) {
        (Err(e), _) | (_, Err(e)) => failed(e),
        (Ok(Some(key_id)), Ok(Some(secret))) => {
            let client = AlpacaClient::new(AlpacaConfig::paper(key_id, secret), clock);
            match client.account().await {
                Ok(_) => passed(),
                Err(BrokerError::Unauthorized) => failed(KEY_REJECTED),
                Err(e) => failed(e.to_string()),
            }
        }
        _ => failed(NO_KEY),
    };
    let finnhub = match stored_key(secrets, SecretKey::FinnhubApiKey) {
        Err(e) => failed(e),
        Ok(None) => failed(NO_KEY),
        Ok(Some(key)) => {
            let client = FinnhubClient::new(static_url(FINNHUB_BASE), key);
            match client.industry(&Symbol::new("AAPL")).await {
                Ok(_) => passed(),
                Err(DataError::Unauthorized) => failed(KEY_REJECTED),
                Err(e) => failed(e.to_string()),
            }
        }
    };
    let anthropic = match stored_key(secrets, SecretKey::AnthropicApiKey) {
        Err(e) => failed(e),
        Ok(None) => failed(NO_KEY),
        Ok(Some(api_key)) => {
            let judge = ClaudeJudge::new(ClaudeConfig {
                base: static_url(ANTHROPIC_BASE),
                api_key,
                model: cfg.news_filter.model.clone(),
                effort: cfg.news_filter.effort.clone(),
            });
            match judge.verify_key().await {
                Ok(()) => passed(),
                Err(JudgeError::Unauthorized) => failed(KEY_REJECTED),
                Err(e) => failed(e.to_string()),
            }
        }
    };
    VerifyResultDto {
        alpaca,
        finnhub,
        anthropic,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use fond_adapters::secrets::MemorySecretStore;
    use fond_core::testkit::FakeClock;

    use super::*;

    async fn factory(secrets: Arc<MemorySecretStore>) -> LiveDepsFactory {
        LiveDepsFactory {
            secrets,
            store: Store::open_in_memory().await.unwrap(),
            clock: Arc::new(FakeClock::new(
                Utc.with_ymd_and_hms(2026, 11, 4, 21, 31, 0).unwrap(),
            )),
            bus: EventBus::new(4),
            health: Arc::new(BrokerHealth::default()),
        }
    }

    fn cfg(news: bool, earnings: bool) -> EngineConfig {
        let mut c = EngineConfig::default();
        c.news_filter.enabled = news;
        c.earnings_filter.enabled = earnings;
        c
    }

    fn missing(f: &LiveDepsFactory, c: &EngineConfig) -> Option<SetupError> {
        f.build(c).err()
    }

    #[tokio::test]
    async fn live_factory_requires_filter_secrets_only_when_enabled() {
        use SecretKey::*;
        let secrets = Arc::new(MemorySecretStore::default());
        let f = factory(secrets.clone()).await;

        assert_eq!(
            missing(&f, &cfg(true, true)),
            Some(SetupError::MissingSecrets(vec![
                AlpacaKeyId,
                AlpacaSecret,
                AnthropicApiKey,
                FinnhubApiKey
            ]))
        );
        assert_eq!(
            missing(&f, &cfg(false, false)),
            Some(SetupError::MissingSecrets(vec![AlpacaKeyId, AlpacaSecret]))
        );

        secrets.set(AlpacaKeyId, "PKTEST1234").unwrap();
        // Порожнє значення — те саме, що відсутнє.
        secrets.set(AlpacaSecret, "  ").unwrap();
        assert_eq!(
            missing(&f, &cfg(false, false)),
            Some(SetupError::MissingSecrets(vec![AlpacaSecret]))
        );
        secrets.set(AlpacaSecret, "secret-5678").unwrap();
        assert!(f.build(&cfg(false, false)).is_ok());
        assert_eq!(
            missing(&f, &cfg(false, true)),
            Some(SetupError::MissingSecrets(vec![FinnhubApiKey]))
        );
        assert_eq!(
            missing(&f, &cfg(true, false)),
            Some(SetupError::MissingSecrets(vec![AnthropicApiKey]))
        );

        secrets.set(FinnhubApiKey, "finnhub-9012").unwrap();
        secrets.set(AnthropicApiKey, "sk-ant-3456").unwrap();
        assert!(f.build(&cfg(true, true)).is_ok());
    }

    #[tokio::test]
    async fn disabled_filters_get_stubs_that_always_fail() {
        let secrets = Arc::new(MemorySecretStore::default());
        secrets.set(SecretKey::AlpacaKeyId, "PKTEST1234").unwrap();
        secrets.set(SecretKey::AlpacaSecret, "secret-5678").unwrap();
        let f = factory(secrets).await;
        let Ok(d) = f.build(&cfg(false, false)) else {
            panic!("очікувались залежності");
        };
        let aapl = Symbol::new("AAPL");
        let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        assert!(matches!(
            d.earnings.next_earnings(&aapl, day, day).await,
            Err(DataError::NotAvailable(_))
        ));
        assert!(matches!(
            d.industry.industry(&aapl).await,
            Err(DataError::NotAvailable(_))
        ));
        // Спільні частини беруться з фабрики, а не створюються наново.
        f.health.record_err();
        assert_eq!(d.health.streak(), 1);
    }

    #[test]
    fn missing_secrets_message_names_accounts_only() {
        let e = SetupError::MissingSecrets(vec![SecretKey::AlpacaKeyId, SecretKey::AlpacaSecret]);
        assert_eq!(
            e.to_string(),
            "не задано секрети: alpaca-key-id, alpaca-secret"
        );
    }
}
