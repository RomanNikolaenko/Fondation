//! Подієвий прогін бектесту: рішення на закритті дня t лише за барами `..=t`,
//! виконання — з відкриття t+1.

use std::collections::HashMap;

use chrono::{Datelike, IsoWeek, NaiveTime};
use fond_core::calendar::TradingCalendar;
use fond_core::filters::earnings_blocks;
use fond_core::price::{to_f64, to_price};
use fond_core::risk::{
    self, Candidate, HeldPosition, PortfolioSnapshot, RejectReason, RiskDecision,
    daily_loss_breached, weekly_loss_breached,
};
use fond_core::scanner::scan_symbol;
use fond_core::signal::{EntrySignal, ExitDecision, MarketContext};
use fond_core::traits::{Session, Strategy};
use fond_core::types::{Bar, StrategyId, Symbol};
use fond_strategies::{
    allocations, build_all, entries_enabled, market_filter_ok, pick_best_per_symbol,
};

use crate::metrics::report;
use crate::request::{
    BacktestError, BacktestRequest, BacktestResult, BtDecision, EquityPoint, HistoricalData,
    Limitation,
};
use crate::sim::{PendingEntry, Sim, bar_index};

const UNKNOWN_SECTOR: &str = "Unknown";
const DAILY_LOSS: &str = "daily loss limit";
const WEEKLY_LOSS: &str = "weekly loss limit";
const EARNINGS: &str = "earnings";

/// Прогін історії за алгоритмом бектесту. Однакові вхідні дані дають побітово
/// однаковий результат: символи обходяться у відсортованому порядку.
pub fn run_backtest(
    req: &BacktestRequest,
    data: &HistoricalData,
) -> Result<BacktestResult, BacktestError> {
    req.strategies
        .validate()
        .map_err(BacktestError::InvalidConfig)?;
    let market = data
        .bars
        .get(&req.strategies.market_filter.symbol)
        .ok_or(BacktestError::MissingMarketSymbol)?;
    if req.from > req.to {
        return Err(BacktestError::EmptyRange);
    }
    let first = market.partition_point(|b| b.date < req.from);
    let end = market.partition_point(|b| b.date <= req.to);
    if first >= end {
        return Err(BacktestError::EmptyRange);
    }

    let mut run = Run::new(req, data, market, first);
    for i in first..end {
        run.day(i, i + 1 == end);
    }
    Ok(run.finish())
}

struct Run<'a> {
    req: &'a BacktestRequest,
    data: &'a HistoricalData,
    market: &'a [Bar],
    /// Усі символи даних (разом з ринковим), відсортовані.
    symbols: Vec<(&'a Symbol, &'a [Bar])>,
    strategies: Vec<Box<dyn Strategy>>,
    entry_enabled: Vec<bool>,
    allocations: HashMap<StrategyId, f64>,
    calendar: TradingCalendar,
    sim: Sim,
    equity: Vec<EquityPoint>,
    decisions: Vec<BtDecision>,
    first_close: f64,
    week: Option<IsoWeek>,
    week_base: f64,
    weekly_blocked: bool,
}

impl<'a> Run<'a> {
    fn new(
        req: &'a BacktestRequest,
        data: &'a HistoricalData,
        market: &'a [Bar],
        first: usize,
    ) -> Self {
        let mut symbols: Vec<(&Symbol, &[Bar])> =
            data.bars.iter().map(|(s, b)| (s, b.as_slice())).collect();
        symbols.sort_by(|a, b| a.0.cmp(b.0));
        let strategies = build_all(&req.strategies);
        let entry_enabled = strategies
            .iter()
            .map(|s| entries_enabled(&req.strategies, &s.id()))
            .collect();
        let open = NaiveTime::from_hms_opt(9, 30, 0).unwrap_or_default();
        let close = NaiveTime::from_hms_opt(16, 0, 0).unwrap_or_default();
        let calendar = TradingCalendar::new(
            market
                .iter()
                .map(|b| Session {
                    date: b.date,
                    open,
                    close,
                })
                .collect(),
        );
        Self {
            req,
            data,
            market,
            symbols,
            strategies,
            entry_enabled,
            allocations: allocations(&req.strategies),
            calendar,
            sim: Sim::new(req.initial_equity, req.costs.clone()),
            equity: Vec::new(),
            decisions: Vec::new(),
            first_close: market[first].close,
            week: None,
            week_base: req.initial_equity,
            weekly_blocked: false,
        }
    }

