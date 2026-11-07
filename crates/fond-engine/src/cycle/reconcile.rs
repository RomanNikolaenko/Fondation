//! Звірка з брокером (щогодини під час сесії): виконання відправлених намірів, захисні стопи,
//! розбіжності позицій і внутрішньоденний знімок капіталу.
//!
//! Брокер — джерело правди про ордери й позиції. Звірка ідемпотентна: повторний прогін після
//! збою чи перезапуску доводить стан до того самого результату й не дублює ордерів. Рушій лише
//! купує, тому захисні Sell Stop ніколи не подаються й не беруться понад обсяг, що є у брокера,
//! а після закриття позиції решта її продажів скасовується.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use fond_api::events::EngineEventDto;
use fond_core::calendar::utc_to_et;
use fond_core::error::BrokerError;
use fond_core::ids::client_order_id;
use fond_core::intent::{Intent, IntentKind, IntentStatus};
use fond_core::orders::{
    BrokerOrder, OrderClass, OrderRequest, OrderStatus, OrderType, Side, TimeInForce,
};
use fond_core::portfolio::{Position, TrackedPosition};
use fond_core::price::{to_f64, to_price};
use fond_core::types::Symbol;
use fond_store::{EquityKind, EquitySnapshot, EventKind};
use rust_decimal::Decimal;
use serde_json::json;

use super::analysis::sector_of;
use super::kill::pause_changed;
use super::{CycleError, journal};
use crate::deps::Deps;

/// Скільки разів перевіряється, що скасований стоп завершився.
const CANCEL_CHECKS: usize = 3;
/// Пауза між перевірками скасування.
#[cfg(not(test))]
const CANCEL_POLL: Duration = Duration::from_secs(1);
#[cfg(test)]
const CANCEL_POLL: Duration = Duration::from_millis(5);

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

fn is_sell_stop(o: &BrokerOrder, sym: &Symbol) -> bool {
    o.symbol == *sym && o.side == Side::Sell && o.order_type == OrderType::Stop
}

/// Робочий захисний стоп на `qty` шт.: відкритий і не `Held` (нога, що чекає виконання
/// батьківського ордера, нічого не захищає).
fn working(o: &BrokerOrder, sym: &Symbol, qty: u32) -> bool {
    is_sell_stop(o, sym) && o.status.is_open() && o.status != OrderStatus::Held && o.qty == qty
}

