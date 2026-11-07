//! Звірка з брокером (щогодини під час сесії): виконання відправлених намірів, захисні стопи,
//! розбіжності позицій і внутрішньоденний знімок капіталу.
//!
//! Брокер — джерело правди про ордери. Звірка ідемпотентна: повторний прогін після збою чи
//! перезапуску доводить стан до того самого результату й не дублює ордерів.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use fond_api::events::EngineEventDto;
use fond_core::calendar::utc_to_et;
use fond_core::error::BrokerError;
use fond_core::ids::client_order_id;
use fond_core::intent::{Intent, IntentKind, IntentStatus};
use fond_core::orders::{
    BrokerOrder, OrderClass, OrderRequest, OrderStatus, OrderType, Side, TimeInForce,
};
use fond_core::portfolio::{BrokerPosition, Position, TrackedPosition};
use fond_core::price::{to_f64, to_price};
use fond_core::types::Symbol;
use fond_store::{EquityKind, EquitySnapshot, EventKind};
use rust_decimal::Decimal;
use serde_json::json;

use super::analysis::sector_of;
use super::kill::pause_changed;
use super::{CycleError, journal};
use crate::deps::Deps;

/// Розбіжність між відстежуваними позиціями й позиціями брокера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    UnknownBrokerPosition {
        symbol: Symbol,
        qty: i64,
    },
    MissingBrokerPosition {
        symbol: Symbol,
    },
    QtyDiffers {
        symbol: Symbol,
        local: u32,
        broker: i64,
    },
}

impl Mismatch {
    /// Опис українською (журнал і причина паузи).
    pub fn describe(&self) -> String {
        match self {
            Self::UnknownBrokerPosition { symbol, qty } => {
                format!("{symbol}: у брокера {qty} шт., локально позиції немає")
            }
            Self::MissingBrokerPosition { symbol } => {
                format!("{symbol}: локально позиція є, у брокера немає")
            }
            Self::QtyDiffers {
                symbol,
                local,
                broker,
            } => format!("{symbol}: локально {local} шт., у брокера {broker} шт."),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub orders_updated: usize,
    pub entries_filled: Vec<Symbol>,
    pub exits_filled: Vec<Symbol>,
    pub stops_hit: Vec<Symbol>,
    pub reprotected: Vec<Symbol>,
    pub mismatches: Vec<Mismatch>,
}

/// Записує результат виклику брокера в `BrokerHealth`. `NotFound` і дубль `client_order_id` —
/// змістовні відповіді брокера (ідемпотентні перевірки), а не збій зв'язку.
pub(crate) fn observe<T>(d: &Deps, r: Result<T, BrokerError>) -> Result<T, BrokerError> {
    match &r {
        Ok(_) | Err(BrokerError::NotFound | BrokerError::DuplicateClientOrderId) => {
            d.health.record_ok();
        }
        Err(_) => {
            d.health.record_err();
        }
    }
    r
}

/// Ордер і його батьківський ордер (`None` для верхнього рівня).
pub(crate) type WithParent<'a> = (&'a BrokerOrder, Option<&'a BrokerOrder>);

/// Ордери разом із ногами (рекурсивно) одним списком; батьківський іде перед своїми ногами.
pub(crate) fn flatten(orders: &[BrokerOrder]) -> Vec<WithParent<'_>> {
    fn walk<'a>(
        o: &'a BrokerOrder,
        parent: Option<&'a BrokerOrder>,
        out: &mut Vec<WithParent<'a>>,
    ) {
        out.push((o, parent));
        for leg in &o.legs {
            walk(leg, Some(o), out);
        }
    }
    let mut out = Vec::new();
    for o in orders {
        walk(o, None, &mut out);
    }
    out
}

/// Помилка брокера, що перериває крок: подія в журналі, далі — помилка нагору.
pub(crate) async fn broker_failure<T>(
    d: &Deps,
    message: &str,
    e: BrokerError,
) -> Result<T, CycleError> {
    journal(
        d,
        EventKind::Error,
        None,
        None,
        format!("{message}: {e}"),
        json!({ "error": e.to_string() }),
    )
    .await?;
    Err(e.into())
}

pub(crate) fn side_name(s: Side) -> &'static str {
    match s {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}

/// Стан ордера українською (для журналу).
pub(crate) fn status_text(s: &OrderStatus) -> String {
    let text = match s {
        OrderStatus::New => "новий",
        OrderStatus::Accepted => "прийнятий",
        OrderStatus::PendingNew => "очікує прийняття",
        OrderStatus::Held => "утримується",
        OrderStatus::PartiallyFilled => "виконаний частково",
        OrderStatus::Filled => "виконаний",
        OrderStatus::DoneForDay => "завершений на день",
        OrderStatus::Canceled => "скасований",
        OrderStatus::Expired => "строк дії минув",
        OrderStatus::Rejected => "відхилений",
        OrderStatus::Replaced => "замінений",
        OrderStatus::Other(raw) => return raw.clone(),
    };
    text.to_string()
}

fn price_text(p: Option<Decimal>) -> String {
    p.map(|x| x.round_dp(4).to_string()).unwrap_or_default()
}

/// Перша (у глибину) нога-стоп ордера.
fn stop_leg(o: &BrokerOrder) -> Option<&BrokerOrder> {
    o.legs.iter().find_map(|l| {
        if l.order_type == OrderType::Stop {
            Some(l)
        } else {
            stop_leg(l)
        }
    })
}

/// Робочий захисний стоп позиції: відкритий Sell Stop на весь її обсяг. Нога, що ще
/// утримується (`Held`, батьківський ордер не виконано), нічого не захищає.
fn protects(o: &BrokerOrder, p: &TrackedPosition) -> bool {
    o.symbol == p.position.symbol
        && o.side == Side::Sell
        && o.order_type == OrderType::Stop
        && o.status.is_open()
        && o.status != OrderStatus::Held
        && o.qty == p.position.qty
}

/// Залишок позиції після продажу `sold` шт. ордером на `planned` шт. Якщо позицію вже
/// зменшено (повторна обробка після збою: `held + sold == planned`), вона не змінюється.
fn remaining_after_sale(held: u32, sold: u32, planned: u32) -> u32 {
    if sold > 0 && held.checked_add(sold) == Some(planned) {
        held
    } else {
        held.saturating_sub(sold)
    }
}

fn with_qty(p: &TrackedPosition, qty: u32) -> TrackedPosition {
    TrackedPosition {
        position: Position {
            qty,
            ..p.position.clone()
        },
        ..p.clone()
    }
}

