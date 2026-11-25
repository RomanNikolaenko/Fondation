//! Побудова ордерів з намірів.

use super::super::orders::{build_entry_order, build_exit_order};
use super::*;

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
