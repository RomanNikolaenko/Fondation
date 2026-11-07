//! Спільний тестовий стенд для кроків циклу: фейкові порти та сховище в пам'яті.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Datelike, Duration, NaiveDate, NaiveTime};
use fond_core::calendar::et_to_utc;
use fond_core::error::DataError;
use fond_core::portfolio::{Position, TrackedPosition};
use fond_core::testkit::{
    FakeBroker, FakeClock, FakeEarnings, FakeJudge, FakeMarketData, FakeNews, bars_from_closes,
    uptrend_then_pullback,
};
use fond_core::traits::{AssetInfo, NewsJudgment, NewsVerdict, Session, Verdict};
use fond_core::types::{Bar, StrategyId, Symbol};
use fond_store::{Store, UniverseEntry, UniverseSource};

use crate::deps::{Deps, IndustryLookup};
use crate::events::EventBus;
use crate::health::BrokerHealth;

/// Кількість денних барів у кожного тікера стенда.
pub(crate) const N_BARS: usize = 300;

pub(crate) fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

pub(crate) fn sym(s: &str) -> Symbol {
    Symbol::new(s)
}

fn hm(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).unwrap()
}

/// Остання дата барів стенда: середа 2026-11-04 (після переходу на зимовий час).
pub(crate) fn last_date() -> NaiveDate {
    d(2026, 11, 4)
}

/// Перший будній день так, щоб `N_BARS` будніх днів закінчувались на `last_date()`.
pub(crate) fn start_date() -> NaiveDate {
    let mut x = last_date();
    let mut n = 1;
    while n < N_BARS {
        x -= Duration::days(1);
        if x.weekday().num_days_from_monday() < 5 {
            n += 1;
        }
    }
    x
}

/// Сесії-будні 09:30–16:00 у `[from, to]`.
pub(crate) fn weekday_sessions(from: NaiveDate, to: NaiveDate) -> Vec<Session> {
    from.iter_days()
        .take_while(|x| *x <= to)
        .filter(|x| x.weekday().num_days_from_monday() < 5)
        .map(|date| Session {
            date,
            open: hm(9, 30),
            close: hm(16, 0),
        })
        .collect()
}

/// Рівний ряд: close = 100, обсяг 1 млн.
pub(crate) fn flat_bars(start: NaiveDate, n: usize) -> Vec<Bar> {
    bars_from_closes(start, &vec![100.0; n], 1_000_000.0)
}

/// Спадний ряд: від 400 з кроком ×0.997 (close нижче SMA200).
pub(crate) fn falling_bars(start: NaiveDate, n: usize) -> Vec<Bar> {
    let closes: Vec<f64> = (0..n).map(|i| 400.0 * 0.997_f64.powi(i as i32)).collect();
    bars_from_closes(start, &closes, 1_000_000.0)
}

pub(crate) fn judgment(verdict: Verdict, reasons: &[&str]) -> NewsJudgment {
    NewsJudgment {
        verdict: NewsVerdict {
            verdict,
            reasons: reasons.iter().map(|s| s.to_string()).collect(),
            key_events: vec![],
        },
        model: "claude-haiku-5-5".into(),
        input_tokens: 2_000,
        output_tokens: 300,
    }
}

pub(crate) fn tracked(
    symbol: &str,
    strategy: &str,
    qty: u32,
    stop: f64,
    stop_order_id: Option<&str>,
) -> TrackedPosition {
    TrackedPosition {
        position: Position {
            symbol: sym(symbol),
            strategy: StrategyId::new(strategy),
            qty,
            entry_price: 100.0,
            entry_date: last_date() - Duration::days(3),
            stop_price: stop,
            take_profit: None,
            sector: "Technology".into(),
        },
        stop_order_id: stop_order_id.map(str::to_string),
        intent_id: format!("20261028-{strategy}-{symbol}-entry"),
    }
}

