//! SQLite (`sqlx`), міграції, репозиторії.

mod bars;
mod cycle;
mod events;
mod state;
mod universe;

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveTime, SecondsFormat, Utc};
use fond_core::intent::IntentStatus;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

pub use cycle::{StepName, StepStatus};
pub use events::{EventKind, EventQuery, NewEvent, StoredEvent};
pub use universe::{ETFS_CSV, SP500_CSV, UniverseEntry, UniverseSource, parse_universe_csv};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("помилка бази даних: {0}")]
    Db(#[from] sqlx::Error),
    #[error("помилка міграції: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("недопустимий перехід наміру: {from:?} -> {to:?}")]
    InvalidTransition {
        from: IntentStatus,
        to: IntentStatus,
    },
    #[error("пошкоджені дані: {0}")]
    Corrupt(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Обгортка над пулом SQLite; усі репозиторії — методи `Store`.
#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Відкриває (створює за потреби) файл БД: WAL, зовнішні ключі, міграції.
    pub async fn open(path: &Path) -> Result<Store> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        Self::migrated(pool).await
    }

    /// БД у пам'яті; один з'єднання, бо in-memory SQLite живе в межах з'єднання.
    pub async fn open_in_memory() -> Result<Store> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(opts)
            .await?;
        Self::migrated(pool).await
    }

    async fn migrated(pool: SqlitePool) -> Result<Store> {
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Store { pool })
    }

    /// Прямий доступ до пулу (для інших репозиторіїв і тестів).
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

pub(crate) fn fmt_date(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

pub(crate) fn parse_date(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| StoreError::Corrupt(format!("дата '{s}': {e}")))
}

pub(crate) fn fmt_time(t: NaiveTime) -> String {
    t.format("%H:%M:%S").to_string()
}

pub(crate) fn parse_time(s: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M:%S")
        .map_err(|e| StoreError::Corrupt(format!("час '{s}': {e}")))
}

pub(crate) fn fmt_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub(crate) fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| StoreError::Corrupt(format!("мітка часу '{s}': {e}")))
}
