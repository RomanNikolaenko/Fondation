//! `MarketData` і `NewsSource` поверх REST API Alpaca (дані: SIP для денних барів, IEX для
//! останньої ціни).

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, SecondsFormat, Utc};
use fond_core::calendar::{et_to_utc, utc_to_et};
use fond_core::error::DataError;
use fond_core::traits::{AssetInfo, MarketData, NewsItem, NewsSource, Session};
use fond_core::types::{Bar, Symbol};
use reqwest::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

use super::trading::{AlpacaClient, RawError, join_url, message_text};

const BATCH: usize = 100;
const NEWS_MAX_LIMIT: usize = 50;
/// На безкоштовному плані SIP дозволено лише для запитів, чий `end` старший за 15 хвилин.
const SIP_DELAY_MINUTES: i64 = 16;

#[derive(Debug, Deserialize)]
struct WireBar {
    t: DateTime<Utc>,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    v: f64,
}

#[derive(Debug, Deserialize)]
struct WireBars {
    #[serde(default)]
    bars: Option<HashMap<String, Vec<WireBar>>>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireSession {
    date: NaiveDate,
    open: String,
    close: String,
}

#[derive(Debug, Deserialize)]
struct WireTrade {
    p: f64,
}

#[derive(Debug, Deserialize)]
struct WireLatestTrade {
    #[serde(default)]
    trade: Option<WireTrade>,
}

#[derive(Debug, Deserialize)]
struct WireAsset {
    symbol: String,
    tradable: bool,
    status: String,
}

#[derive(Debug, Deserialize)]
struct WireNewsItem {
    headline: String,
    #[serde(default)]
    summary: Option<String>,
    created_at: DateTime<Utc>,
    #[serde(default)]
    source: String,
}

#[derive(Debug, Deserialize)]
struct WireNews {
    #[serde(default)]
    news: Vec<WireNewsItem>,
}

fn map_data_error(e: RawError, news: bool) -> DataError {
    match e {
        RawError::Network(m) => DataError::Transient(m),
        RawError::Status(s, text) => {
            let code = s.as_u16();
            match code {
                403 if news => DataError::NotAvailable("news not available on this plan".into()),
                401 | 403 => DataError::Unauthorized,
                429 | 500..=599 => {
                    DataError::Transient(format!("http {code}: {}", message_text(&text)))
                }
                _ => DataError::Other(format!("http {code}: {}", message_text(&text))),
            }
        }
    }
}

fn parse<T: DeserializeOwned>(text: &str) -> Result<T, DataError> {
    serde_json::from_str(text)
        .map_err(|e| DataError::Other(format!("unparseable data response: {e}")))
}

fn parse_hm(s: &str) -> Result<NaiveTime, DataError> {
    NaiveTime::parse_from_str(s, "%H:%M")
        .map_err(|_| DataError::Other(format!("unexpected session time: {s:?}")))
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl AlpacaClient {
    async fn get_data<T: DeserializeOwned>(
        &self,
        base: &Url,
        path: &str,
        query: &[(&str, &str)],
        news: bool,
    ) -> Result<T, DataError> {
        let url = join_url(base, path, query).map_err(DataError::Other)?;
        let text = self
            .get_raw(&url)
            .await
            .map_err(|e| map_data_error(e, news))?;
        parse(&text)
    }
}

#[async_trait]
impl MarketData for AlpacaClient {
    async fn daily_bars(
        &self,
        symbols: &[Symbol],
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<HashMap<Symbol, Vec<Bar>>, DataError> {
        let start = from.and_time(NaiveTime::MIN).and_utc();
        let close = et_to_utc(to, NaiveTime::from_hms_opt(20, 0, 0).expect("valid time"));
        let end = close.min(self.now() - Duration::minutes(SIP_DELAY_MINUTES));
        if end < start {
            return Ok(HashMap::new());
        }
        let (start, end) = (rfc3339(start), rfc3339(end));

        let mut sorted: Vec<&Symbol> = symbols.iter().collect();
        sorted.sort();
        sorted.dedup();

        let mut merged: HashMap<Symbol, BTreeMap<NaiveDate, Bar>> = HashMap::new();
        for chunk in sorted.chunks(BATCH) {
            let joined = chunk
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(",");
            let mut token: Option<String> = None;
            loop {
                let mut q: Vec<(&str, &str)> = vec![
                    ("symbols", &joined),
                    ("timeframe", "1Day"),
                    ("start", &start),
                    ("end", &end),
                    ("adjustment", "all"),
                    ("feed", "sip"),
                    ("limit", "10000"),
                ];
                if let Some(t) = token.as_deref() {
                    q.push(("page_token", t));
                }
                let page: WireBars = self
                    .get_data(self.data_base(), "v2/stocks/bars", &q, false)
                    .await?;
                for (sym, bars) in page.bars.unwrap_or_default() {
                    let slot = merged.entry(Symbol::new(&sym)).or_default();
                    for b in bars {
                        let date = utc_to_et(b.t).date_naive();
                        slot.entry(date).or_insert(Bar {
                            date,
                            open: b.o,
                            high: b.h,
                            low: b.l,
                            close: b.c,
                            volume: b.v,
                        });
                    }
                }
                match page.next_page_token {
                    Some(t) if !t.is_empty() => token = Some(t),
                    _ => break,
                }
            }
        }
        Ok(merged
            .into_iter()
            .filter(|(_, m)| !m.is_empty())
            .map(|(s, m)| (s, m.into_values().collect()))
            .collect())
    }

    async fn calendar(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<Session>, DataError> {
        let (f, t) = (from.to_string(), to.to_string());
        let w: Vec<WireSession> = self
            .get_data(
                self.trading_base(),
                "v2/calendar",
                &[("start", &f), ("end", &t)],
                false,
            )
            .await?;
        w.into_iter()
            .map(|s| {
                Ok(Session {
                    date: s.date,
                    open: parse_hm(&s.open)?,
                    close: parse_hm(&s.close)?,
                })
            })
            .collect()
    }

    async fn latest_trade(&self, symbol: &Symbol) -> Result<Option<f64>, DataError> {
        let mut url = self.data_base().clone();
        url.path_segments_mut()
            .map_err(|()| DataError::Other("data base url cannot be a base".into()))?
            .pop_if_empty()
            .extend(["v2", "stocks", symbol.as_str(), "trades", "latest"]);
        url.query_pairs_mut().append_pair("feed", "iex");
        match self.get_raw(&url).await {
            Ok(text) => Ok(parse::<WireLatestTrade>(&text)?.trade.map(|t| t.p)),
            Err(RawError::Status(StatusCode::NOT_FOUND, _)) => Ok(None),
            Err(e) => Err(map_data_error(e, false)),
        }
    }

    async fn assets(&self) -> Result<Vec<AssetInfo>, DataError> {
        let w: Vec<WireAsset> = self
            .get_data(
                self.trading_base(),
                "v2/assets",
                &[("status", "active"), ("asset_class", "us_equity")],
                false,
            )
            .await?;
        Ok(w.into_iter()
            .map(|a| AssetInfo {
                symbol: Symbol::new(&a.symbol),
                tradable: a.tradable,
                active: a.status == "active",
            })
            .collect())
    }
}

#[async_trait]
impl NewsSource for AlpacaClient {
    async fn news(
        &self,
        symbol: &Symbol,
        since: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<NewsItem>, DataError> {
        let start = rfc3339(since);
        let limit = limit.min(NEWS_MAX_LIMIT).to_string();
        let w: WireNews = self
            .get_data(
                self.data_base(),
                "v1beta1/news",
                &[
                    ("symbols", symbol.as_str()),
                    ("start", &start),
                    ("limit", &limit),
                    ("sort", "desc"),
                ],
                true,
            )
            .await?;
        Ok(w.news
            .into_iter()
            .map(|n| NewsItem {
                headline: n.headline,
                summary: n.summary.unwrap_or_default(),
                created_at: n.created_at,
                source: n.source,
            })
            .collect())
    }
}
