//! Відправка ордерів перед відкриттям (09:00–09:25 ET), дедлайн схвалення, пропущене вікно
//! відправки та ручне закриття позиції.
//!
//! Повторна відправка безпечна: `client_order_id` ордера виводиться з id наміру, тож прогін після
//! збою чи перезапуску знаходить уже поданий ордер замість дубля. Вихід ніколи не продає більше,
//! ніж є у брокера: спершу з підтвердженням скасовуються інші продажі тікера й захисний стоп, а
//! обсяг обмежується знімком позицій брокера. Якщо відповіді брокера на відправку так і не
//! отримано, намір лишається `Submitted` без id ордера: його стан з'ясує звірка.

use std::collections::HashMap;
use std::time::Duration;

use chrono::NaiveDate;
use fond_api::config::EngineConfig;
use fond_core::calendar::utc_to_et;
use fond_core::error::BrokerError;
use fond_core::ids::{IntentTag, client_order_id, intent_id};
use fond_core::intent::{Intent, IntentKind, IntentStatus};
use fond_core::orders::{BrokerOrder, OrderClass, OrderRequest, OrderType, Side, TimeInForce};
use fond_core::portfolio::{BrokerPosition, Position, TrackedPosition};
use fond_core::price::{to_f64, to_price};
use fond_core::risk::price_sane;
use fond_core::types::{EngineMode, Symbol};
use fond_store::{EventKind, StoreError};
use rust_decimal::Decimal;
use serde_json::json;

use super::kill::{pause, reprotect_after_failed_sale};
use super::reconcile::{
    broker_failure, cancel_confirmed, flatten, observe, reconcile, status_text,
};
use super::{CycleError, KILLED_REASON, entries_block_reason, journal, trading_calendar};
use crate::deps::Deps;

/// Причина автопаузи: досягнуто `max_orders_per_day`.
pub const ORDER_LIMIT_REASON: &str = "Ліміт ордерів на день";
/// Причина автопаузи: серія помилок брокера.
pub const BROKER_ERRORS_REASON: &str = "3 помилки брокера поспіль";
/// Скільки помилок брокера поспіль зупиняють відправку.
const BROKER_ERROR_STREAK: u32 = 3;

const STOP_CANCEL_FAILED: &str = "Не вдалося скасувати стоп";
const NO_BROKER_POSITION: &str = "Позиції у брокера немає";
const NO_LAST_PRICE: &str = "Немає останньої ціни";
const MANUAL_EXIT_REASON: &str = "Закриття позиції вручну";
const MANUAL_EXIT_CANCEL: &str = "Позицію закривають вручну";

/// Паузи перед повторною відправкою після `Transient`, коли ордера у брокера не знайдено.
#[cfg(not(test))]
const SUBMIT_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(2)];
#[cfg(test)]
const SUBMIT_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(1), Duration::from_millis(2)];

/// Підсумок кроку відправки: скільки намірів відправлено (за видами), скільки завершилось
/// помилкою і скільки лишилось без змін (заблоковано, зупинено або не потрібно).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubmitSummary {
    pub entries: usize,
    pub exits: usize,
    pub stop_moves: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Sent,
    Failed,
    Skipped,
}

impl SubmitSummary {
    fn count(&mut self, kind: &IntentKind, outcome: Outcome) {
        let slot = match (outcome, kind) {
            (Outcome::Failed, _) => &mut self.failed,
            (Outcome::Skipped, _) => &mut self.skipped,
            (Outcome::Sent, IntentKind::Entry { .. }) => &mut self.entries,
            (Outcome::Sent, IntentKind::Exit { .. }) => &mut self.exits,
            (Outcome::Sent, IntentKind::MoveStop { .. }) => &mut self.stop_moves,
        };
        *slot += 1;
    }
}

/// Buy Limit Day: OTO зі стопом або Bracket, якщо є ціль. Для наміру іншого виду `qty` = 0
/// (такий ордер брокер відхилить).
pub fn build_entry_order(i: &Intent) -> OrderRequest {
    let (qty, limit, stop, take_profit) = match &i.kind {
        IntentKind::Entry {
            qty,
            limit_price,
            stop_price,
            take_profit,
            ..
        } => (*qty, Some(*limit_price), Some(*stop_price), *take_profit),
        _ => (0, None, None, None),
    };
    OrderRequest {
        client_order_id: client_order_id(&i.id),
        symbol: i.symbol.clone(),
        side: Side::Buy,
        qty,
        order_type: OrderType::Limit,
        limit_price: limit,
        stop_price: None,
        tif: TimeInForce::Day,
        class: if take_profit.is_some() {
            OrderClass::Bracket
        } else {
            OrderClass::Oto
        },
        stop_loss: stop,
        take_profit,
    }
}

/// Sell Market Simple на обсяг наміру. Для наміру іншого виду `qty` = 0.
pub fn build_exit_order(i: &Intent, tif: TimeInForce) -> OrderRequest {
    let qty = match &i.kind {
        IntentKind::Exit { qty, .. } => *qty,
        _ => 0,
    };
    OrderRequest {
        client_order_id: client_order_id(&i.id),
        symbol: i.symbol.clone(),
        side: Side::Sell,
        qty,
        order_type: OrderType::Market,
        limit_price: None,
        stop_price: None,
        tif,
        class: OrderClass::Simple,
        stop_loss: None,
        take_profit: None,
    }
}

/// Відправка без дублів. Після `DuplicateClientOrderId` або `Transient` ордер шукається за
/// `client_order_id` (знайдено — його й повертаємо). `Transient` без знайденого ордера —
/// ще до двох спроб з паузами 1 с і 2 с. Кожен виклик брокера враховується в `BrokerHealth`.
pub async fn submit_idempotent(d: &Deps, req: &OrderRequest) -> Result<BrokerOrder, BrokerError> {
    let mut delays = SUBMIT_RETRY_DELAYS.iter();
    loop {
        let err = match observe(d, d.broker.submit(req).await) {
            Ok(order) => return Ok(order),
            Err(e @ (BrokerError::DuplicateClientOrderId | BrokerError::Transient(_))) => e,
            Err(e) => return Err(e),
        };
        // Ордер міг дійти до брокера (відповідь втрачено) або вже існувати.
        if let Ok(Some(order)) = observe(d, d.broker.order_by_client_id(&req.client_order_id).await)
        {
            return Ok(order);
        }
        let delay = match (&err, delays.next()) {
            (BrokerError::Transient(_), Some(delay)) => *delay,
            _ => return Err(err),
        };
        tokio::time::sleep(delay).await;
    }
}

/// Дедлайн схвалення: `PendingApproval` із `session_date ≤ session` -> `Expired`.
/// Наміри наступних сесій не чіпаються.
pub async fn expire_pending(d: &Deps, session: NaiveDate) -> Result<usize, CycleError> {
    expire(
        d,
        session,
        &[IntentStatus::PendingApproval],
        "Не схвалено до дедлайну",
        "Дедлайн схвалення минув",
    )
    .await
}

/// Вікно відправки минуло: `PendingApproval` і `Approved` із `session_date ≤ session` ->
/// `Expired`. Наміри наступних сесій не чіпаються.
pub async fn mark_missed(d: &Deps, session: NaiveDate) -> Result<usize, CycleError> {
    expire(
        d,
        session,
        &[IntentStatus::PendingApproval, IntentStatus::Approved],
        "Вікно відправки минуло",
        "Вікно відправки минуло",
    )
    .await
}

async fn expire(
    d: &Deps,
    session: NaiveDate,
    statuses: &[IntentStatus],
    note: &str,
    title: &str,
) -> Result<usize, CycleError> {
    let now = d.clock.now();
    let mut expired = Vec::new();
    for i in d.store.intents_with_status(statuses).await? {
        if i.session_date > session {
            continue;
        }
        match d
            .store
            .update_intent(&i.id, IntentStatus::Expired, now, None, Some(note))
            .await
        {
            Ok(_) => expired.push(i.id),
            // Статус щойно змінився (наприклад, намір уже відправлено).
            Err(StoreError::InvalidTransition { .. }) => {}
            Err(e) => return Err(e.into()),
        }
    }
    journal(
        d,
        EventKind::Intent,
        None,
        None,
        format!(
            "{title} (сесія {session}): прострочено намірів {}",
            expired.len()
        ),
        json!({ "session": session, "expired": expired }),
    )
    .await?;
    Ok(expired.len())
}

