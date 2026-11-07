//! Finnhub: календар звітності та галузь компанії.

use std::time::Duration;

use async_trait::async_trait;
use chrono::NaiveDate;
use fond_core::error::DataError;
use fond_core::traits::EarningsCalendar;
use fond_core::types::Symbol;
use reqwest::Method;
use serde::Deserialize;
use url::Url;

use crate::http::{
    RateLimiter, RawError, RetryPolicy, Secret, http_client, join_url, send, with_retry,
};

#[derive(Debug, Deserialize)]
struct WireRow {
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireCalendar {
    #[serde(default, rename = "earningsCalendar")]
    earnings_calendar: Option<Vec<WireRow>>,
}

#[derive(Debug, Deserialize)]
struct WireProfile {
    #[serde(default, rename = "finnhubIndustry")]
    industry: Option<String>,
}

pub struct FinnhubClient {
    http: reqwest::Client,
    base: Url,
    key: Secret,
    limiter: RateLimiter,
    retry: RetryPolicy,
}

fn map_error(e: RawError) -> DataError {
    match e {
        RawError::Network(m) => DataError::Transient(m),
        RawError::Status(s, _) => {
            let code = s.as_u16();
            match code {
                401 => DataError::Unauthorized,
                403 => DataError::NotAvailable("premium".into()),
                429 | 500..=599 => DataError::Transient(format!("http {code}")),
                _ => DataError::Other(format!("http {code}")),
            }
        }
    }
}

impl FinnhubClient {
    pub fn new(base: Url, key: Secret) -> Self {
        Self {
            http: http_client(Duration::from_secs(15)),
            base,
            key,
            limiter: RateLimiter::new(55),
            retry: RetryPolicy::default(),
        }
    }

    async fn get_text(&self, path: &str, query: &[(&str, &str)]) -> Result<String, DataError> {
        let url = join_url(&self.base, path, query).map_err(DataError::Other)?;
        with_retry(&self.retry, || async {
            let headers = [("X-Finnhub-Token", self.key.expose())];
            send(
                &self.http,
                &self.limiter,
                Method::GET,
                url.clone(),
                &headers,
                None,
            )
            .await
            .map_err(map_error)
        })
        .await
    }

    /// Галузь за `/stock/profile2`; порожній профіль або порожній рядок дають `None`.
    pub async fn industry(&self, s: &Symbol) -> Result<Option<String>, DataError> {
        let text = self
            .get_text("stock/profile2", &[("symbol", s.as_str())])
            .await?;
        let p: WireProfile = serde_json::from_str(&text)
            .map_err(|e| DataError::Other(format!("unparseable finnhub profile: {e}")))?;
        Ok(p.industry
            .map(|i| i.trim().to_string())
            .filter(|i| !i.is_empty()))
    }
}

#[async_trait]
impl EarningsCalendar for FinnhubClient {
    async fn next_earnings(
        &self,
        symbol: &Symbol,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Option<NaiveDate>, DataError> {
        let (f, t) = (from.to_string(), to.to_string());
        let text = self
            .get_text(
                "calendar/earnings",
                &[("from", &f), ("to", &t), ("symbol", symbol.as_str())],
            )
            .await?;
        let cal: WireCalendar = serde_json::from_str(&text)
            .map_err(|e| DataError::Other(format!("unparseable finnhub calendar: {e}")))?;
        Ok(cal
            .earnings_calendar
            .unwrap_or_default()
            .into_iter()
            .filter(|r| {
                r.symbol
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case(symbol.as_str()))
            })
            .filter_map(|r| {
                r.date
                    .and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
            })
            .filter(|d| *d >= from && *d <= to)
            .min())
    }
}