/// Стан одного прогону звірки.
#[derive(Default)]
struct Pass {
    report: ReconcileReport,
    /// Зміна позиції брокера від ще відкритих, але вже частково виконаних ордерів.
    inflight: HashMap<Symbol, i64>,
    /// Позиції, чий стоп у кроці 2 не перевіряється: відкритий вихід або вже оброблені.
    skip_stop_check: HashSet<Symbol>,
    /// Тікери з невідомим станом (запит до брокера не вдався): не порівнюються.
    uncertain: HashSet<Symbol>,
}

impl Pass {
    fn add_inflight(&mut self, s: &Symbol, delta: i64) {
        *self.inflight.entry(s.clone()).or_default() += delta;
    }
}

/// Звірка: (1) відправлені входи й виходи, (2) стопи позицій без відкритого виходу,
/// (3) порівняння з позиціями брокера — розбіжність ставить паузу, (4) знімок капіталу.
///
/// Збій запиту по окремому наміру чи стопу записується в журнал, і звірка йде далі;
/// збій отримання позицій чи рахунку перериває її (`CycleError::Broker`).
pub async fn reconcile(d: &Deps) -> Result<ReconcileReport, CycleError> {
    let mut pass = Pass::default();

    // 1. Підтягування стопа (MoveStop) завершує крок відправки, тут — лише входи й виходи.
    for intent in d
        .store
        .intents_with_status(&[IntentStatus::Submitted])
        .await?
    {
        if matches!(
            intent.kind,
            IntentKind::Entry { .. } | IntentKind::Exit { .. }
        ) {
            submitted_intent(d, &intent, &mut pass).await?;
        }
    }

    // 2.
    for tp in d.store.positions().await? {
        if !pass.skip_stop_check.contains(&tp.position.symbol) {
            check_stop(d, &tp, &mut pass).await?;
        }
    }

    // 3.
    let tracked = d.store.positions().await?;
    let broker = match observe(d, d.broker.positions().await) {
        Ok(p) => p,
        Err(e) => {
            return broker_failure(d, "Звірку перервано: не вдалося отримати позиції", e).await;
        }
    };
    let mismatches = compare(&tracked, &broker, &pass);
    if !mismatches.is_empty() {
        let details: Vec<String> = mismatches.iter().map(Mismatch::describe).collect();
        let reason = format!("Розбіжність з брокером: {}", details.join("; "));
        // Рушій уже не працює: пауза не ставиться, але розбіжність однаково в журналі.
        if !pause_changed(d, &reason).await? {
            journal(
                d,
                EventKind::Reconcile,
                None,
                None,
                reason,
                json!({ "mismatches": details }),
            )
            .await?;
        }
    }
    pass.report.mismatches = mismatches;

    // 4.
    let account = match observe(d, d.broker.account().await) {
        Ok(a) => a,
        Err(e) => {
            return broker_failure(d, "Звірку перервано: не вдалося отримати рахунок", e).await;
        }
    };
    let now = d.clock.now();
    d.store
        .insert_equity(&EquitySnapshot {
            at: now,
            kind: EquityKind::Intraday,
            session_date: utc_to_et(now).date_naive(),
            equity: to_f64(account.equity),
            cash: to_f64(account.cash),
        })
        .await?;
    d.store.set_last_reconcile_at(now).await?;
    summarize(d, &pass.report).await?;
    Ok(pass.report)
}

/// Розбіжності за кількістю. Очікувана позиція брокера — відстежувана плюс часткові
/// виконання ще відкритих ордерів; тікери з невідомим станом пропускаються.
fn compare(tracked: &[TrackedPosition], broker: &[BrokerPosition], pass: &Pass) -> Vec<Mismatch> {
    let local: BTreeMap<&Symbol, u32> = tracked
        .iter()
        .map(|p| (&p.position.symbol, p.position.qty))
        .collect();
    let remote: BTreeMap<&Symbol, i64> = broker
        .iter()
        .filter(|p| p.qty != 0)
        .map(|p| (&p.symbol, p.qty))
        .collect();
    let symbols: BTreeSet<&Symbol> = local.keys().chain(remote.keys()).copied().collect();
    let mut out = Vec::new();
    for s in symbols.into_iter().filter(|s| !pass.uncertain.contains(*s)) {
        let delta = pass.inflight.get(s).copied().unwrap_or(0);
        let symbol = s.clone();
        let found = match (local.get(s).copied(), remote.get(s).copied()) {
            (Some(local), Some(broker)) if i64::from(local) + delta != broker => {
                Some(Mismatch::QtyDiffers {
                    symbol,
                    local,
                    broker,
                })
            }
            (Some(local), None) if i64::from(local) + delta != 0 => {
                Some(Mismatch::MissingBrokerPosition { symbol })
            }
            (None, Some(qty)) if qty != delta => {
                Some(Mismatch::UnknownBrokerPosition { symbol, qty })
            }
            _ => None,
        };
        out.extend(found);
    }
    out
}

async fn summarize(d: &Deps, r: &ReconcileReport) -> Result<(), CycleError> {
    let changes = r.entries_filled.len()
        + r.exits_filled.len()
        + r.stops_hit.len()
        + r.reprotected.len()
        + r.mismatches.len();
    if changes == 0 {
        return Ok(());
    }
    let names = |v: &[Symbol]| v.iter().map(Symbol::to_string).collect::<Vec<_>>();
    journal(
        d,
        EventKind::Reconcile,
        None,
        None,
        format!(
            "Звірка: виконано входів {}, виходів {}, спрацювало стопів {}, відновлено стопів {}, \
             розбіжностей {}",
            r.entries_filled.len(),
            r.exits_filled.len(),
            r.stops_hit.len(),
            r.reprotected.len(),
            r.mismatches.len()
        ),
        json!({
            "orders_updated": r.orders_updated,
            "entries_filled": names(&r.entries_filled),
            "exits_filled": names(&r.exits_filled),
            "stops_hit": names(&r.stops_hit),
            "reprotected": names(&r.reprotected),
            "mismatches": r.mismatches.iter().map(Mismatch::describe).collect::<Vec<_>>(),
        }),
    )
    .await
}