/// Відправка перед відкриттям сесії `session`.
///
/// 1. Звірка; розбіжність ставить паузу.
/// 2. Аварійна зупинка — нічого не відправляється, усі `Approved` рахуються пропущеними.
/// 3. Підтягування стопів, 4. виходи (Opg) — також на паузі, 5. входи — лише коли входи
///    дозволено (`entries_block_reason`), інакше лишаються `Approved` до `mark_missed`.
///    Ліміт ордерів на день (усі наміри сесії у `Submitted`/`Filled`) ставить паузу.
/// 6. Після 3 помилок брокера поспіль — пауза. Пауза з кроків 5–6 зупиняє відправку, решта
///    намірів лишається `Approved`.
pub async fn run_pre_open_submit(
    d: &Deps,
    cfg: &EngineConfig,
    session: NaiveDate,
) -> Result<SubmitSummary, CycleError> {
    reconcile(d).await?;

    let approved: Vec<Intent> = d
        .store
        .intents_for_session(session)
        .await?
        .into_iter()
        .filter(|i| i.status == IntentStatus::Approved)
        .collect();
    let mut summary = SubmitSummary::default();
    if matches!(d.store.engine_mode().await?, EngineMode::Killed { .. }) {
        summary.skipped = approved.len();
        finish(d, session, &summary, Some(KILLED_REASON)).await?;
        return Ok(summary);
    }

    // Порядок: підтягування стопів, виходи, входи (стабільне сортування зберігає черговість
    // сховища всередині виду).
    let mut queue: Vec<&Intent> = approved.iter().collect();
    queue.sort_by_key(|i| match i.kind {
        IntentKind::MoveStop { .. } => 0,
        IntentKind::Exit { .. } => 1,
        IntentKind::Entry { .. } => 2,
    });
    // Знімок позицій брокера на початку кроку: виходи продають не більше, ніж у ньому є.
    let mut held = if queue
        .iter()
        .any(|i| matches!(i.kind, IntentKind::Exit { .. }))
    {
        match observe(d, d.broker.positions().await) {
            Ok(p) => broker_held(&p),
            Err(e) => {
                let message = "Відправку перервано: не вдалося отримати позиції";
                return broker_failure(d, message, e).await;
            }
        }
    } else {
        HashMap::new()
    };
    let limit = usize::try_from(cfg.risk.max_orders_per_day).unwrap_or(usize::MAX);

    let mut gate: Option<EntryGate> = None;
    let mut stopped: Option<&str> = None;
    for (n, intent) in queue.iter().enumerate() {
        let outcome = match &intent.kind {
            IntentKind::MoveStop { new_stop, .. } => move_stop(d, intent, *new_stop).await?,
            IntentKind::Exit { .. } => send_exit(d, intent, TimeInForce::Opg, &mut held).await?,
            IntentKind::Entry { .. } => {
                if gate.is_none() {
                    gate = Some(entry_gate(d, cfg, session).await?);
                }
                match gate.as_mut() {
                    Some(EntryGate::Open { sent }) => {
                        if *sent >= limit {
                            pause(d, ORDER_LIMIT_REASON).await?;
                            stopped = Some(ORDER_LIMIT_REASON);
                            summary.skipped += queue.len() - n;
                            break;
                        }
                        let outcome = send_entry(d, cfg, intent).await?;
                        if outcome == Outcome::Sent {
                            *sent += 1;
                        }
                        outcome
                    }
                    _ => Outcome::Skipped,
                }
            }
        };
        summary.count(&intent.kind, outcome);
        if d.health.streak() >= BROKER_ERROR_STREAK {
            pause(d, BROKER_ERRORS_REASON).await?;
            stopped = Some(BROKER_ERRORS_REASON);
            summary.skipped += queue.len() - n - 1;
            break;
        }
    }
    finish(d, session, &summary, stopped).await?;
    Ok(summary)
}

/// Чи можна відправляти входи (визначається один раз, перед першим входом).
enum EntryGate {
    /// Входи заборонено (режим, ліміти збитку): лишаються `Approved`.
    Blocked,
    /// `sent` — ордери сесії для ліміту на день: наміри будь-якого виду в `Submitted`/`Filled`.
    Open { sent: usize },
}

async fn entry_gate(
    d: &Deps,
    cfg: &EngineConfig,
    session: NaiveDate,
) -> Result<EntryGate, CycleError> {
    if let Some(why) = entries_block_reason(d, cfg, session).await? {
        journal(
            d,
            EventKind::Risk,
            None,
            None,
            format!("Входи не відправлено: {why}"),
            json!({ "reason": why, "session": session }),
        )
        .await?;
        return Ok(EntryGate::Blocked);
    }
    let sent = d
        .store
        .intents_for_session(session)
        .await?
        .iter()
        .filter(|i| matches!(i.status, IntentStatus::Submitted | IntentStatus::Filled))
        .count();
    Ok(EntryGate::Open { sent })
}

async fn finish(
    d: &Deps,
    session: NaiveDate,
    s: &SubmitSummary,
    stopped: Option<&str>,
) -> Result<(), CycleError> {
    let tail = stopped
        .map(|why| format!("; зупинено: {why}"))
        .unwrap_or_default();
    journal(
        d,
        EventKind::Cycle,
        None,
        None,
        format!(
            "Відправка перед відкриттям (сесія {session}): входів {}, виходів {}, підтягнуто \
             стопів {}, помилок {}, пропущено {}{tail}",
            s.entries, s.exits, s.stop_moves, s.failed, s.skipped
        ),
        json!({
            "session": session,
            "entries": s.entries,
            "exits": s.exits,
            "stop_moves": s.stop_moves,
            "failed": s.failed,
            "skipped": s.skipped,
            "stopped": stopped,
        }),
    )
    .await
}

fn broker_held(positions: &[BrokerPosition]) -> HashMap<Symbol, i64> {
    let mut held = HashMap::new();
    for p in positions {
        *held.entry(p.symbol.clone()).or_default() += p.qty;
    }
    held
}

/// Обсяг, який уже продає відправлений вихід: наступний вихід тікера його не бере.
fn reserve(held: &mut HashMap<Symbol, i64>, sym: &Symbol, qty: u32) {
    *held.entry(sym.clone()).or_default() -= i64::from(qty);
}

async fn tracked(d: &Deps, sym: &Symbol) -> Result<Option<TrackedPosition>, CycleError> {
    Ok(d.store
        .positions()
        .await?
        .into_iter()
        .find(|p| p.position.symbol == *sym))
}

fn kind_label(k: &IntentKind) -> &'static str {
    match k {
        IntentKind::Entry { .. } => "Вхід",
        IntentKind::Exit { .. } => "Вихід",
        IntentKind::MoveStop { .. } => "Підтягування стопа",
    }
}

/// Намір `Failed` з причиною; подія в журналі.
async fn fail(d: &Deps, intent: &Intent, why: &str) -> Result<Outcome, CycleError> {
    fail_with(d, intent, why, json!({})).await
}

async fn fail_with(
    d: &Deps,
    intent: &Intent,
    why: &str,
    mut data: serde_json::Value,
) -> Result<Outcome, CycleError> {
    d.store
        .update_intent(
            &intent.id,
            IntentStatus::Failed,
            d.clock.now(),
            None,
            Some(why),
        )
        .await?;
    data["reason"] = json!(why);
    journal(
        d,
        EventKind::Error,
        Some(&intent.symbol),
        Some(&intent.id),
        format!(
            "{} {} не виконано: {why}",
            kind_label(&intent.kind),
            intent.symbol
        ),
        data,
    )
    .await?;
    Ok(Outcome::Failed)
}

/// Відповіді брокера не отримано: ордер міг дійти до брокера.
fn unanswered_error(e: &BrokerError) -> bool {
    matches!(
        e,
        BrokerError::Transient(_) | BrokerError::DuplicateClientOrderId
    )
}

/// Намір `Submitted` без id ордера: звірка знайде ордер за `client_order_id` або позначить
/// намір `Failed` («Ордер не знайдено у брокера»).
async fn unanswered(d: &Deps, intent: &Intent, e: &BrokerError) -> Result<Outcome, CycleError> {
    let why = format!("Відповіді брокера не отримано ({e}): стан ордера перевірить звірка");
    d.store
        .update_intent(
            &intent.id,
            IntentStatus::Submitted,
            d.clock.now(),
            None,
            Some(&why),
        )
        .await?;
    journal(
        d,
        EventKind::Error,
        Some(&intent.symbol),
        Some(&intent.id),
        format!("{} {}: {why}", kind_label(&intent.kind), intent.symbol),
        json!({ "client_order_id": client_order_id(&intent.id), "error": e.to_string() }),
    )
    .await?;
    Ok(Outcome::Sent)
}

/// Підтягування стопа: `replace_stop` поточного стоп-ордера відстежуваної позиції (не того, що
/// записано в намірі — позицію могли перезахистити), лише вгору.
async fn move_stop(d: &Deps, intent: &Intent, new_stop: Decimal) -> Result<Outcome, CycleError> {
    let sym = &intent.symbol;
    let Some(tp) = tracked(d, sym).await? else {
        return fail(d, intent, "Позиція не відстежується").await;
    };
    let Some(stop_id) = tp.stop_order_id.clone() else {
        return fail(d, intent, "У позиції немає стоп-ордера").await;
    };
    if to_price(tp.position.stop_price).is_ok_and(|current| new_stop <= current) {
        let why = format!(
            "Стоп не підтягнуто: новий рівень {new_stop:.2} не вище поточного {:.2}",
            tp.position.stop_price
        );
        d.store
            .update_intent(
                &intent.id,
                IntentStatus::Cancelled,
                d.clock.now(),
                None,
                Some(&why),
            )
            .await?;
        journal(
            d,
            EventKind::Intent,
            Some(sym),
            Some(&intent.id),
            why,
            json!({ "new_stop": new_stop.to_string(), "stop": tp.position.stop_price }),
        )
        .await?;
        return Ok(Outcome::Skipped);
    }

    journal(
        d,
        EventKind::Order,
        Some(sym),
        Some(&intent.id),
        format!("Відправляю: підтягування стопа {sym} до {new_stop} (ордер {stop_id})"),
        json!({ "stop_order_id": stop_id, "new_stop": new_stop.to_string() }),
    )
    .await?;
    let order = match observe(d, d.broker.replace_stop(&stop_id, new_stop).await) {
        Ok(o) => o,
        Err(e) => return fail(d, intent, &format!("Не вдалося підтягнути стоп: {e}")).await,
    };
    let now = d.clock.now();
    d.store.upsert_order(&order, now).await?;
    let updated = TrackedPosition {
        position: Position {
            stop_price: to_f64(new_stop),
            ..tp.position.clone()
        },
        stop_order_id: Some(order.id.clone()),
        ..tp.clone()
    };
    d.store.upsert_position(&updated, now).await?;
    d.store
        .update_intent(
            &intent.id,
            IntentStatus::Submitted,
            now,
            Some(&order.id),
            None,
        )
        .await?;
    d.store
        .update_intent(&intent.id, IntentStatus::Filled, now, None, None)
        .await?;
    journal(
        d,
        EventKind::Order,
        Some(sym),
        Some(&intent.id),
        format!(
            "Стоп підтягнуто до {new_stop}: ордер {} (замість {stop_id})",
            order.id
        ),
        json!({
            "order_id": order.id,
            "previous_stop_order_id": stop_id,
            "new_stop": new_stop.to_string(),
        }),
    )
    .await?;
    Ok(Outcome::Sent)
}

