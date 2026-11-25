//! Відправка виходу: спершу з підтвердженням скасовуються стоп та інші продажі тікера,
//! обсяг обмежується позицією брокера.

use std::collections::HashMap;

use fond_core::error::BrokerError;
use fond_core::ids::client_order_id;
use fond_core::intent::{Intent, IntentKind, IntentStatus};
use fond_core::orders::{BrokerOrder, OrderRequest, Side, TimeInForce};
use fond_core::types::Symbol;
use fond_store::EventKind;
use serde_json::json;

use crate::cycle::kill::reprotect_after_failed_sale;
use crate::cycle::reconcile::{cancel_confirmed, flatten, observe, status_text};
use crate::cycle::{CycleError, journal};
use crate::deps::Deps;

use super::orders::{build_exit_order, submit_idempotent};
use super::{Outcome, fail, reserve, tracked, unanswered, unanswered_error};

const STOP_CANCEL_FAILED: &str = "Не вдалося скасувати стоп";
const NO_BROKER_POSITION: &str = "Позиції у брокера немає";

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
pub(super) async fn send_exit(
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
