//! Симулятор брокера: позиції, виконання ордерів, комісії.
//!
//! Бари кожного символу впорядковані за датою; бар дня шукається бінарним пошуком
//! без копіювання.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use fond_core::portfolio::Position;
use fond_core::types::{Bar, StrategyId, Symbol};

use crate::request::{BtTrade, CostModel, ExitReason};

/// Частка прослизання: `slippage_bps / 10 000`.
pub fn slip_rate(costs: &CostModel) -> f64 {
    costs.slippage_bps / 10_000.0
}

/// `max(qty·per_share, min)`, якщо хоч одне з двох значень > 0, інакше 0.
pub fn commission(qty: u32, costs: &CostModel) -> f64 {
    if costs.commission_per_share > 0.0 || costs.commission_min > 0.0 {
        (f64::from(qty) * costs.commission_per_share).max(costs.commission_min)
    } else {
        0.0
    }
}

/// Лімітний вхід на відкритті: якщо `open <= limit`, то `min(open·(1+slip), limit)`,
/// інакше ордер не виконується.
pub fn entry_fill_price(open: f64, limit: f64, slip: f64) -> Option<f64> {
    (open <= limit).then(|| (open * (1.0 + slip)).min(limit))
}

/// Вихід протягом дня за стопом або ціллю. Стоп перевіряється першим (песимістично):
/// `low <= stop` -> `min(open, stop)·(1−slip)`; інакше `high >= tp` -> `max(open, tp)·(1−slip)`.
pub fn intraday_exit(
    bar: &Bar,
    stop: f64,
    take_profit: Option<f64>,
    slip: f64,
) -> Option<(f64, ExitReason)> {
    if bar.low <= stop {
        return Some((bar.open.min(stop) * (1.0 - slip), ExitReason::Stop));
    }
    match take_profit {
        Some(tp) if bar.high >= tp => Some((bar.open.max(tp) * (1.0 - slip), ExitReason::Target)),
        _ => None,
    }
}

/// Індекс бару з датою `date` (бінарний пошук).
pub fn bar_index(bars: &[Bar], date: NaiveDate) -> Option<usize> {
    bars.binary_search_by_key(&date, |b| b.date).ok()
}

fn bar_on<'a>(
    bars: &'a HashMap<Symbol, Vec<Bar>>,
    symbol: &Symbol,
    date: NaiveDate,
) -> Option<&'a Bar> {
    let v = bars.get(symbol)?;
    bar_index(v, date).map(|i| &v[i])
}

/// Прийнятий ризик-менеджером вхід, що чекає на відкриття наступного дня.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingEntry {
    pub symbol: Symbol,
    pub strategy: StrategyId,
    pub sector: String,
    pub qty: u32,
    pub limit: f64,
    pub stop: f64,
    pub take_profit: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpenPosition {
    pub position: Position,
    pub entry_commission: f64,
    /// Останнє відоме закриття (для оцінки, якщо бару сьогодні немає).
    pub last_close: f64,
}

impl OpenPosition {
    pub(crate) fn market_value(&self) -> f64 {
        f64::from(self.position.qty) * self.last_close
    }
}

/// Стан рахунку. Позиції й відкладені виходи — у `BTreeMap`, тож обхід детермінований.
#[derive(Debug)]
pub(crate) struct Sim {
    costs: CostModel,
    slip: f64,
    cash: f64,
    positions: BTreeMap<Symbol, OpenPosition>,
    pending_exits: BTreeMap<Symbol, String>,
    pending_entries: Vec<PendingEntry>,
    trades: Vec<BtTrade>,
}

impl Sim {
    pub(crate) fn new(cash: f64, costs: CostModel) -> Self {
        Self {
            slip: slip_rate(&costs),
            costs,
            cash,
            positions: BTreeMap::new(),
            pending_exits: BTreeMap::new(),
            pending_entries: Vec::new(),
            trades: Vec::new(),
        }
    }

    pub(crate) fn cash(&self) -> f64 {
        self.cash
    }

    /// Позиції в порядку тікерів.
    pub(crate) fn positions(&self) -> impl Iterator<Item = &OpenPosition> {
        self.positions.values()
    }

    pub(crate) fn has_positions(&self) -> bool {
        !self.positions.is_empty()
    }

    pub(crate) fn pending_exit_count(&self) -> usize {
        self.pending_exits.len()
    }

    pub(crate) fn queue_exit(&mut self, symbol: Symbol, reason: String) {
        self.pending_exits.entry(symbol).or_insert(reason);
    }