/// Остання ціна: угода (`latest_trade`), інакше останній close у сховищі.
async fn last_price(d: &Deps, sym: &Symbol) -> Result<Option<f64>, CycleError> {
    let usable = |p: &f64| p.is_finite() && *p > 0.0;
    let trade = d.data.latest_trade(sym).await.ok().flatten().filter(usable);
    if trade.is_some() {
        return Ok(trade);
    }
    let Some(date) = d.store.last_bar_date(sym).await? else {
        return Ok(None);
    };
    Ok(d.store
        .bars(sym, date, date)
        .await?
        .last()
        .map(|b| b.close)
        .filter(usable))
}

/// `0.05` -> `5`, `0.025` -> `2.5`.
fn percent(pct: f64) -> String {
    format!("{}", (pct * 10_000.0).round() / 100.0)
}

/// Вхід: перевірка ціни, відправка, намір `Submitted` з id ордера.
async fn send_entry(d: &Deps, cfg: &EngineConfig, intent: &Intent) -> Result<Outcome, CycleError> {
    let IntentKind::Entry {
        qty,
        limit_price,
        stop_price,
        take_profit,
        ..
    } = &intent.kind
    else {
        return Ok(Outcome::Skipped);
    };
    let sym = &intent.symbol;
    let Some(last) = last_price(d, sym).await? else {
        return fail(d, intent, NO_LAST_PRICE).await;
    };
    let pct = cfg.risk.price_sanity_pct;
    if !price_sane(to_f64(*limit_price), last, pct) {
        let why = format!("Ціна поза межами ±{}% від останньої", percent(pct));
        let data = json!({ "limit_price": limit_price.to_string(), "last_price": last });
        return fail_with(d, intent, &why, data).await;
    }

    let req = build_entry_order(intent);
    let target = take_profit
        .map(|t| format!(", ціль {t}"))
        .unwrap_or_default();
    journal(
        d,
        EventKind::Order,
        Some(sym),
        Some(&intent.id),
        format!(
            "Відправляю вхід: купівля {qty} шт. {sym} за лімітом {limit_price}, стоп \
             {stop_price}{target}"
        ),
        json!({ "client_order_id": req.client_order_id, "last_price": last }),
    )
    .await?;
    match submit_idempotent(d, &req).await {
        Ok(order) => {
            let now = d.clock.now();
            d.store.upsert_order(&order, now).await?;
            d.store
                .update_intent(
                    &intent.id,
                    IntentStatus::Submitted,
                    now,
                    Some(&order.id),
                    None,
                )
                .await?;
            journal(
                d,
                EventKind::Order,
                Some(sym),
                Some(&intent.id),
                format!("Вхід відправлено: ордер {}", order.id),
                json!({
                    "order_id": order.id,
                    "client_order_id": order.client_order_id,
                    "status": order.status,
                }),
            )
            .await?;
            Ok(Outcome::Sent)
        }
        Err(e) if unanswered_error(&e) => unanswered(d, intent, &e).await,
        Err(e) => fail(d, intent, &format!("Помилка брокера: {e}")).await,
    }
}

/// Вихід — спільний для відправки перед відкриттям (Opg) і ручного закриття (Day).
///
/// 1. Ордер цього наміру вже є у брокера (збій після відправки) — намір лише позначається
///    `Submitted`; нічого не скасовується й не подається.
/// 2. Обсяг — менше з обсягу наміру й позиції брокера зі знімка `held`; позиції немає —
///    `Failed`.
/// 3. З підтвердженням (остаточний стан) скасовуються інші відкриті продажі тікера, потім стоп
///    позиції. Не підтверджено — `Failed`, продаж не подається. Якщо якийсь із них устиг
///    виконатись, знімок застарів: продаж не подається, позиція знову під стопом.
/// 4. Sell Market `tif`; відмова брокера — `Failed` і новий стоп на позицію.
async fn send_exit(
    d: &Deps,
    intent: &Intent,
    tif: TimeInForce,
    held: &mut HashMap<Symbol, i64>,
) -> Result<Outcome, CycleError> {
    let IntentKind::Exit { qty, .. } = &intent.kind else {
        return Ok(Outcome::Skipped);
    };
    let sym = &intent.symbol;
    let cid = client_order_id(&intent.id);

    match observe(d, d.broker.order_by_client_id(&cid).await) {
        Ok(Some(order)) => return adopt_exit(d, intent, &order, held).await,
        Ok(None) => {}
        Err(e) => {
            return fail(
                d,
                intent,
                &format!("Не вдалося перевірити ордер {cid}: {e}"),
            )
            .await;
        }
    }

    let available = held.get(sym).copied().unwrap_or(0);
    let sell = if available <= 0 {
        0
    } else {
        u32::try_from(available).map_or(*qty, |a| a.min(*qty))
    };
    if sell == 0 {
        return fail(d, intent, NO_BROKER_POSITION).await;
    }

    let position = tracked(d, sym).await?;
    let stop_id = position.as_ref().and_then(|p| p.stop_order_id.clone());
    match clear_sells(d, sym, stop_id.as_deref()).await? {
        Cleared::Done => {}
        Cleared::NotCancelled(why) => return fail(d, intent, &why).await,
        Cleared::SoldMeanwhile(why) => {
            let outcome = fail(d, intent, &why).await?;
            if let Some(p) = &position {
                reprotect_after_failed_sale(d, p).await?;
            }
            return Ok(outcome);
        }
    }

    let req = OrderRequest {
        qty: sell,
        ..build_exit_order(intent, tif)
    };
    let how = if tif == TimeInForce::Opg {
        "за ринком на відкритті"
    } else {
        "за ринком"
    };
    let capped = if sell < *qty {
        format!(" (у намірі {qty}, у брокера {available})")
    } else {
        String::new()
    };
    journal(
        d,
        EventKind::Order,
        Some(sym),
        Some(&intent.id),
        format!("Відправляю вихід: продаж {sell} шт. {sym} {how}{capped}"),
        json!({
            "client_order_id": cid,
            "qty": sell,
            "planned": qty,
            "held": available,
            "tif": tif,
        }),
    )
    .await?;
    match submit_idempotent(d, &req).await {
        Ok(order) => {
            reserve(held, sym, sell);
            let now = d.clock.now();
            d.store.upsert_order(&order, now).await?;
            d.store
                .update_intent(
                    &intent.id,
                    IntentStatus::Submitted,
                    now,
                    Some(&order.id),
                    None,
                )
                .await?;
            journal(
                d,
                EventKind::Order,
                Some(sym),
                Some(&intent.id),
                format!("Вихід відправлено: ордер {}, {sell} шт.", order.id),
                json!({
                    "order_id": order.id,
                    "client_order_id": order.client_order_id,
                    "qty": sell,
                    "status": order.status,
                }),
            )
            .await?;
            Ok(Outcome::Sent)
        }
        Err(e) if unanswered_error(&e) => {
            // Продаж міг дійти до брокера: обсяг зарезервовано, новий стоп не подається.
            reserve(held, sym, sell);
            unanswered(d, intent, &e).await
        }
        Err(e) => {
            let outcome = fail(d, intent, &format!("Помилка брокера: {e}")).await?;
            if let Some(p) = &position {
                reprotect_after_failed_sale(d, p).await?;
            }
            Ok(outcome)
        }
    }
}

/// Ордер виходу вже у брокера: намір `Submitted` з його id.
async fn adopt_exit(
    d: &Deps,
    intent: &Intent,
    order: &BrokerOrder,
    held: &mut HashMap<Symbol, i64>,
) -> Result<Outcome, CycleError> {
    if !order.status.is_terminal() {
        reserve(
            held,
            &intent.symbol,
            order.qty.saturating_sub(order.filled_qty),
        );
    }
    let now = d.clock.now();
    d.store.upsert_order(order, now).await?;
    d.store
        .update_intent(
            &intent.id,
            IntentStatus::Submitted,
            now,
            Some(&order.id),
            None,
        )
        .await?;
    journal(
        d,
        EventKind::Order,
        Some(&intent.symbol),
        Some(&intent.id),
        format!(
            "Вихід уже відправлено раніше: ордер {} ({})",
            order.id,
            status_text(&order.status)
        ),
        json!({ "order_id": order.id, "client_order_id": order.client_order_id }),
    )
    .await?;
    Ok(Outcome::Sent)
}

/// Результат скасування продажів тікера перед виходом.
enum Cleared {
    Done,
    /// Скасування не підтверджено: продаж не подається.
    NotCancelled(String),
    /// Ордер устиг виконатись до скасування: знімок позицій застарів.
    SoldMeanwhile(String),
}