/// Крок 1 для одного наміру: ордер за `client_order_id(intent.id)` і його наслідки.
async fn submitted_intent(d: &Deps, intent: &Intent, pass: &mut Pass) -> Result<(), CycleError> {
    let sym = &intent.symbol;
    let cid = client_order_id(&intent.id);
    let order = match observe(d, d.broker.order_by_client_id(&cid).await) {
        Ok(Some(o)) => o,
        Ok(None) => {
            let why = "Ордер не знайдено у брокера";
            d.store
                .update_intent(
                    &intent.id,
                    IntentStatus::Failed,
                    d.clock.now(),
                    None,
                    Some(why),
                )
                .await?;
            journal(
                d,
                EventKind::Error,
                Some(sym),
                Some(&intent.id),
                format!("{why}: {cid}"),
                json!({ "client_order_id": cid }),
            )
            .await?;
            return Ok(());
        }
        Err(e) => {
            journal(
                d,
                EventKind::Error,
                Some(sym),
                Some(&intent.id),
                format!("Звірка: не вдалося перевірити ордер {cid}: {e}"),
                json!({ "client_order_id": cid, "error": e.to_string() }),
            )
            .await?;
            pass.skip_stop_check.insert(sym.clone());
            pass.uncertain.insert(sym.clone());
            return Ok(());
        }
    };
    d.store.upsert_order(&order, d.clock.now()).await?;
    pass.report.orders_updated += 1;
    match intent.kind {
        IntentKind::Entry { .. } => entry_order(d, intent, &order, pass).await,
        IntentKind::Exit { qty, .. } => exit_order(d, intent, qty, &order, pass).await,
        IntentKind::MoveStop { .. } => Ok(()),
    }
}

async fn entry_order(
    d: &Deps,
    intent: &Intent,
    order: &BrokerOrder,
    pass: &mut Pass,
) -> Result<(), CycleError> {
    let IntentKind::Entry {
        limit_price,
        stop_price,
        take_profit,
        ..
    } = &intent.kind
    else {
        return Ok(());
    };
    let sym = &intent.symbol;
    if !order.status.is_terminal() {
        if order.filled_qty > 0 {
            pass.add_inflight(sym, i64::from(order.filled_qty));
        }
        return Ok(());
    }
    let now = d.clock.now();
    if order.filled_qty == 0 {
        let why = format!("Вхід не виконано: ордер {}", status_text(&order.status));
        d.store
            .update_intent(
                &intent.id,
                IntentStatus::Cancelled,
                now,
                Some(&order.id),
                Some(&why),
            )
            .await?;
        journal(
            d,
            EventKind::Order,
            Some(sym),
            Some(&intent.id),
            format!("{why} ({})", order.id),
            json!({ "order_id": order.id, "status": order.status }),
        )
        .await?;
        return Ok(());
    }

    // Виконано повністю або частково (залишок скасовано чи прострочено).
    let price = order.filled_avg_price.unwrap_or(*limit_price);
    let filled_at = order.filled_at.unwrap_or(now);
    let universe = d.store.universe(true).await?;
    let sector = sector_of(universe.iter().find(|e| e.symbol == *sym));
    let leg = stop_leg(order);
    let previous = d
        .store
        .positions()
        .await?
        .into_iter()
        .find(|p| p.position.symbol == *sym);
    // Повторна обробка того самого наміру (збій до позначки Filled) зберігає відновлений стоп.
    let stop_order_id = match previous {
        Some(p) if p.intent_id == intent.id => p.stop_order_id,
        Some(p) => {
            journal(
                d,
                EventKind::Error,
                Some(sym),
                Some(&intent.id),
                format!(
                    "Позиція {sym} уже відстежувалась (намір {}); її замінено новим входом",
                    p.intent_id
                ),
                json!({ "previous_intent": p.intent_id, "previous_qty": p.position.qty }),
            )
            .await?;
            leg.map(|l| l.id.clone())
        }
        None => leg.map(|l| l.id.clone()),
    };
    let tp = TrackedPosition {
        position: Position {
            symbol: sym.clone(),
            strategy: intent.strategy.clone(),
            qty: order.filled_qty,
            entry_price: to_f64(price),
            entry_date: utc_to_et(filled_at).date_naive(),
            stop_price: to_f64(*stop_price),
            take_profit: take_profit.map(to_f64),
            sector,
        },
        stop_order_id,
        intent_id: intent.id.clone(),
    };
    // Позиція зберігається раніше за позначку наміру: збій посередині не губить виконання.
    d.store.upsert_position(&tp, now).await?;
    pass.skip_stop_check.insert(sym.clone());
    reprotect(d, &tp, leg, pass).await?;
    d.store
        .update_intent(&intent.id, IntentStatus::Filled, now, Some(&order.id), None)
        .await?;
    let price = price_text(Some(price));
    let partial = if order.filled_qty < order.qty {
        format!(" (частково: {} з {})", order.filled_qty, order.qty)
    } else {
        String::new()
    };
    journal(
        d,
        EventKind::Fill,
        Some(sym),
        Some(&intent.id),
        format!(
            "Вхід виконано: {} шт. за {price}{partial}",
            order.filled_qty
        ),
        json!({
            "order_id": order.id,
            "qty": order.filled_qty,
            "ordered": order.qty,
            "price": price,
        }),
    )
    .await?;
    d.bus.publish(EngineEventDto::OrderFilled {
        symbol: sym.to_string(),
        side: side_name(order.side).into(),
        qty: order.filled_qty,
        price,
    });
    pass.report.entries_filled.push(sym.clone());
    Ok(())
}

async fn exit_order(
    d: &Deps,
    intent: &Intent,
    planned: u32,
    order: &BrokerOrder,
    pass: &mut Pass,
) -> Result<(), CycleError> {
    let sym = &intent.symbol;
    if !order.status.is_terminal() {
        // Вихід ще виконується: стоп не відновлюється, щоб не задвоїти продаж.
        pass.skip_stop_check.insert(sym.clone());
        if order.filled_qty > 0 {
            pass.add_inflight(sym, -i64::from(order.filled_qty));
        }
        return Ok(());
    }
    let now = d.clock.now();
    let held = d
        .store
        .positions()
        .await?
        .into_iter()
        .find(|p| p.position.symbol == *sym);
    if order.filled_qty == 0 {
        let why = "Вихід не виконано";
        d.store
            .update_intent(
                &intent.id,
                IntentStatus::Cancelled,
                now,
                Some(&order.id),
                Some(why),
            )
            .await?;
        journal(
            d,
            EventKind::Order,
            Some(sym),
            Some(&intent.id),
            format!("{why}: ордер {} {}", order.id, status_text(&order.status)),
            json!({ "order_id": order.id, "status": order.status }),
        )
        .await?;
        if let Some(tp) = held {
            pass.skip_stop_check.insert(sym.clone());
            reprotect(d, &tp, None, pass).await?;
        }
        return Ok(());
    }

    let sold = order.filled_qty;
    if let Some(tp) = held {
        let remaining = remaining_after_sale(tp.position.qty, sold, planned);
        if remaining == 0 {
            d.store.remove_position(sym).await?;
        } else {
            let rest = with_qty(&tp, remaining);
            d.store.upsert_position(&rest, now).await?;
            pass.skip_stop_check.insert(sym.clone());
            reprotect(d, &rest, None, pass).await?;
        }
    }
    d.store
        .update_intent(&intent.id, IntentStatus::Filled, now, Some(&order.id), None)
        .await?;
    let price = price_text(order.filled_avg_price);
    journal(
        d,
        EventKind::Fill,
        Some(sym),
        Some(&intent.id),
        format!("Вихід виконано: {sold} шт. за {price}"),
        json!({ "order_id": order.id, "qty": sold, "price": price }),
    )
    .await?;
    d.bus.publish(EngineEventDto::OrderFilled {
        symbol: sym.to_string(),
        side: side_name(order.side).into(),
        qty: sold,
        price,
    });
    pass.report.exits_filled.push(sym.clone());
    Ok(())
}