    pub(crate) fn queue_entry(&mut self, entry: PendingEntry) {
        self.pending_entries.push(entry);
    }

    /// Стоп лише піднімається; діє з наступного дня.
    pub(crate) fn raise_stop(&mut self, symbol: &Symbol, new_stop: f64) {
        if let Some(p) = self.positions.get_mut(symbol)
            && new_stop > p.position.stop_price
        {
            p.position.stop_price = new_stop;
        }
    }

    pub(crate) fn equity(&self) -> f64 {
        self.cash
            + self
                .positions
                .values()
                .map(OpenPosition::market_value)
                .sum::<f64>()
    }

    /// Відкриття дня: спершу відкладені виходи (без бару лишаються відкладеними),
    /// потім входи (без бару або з гепом вище limit — скасовуються).
    pub(crate) fn open(&mut self, date: NaiveDate, bars: &HashMap<Symbol, Vec<Bar>>) {
        for (symbol, reason) in std::mem::take(&mut self.pending_exits) {
            match bar_on(bars, &symbol, date) {
                Some(bar) => {
                    let price = bar.open * (1.0 - self.slip);
                    self.close(&symbol, date, price, ExitReason::Signal(reason));
                }
                None => {
                    self.pending_exits.insert(symbol, reason);
                }
            }
        }

        for e in std::mem::take(&mut self.pending_entries) {
            if self.positions.contains_key(&e.symbol) {
                continue;
            }
            let Some(bar) = bar_on(bars, &e.symbol, date) else {
                continue;
            };
            let Some(price) = entry_fill_price(bar.open, e.limit, self.slip) else {
                continue;
            };
            let fee = commission(e.qty, &self.costs);
            self.cash -= f64::from(e.qty) * price + fee;
            let position = Position {
                symbol: e.symbol.clone(),
                strategy: e.strategy,
                qty: e.qty,
                entry_price: price,
                entry_date: date,
                stop_price: e.stop,
                take_profit: e.take_profit,
                sector: e.sector,
            };
            self.positions.insert(
                e.symbol,
                OpenPosition {
                    position,
                    entry_commission: fee,
                    last_close: bar.close,
                },
            );
        }
    }

    /// Стоп і ціль протягом дня, зокрема для позицій, відкритих сьогодні.
    pub(crate) fn intraday(&mut self, date: NaiveDate, bars: &HashMap<Symbol, Vec<Bar>>) {
        let hits: Vec<(Symbol, f64, ExitReason)> = self
            .positions
            .iter()
            .filter_map(|(symbol, p)| {
                let bar = bar_on(bars, symbol, date)?;
                intraday_exit(
                    bar,
                    p.position.stop_price,
                    p.position.take_profit,
                    self.slip,
                )
                .map(|(price, why)| (symbol.clone(), price, why))
            })
            .collect();
        for (symbol, price, why) in hits {
            self.close(&symbol, date, price, why);
        }
    }

    /// Оновлює закриття позицій з баром сьогодні й повертає капітал.
    pub(crate) fn mark_to_market(
        &mut self,
        date: NaiveDate,
        bars: &HashMap<Symbol, Vec<Bar>>,
    ) -> f64 {
        for (symbol, p) in &mut self.positions {
            if let Some(bar) = bar_on(bars, symbol, date) {
                p.last_close = bar.close;
            }
        }
        self.equity()
    }

    /// Кінець тесту: усі позиції закриваються за останнім закриттям.
    pub(crate) fn close_all(&mut self, date: NaiveDate) {
        let all: Vec<(Symbol, f64)> = self
            .positions
            .iter()
            .map(|(s, p)| (s.clone(), p.last_close))
            .collect();
        for (symbol, price) in all {
            self.close(&symbol, date, price, ExitReason::EndOfTest);
        }
        self.pending_exits.clear();
        self.pending_entries.clear();
    }

    pub(crate) fn into_trades(self) -> Vec<BtTrade> {
        self.trades
    }

    fn close(&mut self, symbol: &Symbol, date: NaiveDate, price: f64, reason: ExitReason) {
        let Some(open) = self.positions.remove(symbol) else {
            return;
        };
        let p = open.position;
        let qty = f64::from(p.qty);
        let fee = commission(p.qty, &self.costs);
        self.cash += qty * price - fee;
        self.trades.push(BtTrade {
            pnl: (price - p.entry_price) * qty - open.entry_commission - fee,
            symbol: p.symbol,
            strategy: p.strategy,
            entry_date: p.entry_date,
            entry_price: p.entry_price,
            exit_date: date,
            exit_price: price,
            qty: p.qty,
            exit_reason: reason,
        });
    }
}