/// Скасовує з підтвердженням відкриті продажі тікера: спершу інші (ціль, ручний продаж), поки
/// стоп ще захищає позицію, потім сам стоп `stop_id`. Ноги ще відкритого батьківського ордера
/// (новий вхід) не чіпаються.
async fn clear_sells(d: &Deps, sym: &Symbol, stop_id: Option<&str>) -> Result<Cleared, CycleError> {
    let open = match observe(d, d.broker.open_orders().await) {
        Ok(o) => o,
        Err(e) => {
            let why = format!("Не вдалося отримати відкриті ордери: {e}");
            return Ok(Cleared::NotCancelled(why));
        }
    };
    let orders = flatten(&open);
    let others: Vec<&BrokerOrder> = orders
        .iter()
        .filter(|(o, parent)| {
            o.symbol == *sym
                && o.side == Side::Sell
                && o.status.is_open()
                && parent.is_none_or(|p| !p.status.is_open())
                && Some(o.id.as_str()) != stop_id
        })
        .map(|(o, _)| *o)
        .collect();
    for o in others {
        match confirm_cancel(d, &o.id).await? {
            Confirmed::Pending => {
                let why = format!("Не вдалося скасувати ордер {}", o.id);
                return Ok(Cleared::NotCancelled(why));
            }
            Confirmed::Done(filled) if filled > o.filled_qty => {
                let why = format!(
                    "Ордер {} виконано до скасування ({filled} шт.): продаж не відправлено",
                    o.id
                );
                return Ok(Cleared::SoldMeanwhile(why));
            }
            Confirmed::Done(_) | Confirmed::Absent => {}
        }
    }
    let Some(id) = stop_id else {
        return Ok(Cleared::Done);
    };
    let before = orders
        .iter()
        .find(|(o, _)| o.id == id)
        .map_or(0, |(o, _)| o.filled_qty);
    Ok(match confirm_cancel(d, id).await? {
        Confirmed::Pending => Cleared::NotCancelled(STOP_CANCEL_FAILED.into()),
        Confirmed::Done(filled) if filled > before => Cleared::SoldMeanwhile(format!(
            "Стоп {id} виконано до скасування ({filled} шт.): продаж не відправлено"
        )),
        Confirmed::Done(_) | Confirmed::Absent => Cleared::Done,
    })
}

enum Confirmed {
    /// Ордер завершився; `filled_qty` у кінцевому стані.
    Done(u32),
    /// Брокер ордера не знає: скасовувати нічого.
    Absent,
    /// Завершення не підтверджено (зокрема збій запиту).
    Pending,
}

async fn confirm_cancel(d: &Deps, id: &str) -> Result<Confirmed, CycleError> {
    match cancel_confirmed(d, id).await {
        Ok(Some(o)) => Ok(Confirmed::Done(o.filled_qty)),
        Ok(None) => Ok(Confirmed::Pending),
        Err(CycleError::Broker(BrokerError::NotFound)) => Ok(Confirmed::Absent),
        Err(CycleError::Broker(_)) => Ok(Confirmed::Pending),
        Err(e) => Err(e),
    }
}