/// Обсяг тікера, який уже продають інші відкриті ордери: верхньорівневі продажі, що не є
/// стопами (вихід, аварійне закриття, ручний ордер). Ноги-цілі (OCO зі стопом) не рахуються.
fn reserved_qty(orders: &[WithParent<'_>], sym: &Symbol) -> i64 {
    orders
        .iter()
        .filter(|(o, parent)| {
            parent.is_none()
                && o.symbol == *sym
                && o.side == Side::Sell
                && o.order_type != OrderType::Stop
                && !o.status.is_terminal()
                && o.status != OrderStatus::Held
        })
        .map(|(o, _)| i64::from(o.qty.saturating_sub(o.filled_qty)))
        .sum()
}

/// Скільки акцій захищати стопом: не більше відстежуваного обсягу й не більше, ніж у брокера
/// лишається після інших відкритих продажів.
fn target_qty(tracked: u32, held: i64, reserved: i64) -> u32 {
    let free = (held - reserved).max(0);
    u32::try_from(free).map_or(tracked, |f| f.min(tracked))
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

fn bump(map: &mut HashMap<Symbol, i64>, s: &Symbol, delta: i64) {
    *map.entry(s.clone()).or_default() += delta;
}

/// Стан одного прогону звірки.
#[derive(Default)]
struct Pass {
    report: ReconcileReport,
    /// Позиції брокера на початку звірки.
    held: HashMap<Symbol, i64>,
    /// Зміна позиції від ще відкритих, але вже частково виконаних ордерів.
    inflight: HashMap<Symbol, i64>,
    /// Купівлі й продажі, побачені в цій звірці: знімок позицій зроблено раніше, тож він міг
    /// їх ще не врахувати.
    bought: HashMap<Symbol, i64>,
    sold: HashMap<Symbol, i64>,
    /// Позиції, чий стоп у кроці 2 не перевіряється: відкритий вихід або вже оброблені.
    skip_stop_check: HashSet<Symbol>,
    /// Тікери з невідомим станом (запит до брокера не вдався): не порівнюються.
    uncertain: HashSet<Symbol>,
}

impl Pass {
    fn held(&self, s: &Symbol) -> i64 {
        self.held.get(s).copied().unwrap_or(0)
    }
}

/// Звірка: знімок позицій брокера, далі (1) відправлені входи й виходи, (2) стопи позицій без
/// відкритого виходу, (3) порівняння зі знімком — розбіжність ставить паузу, (4) знімок
/// капіталу.
///
/// Збій запиту по окремому наміру чи стопу записується в журнал, і звірка йде далі;
/// збій отримання позицій чи рахунку перериває її (`CycleError::Broker`).
pub async fn reconcile(d: &Deps) -> Result<ReconcileReport, CycleError> {
    let positions = match observe(d, d.broker.positions().await) {
        Ok(p) => p,
        Err(e) => {
            return broker_failure(d, "Звірку перервано: не вдалося отримати позиції", e).await;
        }
    };
    let mut pass = Pass {
        held: positions
            .iter()
            .filter(|p| p.qty != 0)
            .map(|p| (p.symbol.clone(), p.qty))
            .collect(),
        ..Pass::default()
    };

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
    let mismatches = compare(&tracked, &pass);
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

/// Розбіжності за кількістю зі знімком позицій. Очікувано: відстежувана позиція плюс часткові
/// виконання ще відкритих ордерів. Знімок зроблено до запитів ордерів, тож відхилення в
/// межах побачених у цій звірці купівель (менше) і продажів (більше) допускається — точне
/// порівняння повторить наступна звірка. Тікери з невідомим станом пропускаються.
fn compare(tracked: &[TrackedPosition], pass: &Pass) -> Vec<Mismatch> {
    let local: BTreeMap<&Symbol, u32> = tracked
        .iter()
        .map(|p| (&p.position.symbol, p.position.qty))
        .collect();
    let symbols: BTreeSet<&Symbol> = local.keys().copied().chain(pass.held.keys()).collect();
    let get = |m: &HashMap<Symbol, i64>, s: &Symbol| m.get(s).copied().unwrap_or(0);
    let mut out = Vec::new();
    for s in symbols.into_iter().filter(|s| !pass.uncertain.contains(*s)) {
        let expected = local.get(s).copied().map_or(0, i64::from) + get(&pass.inflight, s);
        let broker = pass.held(s);
        if broker >= expected - get(&pass.bought, s) && broker <= expected + get(&pass.sold, s) {
            continue;
        }
        let symbol = s.clone();
        match (local.get(s).copied(), broker) {
            (Some(_), 0) => out.push(Mismatch::MissingBrokerPosition { symbol }),
            (Some(local), broker) => out.push(Mismatch::QtyDiffers {
                symbol,
                local,
                broker,
            }),
            (None, 0) => {}
            (None, qty) => out.push(Mismatch::UnknownBrokerPosition { symbol, qty }),
        }
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
    let filled = i64::from(order.filled_qty);
    bump(&mut pass.bought, sym, filled);
    if !order.status.is_terminal() {
        bump(&mut pass.inflight, sym, filled);
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
    if order.filled_qty < order.qty {
        // Ноги розраховані на весь ордер: ціль (OCO зі стопом) продала б більше, ніж куплено.
        cancel_target_legs(d, order, &intent.id).await?;
    }
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

/// Скасовує відкриті ноги-продажі входу, крім стопа (стоп замінює `ensure_stop`).
async fn cancel_target_legs(
    d: &Deps,
    order: &BrokerOrder,
    intent_id: &str,
) -> Result<(), CycleError> {
    let legs = flatten(std::slice::from_ref(order));
    for (leg, _) in legs.into_iter().skip(1) {
        if leg.side != Side::Sell || leg.order_type == OrderType::Stop || !leg.status.is_open() {
            continue;
        }
        let (kind, message) = match observe(d, d.broker.cancel(&leg.id).await) {
            Ok(()) => (
                EventKind::Order,
                format!(
                    "Ціль {} скасовано: вхід виконано частково ({} з {} шт.)",
                    leg.id, order.filled_qty, order.qty
                ),
            ),
            Err(e) => (
                EventKind::Error,
                format!("Не вдалося скасувати ціль {}: {e}", leg.id),
            ),
        };
        journal(
            d,
            kind,
            Some(&leg.symbol),
            Some(intent_id),
            message,
            json!({ "order_id": leg.id }),
        )
        .await?;
    }
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
    let sold = order.filled_qty;
    bump(&mut pass.sold, sym, i64::from(sold));
    if !order.status.is_terminal() {
        // Вихід ще виконується: стоп не відновлюється, щоб не задвоїти продаж.
        pass.skip_stop_check.insert(sym.clone());
        bump(&mut pass.inflight, sym, -i64::from(sold));
        return Ok(());
    }
    let now = d.clock.now();
    let held = d
        .store
        .positions()
        .await?
        .into_iter()
        .find(|p| p.position.symbol == *sym);
    if sold == 0 {
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

    if let Some(tp) = held {
        pass.skip_stop_check.insert(sym.clone());
        let remaining = remaining_after_sale(tp.position.qty, sold, planned);
        if remaining == 0 {
            d.store.remove_position(sym).await?;
            cancel_open_sells(d, sym).await?;
        } else {
            let rest = with_qty(&tp, remaining);
            d.store.upsert_position(&rest, now).await?;
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

/// Крок 2 для однієї позиції: стан її стоп-ордера й позиції у брокера.
async fn check_stop(d: &Deps, tp: &TrackedPosition, pass: &mut Pass) -> Result<(), CycleError> {
    let sym = &tp.position.symbol;
    let stop = match current_stop(d, tp, None).await {
        Ok(stop) => stop,
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
    if let Some(stop) = &stop {
        d.store.upsert_order(stop, d.clock.now()).await?;
        pass.report.orders_updated += 1;
        if stop.status.is_terminal() && stop.filled_qty > 0 {
            return stop_filled(d, tp, stop, pass).await;
        }
    }
    let held = pass.held(sym);
    if held <= 0 {
        // Позицію закрито у брокера (ціль, продаж повз рушій): захищати нічого.
        return close_flat(d, tp, pass).await;
    }
    let Some(stop) = stop else {
        return reprotect(d, tp, None, pass).await;
    };
    if stop.status.is_open() {
        if stop.filled_qty > 0 {
            // Стоп саме виконується: позицію уточнить наступна звірка.
            let filled = i64::from(stop.filled_qty);
            bump(&mut pass.inflight, sym, -filled);
            bump(&mut pass.sold, sym, filled);
            return Ok(());
        }
        if working(&stop, sym, target_qty(tp.position.qty, held, 0)) {
            return Ok(());
        }
        return reprotect(d, tp, Some(&stop), pass).await;
    }
    if !stop.status.is_terminal() {
        // Невідомий стан (наприклад, `pending_cancel`): нічого не змінюється до наступної
        // звірки — ні нового стопа, ні порівняння.
        pass.uncertain.insert(sym.clone());
        return Ok(());
    }
    reprotect(d, tp, Some(&stop), pass).await
}

/// Стоп виконано (повністю або частково, далі не виконується).
async fn stop_filled(
    d: &Deps,
    tp: &TrackedPosition,
    stop: &BrokerOrder,
    pass: &mut Pass,
) -> Result<(), CycleError> {
    let sym = &tp.position.symbol;
    let sold = stop.filled_qty;
    bump(&mut pass.sold, sym, i64::from(sold));
    let remaining = remaining_after_sale(tp.position.qty, sold, stop.qty);
    let closed = remaining == 0 || pass.held(sym) <= 0;
    let rest = with_qty(tp, remaining);
    if closed {
        d.store.remove_position(sym).await?;
    } else {
        d.store.upsert_position(&rest, d.clock.now()).await?;
    }
    let price = price_text(stop.filled_avg_price);
    let tail = if closed {
        String::new()
    } else {
        format!(", залишок {remaining} шт.")
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
    if closed {
        cancel_open_sells(d, sym).await
    } else {
        reprotect(d, &rest, Some(stop), pass).await
    }
}

/// Позиції у брокера більше немає: відстеження знімається, решта продажів тікера
/// скасовується, щоб жоден не відкрив шорт.
async fn close_flat(d: &Deps, tp: &TrackedPosition, pass: &mut Pass) -> Result<(), CycleError> {
    let sym = &tp.position.symbol;
    d.store.remove_position(sym).await?;
    journal(
        d,
        EventKind::Fill,
        Some(sym),
        Some(&tp.intent_id),
        format!(
            "Позицію {sym} закрито у брокера ({} шт.): відстеження знято",
            tp.position.qty
        ),
        json!({ "qty": tp.position.qty, "stop_order_id": tp.stop_order_id }),
    )
    .await?;
    cancel_open_sells(d, sym).await?;
    pass.report.exits_filled.push(sym.clone());
    Ok(())
}

/// Скасовує відкриті продажі тікера, чия позиція закрита. Ноги ще відкритого батьківського
/// ордера (новий вхід) не чіпаються. Невдача — подія в журналі, не помилка.
async fn cancel_open_sells(d: &Deps, sym: &Symbol) -> Result<(), CycleError> {
    let open = match observe(d, d.broker.open_orders().await) {
        Ok(o) => o,
        Err(e) => {
            journal(
                d,
                EventKind::Error,
                Some(sym),
                None,
                format!("Не вдалося перевірити відкриті продажі {sym}: {e}"),
                json!({ "error": e.to_string() }),
            )
            .await?;
            return Ok(());
        }
    };
    for (o, parent) in flatten(&open) {
        if o.symbol != *sym
            || o.side != Side::Sell
            || !o.status.is_open()
            || parent.is_some_and(|p| p.status.is_open())
        {
            continue;
        }
        let (kind, message) = match observe(d, d.broker.cancel(&o.id).await) {
            Ok(()) => (
                EventKind::Order,
                format!("Скасовано продаж {} ({sym}): позицію закрито", o.id),
            ),
            Err(e) => (
                EventKind::Error,
                format!("Не вдалося скасувати продаж {} ({sym}): {e}", o.id),
            ),
        };
        journal(
            d,
            kind,
            Some(sym),
            None,
            message,
            json!({ "order_id": o.id }),
        )
        .await?;
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

/// `ensure_stop` усередині звірки (обсяг брокера — зі знімку): збій захисту не перериває
/// звірку (подія в журналі, наступна звірка повторить), збій сховища — перериває.
async fn reprotect(
    d: &Deps,
    tp: &TrackedPosition,
    known: Option<&BrokerOrder>,
    pass: &mut Pass,
) -> Result<(), CycleError> {
    let sym = &tp.position.symbol;
    let result = match current_stop(d, tp, known).await {
        Ok(current) => protect(d, tp, current, pass.held(sym)).await,
        Err(e) => Err(e.into()),
    };
    match result {
        Ok(Protection::Protected(updated)) => {
            if updated.stop_order_id != tp.stop_order_id {
                pass.report.reprotected.push(sym.clone());
            }
            Ok(())
        }
        // Обидва випадки вже в журналі.
        Ok(Protection::Covered | Protection::Waiting(_)) => Ok(()),
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

/// Гарантує робочий захисний стоп (Sell Stop GTC) на позицію — на обсяг, що є у брокера й не
/// продається іншими відкритими ордерами (не більше відстежуваного).
///
/// Чинний стоп лишається як є. Інакше береться відкритий Sell Stop на той самий тікер і обсяг,
/// поданий раніше (зокрема до перезапуску). Усі інші живі стопи тікера скасовуються; новий
/// подається з `client_order_id("{intent_id}-protect-{unix}")`, лише коли кожен старий
/// завершився (на дубль ордер шукається за цим id). Позиція з новим `stop_order_id`
/// зберігається, у журналі — «Стоп відновлено».
///
/// Помилка, якщо у брокера немає вільного обсягу або старий стоп ще не скасовано
/// («Старий стоп ще не скасовано: …»).
pub async fn ensure_stop(d: &Deps, p: &TrackedPosition) -> Result<TrackedPosition, CycleError> {
    let sym = &p.position.symbol;
    let held: i64 = observe(d, d.broker.positions().await)?
        .iter()
        .filter(|b| b.symbol == *sym)
        .map(|b| b.qty)
        .sum();
    let current = current_stop(d, p, None).await?;
    match protect(d, p, current, held).await? {
        Protection::Protected(updated) => Ok(updated),
        Protection::Covered => Err(CycleError::Other(format!(
            "стоп для {sym} не подано: у брокера немає вільного обсягу"
        ))),
        Protection::Waiting(id) => Err(CycleError::Other(format!(
            "Старий стоп ще не скасовано: {id}"
        ))),
    }
}

/// Результат захисту позиції.
enum Protection {
    /// Позиція під робочим стопом (`stop_order_id` міг змінитися).
    Protected(TrackedPosition),
    /// Захищати нічого: у брокера немає вільного обсягу (подія в журналі).
    Covered,
    /// Старий стоп ще не скасовано: новий не подано (подія в журналі).
    Waiting(String),
}

async fn protect(
    d: &Deps,
    p: &TrackedPosition,
    current: Option<BrokerOrder>,
    held: i64,
) -> Result<Protection, CycleError> {
    let pos = &p.position;
    let sym = &pos.symbol;
    let open = observe(d, d.broker.open_orders().await)?;
    let orders = flatten(&open);
    let reserved = reserved_qty(&orders, sym);
    let target = target_qty(pos.qty, held, reserved);
    if target == 0 {
        journal(
            d,
            EventKind::Order,
            Some(sym),
            Some(&p.intent_id),
            format!(
                "Стоп для {sym} не подано: у брокера {held} шт., з них {reserved} шт. уже \
                 продають відкриті ордери"
            ),
            json!({ "held": held, "reserved": reserved, "tracked": pos.qty }),
        )
        .await?;
        return Ok(Protection::Covered);
    }

    // Живі Sell Stop тікера (зокрема ті, що ще скасовуються) і поточний стоп, навіть
    // якщо він утримується чи в невідомому стані.
    let mut live: Vec<BrokerOrder> = orders
        .iter()
        .map(|(o, _)| *o)
        .filter(|o| {
            is_sell_stop(o, sym) && !o.status.is_terminal() && o.status != OrderStatus::Held
        })
        .cloned()
        .collect();
    if let Some(c) = current.as_ref().filter(|c| {
        is_sell_stop(c, sym) && !c.status.is_terminal() && live.iter().all(|o| o.id != c.id)
    }) {
        live.push(c.clone());
    }
    let current_id = p.stop_order_id.as_deref();
    let keep = live
        .iter()
        .position(|o| working(o, sym, target) && Some(o.id.as_str()) == current_id)
        .or_else(|| live.iter().position(|o| working(o, sym, target)))
        .map(|i| live.remove(i));

    // Решта живих стопів зайва: разом із робочим вони продали б більше, ніж є.
    let mut waiting = None;
    for old in &live {
        match cancel_confirmed(d, old).await? {
            Some(done) if done.filled_qty > 0 => {
                return Err(CycleError::Other(format!(
                    "стоп-ордер {} виконано ({} шт.) до скасування; позицію уточнить наступна \
                     звірка",
                    old.id, done.filled_qty
                )));
            }
            Some(_) => {}
            None => {
                journal(
                    d,
                    EventKind::Order,
                    Some(sym),
                    Some(&p.intent_id),
                    format!("Старий стоп ще не скасовано: {}", old.id),
                    json!({ "order_id": old.id, "status": old.status }),
                )
                .await?;
                waiting.get_or_insert_with(|| old.id.clone());
            }
        }
    }
    if let Some(o) = keep {
        if Some(o.id.as_str()) == current_id {
            return Ok(Protection::Protected(p.clone()));
        }
        let message = format!(
            "Стоп відновлено: використано наявний стоп-ордер {} на {} шт.",
            o.id, o.qty
        );
        return record(d, p, &o, message).await.map(Protection::Protected);
    }
    if let Some(id) = waiting {
        return Ok(Protection::Waiting(id));
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
        symbol: sym.clone(),
        side: Side::Sell,
        qty: target,
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
                Some(o) if working(&o, sym, target) => o,
                _ => return Err(BrokerError::DuplicateClientOrderId.into()),
            }
        }
        Err(e) => return Err(e.into()),
    };
    let message = format!(
        "Стоп відновлено: {target} шт., стоп {stop}, ордер {}",
        order.id
    );
    record(d, p, &order, message)
        .await
        .map(Protection::Protected)
}

/// Скасовує ордер і перевіряє (до `CANCEL_CHECKS` разів), що він завершився. `None` — ще не
/// завершився (наприклад, `pending_cancel`, який іще може виконатись).
async fn cancel_confirmed(d: &Deps, o: &BrokerOrder) -> Result<Option<BrokerOrder>, CycleError> {
    // Результат скасування не вирішальний (ордер міг уже завершитись): вирішує повторний запит.
    let _ = observe(d, d.broker.cancel(&o.id).await);
    for check in 0..CANCEL_CHECKS {
        if check > 0 {
            tokio::time::sleep(CANCEL_POLL).await;
        }
        let now = observe(d, d.broker.order(&o.id).await)?;
        if now.status.is_terminal() {
            d.store.upsert_order(&now, d.clock.now()).await?;
            return Ok(Some(now));
        }
    }
    Ok(None)
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
    async fn qty_difference_pauses_and_vanished_position_is_closed() {
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

        // AAPL — розбіжність (пауза); MSFT у брокера немає — позицію закрито, а її стоп
        // скасовано, щоб він не відкрив шорт.
        assert_eq!(
            report.mismatches,
            vec![Mismatch::QtyDiffers {
                symbol: sym("AAPL"),
                local: 10,
                broker: 7
            }]
        );
        assert_eq!(report.exits_filled, vec![sym("MSFT")]);
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].position.symbol, sym("AAPL"));
        assert_eq!(order_status(&h, "ord-2-stop").await, OrderStatus::Canceled);
        assert!(
            journal_messages(&dp)
                .await
                .iter()
                .any(|m| m.starts_with("Позицію MSFT закрито у брокера"))
        );
        assert!(matches!(
            dp.store.engine_mode().await.unwrap(),
            EngineMode::Paused { .. }
        ));
    }

    #[tokio::test]
    async fn fill_seen_after_positions_snapshot_is_not_a_mismatch() {
        let (dp, h) = setup().await;
        // Знімок позицій зроблено до запиту ордера: у знімку купівлі ще немає.
        let (intent, order) = submit_entry(&dp, "AAPL", 10, dec!(101.00), dec!(95.50), None).await;
        h.broker.fill(&order.id, dec!(101.00), 10);
        h.broker.set_position(&sym("AAPL"), 0, dec!(0));

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(status(&dp, &intent.id).await, IntentStatus::Filled);
        assert!(report.mismatches.is_empty());
        assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
        // Стоп-нога не чіпається, поки знімок не підтвердить обсяг.
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::New);
        assert_eq!(h.broker.submitted().len(), 1);
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
        // Запит ордера AAPL падає (один раз).
        h.broker.fail_next_lookup(&client_order_id(&aapl.id));

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
                .any(|m| m == "Старий стоп ще не скасовано: ord-1-stop"),
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

    /// Робочі (відкриті, не `Held`) Sell Stop тікера у брокера: (id, qty).
    async fn live_sell_stops(h: &Handles, symbol: &str) -> Vec<(String, u32)> {
        let open = h.broker.open_orders().await.unwrap();
        flatten(&open)
            .into_iter()
            .map(|(o, _)| o)
            .filter(|o| {
                o.symbol == sym(symbol)
                    && o.side == Side::Sell
                    && o.order_type == OrderType::Stop
                    && o.status.is_open()
                    && o.status != OrderStatus::Held
            })
            .map(|o| (o.id.clone(), o.qty))
            .collect()
    }

    fn stop_request(cid: &str, symbol: &str, qty: u32, stop: Decimal) -> OrderRequest {
        OrderRequest {
            client_order_id: cid.into(),
            symbol: sym(symbol),
            side: Side::Sell,
            qty,
            order_type: OrderType::Stop,
            limit_price: None,
            stop_price: Some(stop),
            tif: TimeInForce::Gtc,
            class: OrderClass::Simple,
            stop_loss: None,
            take_profit: None,
        }
    }

    async fn order_status(h: &Handles, id: &str) -> OrderStatus {
        h.broker.order(id).await.unwrap().status
    }

    fn pending_cancel() -> OrderStatus {
        OrderStatus::Other("pending_cancel".into())
    }

    /// Bracket AAPL 10 шт. (стоп 95.50, ціль 115.25), виконаний і звірений:
    /// стоп-нога `ord-1-stop`, ціль `ord-1-tp`.
    async fn held_aapl_bracket(dp: &Deps, h: &Handles) {
        let (_, order) = submit_entry(
            dp,
            "AAPL",
            10,
            dec!(101.00),
            dec!(95.50),
            Some(dec!(115.25)),
        )
        .await;
        h.broker.fill(&order.id, dec!(101.00), 10);
        reconcile(dp).await.unwrap();
        assert_eq!(dp.store.positions().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn pending_cancel_stop_blocks_reprotect_after_expired_exit() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        // Крок відправки скасував стоп, але брокер ще не завершив скасування.
        h.broker.set_status("ord-1-stop", pending_cancel());
        let (exit, order) = submit_exit(&dp, "AAPL", 10).await;
        h.broker.expire(&order.id);

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(status(&dp, &exit.id).await, IntentStatus::Cancelled);
        assert!(report.reprotected.is_empty());
        // Другого стопа поруч зі старим, що ще може виконатись, немає.
        assert_eq!(h.broker.submitted().len(), 2);
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-1-stop"));
        let journal = journal_messages(&dp).await;
        assert!(
            journal
                .iter()
                .any(|m| m == "Старий стоп ще не скасовано: ord-1-stop"),
            "{journal:?}"
        );
    }

    #[tokio::test]
    async fn pending_cancel_leg_blocks_protect_on_partial_entry() {
        let (dp, h) = setup().await;
        let (_, order) = submit_entry(&dp, "NVDA", 148, dec!(101.00), dec!(96.00), None).await;
        h.broker.fill(&order.id, dec!(101.00), 50);
        h.broker.expire(&order.id);
        h.broker.set_status("ord-1-stop", pending_cancel());

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(report.entries_filled, vec![sym("NVDA")]);
        assert!(report.reprotected.is_empty());
        assert_eq!(h.broker.submitted().len(), 1);
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held[0].position.qty, 50);
        assert_eq!(held[0].stop_order_id.as_deref(), Some("ord-1-stop"));
        assert!(
            journal_messages(&dp)
                .await
                .iter()
                .any(|m| m == "Старий стоп ще не скасовано: ord-1-stop")
        );
    }

    #[tokio::test]
    async fn bracket_target_fill_closes_position_and_cancels_leftover_stop() {
        let (dp, h) = setup().await;
        held_aapl_bracket(&dp, &h).await;
        // Ціль виконано; стоп-нога (FakeBroker не має OCO) ще відкрита.
        h.broker.fill("ord-1-tp", dec!(115.25), 10);

        let report = reconcile(&dp).await.unwrap();

        assert!(dp.store.positions().await.unwrap().is_empty());
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
        assert_eq!(h.broker.submitted().len(), 1);
        assert_eq!(report.exits_filled, vec![sym("AAPL")]);
        assert!(report.reprotected.is_empty());
        assert!(report.mismatches.is_empty());
        assert_eq!(dp.store.engine_mode().await.unwrap(), EngineMode::Running);
    }

    #[tokio::test]
    async fn oco_cancelled_stop_after_target_fill_is_not_replaced() {
        let (dp, h) = setup().await;
        held_aapl_bracket(&dp, &h).await;
        // Брокер скасував стоп (OCO), коли виконалась ціль: позиції більше немає.
        h.broker.cancel("ord-1-stop").await.unwrap();
        h.broker.fill("ord-1-tp", dec!(115.25), 10);

        let report = reconcile(&dp).await.unwrap();

        assert!(dp.store.positions().await.unwrap().is_empty());
        assert_eq!(h.broker.submitted().len(), 1);
        assert!(live_sell_stops(&h, "AAPL").await.is_empty());
        assert!(report.reprotected.is_empty());
        assert!(report.mismatches.is_empty());
    }

    #[tokio::test]
    async fn exit_fill_cancels_leftover_open_stop() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        // Стоп-нога лишилась відкритою, а вихід виконався повністю.
        let (_, order) = submit_exit(&dp, "AAPL", 10).await;
        h.broker.fill(&order.id, dec!(104.00), 10);

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(report.exits_filled, vec![sym("AAPL")]);
        assert!(dp.store.positions().await.unwrap().is_empty());
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
        assert_eq!(h.broker.submitted().len(), 2);
    }

    #[tokio::test]
    async fn stop_hit_cancels_leftover_take_profit() {
        let (dp, h) = setup().await;
        held_aapl_bracket(&dp, &h).await;
        h.broker.fill("ord-1-stop", dec!(95.40), 10);

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(report.stops_hit, vec![sym("AAPL")]);
        assert!(dp.store.positions().await.unwrap().is_empty());
        assert_eq!(order_status(&h, "ord-1-tp").await, OrderStatus::Canceled);
    }

    #[tokio::test]
    async fn partial_exit_adoption_leaves_one_live_stop() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        // Вихід продав 4 з 10 і прострочився; стоп-нога на 10 ще відкрита, а на залишок
        // уже є захисний стоп, поданий раніше.
        let (exit, order) = submit_exit(&dp, "AAPL", 10).await;
        h.broker.fill(&order.id, dec!(104.00), 4);
        h.broker.expire(&order.id);
        let earlier = dp
            .broker
            .submit(&stop_request("fond-earlier", "AAPL", 6, dec!(95.50)))
            .await
            .unwrap();

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(report.exits_filled, vec![sym("AAPL")]);
        assert_eq!(status(&dp, &exit.id).await, IntentStatus::Filled);
        assert_eq!(
            live_sell_stops(&h, "AAPL").await,
            vec![(earlier.id.clone(), 6)]
        );
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
        let held = dp.store.positions().await.unwrap();
        assert_eq!(held[0].position.qty, 6);
        assert_eq!(held[0].stop_order_id.as_deref(), Some(earlier.id.as_str()));
        assert_eq!(h.broker.submitted().len(), 3);
        assert!(report.mismatches.is_empty());
    }

    #[tokio::test]
    async fn partial_bracket_entry_cancels_take_profit_leg() {
        let (dp, h) = setup().await;
        let (_, order) = submit_entry(
            &dp,
            "NVDA",
            148,
            dec!(101.00),
            dec!(96.00),
            Some(dec!(120.00)),
        )
        .await;
        h.broker.fill(&order.id, dec!(101.00), 50);
        h.broker.expire(&order.id);

        reconcile(&dp).await.unwrap();

        // Ноги розраховані на 148 шт.: обидві скасовано, стоп — на виконані 50.
        assert_eq!(order_status(&h, "ord-1-tp").await, OrderStatus::Canceled);
        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
        assert_eq!(
            live_sell_stops(&h, "NVDA").await,
            vec![("ord-2".to_string(), 50)]
        );
    }

    #[tokio::test]
    async fn protective_stop_never_exceeds_broker_qty() {
        let (dp, h) = setup().await;
        held_aapl(&dp, &h).await;
        // 3 шт. продано повз рушій: стоп на 10 при 7 у брокера відкрив би шорт.
        h.broker.set_position(&sym("AAPL"), 7, dec!(101));

        let report = reconcile(&dp).await.unwrap();

        assert_eq!(order_status(&h, "ord-1-stop").await, OrderStatus::Canceled);
        assert_eq!(
            live_sell_stops(&h, "AAPL").await,
            vec![("ord-2".to_string(), 7)]
        );
        assert_eq!(
            report.mismatches,
            vec![Mismatch::QtyDiffers {
                symbol: sym("AAPL"),
                local: 10,
                broker: 7
            }]
        );
    }
}
