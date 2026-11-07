//! Кроки щоденного циклу та спільні допоміжні функції: журнал, календар, блокування входів.

pub mod analysis;
pub mod filters;
pub mod refresh;
#[cfg(test)]
pub(crate) mod test_support;

use chrono::{Datelike, Duration, NaiveDate};
use fond_api::config::EngineConfig;
use fond_api::events::{EngineEventDto, JournalEventDto};
use fond_core::calendar::TradingCalendar;
use fond_core::error::{BrokerError, DataError};
use fond_core::risk::{daily_loss_breached, weekly_loss_breached};
use fond_core::types::{EngineMode, Symbol};
use fond_store::{EventKind, NewEvent, StoreError, StoredEvent};

use crate::deps::Deps;

/// Скільки календарних днів навколо дати береться з торгового календаря.
pub const CALENDAR_SPAN_DAYS: i64 = 40;

pub const KILLED_REASON: &str = "Аварійна зупинка";
pub const DAILY_LOSS_REASON: &str = "Денний ліміт збитку";
pub const WEEKLY_LOSS_REASON: &str = "Тижневий ліміт збитку";

#[derive(Debug, thiserror::Error)]
pub enum CycleError {
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("помилка брокера: {0}")]
    Broker(#[from] BrokerError),
    #[error("помилка ринкових даних: {0}")]
    Data(#[from] DataError),
    #[error("у календарі немає наступної торгової сесії")]
    NoCalendar,
    #[error("{0}")]
    Other(String),
}

pub fn journal_dto(e: &StoredEvent) -> JournalEventDto {
    JournalEventDto {
        id: e.id,
        at: e.at,
        kind: e.kind.as_str().to_string(),
        symbol: e.symbol.as_ref().map(Symbol::to_string),
        intent_id: e.intent_id.clone(),
        message: e.message.clone(),
        data: e.data.clone(),
    }
}

/// Записує подію в журнал (`events`) і публікує її на шину.
pub async fn journal(
    d: &Deps,
    kind: EventKind,
    symbol: Option<&Symbol>,
    intent_id: Option<&str>,
    message: impl Into<String>,
    data: serde_json::Value,
) -> Result<(), CycleError> {
    let e = NewEvent {
        kind,
        symbol: symbol.cloned(),
        intent_id: intent_id.map(str::to_string),
        message: message.into(),
        data,
    };
    let stored = d.store.append_event(d.clock.now(), &e).await?;
    d.bus.publish(EngineEventDto::Journal {
        event: journal_dto(&stored),
    });
    Ok(())
}

/// Календар зі сховища на ±40 днів навколо `around`.
pub async fn trading_calendar(d: &Deps, around: NaiveDate) -> Result<TradingCalendar, CycleError> {
    let span = Duration::days(CALENDAR_SPAN_DAYS);
    let sessions = d.store.sessions(around - span, around + span).await?;
    Ok(TradingCalendar::new(sessions))
}

/// Понеділок ISO-тижня дати.
pub fn week_monday(date: NaiveDate) -> NaiveDate {
    date - Duration::days(i64::from(date.weekday().num_days_from_monday()))
}

/// Ставить паузу, лише якщо рушій працює (аварійну зупинку не «понижує»).
/// Пише подію в журнал і публікує `AutoPaused`. `true`, якщо режим змінено.
pub async fn auto_pause(d: &Deps, reason: &str) -> Result<bool, CycleError> {
    if d.store.engine_mode().await? != EngineMode::Running {
        return Ok(false);
    }
    let mode = EngineMode::Paused {
        reason: reason.to_string(),
    };
    d.store.set_engine_mode(&mode, d.clock.now()).await?;
    journal(
        d,
        EventKind::Pause,
        None,
        None,
        format!("Автопауза: {reason}"),
        serde_json::json!({ "reason": reason }),
    )
    .await?;
    d.bus.publish(EngineEventDto::AutoPaused {
        reason: reason.to_string(),
    });
    Ok(true)
}

/// Чому нові входи заборонені (або `None`).
///
/// Порядок: аварійна зупинка, пауза, денний ліміт, тижневий ліміт. «Поточний» капітал —
/// остання Close-точка на `session` або раніше; денний ліміт порівнює її з попередньою
/// Close-точкою, тижневий — з останньою Close-точкою до понеділка тижня `session`.
/// Порушення тижневого ліміту ставить паузу (лише з `Running`) навіть тоді, коли
/// повертається причина денного ліміту.
pub async fn entries_block_reason(
    d: &Deps,
    cfg: &EngineConfig,
    session: NaiveDate,
) -> Result<Option<String>, CycleError> {
    match d.store.engine_mode().await? {
        EngineMode::Killed { .. } => return Ok(Some(KILLED_REASON.to_string())),
        EngineMode::Paused { reason } => return Ok(Some(format!("Пауза: {reason}"))),
        EngineMode::Running => {}
    }
    let Some((current_date, current)) = d
        .store
        .last_close_equity_before(session + Duration::days(1))
        .await?
    else {
        return Ok(None);
    };
    let previous = d.store.last_close_equity_before(current_date).await?;
    let daily = daily_loss_breached(
        previous.map(|(_, e)| e),
        Some(current),
        cfg.risk.daily_loss_limit,
    );
    let week_start = d
        .store
        .last_close_equity_before(week_monday(session))
        .await?;
    let weekly = weekly_loss_breached(
        week_start.map(|(_, e)| e),
        Some(current),
        cfg.risk.weekly_loss_limit,
    );
    if weekly {
        auto_pause(d, WEEKLY_LOSS_REASON).await?;
    }
    Ok(if daily {
        Some(DAILY_LOSS_REASON.to_string())
    } else if weekly {
        Some(WEEKLY_LOSS_REASON.to_string())
    } else {
        None
    })
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveTime, TimeZone, Utc};
    use fond_store::{EquityKind, EquitySnapshot, EventQuery};

    use super::test_support::{d, fixture};
    use super::*;

    fn close(date: NaiveDate, equity: f64) -> EquitySnapshot {
        EquitySnapshot {
            at: fond_core::calendar::et_to_utc(date, NaiveTime::from_hms_opt(16, 5, 0).unwrap()),
            kind: EquityKind::Close,
            session_date: date,
            equity,
            cash: equity,
        }
    }

    async fn put(dp: &Deps, points: &[(NaiveDate, f64)]) {
        for (date, eq) in points {
            dp.store.insert_equity(&close(*date, *eq)).await.unwrap();
        }
    }

    async fn reason(dp: &Deps, session: NaiveDate) -> Option<String> {
        entries_block_reason(dp, &EngineConfig::default(), session)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn journal_persists_and_publishes() {
        let (dp, _h) = fixture().await;
        let mut rx = dp.bus.subscribe();
        let aapl = Symbol::new("AAPL");
        journal(
            &dp,
            EventKind::Signal,
            Some(&aapl),
            Some("i-1"),
            "Сигнал",
            serde_json::json!({ "x": 1 }),
        )
        .await
        .unwrap();
        let stored = dp.store.events(&EventQuery::default()).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].message, "Сигнал");
        match rx.try_recv().unwrap() {
            EngineEventDto::Journal { event } => {
                assert_eq!(event.id, stored[0].id);
                assert_eq!(event.kind, "signal");
                assert_eq!(event.symbol.as_deref(), Some("AAPL"));
                assert_eq!(event.intent_id.as_deref(), Some("i-1"));
                assert_eq!(event.data["x"], 1);
                assert_eq!(event.at, dp.clock.now());
            }
            other => panic!("очікувався Journal, є {other:?}"),
        }
    }

    #[tokio::test]
    async fn trading_calendar_spans_forty_days_around() {
        let (dp, h) = fixture().await;
        let cal = trading_calendar(&dp, h.last_date()).await.unwrap();
        assert_eq!(cal.next_session_after(h.last_date()), Some(d(2026, 11, 5)));
        let wide = Duration::days(60);
        let around = cal.sessions_between(h.last_date() - wide, h.last_date() + wide);
        // 2026-11-04 ± 40 днів = 2026-09-25 (пт) … 2026-12-14 (пн).
        assert_eq!(around.first().copied(), Some(d(2026, 9, 25)));
        assert_eq!(around.last().copied(), Some(d(2026, 12, 14)));
    }

    #[test]
    fn week_monday_is_iso_monday() {
        assert_eq!(week_monday(d(2026, 11, 4)), d(2026, 11, 2));
        assert_eq!(week_monday(d(2026, 11, 2)), d(2026, 11, 2));
        assert_eq!(week_monday(d(2026, 11, 8)), d(2026, 11, 2));
    }

    #[tokio::test]
    async fn no_history_does_not_block() {
        let (dp, h) = fixture().await;
        assert_eq!(reason(&dp, h.last_date()).await, None);
        put(&dp, &[(h.last_date(), 100_000.0)]).await;
        assert_eq!(reason(&dp, h.last_date()).await, None);
    }

    #[tokio::test]
    async fn killed_and_paused_block_with_reason() {
        let (dp, h) = fixture().await;
        let at = Utc.with_ymd_and_hms(2026, 11, 4, 12, 0, 0).unwrap();
        let paused = EngineMode::Paused {
            reason: "Розбіжність з брокером".into(),
        };
        dp.store.set_engine_mode(&paused, at).await.unwrap();
        assert_eq!(
            reason(&dp, h.last_date()).await.as_deref(),
            Some("Пауза: Розбіжність з брокером")
        );
        let killed = EngineMode::Killed {
            reason: "користувач".into(),
        };
        dp.store.set_engine_mode(&killed, at).await.unwrap();
        assert_eq!(
            reason(&dp, h.last_date()).await.as_deref(),
            Some(KILLED_REASON)
        );
    }

    #[tokio::test]
    async fn daily_loss_blocks_without_pausing() {
        let (dp, h) = fixture().await;
        // Вівторок 100k, середа 96.9k: −3.1% за день.
        put(
            &dp,
            &[(d(2026, 11, 3), 100_000.0), (h.last_date(), 96_900.0)],
        )
        .await;
        assert_eq!(
            reason(&dp, h.last_date()).await.as_deref(),
            Some(DAILY_LOSS_REASON)
        );
        assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
        // Наступного ранку (виклик із датою сесії) — той самий результат.
        assert_eq!(
            reason(&dp, d(2026, 11, 5)).await.as_deref(),
            Some(DAILY_LOSS_REASON)
        );
    }

    #[tokio::test]
    async fn weekly_breach_pauses_even_when_daily_also_breached() {
        let (dp, h) = fixture().await;
        let mut rx = dp.bus.subscribe();
        let points = [
            (d(2026, 10, 30), 100_000.0),
            (d(2026, 11, 3), 97_000.0),
            (h.last_date(), 93_000.0),
        ];
        put(&dp, &points).await;
        assert_eq!(
            reason(&dp, h.last_date()).await.as_deref(),
            Some(DAILY_LOSS_REASON)
        );
        assert_eq!(
            dp.store.engine_mode().await.unwrap(),
            EngineMode::Paused {
                reason: WEEKLY_LOSS_REASON.into()
            }
        );
        let mut got = Vec::new();
        while let Ok(e) = rx.try_recv() {
            got.push(e);
        }
        assert!(got.contains(&EngineEventDto::AutoPaused {
            reason: WEEKLY_LOSS_REASON.into()
        }));
        // Далі причина — сама пауза.
        assert_eq!(
            reason(&dp, h.last_date()).await.as_deref(),
            Some("Пауза: Тижневий ліміт збитку")
        );
    }

    #[tokio::test]
    async fn weekly_breach_does_not_downgrade_killed() {
        let (dp, h) = fixture().await;
        let points = [
            (d(2026, 10, 30), 100_000.0),
            (d(2026, 11, 3), 94_000.0),
            (h.last_date(), 93_000.0),
        ];
        put(&dp, &points).await;
        let killed = EngineMode::Killed {
            reason: "користувач".into(),
        };
        dp.store
            .set_engine_mode(&killed, dp.clock.now())
            .await
            .unwrap();
        assert_eq!(
            reason(&dp, h.last_date()).await.as_deref(),
            Some(KILLED_REASON)
        );
        assert_eq!(dp.store.engine_mode().await.unwrap(), killed);
        assert!(!auto_pause(&dp, WEEKLY_LOSS_REASON).await.unwrap());
        assert_eq!(dp.store.engine_mode().await.unwrap(), killed);
    }

    #[tokio::test]
    async fn weekly_loss_compares_with_last_close_before_monday() {
        let (dp, h) = fixture().await;
        // Минула п'ятниця 100k; пн 98k; вт 94k; ср 93.5k: за день −0.5%,
        // від початку тижня −6.5% (база — п'ятниця, а не понеділок).
        let points = [
            (d(2026, 10, 30), 100_000.0),
            (d(2026, 11, 2), 98_000.0),
            (d(2026, 11, 3), 94_000.0),
            (h.last_date(), 93_500.0),
        ];
        put(&dp, &points).await;
        assert_eq!(
            reason(&dp, h.last_date()).await.as_deref(),
            Some(WEEKLY_LOSS_REASON)
        );
        assert!(matches!(
            dp.store.engine_mode().await.unwrap(),
            EngineMode::Paused { .. }
        ));
    }
}
