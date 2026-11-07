//! Ризик-менеджер: розмір позиції та ліміти. Спільний для рушія і бектестера.

mod limits;
mod sizing;

use std::collections::{HashMap, HashSet};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::price::{to_f64, to_price};
use crate::signal::EntrySignal;
use crate::types::{StrategyId, Symbol};

use limits::EPS;
pub use limits::{daily_loss_breached, price_sane, weekly_loss_breached};
use sizing::{SizeError, size_position};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RiskConfig {
    pub risk_per_trade: f64,
    pub max_open_positions: usize,
    pub max_position_pct: f64,
    pub max_sector_pct: f64,
    pub max_gross_exposure: f64,
    pub daily_loss_limit: f64,
    pub weekly_loss_limit: f64,
    pub max_orders_per_day: u32,
    pub price_sanity_pct: f64,
    pub max_gap: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            risk_per_trade: 0.01,
            max_open_positions: 10,
            max_position_pct: 0.15,
            max_sector_pct: 0.30,
            max_gross_exposure: 1.0,
            daily_loss_limit: 0.03,
            weekly_loss_limit: 0.06,
            max_orders_per_day: 20,
            price_sanity_pct: 0.05,
            max_gap: 0.01,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HeldPosition {
    pub symbol: Symbol,
    pub strategy: StrategyId,
    pub sector: String,
    pub market_value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PortfolioSnapshot {
    pub equity: f64,
    pub cash: f64,
    pub positions: Vec<HeldPosition>,
    pub orders_planned: u32,
    pub entries_blocked: Option<String>,
    pub allocations: HashMap<StrategyId, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Candidate {
    pub signal: EntrySignal,
    pub sector: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "detail", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum RejectReason {
    EntriesBlocked(String),
    InvalidStop,
    AlreadyHolding,
    DuplicateSymbol,
    MaxOpenPositions,
    MaxOrdersPerDay,
    QtyBelowOne,
    SectorLimit,
    GrossExposure,
    StrategyAllocation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum RiskDecision {
    Accepted {
        signal: EntrySignal,
        sector: String,
        qty: u32,
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        limit_price: Decimal,
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        stop_price: Decimal,
        #[cfg_attr(feature = "ts", ts(type = "string | null"))]
        take_profit: Option<Decimal>,
        risk_usd: f64,
    },
    Rejected {
        signal: EntrySignal,
        reason: RejectReason,
    },
}

/// Відбирає кандидатів за силою сигналу та лімітами ризику.
pub fn evaluate(
    mut candidates: Vec<Candidate>,
    portfolio: &PortfolioSnapshot,
    cfg: &RiskConfig,
) -> Vec<RiskDecision> {
    candidates.sort_by(|a, b| {
        b.signal
            .strength
            .total_cmp(&a.signal.strength)
            .then_with(|| a.signal.symbol.cmp(&b.signal.symbol))
    });

    let equity = portfolio.equity;
    let held: HashSet<&Symbol> = portfolio.positions.iter().map(|p| &p.symbol).collect();
    let mut accepted: HashSet<Symbol> = HashSet::new();
    let mut positions = portfolio.positions.len();
    let mut orders = portfolio.orders_planned;
    let mut cash = portfolio.cash;
    let mut gross = 0.0;
    let mut by_sector: HashMap<String, f64> = HashMap::new();
    let mut by_strategy: HashMap<StrategyId, f64> = HashMap::new();
    for p in &portfolio.positions {
        gross += p.market_value;
        *by_sector.entry(p.sector.clone()).or_insert(0.0) += p.market_value;
        *by_strategy.entry(p.strategy.clone()).or_insert(0.0) += p.market_value;
    }

    let mut out = Vec::with_capacity(candidates.len());
    for c in candidates {
        let Candidate { signal, sector } = c;
        let reject = |signal: EntrySignal, reason| RiskDecision::Rejected { signal, reason };

        if let Some(why) = &portfolio.entries_blocked {
            out.push(reject(signal, RejectReason::EntriesBlocked(why.clone())));
            continue;
        }
        if signal.stop_price >= signal.reference_price {
            out.push(reject(signal, RejectReason::InvalidStop));
            continue;
        }
        if held.contains(&signal.symbol) {
            out.push(reject(signal, RejectReason::AlreadyHolding));
            continue;
        }
        if accepted.contains(&signal.symbol) {
            out.push(reject(signal, RejectReason::DuplicateSymbol));
            continue;
        }
        if positions >= cfg.max_open_positions {
            out.push(reject(signal, RejectReason::MaxOpenPositions));
            continue;
        }
        if orders >= cfg.max_orders_per_day {
            out.push(reject(signal, RejectReason::MaxOrdersPerDay));
            continue;
        }

        let sized =
            match size_position(signal.reference_price, signal.stop_price, equity, cash, cfg) {
                Ok(s) => s,
                Err(SizeError::InvalidStop) => {
                    out.push(reject(signal, RejectReason::InvalidStop));
                    continue;
                }
                Err(SizeError::QtyBelowOne) => {
                    out.push(reject(signal, RejectReason::QtyBelowOne));
                    continue;
                }
            };

        let value = f64::from(sized.qty) * to_f64(sized.limit);
        let sector_now = by_sector.get(&sector).copied().unwrap_or(0.0);
        if sector_now + value > equity * cfg.max_sector_pct + EPS {
            out.push(reject(signal, RejectReason::SectorLimit));
            continue;
        }
        if gross + value > equity * cfg.max_gross_exposure + EPS {
            out.push(reject(signal, RejectReason::GrossExposure));
            continue;
        }
        let alloc = portfolio
            .allocations
            .get(&signal.strategy)
            .copied()
            .unwrap_or(0.0);
        let strat_now = by_strategy.get(&signal.strategy).copied().unwrap_or(0.0);
        if strat_now + value > equity * alloc + EPS {
            out.push(reject(signal, RejectReason::StrategyAllocation));
            continue;
        }

        positions += 1;
        orders += 1;
        cash -= value;
        gross += value;
        *by_sector.entry(sector.clone()).or_insert(0.0) += value;
        *by_strategy.entry(signal.strategy.clone()).or_insert(0.0) += value;
        accepted.insert(signal.symbol.clone());
        let risk_usd = f64::from(sized.qty) * to_f64(sized.limit - sized.stop);
        let take_profit = signal.take_profit.and_then(|t| to_price(t).ok());
        out.push(RiskDecision::Accepted {
            signal,
            sector,
            qty: sized.qty,
            limit_price: sized.limit,
            stop_price: sized.stop,
            take_profit,
            risk_usd,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use rust_decimal_macros::dec;

    use super::*;

    fn snap(equity: f64) -> PortfolioSnapshot {
        PortfolioSnapshot {
            equity,
            cash: equity,
            positions: vec![],
            orders_planned: 0,
            entries_blocked: None,
            allocations: HashMap::from([
                (StrategyId::new("tp"), 0.5),
                (StrategyId::new("bo"), 0.5),
            ]),
        }
    }

    fn cand_for(
        strategy: &str,
        sym: &str,
        r: f64,
        s: f64,
        strength: f64,
        sector: &str,
    ) -> Candidate {
        Candidate {
            signal: EntrySignal {
                strategy: StrategyId::new(strategy),
                symbol: Symbol::new(sym),
                signal_date: NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
                reference_price: r,
                stop_price: s,
                take_profit: None,
                strength,
                explanation: String::new(),
            },
            sector: sector.into(),
        }
    }

    fn cand(sym: &str, r: f64, s: f64, strength: f64, sector: &str) -> Candidate {
        cand_for("tp", sym, r, s, strength, sector)
    }

    fn held(sym: &str, strategy: &str, sector: &str, mv: f64) -> HeldPosition {
        HeldPosition {
            symbol: Symbol::new(sym),
            strategy: StrategyId::new(strategy),
            sector: sector.into(),
            market_value: mv,
        }
    }

    fn reason(d: &RiskDecision) -> &RejectReason {
        match d {
            RiskDecision::Rejected { reason, .. } => reason,
            other => panic!("expected rejection, got {other:?}"),
        }
    }

    fn sym_of(d: &RiskDecision) -> &str {
        match d {
            RiskDecision::Accepted { signal, .. } | RiskDecision::Rejected { signal, .. } => {
                signal.symbol.as_str()
            }
        }
    }

    #[test]
    fn sizes_by_risk_per_trade() {
        let d = evaluate(
            vec![cand("AAPL", 100.0, 95.0, 0.9, "Tech")],
            &snap(100_000.0),
            &RiskConfig::default(),
        );
        assert!(matches!(
            &d[0],
            RiskDecision::Accepted { qty: 148, limit_price, .. } if *limit_price == dec!(101.00)
        ));
        let RiskDecision::Accepted {
            risk_usd,
            stop_price,
            ..
        } = &d[0]
        else {
            panic!("expected accepted")
        };
        assert_eq!(*stop_price, dec!(95.00));
        assert!((risk_usd - 148.0 * 6.0).abs() < 1e-9);
    }

    #[test]
    fn rejects_when_entries_blocked() {
        let mut p = snap(100_000.0);
        p.entries_blocked = Some("daily loss".into());
        let d = evaluate(
            vec![cand("AAPL", 100.0, 95.0, 0.9, "Tech")],
            &p,
            &RiskConfig::default(),
        );
        assert_eq!(
            reason(&d[0]),
            &RejectReason::EntriesBlocked("daily loss".into())
        );
    }

    #[test]
    fn rejects_invalid_stop() {
        let d = evaluate(
            vec![cand("AAPL", 100.0, 100.0, 0.9, "Tech")],
            &snap(100_000.0),
            &RiskConfig::default(),
        );
        assert_eq!(reason(&d[0]), &RejectReason::InvalidStop);
    }

    #[test]
    fn rejects_already_holding() {
        let mut p = snap(100_000.0);
        p.positions.push(held("AAPL", "tp", "Tech", 1000.0));
        let d = evaluate(
            vec![cand("AAPL", 100.0, 95.0, 0.9, "Tech")],
            &p,
            &RiskConfig::default(),
        );
        assert_eq!(reason(&d[0]), &RejectReason::AlreadyHolding);
    }

    #[test]
    fn max_open_positions_respected_in_strength_order() {
        let mut p = snap(100_000.0);
        for i in 0..9 {
            p.positions
                .push(held(&format!("H{i}"), "bo", &format!("S{i}"), 100.0));
        }
        let d = evaluate(
            vec![
                cand("WEAK", 100.0, 95.0, 0.2, "Energy"),
                cand("STRONG", 100.0, 95.0, 0.8, "Health"),
            ],
            &p,
            &RiskConfig::default(),
        );
        assert!(matches!(&d[0], RiskDecision::Accepted { .. }));
        assert_eq!(sym_of(&d[0]), "STRONG");
        assert_eq!(sym_of(&d[1]), "WEAK");
        assert_eq!(reason(&d[1]), &RejectReason::MaxOpenPositions);
    }

    #[test]
    fn ties_break_by_symbol() {
        let d = evaluate(
            vec![
                cand("ZZZ", 100.0, 95.0, 0.5, "A"),
                cand("AAA", 100.0, 95.0, 0.5, "B"),
            ],
            &snap(100_000.0),
            &RiskConfig::default(),
        );
        assert_eq!(sym_of(&d[0]), "AAA");
    }

    #[test]
    fn max_orders_per_day_respected() {
        let mut p = snap(100_000.0);
        p.orders_planned = 19;
        let d = evaluate(
            vec![
                cand("AAA", 100.0, 95.0, 0.9, "A"),
                cand("BBB", 100.0, 95.0, 0.8, "B"),
            ],
            &p,
            &RiskConfig::default(),
        );
        assert!(matches!(&d[0], RiskDecision::Accepted { .. }));
        assert_eq!(reason(&d[1]), &RejectReason::MaxOrdersPerDay);
    }

    #[test]
    fn sector_limit_rejects() {
        let mut p = snap(100_000.0);
        p.positions.push(held("MSFT", "bo", "Tech", 25_000.0));
        let d = evaluate(
            vec![cand("AAPL", 100.0, 95.0, 0.9, "Tech")],
            &p,
            &RiskConfig::default(),
        );
        assert_eq!(reason(&d[0]), &RejectReason::SectorLimit);
    }

    #[test]
    fn gross_exposure_rejects() {
        let mut p = snap(100_000.0);
        p.positions.push(held("MSFT", "bo", "Tech", 90_000.0));
        let d = evaluate(
            vec![cand("XOM", 100.0, 95.0, 0.9, "Energy")],
            &p,
            &RiskConfig::default(),
        );
        assert_eq!(reason(&d[0]), &RejectReason::GrossExposure);
    }

    #[test]
    fn strategy_allocation_rejects() {
        let mut p = snap(100_000.0);
        p.positions.push(held("MSFT", "tp", "Tech", 45_000.0));
        let d = evaluate(
            vec![cand("XOM", 100.0, 95.0, 0.9, "Energy")],
            &p,
            &RiskConfig::default(),
        );
        assert_eq!(reason(&d[0]), &RejectReason::StrategyAllocation);
    }

    #[test]
    fn missing_allocation_rejects() {
        let d = evaluate(
            vec![cand_for("other", "XOM", 100.0, 95.0, 0.9, "Energy")],
            &snap(100_000.0),
            &RiskConfig::default(),
        );
        assert_eq!(reason(&d[0]), &RejectReason::StrategyAllocation);
    }

    #[test]
    fn qty_below_one() {
        let d = evaluate(
            vec![cand("BKNG", 900.0, 850.0, 0.9, "Cons")],
            &snap(1_000.0),
            &RiskConfig::default(),
        );
        assert_eq!(reason(&d[0]), &RejectReason::QtyBelowOne);
    }

    #[test]
    fn duplicate_symbol_in_round_rejected() {
        let d = evaluate(
            vec![
                cand("AAPL", 100.0, 95.0, 0.9, "Tech"),
                cand("AAPL", 100.0, 95.0, 0.5, "Tech"),
            ],
            &snap(100_000.0),
            &RiskConfig::default(),
        );
        assert!(matches!(&d[0], RiskDecision::Accepted { .. }));
        assert_eq!(reason(&d[1]), &RejectReason::DuplicateSymbol);
    }

    #[test]
    fn accepted_candidates_consume_sector_capacity() {
        // Дві позиції по ~14.9% в одному секторі (29.9% < 30%), третя не влізає.
        let d = evaluate(
            vec![
                cand("AAA", 100.0, 95.0, 0.9, "Tech"),
                cand("BBB", 100.0, 95.0, 0.8, "Tech"),
                cand("CCC", 100.0, 95.0, 0.7, "Tech"),
            ],
            &snap(100_000.0),
            &RiskConfig::default(),
        );
        assert!(matches!(&d[0], RiskDecision::Accepted { .. }));
        assert!(matches!(&d[1], RiskDecision::Accepted { .. }));
        assert_eq!(reason(&d[2]), &RejectReason::SectorLimit);
    }

    #[test]
    fn take_profit_is_converted() {
        let mut c = cand("AAPL", 100.0, 95.0, 0.9, "Tech");
        c.signal.take_profit = Some(110.004);
        let d = evaluate(vec![c], &snap(100_000.0), &RiskConfig::default());
        assert!(matches!(
            &d[0],
            RiskDecision::Accepted { take_profit: Some(t), .. } if *t == dec!(110.00)
        ));
    }

    #[test]
    fn reject_reason_serializes_tagged() {
        let v = serde_json::to_value(RejectReason::EntriesBlocked("x".into())).unwrap();
        assert_eq!(v["type"], "entries_blocked");
        assert_eq!(v["detail"], "x");
        let v = serde_json::to_value(RejectReason::InvalidStop).unwrap();
        assert_eq!(v["type"], "invalid_stop");
    }

    #[test]
    fn default_config_matches_constraints() {
        let c = RiskConfig::default();
        assert_eq!(c.max_open_positions, 10);
        assert_eq!(c.max_orders_per_day, 20);
        assert!((c.max_position_pct - 0.15).abs() < f64::EPSILON);
        let parsed: RiskConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn loss_limits() {
        assert!(daily_loss_breached(Some(100_000.0), Some(96_900.0), 0.03));
        assert!(!daily_loss_breached(Some(100_000.0), Some(97_100.0), 0.03));
        assert!(weekly_loss_breached(Some(100_000.0), Some(93_900.0), 0.06));
        assert!(daily_loss_breached(Some(100_000.0), Some(97_000.0), 0.03));
    }

    #[test]
    fn loss_limits_without_history_do_not_block() {
        assert!(!daily_loss_breached(None, Some(100_000.0), 0.03));
        assert!(!weekly_loss_breached(None, None, 0.06));
    }

    #[test]
    fn price_sanity() {
        assert!(price_sane(105.0, 100.0, 0.05));
        assert!(!price_sane(105.01, 100.0, 0.05));
        assert!(!price_sane(100.0, 0.0, 0.05));
    }
}
