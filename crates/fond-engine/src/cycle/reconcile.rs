//! Звірка з брокером (щогодини під час сесії): виконання відправлених намірів, захисні стопи,
//! розбіжності позицій і внутрішньоденний знімок капіталу.
//!
//! Брокер — джерело правди про ордери й позиції. Звірка ідемпотентна: повторний прогін після
//! збою чи перезапуску доводить стан до того самого результату й не дублює ордерів. Рушій лише
//! купує, тому захисні Sell Stop ніколи не подаються й не беруться понад обсяг, що є у брокера,
//! а після закриття позиції решта її продажів скасовується.
//!
//! Тут — прохід звірки (`reconcile`), порівняння позицій, підсумок і спільні допоміжні
//! функції; обробка намірів, позицій і захисних стопів — у підмодулях.

mod intents;
mod positions;
mod protect;
mod rebalance;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use fond_core::calendar::utc_to_et;
use fond_core::error::BrokerError;
use fond_core::intent::{IntentKind, IntentStatus};
use fond_core::orders::{BrokerOrder, OrderStatus, OrderType, Side};
use fond_core::portfolio::{Position, TrackedPosition};
use fond_core::price::to_f64;
use fond_core::types::Symbol;
use fond_store::{EquityKind, EquitySnapshot, EventKind};
use rust_decimal::Decimal;
use serde_json::json;

use super::kill::pause_changed;
use super::{CycleError, journal};
use crate::deps::Deps;
use intents::submitted_intent;
use positions::{check_stop, close_flat};

pub(crate) use protect::cancel_confirmed;
pub use protect::ensure_stop;

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
    pub rebalances_filled: Vec<Symbol>,
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

fn is_sell_stop(o: &BrokerOrder, sym: &Symbol) -> bool {
    o.symbol == *sym && o.side == Side::Sell && o.order_type == OrderType::Stop
}

/// Робочий захисний стоп на `qty` шт.: відкритий і не `Held` (нога, що чекає виконання
/// батьківського ордера, нічого не захищає).
fn working(o: &BrokerOrder, sym: &Symbol, qty: u32) -> bool {
    is_sell_stop(o, sym) && o.status.is_open() && o.status != OrderStatus::Held && o.qty == qty
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

    // 1. Підтягування стопа (MoveStop) завершує крок відправки, тут — входи, виходи
    //    і ребаланси.
    for intent in d
        .store
        .intents_with_status(&[IntentStatus::Submitted])
        .await?
    {
        if matches!(
            intent.kind,
            IntentKind::Entry { .. } | IntentKind::Exit { .. } | IntentKind::Rebalance { .. }
        ) {
            submitted_intent(d, &intent, &mut pass).await?;
        }
    }

    // 2.
    for tp in d.store.positions().await? {
        let sym = tp.position.symbol.clone();
        if pass.skip_stop_check.contains(&sym) {
            continue;
        }
        // Позиції портфеля живуть без стоп-леги: перевіряємо лише, що брокер
        // ще тримає їх; закриту в брокера знімаємо з відстеження.
        if tp.position.strategy.as_str() == fond_core::rebalance::PORTFOLIO_STRATEGY {
            if pass.held(&sym) <= 0 {
                close_flat(d, &tp, &mut pass).await?;
            }
            continue;
        }
        check_stop(d, &tp, &mut pass).await?;
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
        + r.rebalances_filled.len()
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
            "Звірка: виконано входів {}, виходів {}, ребалансу {}, спрацювало стопів {}, \
             відновлено стопів {}, розбіжностей {}",
            r.entries_filled.len(),
            r.exits_filled.len(),
            r.rebalances_filled.len(),
            r.stops_hit.len(),
            r.reprotected.len(),
            r.mismatches.len()
        ),
        json!({
            "orders_updated": r.orders_updated,
            "entries_filled": names(&r.entries_filled),
            "exits_filled": names(&r.exits_filled),
            "rebalances_filled": names(&r.rebalances_filled),
            "stops_hit": names(&r.stops_hit),
            "reprotected": names(&r.reprotected),
            "mismatches": r.mismatches.iter().map(Mismatch::describe).collect::<Vec<_>>(),
        }),
    )
    .await
}

#[cfg(test)]
mod tests;