    /// Один торговий день `market[i]`: відкриття, день, закриття, ліміти, рішення.
    fn day(&mut self, i: usize, last: bool) {
        let today = &self.market[i];
        let t = today.date;
        let bars = &self.data.bars;

        self.sim.open(t, bars);
        self.sim.intraday(t, bars);
        // Останній день: ліквідація до фіксації equity, щоб метрики врахували
        // прослизання й комісії закриття.
        if last {
            self.sim.close_all(t);
        }
        let equity = self.sim.mark_to_market(t, bars);

        let prev = self.equity.last().map(|p| p.equity);
        let iso = t.iso_week();
        if self.week != Some(iso) {
            // База тижня — капітал на останньому закритті попереднього ISO-тижня.
            self.week = Some(iso);
            self.week_base = prev.unwrap_or(self.req.initial_equity);
            self.weekly_blocked = false;
        }
        let benchmark = if self.first_close > 0.0 {
            self.req.initial_equity * today.close / self.first_close
        } else {
            self.req.initial_equity
        };
        self.equity.push(EquityPoint {
            date: t,
            equity,
            benchmark,
            in_market: self.sim.has_positions(),
        });

        let limits = &self.req.risk;
        if weekly_loss_breached(Some(self.week_base), Some(equity), limits.weekly_loss_limit) {
            self.weekly_blocked = true;
        }
        let daily = daily_loss_breached(prev, Some(equity), limits.daily_loss_limit);

        if last {
            return;
        }
        let blocked = if self.weekly_blocked {
            Some(WEEKLY_LOSS)
        } else if daily {
            Some(DAILY_LOSS)
        } else {
            None
        };
        self.decide(i, equity, blocked);
    }

    /// Рішення на закритті `market[i]` за зрізами `..=t`.
    fn decide(&mut self, i: usize, equity: f64, blocked: Option<&str>) {
        let t = self.market[i].date;
        let ctx = MarketContext {
            date: t,
            market_filter_ok: market_filter_ok(
                &self.market[..=i],
                &self.req.strategies.market_filter,
            ),
        };

        self.review_positions(&ctx);

        let mut kept = Vec::new();
        for signal in self.entry_signals(&ctx) {
            if self.earnings_blocked(&signal) {
                self.decisions
                    .push(decision(&signal, false, Some(EARNINGS.to_string())));
            } else {
                kept.push(signal);
            }
        }
        let picked = pick_best_per_symbol(kept);
        if picked.is_empty() {
            return;
        }

        let candidates = picked
            .into_iter()
            .map(|signal| Candidate {
                sector: self.sector(&signal.symbol),
                signal,
            })
            .collect();
        let snapshot = self.snapshot(equity, blocked);
        for d in risk::evaluate(candidates, &snapshot, &self.req.risk) {
            self.record(d);
        }
    }

    /// Виходи стратегій по відкритих позиціях: `Exit` — у відкладені, `MoveStop` — лише вгору.
    fn review_positions(&mut self, ctx: &MarketContext) {
        let mut exits = Vec::new();
        let mut stops = Vec::new();
        for p in self.sim.positions() {
            let pos = &p.position;
            let Some(bars) = self.data.bars.get(&pos.symbol) else {
                continue;
            };
            let Some(idx) = bar_index(bars, ctx.date) else {
                continue;
            };
            let Some(strategy) = self.strategies.iter().find(|s| s.id() == pos.strategy) else {
                continue;
            };
            match strategy.evaluate_exit(ctx, pos, &bars[..=idx]) {
                ExitDecision::Hold => {}
                ExitDecision::Exit { reason } => exits.push((pos.symbol.clone(), reason)),
                ExitDecision::MoveStop { new_stop, .. } => {
                    if let Ok(stop) = to_price(new_stop) {
                        stops.push((pos.symbol.clone(), to_f64(stop)));
                    }
                }
            }
        }
        for (symbol, reason) in exits {
            self.sim.queue_exit(symbol, reason);
        }
        for (symbol, stop) in stops {
            self.sim.raise_stop(&symbol, stop);
        }
    }

