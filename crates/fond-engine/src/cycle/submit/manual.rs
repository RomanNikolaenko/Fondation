//! Ручне закриття позиції з UI.

use fond_core::calendar::utc_to_et;
use fond_core::ids::{IntentTag, intent_id};
use fond_core::intent::{Intent, IntentKind, IntentStatus};
use fond_core::orders::TimeInForce;
use fond_core::types::Symbol;
use fond_store::EventKind;
use serde_json::json;

use crate::cycle::reconcile::{broker_failure, observe};
use crate::cycle::{CycleError, journal, trading_calendar};
use crate::deps::Deps;

use super::exit::send_exit;
use super::{broker_held, tracked};

const MANUAL_EXIT_REASON: &str = "Закриття позиції вручну";
const MANUAL_EXIT_CANCEL: &str = "Позицію закривають вручну";

/// Номер спроби ручного закриття: `base` -> 1, `"{base}-{n}"` -> n (n ≥ 2); інший id — `None`.
pub(super) fn manual_attempt(base: &str, id: &str) -> Option<u32> {
    let rest = id.strip_prefix(base)?;
    if rest.is_empty() {
        return Some(1);
    }
    rest.strip_prefix('-')?.parse().ok().filter(|n| *n >= 2)
}

/// Закрити позицію зараз (з UI, у будь-якому режимі рушія): намір на вихід на сьогоднішню
/// сесію (або наступну, якщо сьогодні торгів немає) і одразу Sell Market Day — тим самим
/// шляхом, що й вихід перед відкриттям, зі свіжим знімком позицій брокера. Незавершені (ще не
/// відправлені) наміри на вихід за тікером спершу скасовуються, щоб тікер не продали двічі.
///
/// Перша спроба за сесію — `"{id виходу}-manual"`. Остання спроба:
/// - `Approved` (виклик перервався) — продовжується;
/// - `Submitted`/`Filled` (продаж відправлено чи виконано) — повертається як є, без продажу;
/// - завершилась без продажу (`Failed`/`Cancelled`/`Expired`) — нова спроба
///   `"{id виходу}-manual-{n}"`, n = кількість спроб + 1.
pub async fn close_position_now(d: &Deps, symbol: &Symbol) -> Result<Intent, CycleError> {
    let today = utc_to_et(d.clock.now()).date_naive();
    let cal = trading_calendar(d, today).await?;
    let session = match cal.session(today) {
        Some(_) => today,
        None => cal
            .next_session_after(today)
            .ok_or(CycleError::NoCalendar)?,
    };
    let Some(position) = tracked(d, symbol).await? else {
        return Err(CycleError::Other(format!(
            "Позиції {symbol} немає в обліку"
        )));
    };
    let p = &position.position;
    let base = format!(
        "{}-manual",
        intent_id(session, &p.strategy, symbol, IntentTag::Exit)
    );
    let latest = d
        .store
        .intents_for_session(session)
        .await?
        .into_iter()
        .filter_map(|i| manual_attempt(&base, &i.id).map(|n| (n, i)))
        .max_by_key(|(n, _)| *n);
    // Спроби нумеруються підряд, тож наступний номер = кількість спроб + 1.
    let (id, existing, previous) = match latest {
        None => (base, None, None),
        Some((_, i)) if i.status == IntentStatus::Approved => (i.id.clone(), Some(i), None),
        Some((n, i))
            if matches!(
                i.status,
                IntentStatus::Failed
                    | IntentStatus::Cancelled
                    | IntentStatus::Expired
                    | IntentStatus::Rejected
            ) =>
        {
            (format!("{base}-{}", n + 1), None, Some(i))
        }
        // Продаж відправлено чи виконано: повтор нічого не продає.
        Some((_, i)) => return Ok(i),
    };
    let mut held = match observe(d, d.broker.positions().await) {
        Ok(positions) => broker_held(&positions),
        Err(e) => {
            let message = format!("Закриття {symbol} вручну не виконано: немає позицій брокера");
            return broker_failure(d, &message, e).await;
        }
    };

    let now = d.clock.now();
    let intent = match existing {
        // Попередній виклик перервався до відправки.
        Some(i) => i,
        None => {
            let intent = Intent {
                id: id.clone(),
                session_date: session,
                signal_date: today,
                symbol: symbol.clone(),
                strategy: p.strategy.clone(),
                kind: IntentKind::Exit {
                    qty: p.qty,
                    reason: MANUAL_EXIT_REASON.into(),
                },
                status: IntentStatus::Approved,
                explanation: MANUAL_EXIT_REASON.into(),
                created_at: now,
                updated_at: now,
                broker_order_id: None,
                error: None,
            };
            d.store.insert_intent(&intent).await?;
            let retry = previous
                .as_ref()
                .map(|i| format!(" (попередня спроба {} завершилась без продажу)", i.id))
                .unwrap_or_default();
            journal(
                d,
                EventKind::Intent,
                Some(symbol),
                Some(&id),
                format!("Намір на вихід (вручну): {} шт.{retry}", p.qty),
                json!({
                    "kind": "exit",
                    "strategy": p.strategy,
                    "qty": p.qty,
                    "session": session,
                    "status": IntentStatus::Approved,
                    "manual": true,
                    "previous": previous.as_ref().map(|i| &i.id),
                    "previous_status": previous.as_ref().map(|i| i.status),
                }),
            )
            .await?;
            intent
        }
    };

    for other in d
        .store
        .intents_with_status(&[IntentStatus::PendingApproval, IntentStatus::Approved])
        .await?
    {
        if other.id == id
            || other.symbol != *symbol
            || !matches!(other.kind, IntentKind::Exit { .. })
        {
            continue;
        }
        d.store
            .update_intent(
                &other.id,
                IntentStatus::Cancelled,
                now,
                None,
                Some(MANUAL_EXIT_CANCEL),
            )
            .await?;
        journal(
            d,
            EventKind::Intent,
            Some(symbol),
            Some(&other.id),
            format!(
                "Намір на вихід {} скасовано: позицію закривають вручну",
                other.id
            ),
            json!({ "manual_intent": id }),
        )
        .await?;
    }

    send_exit(d, &intent, TimeInForce::Day, &mut held).await?;
    d.store
        .intent(&id)
        .await?
        .ok_or_else(|| CycleError::Other(format!("намір {id} не знайдено")))
}
