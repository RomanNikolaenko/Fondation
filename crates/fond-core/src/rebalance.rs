//! Ребалансування портфеля: чиста математика дельт «ціль − факт».
//!
//! Контракт: `positions` містить **лише** позиції, якими керує портфель
//! (стратегія [`PORTFOLIO_STRATEGY`]); swing-позиції фільтрує рушій, бо ними
//! керують їхні стратегії й стопи. Тікер, який є в позиціях, але зник із цілей,
//! продається до нуля; тікер без ціни — пропускається.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::orders::Side;
use crate::portfolio::Position;
use crate::price::{to_f64, to_price};
use crate::types::Symbol;

/// Стратегія намірів і позицій, створених ребалансуванням.
pub const PORTFOLIO_STRATEGY: &str = "portfolio";

/// Параметри ребалансу з конфігу.
#[derive(Debug, Clone, PartialEq)]
pub struct RebalanceParams {
    /// Поріг ваги: |ціль − факт| нижче `drift × equity` не торгується.
    pub drift: f64,
    /// Дельти дешевше цього (USD) не стають ордерами.
    pub min_order_usd: f64,
    /// Відступ лімітної ціни від останньої ціни (з `RiskConfig`).
    pub max_gap: f64,
}

/// Один намір ребалансу: купівля або продаж до цілі.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RebalanceDelta {
    pub symbol: Symbol,
    pub side: Side,
    pub qty: u32,
    /// Лімітна ціна ордера: ціна ± `max_gap`, округлена до центів.
    pub limit_price: Decimal,
    pub target_value: f64,
    pub current_value: f64,
}

/// Чому тікер пропущено (для журналу).
#[derive(Debug, Clone, PartialEq)]
pub enum SkipReason {
    /// Немає ціни останнього бару — неможливо оцінити ні дельту, ні ордер.
    NoPrice,
    /// Дельта нижче порогів (`drift × equity` і `min_order_usd`).
    BelowThreshold,
    /// Дельти не вистачає навіть на одну акцію за лімітною ціною.
    BelowOneShare,
    /// Вільного кешу не вистачає на купівлю.
    NotEnoughCash,
    /// Ціна не конвертується в коректний ліміт (не скінченна/не додатна).
    BadPrice,
}

impl SkipReason {
    /// Український текст для журналу та підказок.
    pub fn text(&self) -> &'static str {
        match self {
            Self::NoPrice => "немає ціни останнього бару",
            Self::BelowThreshold => "дельта нижче порогу ребалансу",
            Self::BelowOneShare => "дельти не вистачає на одну акцію",
            Self::NotEnoughCash => "не вистачає вільного кешу",
            Self::BadPrice => "некоректна ціна",
        }
    }
}

/// Пропущений тікер.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    pub symbol: Symbol,
    pub reason: SkipReason,
}

/// Результат обчислення дельт: наміри + пропуски (для журналу).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RebalancePlan {
    /// Продажі спочатку (звільняють кеш), потім купівлі; у кожній групі —
    /// за спаданням модуля дельти.
    pub deltas: Vec<RebalanceDelta>,
    pub skipped: Vec<Skipped>,
}

fn fin(x: f64) -> f64 {
    if x.is_finite() { x } else { 0.0 }
}