/// Крок 2 для однієї позиції: стан її стоп-ордера.
async fn check_stop(d: &Deps, tp: &TrackedPosition, pass: &mut Pass) -> Result<(), CycleError> {
    let sym = &tp.position.symbol;
    let stop = match current_stop(d, tp, None).await {
        Ok(Some(o)) => o,
        Ok(None) => return reprotect(d, tp, None, pass).await,
        Err(e) => {
            let id = tp.stop_order_id.as_deref().unwrap_or_default();
            journal(
                d,
                EventKind::Error,
                Some(sym),
                Some(&tp.intent_id),
                format!("Звірка: не вдалося перевірити стоп-ордер {id} ({sym}): {e}"),
                json!({ "stop_order_id": id, "error": e.to_string() }),
            )
            .await?;
            pass.uncertain.insert(sym.clone());
            return Ok(());
        }
    };
    d.store.upsert_order(&stop, d.clock.now()).await?;
    pass.report.orders_updated += 1;
    if stop.status.is_open() {
        if stop.filled_qty > 0 {
            // Стоп саме виконується: позицію уточнить наступна звірка.
            pass.add_inflight(sym, -i64::from(stop.filled_qty));
            return Ok(());
        }
        if protects(&stop, tp) {
            return Ok(());
        }
        return reprotect(d, tp, Some(&stop), pass).await;
    }
    if !stop.status.is_terminal() {
        // Невідомий стан брокера: нічого не змінюється до наступної звірки.
        pass.uncertain.insert(sym.clone());
        return Ok(());
    }
    if stop.filled_qty == 0 {
        return reprotect(d, tp, Some(&stop), pass).await;
    }

    let sold = stop.filled_qty;
    let remaining = remaining_after_sale(tp.position.qty, sold, stop.qty);
    let rest = with_qty(tp, remaining);
    if remaining == 0 {
        d.store.remove_position(sym).await?;
    } else {
        d.store.upsert_position(&rest, d.clock.now()).await?;
    }
    let price = price_text(stop.filled_avg_price);
    let tail = if remaining > 0 {
        format!(", залишок {remaining} шт.")
    } else {
        String::new()
    };
    journal(
        d,
        EventKind::Fill,
        Some(sym),
        Some(&tp.intent_id),
        format!("Спрацював стоп: {sold} шт. за {price}{tail}"),
        json!({ "order_id": stop.id, "qty": sold, "price": price, "remaining": remaining }),
    )
    .await?;
    if stop.status == OrderStatus::Filled {
        d.bus.publish(EngineEventDto::StopHit {
            symbol: sym.to_string(),
            price,
        });
        pass.report.stops_hit.push(sym.clone());
    }
    if remaining > 0 {
        reprotect(d, &rest, Some(&stop), pass).await?;
    }
    Ok(())
}

