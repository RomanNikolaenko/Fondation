//! Відправка ордерів перед відкриттям (09:00–09:25 ET), дедлайн схвалення, пропущене вікно
//! відправки та ручне закриття позиції.
//!
//! Повторна відправка безпечна: `client_order_id` ордера виводиться з id наміру, тож прогін після
//! збою чи перезапуску знаходить уже поданий ордер замість дубля. Вихід ніколи не продає більше,
//! ніж є у брокера: спершу з підтвердженням скасовуються інші продажі тікера й захисний стоп, а
//! обсяг обмежується знімком позицій брокера. Якщо відповіді брокера на відправку так і не
//! отримано, намір лишається `Submitted` без id ордера: його стан з'ясує звірка.
//!
//! Тут — черга відправки (`run_pre_open_submit`), gate входів і спільні допоміжні функції;
//! відправка кожного виду наміру — у підмодулях.

mod entry;
mod exit;
mod expire;
mod manual;
mod orders;
mod rebalance;
mod stop;

use std::collections::HashMap;

use chrono::NaiveDate;
use fond_api::config::EngineConfig;
use fond_core::error::BrokerError;
use fond_core::ids::client_order_id;
use fond_core::intent::{Intent, IntentKind, IntentStatus};
use fond_core::orders::{Side, TimeInForce};
use fond_core::portfolio::{BrokerPosition, TrackedPosition};
use fond_core::types::{EngineMode, Symbol};
use fond_store::EventKind;
use serde_json::json;

use super::kill::pause;
use super::reconcile::{broker_failure, observe, reconcile};
use super::{CycleError, KILLED_REASON, entries_block_reason, journal};
use crate::deps::Deps;
use entry::send_entry;
use exit::send_exit;
use rebalance::send_rebalance;
use stop::move_stop;

pub use expire::{expire_pending, mark_missed};
pub use manual::close_position_now;

/// Причина автопаузи: досягнуто `max_orders_per_day`.
pub const ORDER_LIMIT_REASON: &str = "Ліміт ордерів на день";
/// Причина автопаузи: серія помилок брокера.
pub const BROKER_ERRORS_REASON: &str = "3 помилки брокера поспіль";
/// Скільки помилок брокера поспіль зупиняють відправку.
const BROKER_ERROR_STREAK: u32 = 3;
const NO_LAST_PRICE: &str = "Немає останньої ціни";

/// Підсумок кроку відправки: скільки намірів відправлено (за видами), скільки завершилось
/// помилкою і скільки лишилось без змін (заблоковано, зупинено або не потрібно).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubmitSummary {
    pub entries: usize,
    pub exits: usize,
    pub stop_moves: usize,
    pub rebalances: usize,
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
            (Outcome::Sent, IntentKind::Rebalance { .. }) => &mut self.rebalances,
        };
        *slot += 1;
    }
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

    // Порядок: підтягування стопів, виходи, продажі ребалансу (звільняють кеш),
    // купівлі ребалансу, входи (стабільне сортування зберігає черговість
    // сховища всередині виду).
    let mut queue: Vec<&Intent> = approved.iter().collect();
    queue.sort_by_key(|i| match i.kind {
        IntentKind::MoveStop { .. } => 0,
        IntentKind::Exit { .. } => 1,
        IntentKind::Rebalance {
            side: Side::Sell, ..
        } => 2,
        IntentKind::Rebalance {
            side: Side::Buy, ..
        } => 3,
        IntentKind::Entry { .. } => 4,
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
            // Купівлі ребалансу йдуть у тому самому gate, що й входи: спільний
            // ліміт ордерів на день і перевірки режиму/лімітів збитку.
            IntentKind::Entry { .. }
            | IntentKind::Rebalance {
                side: Side::Buy, ..
            } => {
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
                        let outcome = match &intent.kind {
                            IntentKind::Entry { .. } => send_entry(d, cfg, intent).await?,
                            _ => send_rebalance(d, cfg, intent).await?,
                        };
                        if outcome == Outcome::Sent {
                            *sent += 1;
                        }
                        outcome
                    }
                    _ => Outcome::Skipped,
                }
            }
            // Продаж ребалансу не блокується gate входів: звільняє кеш і не додає
            // експозицію. Обсяг не більший за позицію за побудовою (`compute_deltas`
            // продає не більше наявного), на тікер — один намір за сесію.
            IntentKind::Rebalance {
                side: Side::Sell, ..
            } => send_rebalance(d, cfg, intent).await?,
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
             стопів {}, ребалансу {}, помилок {}, пропущено {}{tail}",
            s.entries, s.exits, s.stop_moves, s.rebalances, s.failed, s.skipped
        ),
        json!({
            "session": session,
            "entries": s.entries,
            "exits": s.exits,
            "stop_moves": s.stop_moves,
            "rebalances": s.rebalances,
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
        IntentKind::Rebalance { .. } => "Ребаланс",
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

#[cfg(test)]
mod tests;