/// Вставки в таблицю `table` завершуються помилкою SQLite (тригер); читання працюють.
pub(crate) async fn fail_inserts(store: &Store, table: &str) {
    let sql = format!(
        "CREATE TRIGGER fail_insert_{table} BEFORE INSERT ON {table} \
         BEGIN SELECT RAISE(ABORT, 'test: inserts into {table} are disabled'); END"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .execute(store.pool())
        .await
        .unwrap();
}

/// Галузь завжди «Technology».
pub(crate) struct FakeIndustry;

#[async_trait]
impl IndustryLookup for FakeIndustry {
    async fn industry(&self, _s: &Symbol) -> Result<Option<String>, DataError> {
        Ok(Some("Technology".into()))
    }
}

pub(crate) struct Handles {
    pub broker: Arc<FakeBroker>,
    pub data: Arc<FakeMarketData>,
    pub news: Arc<FakeNews>,
    pub earnings: Arc<FakeEarnings>,
    pub judge: Arc<FakeJudge>,
    pub clock: Arc<FakeClock>,
    start: NaiveDate,
    last: NaiveDate,
}

impl Handles {
    /// Дата останнього бару (день аналізу D).
    pub fn last_date(&self) -> NaiveDate {
        self.last
    }

    pub fn start_date(&self) -> NaiveDate {
        self.start
    }
}

/// Сховище в пам'яті з universe AAPL, MSFT, SPY та сесіями-буднями; годинник — D 16:30 ET.
/// Бари: SPY — чистий підйом, AAPL — підйом із відкатом (сигнал `trend_pullback`), MSFT — рівний.
/// Капітал брокера 100 000, суддя відповідає `Allow`, звітів немає.
pub(crate) async fn fixture() -> (Deps, Handles) {
    let start = start_date();
    let last = last_date();
    let clock = Arc::new(FakeClock::new(et_to_utc(last, hm(16, 30))));

    let store = Store::open_in_memory().await.unwrap();
    for (s, name, sector) in [
        ("AAPL", "Apple Inc.", "Technology"),
        ("MSFT", "Microsoft Corp.", "Technology"),
        ("SPY", "SPDR S&P 500 ETF Trust", "Broad Market ETF"),
    ] {
        store
            .add_manual(&UniverseEntry {
                symbol: sym(s),
                name: name.into(),
                sector: sector.into(),
                source: UniverseSource::Manual,
                excluded: false,
            })
            .await
            .unwrap();
    }
    let sessions = weekday_sessions(start, last + Duration::days(120));
    store.upsert_sessions(&sessions).await.unwrap();

    let data = Arc::new(FakeMarketData::default());
    data.set_sessions(sessions);
    data.set_assets(
        ["AAPL", "MSFT", "SPY"]
            .into_iter()
            .map(|s| AssetInfo {
                symbol: sym(s),
                tradable: true,
                active: true,
            })
            .collect(),
    );
    data.set_bars(&sym("SPY"), uptrend_then_pullback(start, N_BARS, 0));
    data.set_bars(&sym("AAPL"), uptrend_then_pullback(start, N_BARS - 3, 3));
    data.set_bars(&sym("MSFT"), flat_bars(start, N_BARS));

    let broker = Arc::new(FakeBroker::with_clock(clock.clone()));
    let news = Arc::new(FakeNews::default());
    let earnings = Arc::new(FakeEarnings::default());
    let judge = Arc::new(FakeJudge::new(Ok(judgment(Verdict::Allow, &[]))));

    let deps = Deps {
        broker: broker.clone(),
        data: data.clone(),
        news: news.clone(),
        earnings: earnings.clone(),
        judge: judge.clone(),
        industry: Arc::new(FakeIndustry),
        store,
        clock: clock.clone(),
        bus: EventBus::new(1024),
        health: Arc::new(BrokerHealth::default()),
    };
    let handles = Handles {
        broker,
        data,
        news,
        earnings,
        judge,
        clock,
        start,
        last,
    };
    (deps, handles)
}