/// Поточний стоп-ордер позиції: `known`, якщо це він, інакше — запит до брокера.
/// Ордер, якого брокер не знає, вважається відсутнім.
async fn current_stop(
    d: &Deps,
    tp: &TrackedPosition,
    known: Option<&BrokerOrder>,
) -> Result<Option<BrokerOrder>, BrokerError> {
    let Some(id) = tp.stop_order_id.as_deref() else {
        return Ok(None);
    };
    if let Some(o) = known.filter(|o| o.id == id) {
        return Ok(Some(o.clone()));
    }
    match observe(d, d.broker.order(id).await) {
        Ok(o) => Ok(Some(o)),
        Err(BrokerError::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `ensure_stop` усередині звірки: збій захисту не перериває звірку (подія в журналі,
/// наступна звірка повторить), збій сховища — перериває.
async fn reprotect(
    d: &Deps,
    tp: &TrackedPosition,
    known: Option<&BrokerOrder>,
    pass: &mut Pass,
) -> Result<(), CycleError> {
    let sym = &tp.position.symbol;
    let result = match current_stop(d, tp, known).await {
        Ok(current) => protect(d, tp, current).await,
        Err(e) => Err(e.into()),
    };
    match result {
        Ok(updated) => {
            if updated.stop_order_id != tp.stop_order_id {
                pass.report.reprotected.push(sym.clone());
            }
            Ok(())
        }
        Err(CycleError::Store(e)) => Err(CycleError::Store(e)),
        Err(e) => {
            journal(
                d,
                EventKind::Error,
                Some(sym),
                Some(&tp.intent_id),
                format!("Не вдалося захистити позицію {sym} стопом: {e}"),
                json!({ "stop_order_id": tp.stop_order_id, "error": e.to_string() }),
            )
            .await
        }
    }
}

/// Гарантує робочий захисний стоп (Sell Stop GTC) на весь обсяг позиції.
///
/// Чинний стоп лишається як є. Інакше береться відкритий Sell Stop на той самий тікер і
/// обсяг, поданий раніше (зокрема до перезапуску). Лише коли такого немає, застарілий стоп
/// (інший обсяг або ще утримується) скасовується й подається новий з
/// `client_order_id("{intent_id}-protect-{unix}")`; на дубль ордер шукається за цим id.
/// Позиція з новим `stop_order_id` зберігається, у журналі — «Стоп відновлено».
pub async fn ensure_stop(d: &Deps, p: &TrackedPosition) -> Result<TrackedPosition, CycleError> {
    let current = current_stop(d, p, None).await?;
    protect(d, p, current).await
}

async fn protect(
    d: &Deps,
    p: &TrackedPosition,
    current: Option<BrokerOrder>,
) -> Result<TrackedPosition, CycleError> {
    if current.as_ref().is_some_and(|o| protects(o, p)) {
        return Ok(p.clone());
    }
    let pos = &p.position;
    let open = observe(d, d.broker.open_orders().await)?;
    let existing = flatten(&open)
        .into_iter()
        .map(|(o, _)| o)
        .find(|o| protects(o, p))
        .cloned();
    if let Some(o) = existing {
        let message = format!(
            "Стоп відновлено: використано наявний стоп-ордер {} на {} шт.",
            o.id, o.qty
        );
        return record(d, p, &o, message).await;
    }
    if let Some(old) = current.as_ref().filter(|o| o.status.is_open()) {
        cancel_stale(d, old).await?;
    }
    let stop = to_price(pos.stop_price).map_err(|e| {
        CycleError::Other(format!(
            "некоректна ціна стопа {} для {}: {e}",
            pos.stop_price, pos.symbol
        ))
    })?;
    let cid = client_order_id(&format!(
        "{}-protect-{}",
        p.intent_id,
        d.clock.now().timestamp()
    ));
    let req = OrderRequest {
        client_order_id: cid.clone(),
        symbol: pos.symbol.clone(),
        side: Side::Sell,
        qty: pos.qty,
        order_type: OrderType::Stop,
        limit_price: None,
        stop_price: Some(stop),
        tif: TimeInForce::Gtc,
        class: OrderClass::Simple,
        stop_loss: None,
        take_profit: None,
    };
    let order = match observe(d, d.broker.submit(&req).await) {
        Ok(o) => o,
        // Ордер із цим id уже є: береться, лише якщо він і є робочим стопом позиції;
        // інакше — помилка, наступна звірка подасть стоп з новою міткою часу.
        Err(BrokerError::DuplicateClientOrderId) => {
            match observe(d, d.broker.order_by_client_id(&cid).await)? {
                Some(o) if protects(&o, p) => o,
                _ => return Err(BrokerError::DuplicateClientOrderId.into()),
            }
        }
        Err(e) => return Err(e.into()),
    };
    let message = format!(
        "Стоп відновлено: {} шт., стоп {stop}, ордер {}",
        pos.qty, order.id
    );
    record(d, p, &order, message).await
}

/// Скасовує застарілий стоп. Новий не подається, доки старий не завершився без виконання
/// (перевіряється повторним запитом, навіть якщо скасування повернуло помилку), інакше
/// продаж може задвоїтись.
async fn cancel_stale(d: &Deps, old: &BrokerOrder) -> Result<(), CycleError> {
    let cancelled = observe(d, d.broker.cancel(&old.id).await);
    let after = observe(d, d.broker.order(&old.id).await)?;
    d.store.upsert_order(&after, d.clock.now()).await?;
    if after.status.is_open() {
        let why = cancelled
            .err()
            .map(|e| format!(": {e}"))
            .unwrap_or_default();
        return Err(CycleError::Other(format!(
            "стоп-ордер {} ще не скасовано{why}",
            old.id
        )));
    }
    if after.filled_qty > 0 {
        return Err(CycleError::Other(format!(
            "стоп-ордер {} виконано ({} шт.) до скасування; позицію уточнить наступна звірка",
            old.id, after.filled_qty
        )));
    }
    Ok(())
}

/// Зберігає стоп-ордер і позицію з ним; подія в журналі.
async fn record(
    d: &Deps,
    p: &TrackedPosition,
    order: &BrokerOrder,
    message: String,
) -> Result<TrackedPosition, CycleError> {
    let now = d.clock.now();
    d.store.upsert_order(order, now).await?;
    let updated = TrackedPosition {
        stop_order_id: Some(order.id.clone()),
        ..p.clone()
    };
    d.store.upsert_position(&updated, now).await?;
    journal(
        d,
        EventKind::Order,
        Some(&p.position.symbol),
        Some(&p.intent_id),
        message,
        json!({
            "order_id": order.id,
            "client_order_id": order.client_order_id,
            "qty": order.qty,
            "stop_price": order.stop_price.map(|x| x.to_string()),
            "previous_stop_order_id": p.stop_order_id,
        }),
    )
    .await?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, Utc};
    use fond_api::events::EngineEventDto;
    use fond_core::error::BrokerError;
    use fond_core::ids::{IntentTag, client_order_id};
    use fond_core::intent::{IntentKind, IntentStatus};
    use fond_core::orders::{OrderClass, OrderRequest, OrderStatus, OrderType, Side, TimeInForce};
    use fond_core::traits::Broker;
    use fond_core::types::EngineMode;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    use super::super::test_support::{
        Handles, d, drain, entry_kind, et, fixture, held_aapl, intent_with, journal_messages,
        session, submit_entry, submit_exit, sym,
    };
    use super::*;

    /// Стенд із годинником на 10:30 ET сесії (після відкриття).
    async fn setup() -> (Deps, Handles) {
        let (dp, h) = fixture().await;
        h.clock.set(et(session(), 10, 30));
        (dp, h)
    }

    async fn status(dp: &Deps, id: &str) -> IntentStatus {
        dp.store.intent(id).await.unwrap().unwrap().status
    }

    async fn equity_rows(dp: &Deps) -> Vec<(String, String, f64)> {
        sqlx::query_as("SELECT kind, session_date, equity FROM equity_snapshots ORDER BY id")
            .fetch_all(dp.store.pool())
            .await
            .unwrap()
    }

    async fn stored_order_status(dp: &Deps, id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT status FROM orders WHERE id = ?")
            .bind(id)
            .fetch_optional(dp.store.pool())
            .await
            .unwrap()
    }

    fn price_of(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    fn filled_events(events: &[EngineEventDto]) -> Vec<(String, String, u32, Decimal)> {
        events
            .iter()
            .filter_map(|e| match e {
                EngineEventDto::OrderFilled {
                    symbol,
                    side,
                    qty,
                    price,
                } => Some((symbol.clone(), side.clone(), *qty, price_of(price))),
                _ => None,
            })
            .collect()
    }

    fn protect_request(req: &OrderRequest, symbol: &str, qty: u32, stop: Decimal) {
        assert_eq!(req.symbol, sym(symbol));
        assert_eq!(req.side, Side::Sell);
        assert_eq!(req.order_type, OrderType::Stop);
        assert_eq!(req.class, OrderClass::Simple);
        assert_eq!(req.tif, TimeInForce::Gtc);
        assert_eq!(req.qty, qty);
        assert_eq!(req.stop_price, Some(stop));
        assert_eq!(req.limit_price, None);
        assert_eq!(req.stop_loss, None);
    }

    fn protect_cid(intent_id: &str, at: DateTime<Utc>) -> String {
        client_order_id(&format!("{intent_id}-protect-{}", at.timestamp()))
    }

    #[tokio::test]
    async fn entry_fill_creates_tracked_position_with_stop_leg() {
        let (dp, h) = setup().await;
        let mut rx = dp.bus.subscribe();
        let (intent, order) = submit_entry(
            &dp,
            "AAPL",
            10,
            dec!(101.00),
            dec!(95.50),
            Some(dec!(115.25)),
        )
        .await;
        // Виконання о 19:30 ET 5 листопада = 00:30 UTC 6 листопада: дата входу — за ET.
        let filled_at = et(session(), 19, 30);
        assert_eq!(filled_at.date_naive(), d(2026, 11, 6));
        h.broker.fill_at(&order.id, dec!(101.00), 10, filled_at);
        // Звірка наступного дня: дата входу береться з filled_at, а не з годинника.
        let now = et(d(2026, 11, 6), 10, 30);
        h.clock.set(now);

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(report.entries_filled, vec![sym("AAPL")]);
        assert_eq!(report.orders_updated, 1);
        assert!(report.exits_filled.is_empty() && report.stops_hit.is_empty());
        assert!(report.reprotected.is_empty());
        assert!(report.mismatches.is_empty());

        let held = dp.store.positions().await.unwrap();
        assert_eq!(held.len(), 1);
        let p = &held[0];
        assert_eq!(p.position.symbol, sym("AAPL"));
        assert_eq!(p.position.strategy.as_str(), "trend_pullback");
        assert_eq!(p.position.qty, 10);
        assert_eq!(p.position.entry_price, 101.0);
        assert_eq!(p.position.entry_date, session());
        assert_eq!(p.position.stop_price, 95.5);
        assert_eq!(p.position.take_profit, Some(115.25));
        assert_eq!(p.position.sector, "Technology");
        assert_eq!(p.stop_order_id.as_deref(), Some("ord-1-stop"));
        assert_eq!(p.intent_id, intent.id);

        let i = dp.store.intent(&intent.id).await.unwrap().unwrap();
        assert_eq!(i.status, IntentStatus::Filled);
        assert_eq!(i.broker_order_id.as_deref(), Some("ord-1"));
        // Нога OTO вже захищає позицію: додаткового стопа немає.
        assert_eq!(h.broker.submitted().len(), 1);
        assert!(h.broker.cancels().is_empty());
        assert_eq!(
            stored_order_status(&dp, "ord-1").await.as_deref(),
            Some("filled")
        );

        let events = drain(&mut rx);
        assert_eq!(
            filled_events(&events),
            vec![("AAPL".into(), "buy".into(), 10, dec!(101))]
        );
        assert_eq!(
            equity_rows(&dp).await,
            vec![("intraday".into(), "2026-11-06".into(), 100_000.0)]
        );
        assert_eq!(dp.store.last_reconcile_at().await.unwrap(), Some(now));
        assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
    }

    #[tokio::test]
    async fn partial_fill_then_expire_tracks_filled_qty() {
        let (dp, h) = setup().await;
        // NVDA немає в universe: сектор «Unknown».
        let (intent, order) = submit_entry(&dp, "NVDA", 148, dec!(101.00), dec!(96.00), None).await;
        h.broker.fill(&order.id, dec!(101.0), 50);
        h.broker.expire(&order.id);
        let now = dp.clock.now();

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(report.entries_filled, vec![sym("NVDA")]);
        assert_eq!(report.reprotected, vec![sym("NVDA")]);
        assert!(report.mismatches.is_empty());
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held.len(), 1);
        let p = &held[0];
        assert_eq!(p.position.qty, 50);
        assert_eq!(p.position.entry_price, 101.0);
        // Часткове виконання без filled_at: дата входу — дата ET годинника.
        assert_eq!(p.position.entry_date, session());
        assert_eq!(p.position.sector, "Unknown");
        assert_eq!(p.stop_order_id.as_deref(), Some("ord-2"));

        // Нога OTO на 148 шт. скасована, подано захисний стоп на 50.
        assert_eq!(h.broker.cancels(), vec!["ord-1-stop".to_string()]);
        assert_eq!(
            h.broker.order("ord-1-stop").await.unwrap().status,
            OrderStatus::Canceled
        );
        let submitted = h.broker.submitted();
        assert_eq!(submitted.len(), 2);
        protect_request(&submitted[1], "NVDA", 50, dec!(96.00));
        assert_eq!(submitted[1].client_order_id, protect_cid(&intent.id, now));
        assert_eq!(
            stored_order_status(&dp, "ord-2").await.as_deref(),
            Some("accepted")
        );

        assert_eq!(status(&dp, &intent.id).await, IntentStatus::Filled);
        let journal = journal_messages(&dp).await;
        assert!(
            journal
                .iter()
                .any(|m| m.starts_with("Стоп відновлено") && m.contains("ord-2")),
            "{journal:?}"
        );
    }

    #[tokio::test]
    async fn unfilled_cancel_marks_intent_cancelled() {
        let (dp, h) = setup().await;
        let mut rx = dp.bus.subscribe();
        let (intent, order) = submit_entry(&dp, "AAPL", 10, dec!(101.00), dec!(95.50), None).await;
        h.broker.cancel(&order.id).await.unwrap();

        let report = reconcile(&dp).await.unwrap();

        assert!(report.entries_filled.is_empty());
        assert_eq!(report.orders_updated, 1);
        assert!(report.mismatches.is_empty());
        let i = dp.store.intent(&intent.id).await.unwrap().unwrap();
        assert_eq!(i.status, IntentStatus::Cancelled);
        assert_eq!(i.broker_order_id.as_deref(), Some("ord-1"));
        assert!(dp.store.positions().await.unwrap().is_empty());
        assert_eq!(h.broker.submitted().len(), 1);
        assert!(filled_events(&drain(&mut rx)).is_empty());
        assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
    }

    #[tokio::test]
    async fn stop_fill_removes_position_and_emits_stop_hit() {
        let (dp, h) = setup().await;
        let (_, p) = held_aapl(&dp, &h).await;
        assert_eq!(p.stop_order_id.as_deref(), Some("ord-1-stop"));
        h.broker.fill("ord-1-stop", dec!(95.40), 10);
        let mut rx = dp.bus.subscribe();

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(report.stops_hit, vec![sym("AAPL")]);
        assert!(report.reprotected.is_empty() && report.exits_filled.is_empty());
        assert!(report.mismatches.is_empty());
        assert!(dp.store.positions().await.unwrap().is_empty());
        let hits: Vec<(String, Decimal)> = drain(&mut rx)
            .into_iter()
            .filter_map(|e| match e {
                EngineEventDto::StopHit { symbol, price } => Some((symbol, price_of(&price))),
                _ => None,
            })
            .collect();
        assert_eq!(hits, vec![("AAPL".to_string(), dec!(95.40))]);
        assert_eq!(h.broker.submitted().len(), 1);
        assert_eq!(
            stored_order_status(&dp, "ord-1-stop").await.as_deref(),
            Some("filled")
        );
        assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
    }

    #[tokio::test]
    async fn exit_fill_removes_position() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        // Крок відправки скасовує стоп і продає за ринком на відкритті.
        h.broker.cancel("ord-1-stop").await.unwrap();
        let (exit, order) = submit_exit(&dp, "AAPL", 10).await;
        h.broker.fill(&order.id, dec!(104.00), 10);
        let mut rx = dp.bus.subscribe();

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(report.exits_filled, vec![sym("AAPL")]);
        assert!(report.reprotected.is_empty() && report.stops_hit.is_empty());
        assert!(report.mismatches.is_empty());
        assert!(dp.store.positions().await.unwrap().is_empty());
        let i = dp.store.intent(&exit.id).await.unwrap().unwrap();
        assert_eq!(i.status, IntentStatus::Filled);
        assert_eq!(i.broker_order_id.as_deref(), Some(order.id.as_str()));
        assert_eq!(
            filled_events(&drain(&mut rx)),
            vec![("AAPL".into(), "sell".into(), 10, dec!(104))]
        );
        // Нових стопів немає.
        assert_eq!(h.broker.submitted().len(), 2);
    }

    #[tokio::test]
    async fn expired_exit_reprotects_position() {
        let (dp, h) = setup().await;
        let (entry, _) = held_aapl(&dp, &h).await;
        h.broker.cancel("ord-1-stop").await.unwrap();
        let (exit, order) = submit_exit(&dp, "AAPL", 10).await;
        h.broker.expire(&order.id);
        let now = dp.clock.now();

        let report = reconcile(&dp).await.unwrap();

        assert!(report.exits_filled.is_empty());
        assert_eq!(report.reprotected, vec![sym("AAPL")]);
        assert!(report.mismatches.is_empty());
        assert_eq!(status(&dp, &exit.id).await, IntentStatus::Cancelled);
        let submitted = h.broker.submitted();
        assert_eq!(submitted.len(), 3);
        protect_request(&submitted[2], "AAPL", 10, dec!(95.50));
        assert_eq!(submitted[2].client_order_id, protect_cid(&entry.id, now));
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].position.qty, 10);
        assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-3"));
        let journal = journal_messages(&dp).await;
        assert!(
            journal.iter().any(|m| m.starts_with("Вихід не виконано")),
            "{journal:?}"
        );
        assert!(journal.iter().any(|m| m.starts_with("Стоп відновлено")));
    }

    #[tokio::test]
    async fn pending_exit_does_not_trigger_reprotect() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        h.broker.cancel("ord-1-stop").await.unwrap();
        let (exit, _) = submit_exit(&dp, "AAPL", 10).await;

        let report = reconcile(&dp).await.unwrap();

        assert!(report.reprotected.is_empty());
        assert!(report.exits_filled.is_empty());
        assert!(report.mismatches.is_empty());
        assert_eq!(h.broker.submitted().len(), 2);
        assert_eq!(status(&dp, &exit.id).await, IntentStatus::Submitted);
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-1-stop"));
    }

    #[tokio::test]
    async fn unknown_broker_position_autopauses() {
        let (dp, h) = setup().await;
        let mut rx = dp.bus.subscribe();
        h.broker.set_position(&sym("TSLA"), 5, dec!(250));

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(
            report.mismatches,
            vec![Mismatch::UnknownBrokerPosition {
                symbol: sym("TSLA"),
                qty: 5
            }]
        );
        let EngineMode::Paused { reason } = dp.store.engine_mode().await.unwrap() else {
            panic!("очікувалась пауза");
        };
        assert!(reason.starts_with("Розбіжність з брокером: "), "{reason}");
        assert!(reason.contains("TSLA"), "{reason}");
        assert!(drain(&mut rx).contains(&EngineEventDto::AutoPaused { reason }));
        // Звірка завершується: знімок капіталу й мітка часу записані.
        assert_eq!(equity_rows(&dp).await.len(), 1);
        assert_eq!(
            dp.store.last_reconcile_at().await.unwrap(),
            Some(dp.clock.now())
        );
    }

    #[tokio::test]
    async fn qty_difference_and_missing_position_are_reported() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        let (_, msft) = submit_entry(&dp, "MSFT", 20, dec!(50.00), dec!(47.00), None).await;
        h.broker.fill(&msft.id, dec!(50.00), 20);
        reconcile(&dp).await.unwrap();
        assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
        // Хтось продав частину AAPL і весь MSFT повз рушій.
        h.broker.set_position(&sym("AAPL"), 7, dec!(101));
        h.broker.set_position(&sym("MSFT"), 0, dec!(0));

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(
            report.mismatches,
            vec![
                Mismatch::QtyDiffers {
                    symbol: sym("AAPL"),
                    local: 10,
                    broker: 7
                },
                Mismatch::MissingBrokerPosition {
                    symbol: sym("MSFT")
                },
            ]
        );
        assert!(matches!(
            dp.store.engine_mode().await.unwrap(),
            EngineMode::Paused { .. }
        ));
    }

    #[tokio::test]
    async fn open_partial_entry_is_not_a_mismatch() {
        let (dp, h) = setup().await;
        let (intent, order) = submit_entry(&dp, "AAPL", 100, dec!(101.00), dec!(95.50), None).await;
        h.broker.fill(&order.id, dec!(101.00), 40);

        let report = reconcile(&dp).await.unwrap();

        assert!(report.mismatches.is_empty());
        assert!(report.entries_filled.is_empty());
        assert_eq!(status(&dp, &intent.id).await, IntentStatus::Submitted);
        assert!(dp.store.positions().await.unwrap().is_empty());
        assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
    }

    #[tokio::test]
    async fn missing_broker_order_marks_intent_failed() {
        let (dp, _h) = setup().await;
        let intent = intent_with(
            "AAPL",
            IntentTag::Entry,
            entry_kind(10, dec!(101.00), dec!(95.50), None),
            IntentStatus::Submitted,
            Some("ord-404"),
        );
        dp.store.insert_intent(&intent).await.unwrap();

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(report.orders_updated, 0);
        let i = dp.store.intent(&intent.id).await.unwrap().unwrap();
        assert_eq!(i.status, IntentStatus::Failed);
        assert_eq!(i.error.as_deref(), Some("Ордер не знайдено у брокера"));
    }

    #[tokio::test]
    async fn lookup_failure_is_journaled_and_next_intent_is_processed() {
        let (dp, h) = setup().await;
        let (aapl, a) = submit_entry(&dp, "AAPL", 10, dec!(101.00), dec!(95.50), None).await;
        let (msft, m) = submit_entry(&dp, "MSFT", 20, dec!(50.00), dec!(47.00), None).await;
        h.broker.fill(&a.id, dec!(101.00), 10);
        h.broker.fill(&m.id, dec!(50.00), 20);
        // Перший запит звірки (ордер AAPL) падає.
        h.broker.fail_next(BrokerError::Transient("503".into()));

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(report.entries_filled, vec![sym("MSFT")]);
        assert_eq!(status(&dp, &aapl.id).await, IntentStatus::Submitted);
        assert_eq!(status(&dp, &msft.id).await, IntentStatus::Filled);
        // Стан AAPL невідомий: розбіжністю не вважається, паузи немає.
        assert!(report.mismatches.is_empty());
        assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
        let journal = journal_messages(&dp).await;
        assert!(
            journal
                .iter()
                .any(|m| m.contains("не вдалося перевірити ордер") && m.contains("AAPL")),
            "{journal:?}"
        );
        assert_eq!(dp.health.streak(), 0);

        // Наступна звірка добирає AAPL.
        let report = reconcile(&dp).await.unwrap();
        assert_eq!(report.entries_filled, vec![sym("AAPL")]);
        assert_eq!(status(&dp, &aapl.id).await, IntentStatus::Filled);
    }

    #[tokio::test]
    async fn broker_positions_failure_is_fatal_and_journaled() {
        let (dp, h) = setup().await;
        h.broker.fail_next(BrokerError::Transient("timeout".into()));

        let err = reconcile(&dp).await.unwrap_err();

        assert!(matches!(err, CycleError::Broker(BrokerError::Transient(_))));
        assert_eq!(dp.health.streak(), 1);
        assert_eq!(dp.store.last_reconcile_at().await.unwrap(), None);
        assert!(equity_rows(&dp).await.is_empty());
        let journal = journal_messages(&dp).await;
        assert!(
            journal.iter().any(|m| m.starts_with("Звірку перервано")),
            "{journal:?}"
        );
    }

    #[tokio::test]
    async fn ensure_stop_adopts_existing_open_stop() {
        let (dp, h) = setup().await;
        let (_, p) = held_aapl(&dp, &h).await;
        // Після перезапуску: нога скасована, а захисний стоп уже подано раніше.
        h.broker.cancel("ord-1-stop").await.unwrap();
        let earlier = OrderRequest {
            client_order_id: "fond-earlier-protect".into(),
            symbol: sym("AAPL"),
            side: Side::Sell,
            qty: 10,
            order_type: OrderType::Stop,
            limit_price: None,
            stop_price: Some(dec!(95.50)),
            tif: TimeInForce::Gtc,
            class: OrderClass::Simple,
            stop_loss: None,
            take_profit: None,
        };
        let existing = dp.broker.submit(&earlier).await.unwrap();

        let updated = ensure_stop(&dp, &p).await.unwrap();

        assert_eq!(updated.stop_order_id.as_deref(), Some(existing.id.as_str()));
        assert_eq!(h.broker.submitted().len(), 2);
        let stored = dp.store.positions().await.unwrap();
        assert_eq!(stored[0].stop_order_id, updated.stop_order_id);

        // Повторний виклик нічого не змінює.
        assert_eq!(ensure_stop(&dp, &updated).await.unwrap(), updated);
        assert_eq!(h.broker.submitted().len(), 2);
    }

    #[tokio::test]
    async fn ensure_stop_without_stop_order_submits_protect() {
        let (dp, h) = setup().await;
        let (entry, p) = held_aapl(&dp, &h).await;
        h.broker.cancel("ord-1-stop").await.unwrap();
        let bare = TrackedPosition {
            stop_order_id: None,
            ..p
        };
        h.clock.advance(Duration::minutes(5));

        let updated = ensure_stop(&dp, &bare).await.unwrap();

        let submitted = h.broker.submitted();
        assert_eq!(submitted.len(), 2);
        protect_request(&submitted[1], "AAPL", 10, dec!(95.50));
        assert_eq!(
            submitted[1].client_order_id,
            protect_cid(&entry.id, dp.clock.now())
        );
        assert_eq!(updated.stop_order_id.as_deref(), Some("ord-2"));
        assert_eq!(
            dp.store.positions().await.unwrap()[0].stop_order_id,
            updated.stop_order_id
        );
    }

    #[tokio::test]
    async fn duplicate_protect_id_of_dead_order_is_not_adopted() {
        let (dp, h) = setup().await;
        let (_, p) = held_aapl(&dp, &h).await;
        h.broker.cancel("ord-1-stop").await.unwrap();
        let first = ensure_stop(&dp, &p).await.unwrap();
        assert_eq!(first.stop_order_id.as_deref(), Some("ord-2"));
        // Той самий момент часу: новий стоп мав би той самий client id, а ордер з ним
        // уже скасовано — він не береться як захист.
        h.broker.cancel("ord-2").await.unwrap();

        let err = ensure_stop(&dp, &first).await.unwrap_err();

        assert!(matches!(
            err,
            CycleError::Broker(BrokerError::DuplicateClientOrderId)
        ));
        assert_eq!(h.broker.submitted().len(), 2);
        assert_eq!(
            dp.store.positions().await.unwrap()[0]
                .stop_order_id
                .as_deref(),
            Some("ord-2")
        );
        // Наступна спроба (інша секунда) подає новий стоп.
        h.clock.advance(Duration::seconds(1));
        let next = ensure_stop(&dp, &first).await.unwrap();
        assert_eq!(next.stop_order_id.as_deref(), Some("ord-3"));
    }

    #[tokio::test]
    async fn stale_leg_that_cannot_be_cancelled_blocks_new_stop() {
        let (dp, h) = setup().await;
        let (intent, order) = submit_entry(&dp, "NVDA", 148, dec!(101.00), dec!(96.00), None).await;
        h.broker.fill(&order.id, dec!(101.00), 50);
        h.broker.expire(&order.id);
        h.broker.fail_cancel("ord-1-stop");

        let report = reconcile(&dp).await.unwrap();

        // Позицію й виконання збережено, але другий стоп поверх старої ноги не подано.
        assert_eq!(report.entries_filled, vec![sym("NVDA")]);
        assert!(report.reprotected.is_empty());
        assert_eq!(h.broker.submitted().len(), 1);
        assert_eq!(status(&dp, &intent.id).await, IntentStatus::Filled);
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held[0].position.qty, 50);
        assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-1-stop"));
        let journal = journal_messages(&dp).await;
        assert!(
            journal
                .iter()
                .any(|m| m.starts_with("Не вдалося захистити позицію NVDA")),
            "{journal:?}"
        );

        // Наступна звірка знову перевіряє ногу (інший обсяг) і знову не подвоює стоп.
        reconcile(&dp).await.unwrap();
        assert_eq!(h.broker.submitted().len(), 1);
    }

    #[tokio::test]
    async fn submitted_move_stop_intent_is_left_to_submit_step() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        let kind = IntentKind::MoveStop {
            new_stop: dec!(98.00),
            stop_order_id: "ord-1-stop".into(),
        };
        let mv = intent_with("AAPL", IntentTag::Stop, kind, IntentStatus::Submitted, None);
        dp.store.insert_intent(&mv).await.unwrap();

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(status(&dp, &mv.id).await, IntentStatus::Submitted);
        assert!(report.reprotected.is_empty());
        assert_eq!(h.broker.submitted().len(), 1);
    }
}