/// Обчислює дельти ребалансу. Детермінована й чиста: жодного I/O і часу.
///
/// `targets` — тікер → вага (частка equity); позиції тікерів поза цілями
/// трактуються як ціль 0 (продаються до нуля). Купівлі обрізаються
/// послідовно під `cash` у порядку спадання дельти.
pub fn compute_deltas(
    targets: &BTreeMap<Symbol, f64>,
    positions: &[Position],
    prices: &HashMap<Symbol, f64>,
    equity: f64,
    cash: f64,
    params: &RebalanceParams,
) -> RebalancePlan {
    let equity = fin(equity);
    let threshold = (fin(params.drift) * equity).max(fin(params.min_order_usd));

    // Симетрична різниця: цілі ∪ позиції портфеля.
    let mut symbols: BTreeSet<&Symbol> = targets.keys().collect();
    symbols.extend(positions.iter().map(|p| &p.symbol));

    let mut plan = RebalancePlan::default();
    let mut rows: Vec<(f64, RebalanceDelta)> = Vec::new(); // (дельта, намір)
    let mut cash_left = fin(cash).max(0.0);

    for key in symbols {
        let symbol = key.clone();
        let weight = fin(*targets.get(&symbol).unwrap_or(&0.0));
        let qty_now = positions
            .iter()
            .find(|p| p.symbol == symbol)
            .map_or(0_u32, |p| p.qty);
        let price = fin(*prices.get(&symbol).unwrap_or(&f64::NAN));

        if weight <= 0.0 && qty_now == 0 {
            continue; // ні цілі, ні позиції — робити нічого
        }
        if price <= 0.0 {
            plan.skipped.push(Skipped {
                symbol,
                reason: SkipReason::NoPrice,
            });
            continue;
        }

        let target_value = weight * equity;
        let current_value = f64::from(qty_now) * price;
        let delta = target_value - current_value;
        if delta.abs() < threshold {
            plan.skipped.push(Skipped {
                symbol,
                reason: SkipReason::BelowThreshold,
            });
            continue;
        }

        if delta > 0.0 {
            match buy_delta(
                &symbol,
                delta,
                price,
                target_value,
                current_value,
                params,
                &mut cash_left,
            ) {
                Ok(row) => rows.push((delta, row)),
                Err(reason) => plan.skipped.push(Skipped { symbol, reason }),
            }
        } else {
            // Продаж: не більше наявної кількості.
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let qty = ((-delta / price).floor().min(f64::from(qty_now))) as u32;
            if qty < 1 {
                plan.skipped.push(Skipped {
                    symbol,
                    reason: SkipReason::BelowOneShare,
                });
                continue;
            }
            let limit = match to_price(price * (1.0 - fin(params.max_gap))) {
                Ok(limit) if to_f64(limit) > 0.0 => limit,
                _ => {
                    plan.skipped.push(Skipped {
                        symbol,
                        reason: SkipReason::BadPrice,
                    });
                    continue;
                }
            };
            rows.push((
                delta,
                RebalanceDelta {
                    symbol,
                    side: Side::Sell,
                    qty,
                    limit_price: limit,
                    target_value,
                    current_value,
                },
            ));
        }
    }

    // Продажі перед купівлями; у групах — за спаданням модуля дельти.
    let is_sell = |d: &RebalanceDelta| d.side == Side::Sell;
    rows.sort_by(|a, b| {
        is_sell(&b.1)
            .cmp(&is_sell(&a.1))
            .then_with(|| b.0.abs().total_cmp(&a.0.abs()))
    });
    plan.deltas = rows.into_iter().map(|(_, d)| d).collect();
    plan
}