    /// Сигнали входу від увімкнених стратегій для символів, що пройшли сканер у день t.
    fn entry_signals(&self, ctx: &MarketContext) -> Vec<EntrySignal> {
        let mut out = Vec::new();
        for &(symbol, bars) in &self.symbols {
            let Some(idx) = bar_index(bars, ctx.date) else {
                continue;
            };
            let history = &bars[..=idx];
            if scan_symbol(history, true, ctx.date, &self.req.scanner).is_err() {
                continue;
            }
            for (strategy, &enabled) in self.strategies.iter().zip(&self.entry_enabled) {
                if !enabled {
                    continue;
                }
                if let Some(signal) = strategy.evaluate_entry(ctx, symbol, history) {
                    out.push(signal);
                }
            }
        }
        out
    }

    /// Найближчий звіт `>= signal_date` у вікні `earnings_blocks`; без календаря звітів — ні.
    fn earnings_blocked(&self, signal: &EntrySignal) -> bool {
        let Some(earnings) = &self.data.earnings else {
            return false;
        };
        let next = earnings.get(&signal.symbol).and_then(|dates| {
            dates
                .iter()
                .filter(|d| **d >= signal.signal_date)
                .min()
                .copied()
        });
        earnings_blocks(
            signal.signal_date,
            next,
            &self.calendar,
            self.data.earnings_days_ahead,
        )
    }

    fn sector(&self, symbol: &Symbol) -> String {
        self.data
            .sectors
            .get(symbol)
            .cloned()
            .unwrap_or_else(|| UNKNOWN_SECTOR.to_string())
    }

    fn snapshot(&self, equity: f64, blocked: Option<&str>) -> PortfolioSnapshot {
        PortfolioSnapshot {
            equity,
            cash: self.sim.cash(),
            positions: self
                .sim
                .positions()
                .map(|p| HeldPosition {
                    symbol: p.position.symbol.clone(),
                    strategy: p.position.strategy.clone(),
                    sector: p.position.sector.clone(),
                    market_value: p.market_value(),
                })
                .collect(),
            orders_planned: u32::try_from(self.sim.pending_exit_count()).unwrap_or(u32::MAX),
            entries_blocked: blocked.map(str::to_string),
            allocations: self.allocations.clone(),
        }
    }

    fn record(&mut self, d: RiskDecision) {
        match d {
            RiskDecision::Accepted {
                signal,
                sector,
                qty,
                limit_price,
                stop_price,
                take_profit,
                ..
            } => {
                self.decisions.push(decision(&signal, true, None));
                self.sim.queue_entry(PendingEntry {
                    symbol: signal.symbol,
                    strategy: signal.strategy,
                    sector,
                    qty,
                    limit: to_f64(limit_price),
                    stop: to_f64(stop_price),
                    take_profit: take_profit.map(to_f64),
                });
            }
            RiskDecision::Rejected { signal, reason } => {
                self.decisions
                    .push(decision(&signal, false, Some(reject_code(&reason))));
            }
        }
    }

    fn finish(self) -> BacktestResult {
        let trades = self.sim.into_trades();
        let metrics = report(&self.equity, &trades, self.req.split);
        let mut limitations = vec![
            Limitation::SurvivorshipBias,
            Limitation::NewsFilterNotSimulated,
        ];
        if self.data.earnings.is_none() {
            limitations.push(Limitation::EarningsFilterNotApplied);
        }
        limitations.push(Limitation::WeeklyLossApproximation);
        BacktestResult {
            trades,
            equity: self.equity,
            decisions: self.decisions,
            metrics,
            limitations,
        }
    }
}

fn decision(signal: &EntrySignal, accepted: bool, reason: Option<String>) -> BtDecision {
    BtDecision {
        date: signal.signal_date,
        symbol: signal.symbol.clone(),
        strategy: signal.strategy.clone(),
        accepted,
        reason,
    }
}

/// Коротка причина відмови: назва варіанта в snake_case.
fn reject_code(reason: &RejectReason) -> String {
    let code = match reason {
        RejectReason::EntriesBlocked(why) => return format!("entries_blocked: {why}"),
        RejectReason::InvalidStop => "invalid_stop",
        RejectReason::AlreadyHolding => "already_holding",
        RejectReason::DuplicateSymbol => "duplicate_symbol",
        RejectReason::MaxOpenPositions => "max_open_positions",
        RejectReason::MaxOrdersPerDay => "max_orders_per_day",
        RejectReason::QtyBelowOne => "qty_below_one",
        RejectReason::SectorLimit => "sector_limit",
        RejectReason::GrossExposure => "gross_exposure",
        RejectReason::StrategyAllocation => "strategy_allocation",
    };
    code.to_string()
}
