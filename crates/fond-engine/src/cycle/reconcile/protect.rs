//! Захисні стопи: пошук поточного, перезахист і подання Sell Stop не понад позицію брокера.

use std::time::Duration;

use fond_core::error::BrokerError;
use fond_core::ids::client_order_id;
use fond_core::orders::{
    BrokerOrder, OrderClass, OrderRequest, OrderStatus, OrderType, Side, TimeInForce,
};
use fond_core::portfolio::TrackedPosition;
use fond_core::price::to_price;
use fond_core::types::Symbol;
use fond_store::EventKind;
use serde_json::json;

use crate::cycle::{CycleError, journal};
use crate::deps::Deps;

use super::{Pass, WithParent, flatten, is_sell_stop, observe, target_qty, working};

/// Скільки разів перевіряється, що скасований стоп завершився.
const CANCEL_CHECKS: usize = 3;
/// Пауза між перевірками скасування.
#[cfg(not(test))]
const CANCEL_POLL: Duration = Duration::from_secs(1);
#[cfg(test)]
const CANCEL_POLL: Duration = Duration::from_millis(5);

/// Поточний стоп-ордер позиції: `known`, якщо це він, інакше — запит до брокера.
/// Ордер, якого брокер не знає, вважається відсутнім.
pub(super) async fn current_stop(
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
pub(super) async fn reprotect(
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
        match cancel_confirmed(d, &old.id).await? {
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

/// Скасовує ордер `id` і перевіряє (до `CANCEL_CHECKS` разів), що він завершився. `None` — ще
/// не завершився (наприклад, `pending_cancel`, який іще може виконатись).
pub(crate) async fn cancel_confirmed(
    d: &Deps,
    id: &str,
) -> Result<Option<BrokerOrder>, CycleError> {
    // Результат скасування не вирішальний (ордер міг уже завершитись): вирішує повторний запит.
    let _ = observe(d, d.broker.cancel(id).await);
    for check in 0..CANCEL_CHECKS {
        if check > 0 {
            tokio::time::sleep(CANCEL_POLL).await;
        }
        let now = observe(d, d.broker.order(id).await)?;
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