/// Купівля: кількість за дельтою, обрізана під залишок кешу за лімітною ціною.
/// `cash_left` зменшується на вартість створеної купівлі.
fn buy_delta(
    symbol: &Symbol,
    delta: f64,
    price: f64,
    target_value: f64,
    current_value: f64,
    params: &RebalanceParams,
    cash_left: &mut f64,
) -> Result<RebalanceDelta, SkipReason> {
    let limit = to_price(price * (1.0 + fin(params.max_gap)))
        .ok()
        .filter(|limit| to_f64(*limit) > 0.0)
        .ok_or(SkipReason::BadPrice)?;
    let limit_f = to_f64(limit);

    let mut qty = (delta / price).floor();
    if qty < 1.0 {
        return Err(SkipReason::BelowOneShare);
    }
    let affordable = (*cash_left / limit_f).floor();
    if qty > affordable {
        qty = affordable;
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let qty = qty as u32;
    if qty < 1 {
        return Err(SkipReason::NotEnoughCash);
    }
    *cash_left -= f64::from(qty) * limit_f;
    Ok(RebalanceDelta {
        symbol: symbol.clone(),
        side: Side::Buy,
        qty,
        limit_price: limit,
        target_value,
        current_value,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rust_decimal_macros::dec;

    use super::*;
    use crate::types::StrategyId;

    fn pos(symbol: &str, qty: u32) -> Position {
        Position {
            symbol: Symbol::new(symbol),
            strategy: StrategyId::new(PORTFOLIO_STRATEGY),
            qty,
            entry_price: 100.0,
            entry_date: chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            stop_price: 0.0,
            take_profit: None,
            sector: String::new(),
        }
    }

    fn targets(pairs: &[(&str, f64)]) -> BTreeMap<Symbol, f64> {
        pairs.iter().map(|(s, w)| (Symbol::new(s), *w)).collect()
    }

    fn prices(pairs: &[(&str, f64)]) -> HashMap<Symbol, f64> {
        pairs.iter().map(|(s, p)| (Symbol::new(s), *p)).collect()
    }

    fn params() -> RebalanceParams {
        RebalanceParams {
            drift: 0.05,
            min_order_usd: 100.0,
            max_gap: 0.01,
        }
    }

    #[test]
    fn buys_and_sells_to_targets() {
        // equity 10 000: AAPL ціль 20% (2 000), факту нема; MSFT ціль 0,
        // позиція 30 × 100 = 3 000 → продати до нуля.
        let plan = compute_deltas(
            &targets(&[("AAPL", 0.2), ("MSFT", 0.0)]),
            &[pos("MSFT", 30)],
            &prices(&[("AAPL", 100.0), ("MSFT", 100.0)]),
            10_000.0,
            20_000.0,
            &params(),
        );
        assert_eq!(plan.deltas.len(), 2);
        // Продаж спочатку.
        assert_eq!(plan.deltas[0].side, Side::Sell);
        assert_eq!(plan.deltas[0].symbol, Symbol::new("MSFT"));
        assert_eq!(plan.deltas[0].qty, 30);
        assert_eq!(plan.deltas[0].limit_price, dec!(99.00));
        let buy = &plan.deltas[1];
        assert_eq!(buy.side, Side::Buy);
        assert_eq!(buy.symbol, Symbol::new("AAPL"));
        assert_eq!(buy.qty, 20);
        assert_eq!(buy.limit_price, dec!(101.00));
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn small_delta_is_skipped_by_drift_and_min_order() {
        // Дельта 2% від equity — нижче drift 5%.
        let plan = compute_deltas(
            &targets(&[("AAPL", 0.2)]),
            &[pos("AAPL", 18)], // 1 800 проти цілі 2 000
            &prices(&[("AAPL", 100.0)]),
            10_000.0,
            0.0,
            &params(),
        );
        assert!(plan.deltas.is_empty());
        assert_eq!(plan.skipped[0].reason, SkipReason::BelowThreshold);

        // drift 0 → працює лише мінімальний ордер: 200 USD ≥ 100.
        let p = RebalanceParams {
            drift: 0.0,
            ..params()
        };
        let plan = compute_deltas(
            &targets(&[("AAPL", 0.2)]),
            &[pos("AAPL", 18)],
            &prices(&[("AAPL", 100.0)]),
            10_000.0,
            1_000.0,
            &p,
        );
        assert_eq!(plan.deltas.len(), 1);
        assert_eq!(plan.deltas[0].qty, 2);

        // Дельта 100 USD — нижче min_order_usd 150 навіть без drift.
        let p = RebalanceParams {
            drift: 0.0,
            min_order_usd: 150.0,
            ..params()
        };
        let plan = compute_deltas(
            &targets(&[("AAPL", 0.2)]),
            &[pos("AAPL", 19)], // 1 900 проти цілі 2 000
            &prices(&[("AAPL", 100.0)]),
            10_000.0,
            0.0,
            &p,
        );
        assert!(plan.deltas.is_empty());
        assert_eq!(plan.skipped[0].reason, SkipReason::BelowThreshold);
    }

    #[test]
    fn sells_never_exceed_held_qty() {
        // Ціль 0, позиція 5 × 100 → продати рівно 5, не більше.
        let plan = compute_deltas(
            &targets(&[]),
            &[pos("AAPL", 5)],
            &prices(&[("AAPL", 100.0)]),
            10_000.0,
            0.0,
            &params(),
        );
        assert_eq!(plan.deltas[0].side, Side::Sell);
        assert_eq!(plan.deltas[0].qty, 5);
    }

    #[test]
    fn buys_are_cut_to_available_cash_in_delta_order() {
        // Дві цілі по 30% (дельти по 3 000), кеш 600: AAPL влізає на
        // floor(600/101)=5 акцій, TSLA — на floor(залишок/50.5).
        let plan = compute_deltas(
            &targets(&[("AAPL", 0.3), ("TSLA", 0.3)]),
            &[],
            &prices(&[("AAPL", 100.0), ("TSLA", 50.0)]),
            10_000.0,
            600.0,
            &params(),
        );
        assert_eq!(plan.deltas.len(), 2);
        let aapl = &plan.deltas[0];
        assert_eq!(aapl.symbol, Symbol::new("AAPL"));
        assert_eq!(aapl.side, Side::Buy);
        assert_eq!(aapl.qty, 5); // floor(600/101) = 5
        let tsla = &plan.deltas[1];
        assert_eq!(tsla.side, Side::Buy);
        assert_eq!(tsla.qty, 1); // залишок 95 → floor(95/50.5) = 1
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn missing_price_skips() {
        // Обидва тікери без ціни: ціль без позиції і позиція без цілі.
        let plan = compute_deltas(
            &targets(&[("AAPL", 0.5), ("NVDA", 0.0)]),
            &[pos("NVDA", 10)],
            &prices(&[]),
            10_000.0,
            0.0,
            &params(),
        );
        assert!(plan.deltas.is_empty());
        let mut skipped = plan.skipped.iter().map(|s| (&s.symbol, &s.reason));
        assert_eq!(
            skipped.next(),
            Some((&Symbol::new("AAPL"), &SkipReason::NoPrice))
        );
        assert_eq!(
            skipped.next(),
            Some((&Symbol::new("NVDA"), &SkipReason::NoPrice))
        );
    }

    #[test]
    fn deltas_sorted_sells_first_then_by_magnitude() {
        // Дві купівлі: більша дельта перша.
        let plan = compute_deltas(
            &targets(&[("AAPL", 0.3), ("MSFT", 0.1)]),
            &[],
            &prices(&[("AAPL", 100.0), ("MSFT", 100.0)]),
            10_000.0,
            10_000.0,
            &params(),
        );
        assert_eq!(plan.deltas[0].symbol, Symbol::new("AAPL"));
        assert_eq!(plan.deltas[1].symbol, Symbol::new("MSFT"));
    }

    #[test]
    fn no_targets_and_no_positions_is_empty() {
        let plan = compute_deltas(
            &targets(&[]),
            &[],
            &prices(&[]),
            10_000.0,
            1_000.0,
            &params(),
        );
        assert_eq!(plan, RebalancePlan::default());
    }
}