/// Закрити позицію зараз (з UI, у будь-якому режимі рушія): намір на вихід
/// `"{id виходу}-manual"` на сьогоднішню сесію (або наступну, якщо сьогодні торгів немає) і
/// одразу Sell Market Day — тим самим шляхом, що й вихід перед відкриттям, зі свіжим знімком
/// позицій брокера. Незавершені (ще не відправлені) наміри на вихід за тікером спершу
/// скасовуються, щоб тікер не продали двічі. Повторний виклик після відправки повертає той
/// самий намір.
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
    let id = format!(
        "{}-manual",
        intent_id(session, &p.strategy, symbol, IntentTag::Exit)
    );
    let existing = d.store.intent(&id).await?;
    match &existing {
        Some(i) if matches!(i.status, IntentStatus::Submitted | IntentStatus::Filled) => {
            return Ok(i.clone());
        }
        Some(i) if i.status != IntentStatus::Approved => {
            let why = i.error.as_deref().unwrap_or("без причини");
            return Err(CycleError::Other(format!(
                "Ручне закриття {symbol} на сесію {session} уже завершилось без продажу: {why}"
            )));
        }
        _ => {}
    }
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
            journal(
                d,
                EventKind::Intent,
                Some(symbol),
                Some(&id),
                format!("Намір на вихід (вручну): {} шт.", p.qty),
                json!({
                    "kind": "exit",
                    "strategy": p.strategy,
                    "qty": p.qty,
                    "session": session,
                    "status": IntentStatus::Approved,
                    "manual": true,
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use fond_api::events::EngineEventDto;
    use fond_core::ids::{IntentTag, client_order_id};
    use fond_core::intent::{IntentKind, IntentStatus};
    use fond_core::orders::{OrderClass, OrderStatus, OrderType, Side};
    use fond_core::portfolio::{Account, BrokerPosition, Position, TrackedPosition};
    use fond_core::risk::RiskConfig;
    use fond_core::testkit::FakeBroker;
    use fond_core::traits::Broker;
    use fond_core::types::EngineMode;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    use super::super::reconcile::{flatten, reconcile};
    use super::super::test_support::{
        Handles, N_BARS, d, drain, entry_kind, et, fixture, flat_bars, held_aapl, intent_with,
        journal_messages, session, start_date, submit_entry, sym,
    };
    use super::*;

    /// Стенд із годинником на 09:00 ET сесії (вікно відправки).
    async fn setup() -> (Deps, Handles) {
        let (dp, h) = fixture().await;
        h.clock.set(et(session(), 9, 0));
        (dp, h)
    }

    async fn intent(dp: &Deps, id: &str) -> Intent {
        dp.store.intent(id).await.unwrap().unwrap()
    }

    async fn insert(dp: &Deps, intents: &[&Intent]) {
        for i in intents {
            assert!(dp.store.insert_intent(i).await.unwrap(), "{}", i.id);
        }
    }

    async fn order_status(h: &Handles, id: &str) -> OrderStatus {
        h.broker.order(id).await.unwrap().status
    }

    async fn stored_order_status(dp: &Deps, id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT status FROM orders WHERE id = ?")
            .bind(id)
            .fetch_optional(dp.store.pool())
            .await
            .unwrap()
    }

    async fn run(dp: &Deps) -> SubmitSummary {
        run_with(dp, &EngineConfig::default()).await
    }

    async fn run_with(dp: &Deps, cfg: &EngineConfig) -> SubmitSummary {
        run_pre_open_submit(dp, cfg, session()).await.unwrap()
    }

    fn approved_entry(symbol: &str, take_profit: Option<Decimal>) -> Intent {
        intent_with(
            symbol,
            IntentTag::Entry,
            entry_kind(10, dec!(101.00), dec!(95.50), take_profit),
            IntentStatus::Approved,
            None,
        )
    }

    fn approved_exit(symbol: &str, qty: u32) -> Intent {
        let kind = IntentKind::Exit {
            qty,
            reason: "сигнал виходу".into(),
        };
        intent_with(symbol, IntentTag::Exit, kind, IntentStatus::Approved, None)
    }

    fn approved_move(symbol: &str, new_stop: Decimal, stop_order_id: &str) -> Intent {
        let kind = IntentKind::MoveStop {
            new_stop,
            stop_order_id: stop_order_id.into(),
        };
        intent_with(symbol, IntentTag::Stop, kind, IntentStatus::Approved, None)
    }

    /// Той самий намір, але на сесію `date` (id і `session_date`).
    fn on(date: NaiveDate, i: Intent) -> Intent {
        Intent {
            id: i
                .id
                .replacen("20261105", &date.format("%Y%m%d").to_string(), 1),
            session_date: date,
            ..i
        }
    }

    /// AAPL (10 шт., стоп 95.50, `ord-1-stop`) і MSFT (20 шт., стоп 47.00, `ord-2-stop`),
    /// виконані й звірені.
    async fn held_aapl_msft(dp: &Deps, h: &Handles) {
        let (_, aapl) = submit_entry(dp, "AAPL", 10, dec!(101.00), dec!(95.50), None).await;
        let (_, msft) = submit_entry(dp, "MSFT", 20, dec!(50.00), dec!(47.00), None).await;
        h.broker.fill(&aapl.id, dec!(101.00), 10);
        h.broker.fill(&msft.id, dec!(50.00), 20);
        reconcile(dp).await.unwrap();
        assert_eq!(dp.store.positions().await.unwrap().len(), 2);
    }

    fn killed() -> EngineMode {
        EngineMode::Killed {
            reason: "тест".into(),
        }
    }

    /// Що зробить черговий `submit` обгортки.
    enum Script {
        /// Помилка; ордер до брокера не дійшов.
        Fail(BrokerError),
        /// Ордер подано, але відповідь втрачено: повертається помилка.
        Lose(BrokerError),
    }

    /// Обгортка над `FakeBroker`: виконує сценарій для `submit` і веде журнал змінних
    /// викликів (`submit`, `cancel`, `replace_stop`); решта методів — без змін.
    struct ScriptedBroker {
        inner: Arc<FakeBroker>,
        script: Mutex<VecDeque<Script>>,
        submits: AtomicUsize,
        log: Mutex<Vec<String>>,
        /// Ордер, який устигає виконатись (`qty` шт. за `price`) перед своїм скасуванням.
        fill_on_cancel: Mutex<Option<(String, Decimal, u32)>>,
    }

    impl ScriptedBroker {
        fn submits(&self) -> usize {
            self.submits.load(Ordering::SeqCst)
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }

        fn note(&self, entry: String) {
            self.log.lock().unwrap().push(entry);
        }
    }

    #[async_trait]
    impl Broker for ScriptedBroker {
        async fn account(&self) -> Result<Account, BrokerError> {
            self.inner.account().await
        }

        async fn positions(&self) -> Result<Vec<BrokerPosition>, BrokerError> {
            self.inner.positions().await
        }

        async fn open_orders(&self) -> Result<Vec<BrokerOrder>, BrokerError> {
            self.inner.open_orders().await
        }

        async fn order(&self, id: &str) -> Result<BrokerOrder, BrokerError> {
            self.inner.order(id).await
        }

        async fn order_by_client_id(
            &self,
            client_order_id: &str,
        ) -> Result<Option<BrokerOrder>, BrokerError> {
            self.inner.order_by_client_id(client_order_id).await
        }

        async fn submit(&self, req: &OrderRequest) -> Result<BrokerOrder, BrokerError> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            self.note(format!("submit {}", req.client_order_id));
            let next = self.script.lock().unwrap().pop_front();
            match next {
                Some(Script::Fail(e)) => Err(e),
                Some(Script::Lose(e)) => {
                    self.inner.submit(req).await?;
                    Err(e)
                }
                None => self.inner.submit(req).await,
            }
        }

        async fn cancel(&self, order_id: &str) -> Result<(), BrokerError> {
            self.note(format!("cancel {order_id}"));
            let race = self.fill_on_cancel.lock().unwrap().take();
            match race {
                Some((id, price, qty)) if id == order_id => self.inner.fill(&id, price, qty),
                other => *self.fill_on_cancel.lock().unwrap() = other,
            }
            self.inner.cancel(order_id).await
        }

        async fn replace_stop(
            &self,
            order_id: &str,
            new_stop: Decimal,
        ) -> Result<BrokerOrder, BrokerError> {
            self.note(format!("replace {order_id}"));
            self.inner.replace_stop(order_id, new_stop).await
        }
    }

    /// Залежності з `ScriptedBroker` замість брокера стенда (стан брокера спільний).
    fn scripted(dp: &Deps, h: &Handles, script: Vec<Script>) -> (Deps, Arc<ScriptedBroker>) {
        let broker = Arc::new(ScriptedBroker {
            inner: h.broker.clone(),
            script: Mutex::new(script.into()),
            submits: AtomicUsize::new(0),
            log: Mutex::new(Vec::new()),
            fill_on_cancel: Mutex::new(None),
        });
        let deps = Deps {
            broker: broker.clone(),
            ..dp.clone()
        };
        (deps, broker)
    }

    fn transient() -> BrokerError {
        BrokerError::Transient("timeout".into())
    }

    // ---------------------------------------------------------------- побудова ордерів

    #[test]
    fn entry_order_is_oto_limit_day_with_stop() {
        let i = approved_entry("AAPL", None);
        assert_eq!(
            build_entry_order(&i),
            OrderRequest {
                client_order_id: "fond-20261105-trend_pullback-AAPL-entry".into(),
                symbol: sym("AAPL"),
                side: Side::Buy,
                qty: 10,
                order_type: OrderType::Limit,
                limit_price: Some(dec!(101.00)),
                stop_price: None,
                tif: TimeInForce::Day,
                class: OrderClass::Oto,
                stop_loss: Some(dec!(95.50)),
                take_profit: None,
            }
        );
    }

    #[test]
    fn entry_with_take_profit_is_bracket() {
        let i = approved_entry("AAPL", Some(dec!(115.25)));
        let req = build_entry_order(&i);
        assert_eq!(req.class, OrderClass::Bracket);
        assert_eq!(req.take_profit, Some(dec!(115.25)));
        assert_eq!(req.stop_loss, Some(dec!(95.50)));
        assert_eq!(req.limit_price, Some(dec!(101.00)));
        assert_eq!(req.order_type, OrderType::Limit);
        assert_eq!(
            (req.side, req.tif, req.qty),
            (Side::Buy, TimeInForce::Day, 10)
        );
        assert_eq!(req.client_order_id, client_order_id(&i.id));
    }

    #[test]
    fn exit_order_is_market_sell() {
        let i = approved_exit("AAPL", 10);
        for tif in [TimeInForce::Opg, TimeInForce::Day] {
            assert_eq!(
                build_exit_order(&i, tif),
                OrderRequest {
                    client_order_id: "fond-20261105-trend_pullback-AAPL-exit".into(),
                    symbol: sym("AAPL"),
                    side: Side::Sell,
                    qty: 10,
                    order_type: OrderType::Market,
                    limit_price: None,
                    stop_price: None,
                    tif,
                    class: OrderClass::Simple,
                    stop_loss: None,
                    take_profit: None,
                }
            );
        }
    }

    // ---------------------------------------------------------------- ідемпотентна відправка

    #[tokio::test]
    async fn submit_marks_intent_submitted_with_broker_id() {
        let (dp, h) = setup().await;
        let entry = approved_entry("AAPL", None);
        insert(&dp, &[&entry]).await;
        h.data.set_latest_trade(&sym("AAPL"), 100.40);

        let summary = run(&dp).await;

        assert_eq!(
            summary,
            SubmitSummary {
                entries: 1,
                ..SubmitSummary::default()
            }
        );
        assert_eq!(h.broker.submitted(), vec![build_entry_order(&entry)]);
        let stored = intent(&dp, &entry.id).await;
        assert_eq!(stored.status, IntentStatus::Submitted);
        assert_eq!(stored.broker_order_id.as_deref(), Some("ord-1"));
        assert_eq!(stored.error, None);
        assert_eq!(
            stored_order_status(&dp, "ord-1").await.as_deref(),
            Some("accepted")
        );
        // Журнал (новіші першими): запис перед відправкою і після неї.
        let journal = journal_messages(&dp).await;
        let at = |m: &str| {
            journal
                .iter()
                .position(|x| x == m)
                .unwrap_or_else(|| panic!("немає «{m}» у {journal:?}"))
        };
        let before = at("Відправляю вхід: купівля 10 шт. AAPL за лімітом 101.00, стоп 95.50");
        let after = at("Вхід відправлено: ордер ord-1");
        assert!(after < before, "{journal:?}");
        assert_eq!(
            journal[0],
            "Відправка перед відкриттям (сесія 2026-11-05): входів 1, виходів 0, підтягнуто \
             стопів 0, помилок 0, пропущено 0"
        );
    }

    #[tokio::test]
    async fn duplicate_after_restart_resolves_existing_order() {
        let (dp, h) = setup().await;
        let entry = approved_entry("AAPL", None);
        insert(&dp, &[&entry]).await;
        h.data.set_latest_trade(&sym("AAPL"), 101.00);
        // Збій після подання ордера, до позначки наміру: ордер уже у брокера.
        let existing = dp.broker.submit(&build_entry_order(&entry)).await.unwrap();

        let summary = run(&dp).await;

        assert_eq!(summary.entries, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(h.broker.submitted().len(), 1);
        let stored = intent(&dp, &entry.id).await;
        assert_eq!(stored.status, IntentStatus::Submitted);
        assert_eq!(stored.broker_order_id, Some(existing.id));
    }

    #[tokio::test]
    async fn transient_then_found_does_not_double_submit() {
        let (dp, h) = setup().await;
        let entry = approved_entry("AAPL", None);
        insert(&dp, &[&entry]).await;
        h.data.set_latest_trade(&sym("AAPL"), 101.00);
        // Брокер прийняв ордер, але відповідь загубилась.
        let (sd, broker) = scripted(&dp, &h, vec![Script::Lose(transient())]);

        let summary = run(&sd).await;

        assert_eq!(summary.entries, 1);
        assert_eq!(broker.submits(), 1, "без повторної відправки");
        assert_eq!(h.broker.submitted().len(), 1);
        let stored = intent(&dp, &entry.id).await;
        assert_eq!(stored.status, IntentStatus::Submitted);
        assert_eq!(stored.broker_order_id.as_deref(), Some("ord-1"));
        assert_eq!(dp.health.streak(), 0);
    }

    #[tokio::test]
    async fn transient_without_order_retries_twice_then_gives_up() {
        let (dp, h) = setup().await;
        let req = build_entry_order(&approved_entry("AAPL", None));

        // Два збої без ордера у брокера: третя спроба проходить.
        let (sd, broker) = scripted(
            &dp,
            &h,
            vec![Script::Fail(transient()), Script::Fail(transient())],
        );
        let order = submit_idempotent(&sd, &req).await.unwrap();
        assert_eq!(order.client_order_id, req.client_order_id);
        assert_eq!(broker.submits(), 3);
        assert_eq!(h.broker.submitted().len(), 1);

        // Три збої поспіль: більше спроб немає.
        let other = OrderRequest {
            client_order_id: "fond-other".into(),
            ..req.clone()
        };
        let failing = vec![
            Script::Fail(transient()),
            Script::Fail(transient()),
            Script::Fail(transient()),
        ];
        let (sd, broker) = scripted(&dp, &h, failing);
        assert_eq!(submit_idempotent(&sd, &other).await, Err(transient()));
        assert_eq!(broker.submits(), 3);
        assert_eq!(h.broker.submitted().len(), 1);

        // Відмова брокера не повторюється й рахується в серії помилок.
        let rejected = BrokerError::Rejected {
            status: 422,
            message: "x".into(),
        };
        let (sd, broker) = scripted(&dp, &h, vec![Script::Fail(rejected.clone())]);
        assert_eq!(submit_idempotent(&sd, &other).await, Err(rejected));
        assert_eq!(broker.submits(), 1);
        assert_eq!(dp.health.streak(), 1);
    }

    // ---------------------------------------------------------------- виходи

    #[tokio::test]
    async fn exit_cancels_stop_then_sells_opg() {
        let (dp, h) = setup().await;
        let (_, held) = held_aapl(&dp, &h).await;
        assert_eq!(held.stop_order_id.as_deref(), Some("ord-1-stop"));
        let exit = approved_exit("AAPL", 10);
        insert(&dp, &[&exit]).await;
        let (sd, broker) = scripted(&dp, &h, vec![]);

        let summary = run(&sd).await;

        assert_eq!(
            summary,
            SubmitSummary {
                exits: 1,
                ..SubmitSummary::default()
            }
        );
        assert_eq!(h.broker.cancels()[0], "ord-1-stop");
        // Спершу підтверджене скасування стопа, потім продаж.
        assert_eq!(
            broker.log(),
            vec![
                "cancel ord-1-stop".to_string(),
                format!("submit {}", client_order_id(&exit.id)),
            ]
        );
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
        let submitted = h.broker.submitted();
        assert_eq!(submitted.len(), 2);
        assert_eq!(submitted[1], build_exit_order(&exit, TimeInForce::Opg));
        let stored = intent(&dp, &exit.id).await;
        assert_eq!(stored.status, IntentStatus::Submitted);
        assert_eq!(stored.broker_order_id.as_deref(), Some("ord-2"));
        // Позиція лишається в обліку до виконання продажу (її зніме звірка).
        assert_eq!(dp.store.positions().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn exit_not_sent_if_cancel_fails() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        h.broker.fail_cancel("ord-1-stop");
        let exit = approved_exit("AAPL", 10);
        insert(&dp, &[&exit]).await;

        let summary = run(&dp).await;

        assert_eq!(
            summary,
            SubmitSummary {
                failed: 1,
                ..SubmitSummary::default()
            }
        );
        let stored = intent(&dp, &exit.id).await;
        assert_eq!(stored.status, IntentStatus::Failed);
        assert_eq!(stored.error.as_deref(), Some("Не вдалося скасувати стоп"));
        assert_eq!(h.broker.submitted().len(), 1, "продажу немає");
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::New);
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-1-stop"));
    }

    #[tokio::test]
    async fn exit_never_sells_more_than_broker_holds() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        // У брокера лише 7 із 10 відстежуваних акцій: звірка ставить паузу й зменшує стоп.
        h.broker.set_position(&sym("AAPL"), 7, dec!(101.00));
        let exit = approved_exit("AAPL", 10);
        insert(&dp, &[&exit]).await;

        let summary = run(&dp).await;

        assert_eq!(summary.exits, 1);
        assert!(matches!(
            dp.store.engine_mode().await.unwrap(),
            EngineMode::Paused { .. }
        ));
        let sale = h.broker.submitted().last().cloned().unwrap();
        assert_eq!(sale.client_order_id, client_order_id(&exit.id));
        assert_eq!(
            (sale.side, sale.qty, sale.tif),
            (Side::Sell, 7, TimeInForce::Opg)
        );
        // Поряд із виходом не лишилося жодного іншого відкритого продажу.
        let open = h.broker.open_orders().await.unwrap();
        let open_sells: Vec<String> = flatten(&open)
            .into_iter()
            .filter(|(o, _)| o.side == Side::Sell && o.status.is_open())
            .map(|(o, _)| o.id.clone())
            .collect();
        let sale_id = intent(&dp, &exit.id).await.broker_order_id.unwrap();
        assert_eq!(open_sells, vec![sale_id]);
    }

    #[tokio::test]
    async fn exit_without_broker_position_fails() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        h.broker.set_position(&sym("AAPL"), 0, Decimal::ZERO);
        let exit = approved_exit("AAPL", 10);
        insert(&dp, &[&exit]).await;

        let summary = run(&dp).await;

        assert_eq!(
            summary,
            SubmitSummary {
                failed: 1,
                ..SubmitSummary::default()
            }
        );
        let stored = intent(&dp, &exit.id).await;
        assert_eq!(stored.status, IntentStatus::Failed);
        assert_eq!(stored.error.as_deref(), Some("Позиції у брокера немає"));
        assert!(h.broker.submitted().iter().all(|r| r.side == Side::Buy));
    }

    #[tokio::test]
    async fn exit_after_restart_adopts_existing_sale() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        let exit = approved_exit("AAPL", 10);
        insert(&dp, &[&exit]).await;
        // Збій після скасування стопа й подання продажу, до позначки наміру.
        h.broker.cancel("ord-1-stop").await.unwrap();
        let sale = dp
            .broker
            .submit(&build_exit_order(&exit, TimeInForce::Opg))
            .await
            .unwrap();

        let summary = run(&dp).await;

        assert_eq!(summary.exits, 1);
        let stored = intent(&dp, &exit.id).await;
        assert_eq!(stored.status, IntentStatus::Submitted);
        assert_eq!(stored.broker_order_id.as_deref(), Some(sale.id.as_str()));
        assert_eq!(h.broker.submitted().len(), 2, "продаж не дублюється");
        assert_eq!(h.broker.cancels(), vec!["ord-1-stop".to_string()]);
        assert!(order_status(&h, &sale.id).await.is_open());
    }

    #[tokio::test]
    async fn rejected_sale_reprotects_position() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        let exit = approved_exit("AAPL", 10);
        insert(&dp, &[&exit]).await;
        let rejected = BrokerError::Rejected {
            status: 422,
            message: "market closed".into(),
        };
        let (sd, _) = scripted(&dp, &h, vec![Script::Fail(rejected)]);

        let summary = run(&sd).await;

        assert_eq!(
            summary,
            SubmitSummary {
                failed: 1,
                ..SubmitSummary::default()
            }
        );
        let stored = intent(&dp, &exit.id).await;
        assert_eq!(stored.status, IntentStatus::Failed);
        assert_eq!(
            stored.error.as_deref(),
            Some("Помилка брокера: order rejected (422): market closed")
        );
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
        // Стоп уже скасовано, продажу немає: позиція знову під стопом.
        let held = dp.store.positions().await.unwrap();
        let stop_id = held[0].stop_order_id.clone().unwrap();
        assert_ne!(stop_id, "ord-1-stop");
        let stop = h.broker.order(&stop_id).await.unwrap();
        assert!(stop.status.is_open());
        assert_eq!(
            (stop.side, stop.order_type, stop.qty, stop.stop_price),
            (Side::Sell, OrderType::Stop, 10, Some(dec!(95.50)))
        );
        assert!(
            h.broker
                .submitted()
                .iter()
                .all(|r| r.order_type != OrderType::Market)
        );
    }

    #[tokio::test]
    async fn unanswered_sale_is_left_to_reconcile() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        let exit = approved_exit("AAPL", 10);
        insert(&dp, &[&exit]).await;
        let failing = vec![
            Script::Fail(transient()),
            Script::Fail(transient()),
            Script::Fail(transient()),
        ];
        let (sd, broker) = scripted(&dp, &h, failing);

        let summary = run(&sd).await;

        assert_eq!(summary.exits, 1);
        assert_eq!(broker.submits(), 3);
        let stored = intent(&dp, &exit.id).await;
        assert_eq!(stored.status, IntentStatus::Submitted);
        assert_eq!(stored.broker_order_id, None);
        assert!(
            stored
                .error
                .as_deref()
                .is_some_and(|e| e.ends_with("стан ордера перевірить звірка")),
            "{:?}",
            stored.error
        );
        // Продаж міг дійти до брокера: новий стоп не подається, доки звірка не з'ясує стан.
        assert_eq!(h.broker.submitted().len(), 1);

        reconcile(&dp).await.unwrap();

        assert_eq!(intent(&dp, &exit.id).await.status, IntentStatus::Failed);
        let held = dp.store.positions().await.unwrap();
        let stop = h
            .broker
            .order(held[0].stop_order_id.as_deref().unwrap())
            .await
            .unwrap();
        assert!(stop.status.is_open());
        assert_eq!((stop.side, stop.qty), (Side::Sell, 10));
    }

    #[tokio::test]
    async fn exit_cancels_take_profit_leg_before_stop() {
        let (dp, h) = setup().await;
        let (_, order) = submit_entry(
            &dp,
            "AAPL",
            10,
            dec!(101.00),
            dec!(95.50),
            Some(dec!(115.25)),
        )
        .await;
        h.broker.fill(&order.id, dec!(101.00), 10);
        reconcile(&dp).await.unwrap();
        let exit = approved_exit("AAPL", 10);
        insert(&dp, &[&exit]).await;
        let (sd, broker) = scripted(&dp, &h, vec![]);

        let summary = run(&sd).await;

        assert_eq!(summary.exits, 1);
        // Ціль — першою: поки стоп живий, позиція під захистом.
        assert_eq!(
            broker.log(),
            vec![
                "cancel ord-1-tp".to_string(),
                "cancel ord-1-stop".to_string(),
                format!("submit {}", client_order_id(&exit.id)),
            ]
        );
        assert_eq!(order_status(&h, "ord-1-tp").await, OrderStatus::Canceled);
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
    }

    #[tokio::test]
    async fn exit_after_partial_stop_fill_sells_only_the_rest() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        // Стоп уже продав 4 шт. і ще виконується: у брокера 6.
        h.broker.fill("ord-1-stop", dec!(95.40), 4);
        let exit = approved_exit("AAPL", 10);
        insert(&dp, &[&exit]).await;

        let summary = run(&dp).await;

        assert_eq!(summary.exits, 1);
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
        let sale = h.broker.submitted().last().cloned().unwrap();
        assert_eq!((sale.side, sale.qty), (Side::Sell, 6));
    }

    #[tokio::test]
    async fn stop_filled_while_cancelling_blocks_sale_and_reprotects() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        let exit = approved_exit("AAPL", 10);
        insert(&dp, &[&exit]).await;
        let (sd, broker) = scripted(&dp, &h, vec![]);
        // Між знімком позицій і скасуванням стоп продав 4 шт.
        *broker.fill_on_cancel.lock().unwrap() = Some(("ord-1-stop".into(), dec!(95.40), 4));

        let summary = run(&sd).await;

        assert_eq!(
            summary,
            SubmitSummary {
                failed: 1,
                ..SubmitSummary::default()
            }
        );
        let stored = intent(&dp, &exit.id).await;
        assert_eq!(stored.status, IntentStatus::Failed);
        assert_eq!(
            stored.error.as_deref(),
            Some("Стоп ord-1-stop виконано до скасування (4 шт.): продаж не відправлено")
        );
        assert!(
            h.broker
                .submitted()
                .iter()
                .all(|r| r.order_type != OrderType::Market),
            "продажу за застарілим знімком немає"
        );
        // Решта (6 шт.) знову під стопом.
        let held = dp.store.positions().await.unwrap();
        let stop = h
            .broker
            .order(held[0].stop_order_id.as_deref().unwrap())
            .await
            .unwrap();
        assert!(stop.status.is_open());
        assert_eq!(
            (stop.side, stop.order_type, stop.qty),
            (Side::Sell, OrderType::Stop, 6)
        );
    }

    // ---------------------------------------------------------------- підтягування стопа

    #[tokio::test]
    async fn move_stop_failure_marks_failed_and_keeps_position() {
        let (dp, h) = setup().await;
        let (_, held) = held_aapl(&dp, &h).await;
        // Стоп у стані, який не можна замінити (звірка лишає його без змін).
        h.broker
            .set_status("ord-1-stop", OrderStatus::Other("pending_replace".into()));
        let mv = approved_move("AAPL", dec!(98.00), "ord-1-stop");
        insert(&dp, &[&mv]).await;

        let summary = run(&dp).await;

        assert_eq!(
            summary,
            SubmitSummary {
                failed: 1,
                ..SubmitSummary::default()
            }
        );
        let stored = intent(&dp, &mv.id).await;
        assert_eq!(stored.status, IntentStatus::Failed);
        assert_eq!(
            stored.error.as_deref(),
            Some(
                "Не вдалося підтягнути стоп: order rejected (422): order is not a \
                 replaceable stop"
            )
        );
        assert_eq!(dp.store.positions().await.unwrap(), vec![held]);
    }

    #[tokio::test]
    async fn move_stop_replaces_and_updates_position() {
        let (dp, h) = setup().await;
        let (_, held) = held_aapl(&dp, &h).await;
        let mv = approved_move("AAPL", dec!(98.00), "ord-1-stop");
        insert(&dp, &[&mv]).await;

        let summary = run(&dp).await;

        assert_eq!(
            summary,
            SubmitSummary {
                stop_moves: 1,
                ..SubmitSummary::default()
            }
        );
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Replaced);
        let new = h.broker.order("ord-2").await.unwrap();
        assert!(new.status.is_open());
        assert_eq!(
            (new.side, new.order_type, new.qty, new.stop_price),
            (Side::Sell, OrderType::Stop, 10, Some(dec!(98.00)))
        );
        assert_eq!(
            dp.store.positions().await.unwrap(),
            vec![TrackedPosition {
                position: Position {
                    stop_price: 98.0,
                    ..held.position.clone()
                },
                stop_order_id: Some("ord-2".into()),
                ..held.clone()
            }]
        );
        let stored = intent(&dp, &mv.id).await;
        assert_eq!(stored.status, IntentStatus::Filled);
        assert_eq!(stored.broker_order_id.as_deref(), Some("ord-2"));
        assert_eq!(
            stored_order_status(&dp, "ord-2").await.as_deref(),
            Some("new")
        );
        assert!(h.broker.cancels().is_empty());
        assert_eq!(h.broker.submitted().len(), 1);
    }

    #[tokio::test]
    async fn move_stop_uses_tracked_stop_and_never_lowers() {
        let (dp, h) = setup().await;
        held_aapl_msft(&dp, &h).await;
        // Намір створено з уже неактуальним id стопа (позицію відтоді перезахищено).
        let up = approved_move("AAPL", dec!(97.00), "ord-stale");
        let down = approved_move("MSFT", dec!(46.00), "ord-2-stop");
        insert(&dp, &[&up, &down]).await;

        let summary = run(&dp).await;

        assert_eq!(
            summary,
            SubmitSummary {
                stop_moves: 1,
                skipped: 1,
                ..SubmitSummary::default()
            }
        );
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Replaced);
        assert_eq!(order_status(&h, "ord-2-stop").await, OrderStatus::New);
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-3"));
        assert_eq!(held[0].position.stop_price, 97.0);
        assert_eq!(held[1].stop_order_id.as_deref(), Some("ord-2-stop"));
        assert_eq!(held[1].position.stop_price, 47.0);
        assert_eq!(intent(&dp, &up.id).await.status, IntentStatus::Filled);
        let down = intent(&dp, &down.id).await;
        assert_eq!(down.status, IntentStatus::Cancelled);
        assert_eq!(
            down.error.as_deref(),
            Some("Стоп не підтягнуто: новий рівень 46.00 не вище поточного 47.00")
        );
    }

    // ---------------------------------------------------------------- режими й ліміти

    #[tokio::test]
    async fn paused_moves_stops_and_exits_but_skips_entries() {
        let (dp, h) = setup().await;
        held_aapl_msft(&dp, &h).await;
        // Невідома позиція: звірка на початку кроку ставить паузу.
        h.broker.set_position(&sym("TSLA"), 5, dec!(250));
        h.data.set_latest_trade(&sym("SPY"), 101.00);
        let mv = approved_move("AAPL", dec!(98.00), "ord-1-stop");
        let exit = approved_exit("MSFT", 20);
        let entry = approved_entry("SPY", None);
        insert(&dp, &[&mv, &exit, &entry]).await;
        let mut rx = dp.bus.subscribe();

        let summary = run(&dp).await;

        assert_eq!(
            summary,
            SubmitSummary {
                stop_moves: 1,
                exits: 1,
                skipped: 1,
                ..SubmitSummary::default()
            }
        );
        assert!(matches!(
            dp.store.engine_mode().await.unwrap(),
            EngineMode::Paused { reason } if reason.starts_with("Розбіжність з брокером")
        ));
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, EngineEventDto::AutoPaused { .. }))
        );
        assert_eq!(intent(&dp, &mv.id).await.status, IntentStatus::Filled);
        assert_eq!(intent(&dp, &exit.id).await.status, IntentStatus::Submitted);
        assert_eq!(intent(&dp, &entry.id).await.status, IntentStatus::Approved);
        let submitted = h.broker.submitted();
        assert_eq!(submitted.len(), 3, "два входи стенда й продаж MSFT");
        assert_eq!(
            (
                submitted[2].symbol.as_str(),
                submitted[2].side,
                submitted[2].qty
            ),
            ("MSFT", Side::Sell, 20)
        );
        assert!(submitted.iter().all(|r| r.symbol != sym("SPY")));
        let journal = journal_messages(&dp).await;
        assert!(
            journal
                .iter()
                .any(|m| m.starts_with("Входи не відправлено: Пауза: Розбіжність з брокером")),
            "{journal:?}"
        );
    }

    #[tokio::test]
    async fn killed_sends_nothing() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        h.data.set_latest_trade(&sym("SPY"), 101.00);
        let mv = approved_move("AAPL", dec!(98.00), "ord-1-stop");
        let exit = approved_exit("AAPL", 10);
        let entry = approved_entry("SPY", None);
        insert(&dp, &[&mv, &exit, &entry]).await;
        dp.store
            .set_engine_mode(&killed(), dp.clock.now())
            .await
            .unwrap();
        let (sd, broker) = scripted(&dp, &h, vec![]);

        let summary = run(&sd).await;

        assert_eq!(
            summary,
            SubmitSummary {
                skipped: 3,
                ..SubmitSummary::default()
            }
        );
        assert!(broker.log().is_empty(), "{:?}", broker.log());
        assert_eq!(h.broker.submitted().len(), 1);
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::New);
        for i in [&mv, &exit, &entry] {
            assert_eq!(intent(&dp, &i.id).await.status, IntentStatus::Approved);
        }
        assert_eq!(dp.store.engine_mode().await.unwrap(), killed());
        assert_eq!(
            journal_messages(&dp).await[0],
            "Відправка перед відкриттям (сесія 2026-11-05): входів 0, виходів 0, підтягнуто \
             стопів 0, помилок 0, пропущено 3; зупинено: Аварійна зупинка"
        );
    }

    #[tokio::test]
    async fn price_sanity_fails_far_limit() {
        let (dp, h) = setup().await;
        h.data.set_latest_trade(&sym("AAPL"), 90.0);
        let entry = approved_entry("AAPL", None);
        insert(&dp, &[&entry]).await;

        let summary = run(&dp).await;

        assert_eq!(
            summary,
            SubmitSummary {
                failed: 1,
                ..SubmitSummary::default()
            }
        );
        let stored = intent(&dp, &entry.id).await;
        assert_eq!(stored.status, IntentStatus::Failed);
        assert_eq!(
            stored.error.as_deref(),
            Some("Ціна поза межами ±5% від останньої")
        );
        assert!(h.broker.submitted().is_empty());
    }

    #[tokio::test]
    async fn price_sanity_uses_stored_close_or_fails_without_price() {
        let (dp, h) = setup().await;
        // MSFT: угод немає, останній close у сховищі 100 — ліміт 101 у межах ±5%.
        dp.store
            .upsert_bars(&sym("MSFT"), &flat_bars(start_date(), N_BARS))
            .await
            .unwrap();
        let msft = approved_entry("MSFT", None);
        // SPY: ні угод, ні барів у сховищі.
        let spy = approved_entry("SPY", None);
        insert(&dp, &[&msft, &spy]).await;

        let summary = run(&dp).await;

        assert_eq!(
            summary,
            SubmitSummary {
                entries: 1,
                failed: 1,
                ..SubmitSummary::default()
            }
        );
        assert_eq!(intent(&dp, &msft.id).await.status, IntentStatus::Submitted);
        let spy = intent(&dp, &spy.id).await;
        assert_eq!(spy.status, IntentStatus::Failed);
        assert_eq!(spy.error.as_deref(), Some("Немає останньої ціни"));
        assert_eq!(h.broker.submitted().len(), 1);
    }

    #[tokio::test]
    async fn max_orders_per_day_pauses() {
        let (dp, h) = setup().await;
        let aapl = approved_entry("AAPL", None);
        let msft = approved_entry("MSFT", None);
        insert(&dp, &[&aapl, &msft]).await;
        h.data.set_latest_trade(&sym("AAPL"), 101.00);
        h.data.set_latest_trade(&sym("MSFT"), 101.00);
        let cfg = EngineConfig {
            risk: RiskConfig {
                max_orders_per_day: 1,
                ..RiskConfig::default()
            },
            ..EngineConfig::default()
        };
        let mut rx = dp.bus.subscribe();

        let summary = run_with(&dp, &cfg).await;

        assert_eq!(
            summary,
            SubmitSummary {
                entries: 1,
                skipped: 1,
                ..SubmitSummary::default()
            }
        );
        assert_eq!(intent(&dp, &aapl.id).await.status, IntentStatus::Submitted);
        assert_eq!(intent(&dp, &msft.id).await.status, IntentStatus::Approved);
        assert_eq!(h.broker.submitted().len(), 1);
        let reason = "Ліміт ордерів на день";
        assert_eq!(
            dp.store.engine_mode().await.unwrap(),
            EngineMode::Paused {
                reason: reason.into()
            }
        );
        assert!(drain(&mut rx).contains(&EngineEventDto::AutoPaused {
            reason: reason.into()
        }));
    }

    #[tokio::test]
    async fn three_broker_errors_autopause() {
        let (dp, h) = setup().await;
        let entries: Vec<Intent> = ["AAPL", "MSFT", "NVDA", "SPY"]
            .into_iter()
            .map(|s| approved_entry(s, None))
            .collect();
        for e in &entries {
            insert(&dp, &[e]).await;
            h.data.set_latest_trade(&e.symbol, 101.00);
        }
        let rejected = || {
            Script::Fail(BrokerError::Rejected {
                status: 403,
                message: "insufficient buying power".into(),
            })
        };
        let (sd, broker) = scripted(&dp, &h, vec![rejected(), rejected(), rejected()]);
        let mut rx = dp.bus.subscribe();

        let summary = run(&sd).await;

        assert_eq!(
            summary,
            SubmitSummary {
                failed: 3,
                skipped: 1,
                ..SubmitSummary::default()
            }
        );
        assert_eq!(broker.submits(), 3);
        assert!(h.broker.submitted().is_empty());
        for e in &entries[..3] {
            assert_eq!(intent(&dp, &e.id).await.status, IntentStatus::Failed);
        }
        assert_eq!(
            intent(&dp, &entries[3].id).await.status,
            IntentStatus::Approved
        );
        let reason = "3 помилки брокера поспіль";
        assert_eq!(
            dp.store.engine_mode().await.unwrap(),
            EngineMode::Paused {
                reason: reason.into()
            }
        );
        assert!(drain(&mut rx).contains(&EngineEventDto::AutoPaused {
            reason: reason.into()
        }));
    }

    // ---------------------------------------------------------------- дедлайни

    #[tokio::test]
    async fn expire_pending_at_deadline() {
        let (dp, _h) = setup().await;
        let entry = || entry_kind(10, dec!(101.00), dec!(95.50), None);
        let pending = |s: &str| {
            intent_with(
                s,
                IntentTag::Entry,
                entry(),
                IntentStatus::PendingApproval,
                None,
            )
        };
        let today = pending("AAPL");
        let earlier = on(d(2026, 11, 4), pending("MSFT"));
        // Наміри наступної сесії (щойно створені аналізом) не чіпаються.
        let next = on(d(2026, 11, 6), pending("SPY"));
        let approved = intent_with(
            "NVDA",
            IntentTag::Entry,
            entry(),
            IntentStatus::Approved,
            None,
        );
        insert(&dp, &[&today, &earlier, &next, &approved]).await;

        let n = expire_pending(&dp, session()).await.unwrap();

        assert_eq!(n, 2);
        for i in [&today, &earlier] {
            let stored = intent(&dp, &i.id).await;
            assert_eq!(stored.status, IntentStatus::Expired, "{}", i.id);
            assert_eq!(stored.error.as_deref(), Some("Не схвалено до дедлайну"));
        }
        assert_eq!(
            intent(&dp, &next.id).await.status,
            IntentStatus::PendingApproval
        );
        assert_eq!(
            intent(&dp, &approved.id).await.status,
            IntentStatus::Approved
        );
        assert_eq!(
            journal_messages(&dp).await,
            vec!["Дедлайн схвалення минув (сесія 2026-11-05): прострочено намірів 2".to_string()]
        );
    }

    #[tokio::test]
    async fn mark_missed_expires_approved_and_pending() {
        let (dp, _h) = setup().await;
        let entry = || entry_kind(10, dec!(101.00), dec!(95.50), None);
        let pending = intent_with(
            "AAPL",
            IntentTag::Entry,
            entry(),
            IntentStatus::PendingApproval,
            None,
        );
        let approved = approved_entry("MSFT", None);
        let old_exit = on(d(2026, 11, 4), approved_exit("SPY", 5));
        let next = on(d(2026, 11, 6), approved_entry("NVDA", None));
        let submitted = intent_with(
            "XOM",
            IntentTag::Entry,
            entry(),
            IntentStatus::Submitted,
            Some("ord-9"),
        );
        insert(&dp, &[&pending, &approved, &old_exit, &next, &submitted]).await;

        let n = mark_missed(&dp, session()).await.unwrap();

        assert_eq!(n, 3);
        for i in [&pending, &approved, &old_exit] {
            let stored = intent(&dp, &i.id).await;
            assert_eq!(stored.status, IntentStatus::Expired, "{}", i.id);
            assert_eq!(stored.error.as_deref(), Some("Вікно відправки минуло"));
        }
        assert_eq!(intent(&dp, &next.id).await.status, IntentStatus::Approved);
        assert_eq!(
            intent(&dp, &submitted.id).await.status,
            IntentStatus::Submitted
        );
        assert_eq!(
            journal_messages(&dp).await,
            vec!["Вікно відправки минуло (сесія 2026-11-05): прострочено намірів 3".to_string()]
        );
    }

    // ---------------------------------------------------------------- ручне закриття

    #[tokio::test]
    async fn close_position_now_cancels_stop_and_sells_day() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        h.clock.set(et(session(), 10, 30));

        let closed = close_position_now(&dp, &sym("AAPL")).await.unwrap();

        assert_eq!(closed.id, "20261105-trend_pullback-AAPL-exit-manual");
        assert_eq!(closed.session_date, session());
        assert_eq!(closed.strategy.as_str(), "trend_pullback");
        assert!(matches!(closed.kind, IntentKind::Exit { qty: 10, .. }));
        assert_eq!(closed.status, IntentStatus::Submitted);
        assert_eq!(closed.broker_order_id.as_deref(), Some("ord-2"));
        assert_eq!(h.broker.cancels(), vec!["ord-1-stop".to_string()]);
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
        let submitted = h.broker.submitted();
        assert_eq!(submitted.len(), 2);
        assert_eq!(
            submitted[1],
            OrderRequest {
                client_order_id: "fond-20261105-trend_pullback-AAPL-exit-manual".into(),
                symbol: sym("AAPL"),
                side: Side::Sell,
                qty: 10,
                order_type: OrderType::Market,
                limit_price: None,
                stop_price: None,
                tif: TimeInForce::Day,
                class: OrderClass::Simple,
                stop_loss: None,
                take_profit: None,
            }
        );
        assert_eq!(intent(&dp, &closed.id).await, closed);
    }

    #[tokio::test]
    async fn close_position_now_cancels_analysis_exit() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        let planned = approved_exit("AAPL", 10);
        insert(&dp, &[&planned]).await;
        // Операторська дія працює в будь-якому режимі, зокрема після аварійної зупинки.
        dp.store
            .set_engine_mode(&killed(), dp.clock.now())
            .await
            .unwrap();
        // Субота: ручне закриття належить наступній сесії (понеділок).
        h.clock.set(et(d(2026, 11, 7), 12, 0));

        let closed = close_position_now(&dp, &sym("AAPL")).await.unwrap();

        assert_eq!(closed.id, "20261109-trend_pullback-AAPL-exit-manual");
        assert_eq!(closed.session_date, d(2026, 11, 9));
        assert_eq!(closed.status, IntentStatus::Submitted);
        let planned = intent(&dp, &planned.id).await;
        assert_eq!(planned.status, IntentStatus::Cancelled);
        assert_eq!(planned.error.as_deref(), Some("Позицію закривають вручну"));
        let sells: Vec<OrderRequest> = h
            .broker
            .submitted()
            .into_iter()
            .filter(|r| r.side == Side::Sell)
            .collect();
        assert_eq!(sells.len(), 1);
        assert_eq!(
            (
                sells[0].tif,
                sells[0].qty,
                sells[0].client_order_id.as_str()
            ),
            (
                TimeInForce::Day,
                10,
                "fond-20261109-trend_pullback-AAPL-exit-manual"
            )
        );
        let journal = journal_messages(&dp).await;
        let note = format!(
            "Намір на вихід {} скасовано: позицію закривають вручну",
            planned.id
        );
        assert!(journal.contains(&note), "{journal:?}");
        assert_eq!(dp.store.engine_mode().await.unwrap(), killed());

        // Повторне натискання не продає вдруге.
        let again = close_position_now(&dp, &sym("AAPL")).await.unwrap();
        assert_eq!(again, closed);
        assert_eq!(h.broker.submitted().len(), 2);
    }
}
