//! Крок аналізу (16:30 ET): дані → скан → виходи → входи → фільтри → ризик → наміри.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::NaiveDate;
use fond_api::config::EngineConfig;
use fond_api::events::EngineEventDto;
use fond_core::calendar::TradingCalendar;
use fond_core::ids::{IntentTag, intent_id};
use fond_core::intent::{Intent, IntentKind, IntentStatus};
use fond_core::portfolio::TrackedPosition;
use fond_core::price::{to_f64, to_price};
use fond_core::risk::{
    Candidate, HeldPosition, PortfolioSnapshot, RejectReason, RiskDecision, evaluate,
};
use fond_core::scanner::{ScanReject, scan_symbol, validate_bars};
use fond_core::signal::{EntrySignal, ExitDecision, MarketContext};
use fond_core::traits::{NewsVerdict, Strategy};
use fond_core::types::{Bar, ExecutionMode, StrategyId, Symbol};
use fond_store::{
    EquityKind, EquitySnapshot, EventKind, SignalRecord, SignalStatus, UniverseEntry,
};
use fond_strategies::{
    allocations, build_all, entries_enabled, market_filter_ok, pick_best_per_symbol,
};
use rust_decimal::Decimal;
use serde_json::json;

use super::filters::{FilterOutcome, earnings_filter, news_filter};
use super::refresh::{refresh_market_data, window_start};
use super::{CycleError, entries_block_reason, journal, trading_calendar};
use crate::deps::Deps;

const UNKNOWN_SECTOR: &str = "Unknown";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnalysisSummary {
    pub scanned: usize,
    pub candidates: usize,
    pub signals: usize,
    pub entries_created: usize,
    pub exits_created: usize,
    pub stop_moves_created: usize,
}

/// Причина відхилення ризик-менеджером — коротко, українською (показується в UI).
pub fn reject_text(r: &RejectReason) -> String {
    let text = match r {
        RejectReason::EntriesBlocked(why) => return format!("Ризик: входи заблоковано ({why})"),
        RejectReason::InvalidStop => "некоректний стоп (не нижче ціни входу)",
        RejectReason::AlreadyHolding => "позиція вже відкрита",
        RejectReason::DuplicateSymbol => "тікер уже прийнято в цьому циклі",
        RejectReason::MaxOpenPositions => "досягнуто максимум відкритих позицій",
        RejectReason::MaxOrdersPerDay => "досягнуто ліміт ордерів на день",
        RejectReason::QtyBelowOne => "розмір позиції менше однієї акції",
        RejectReason::SectorLimit => "перевищено ліміт на сектор",
        RejectReason::GrossExposure => "перевищено загальну експозицію",
        RejectReason::StrategyAllocation => "перевищено частку капіталу стратегії",
    };
    format!("Ризик: {text}")
}

/// Сектор з universe; порожній або відсутній — «Unknown».
pub(crate) fn sector_of(entry: Option<&UniverseEntry>) -> String {
    entry
        .map(|e| e.sector.trim())
        .filter(|x| !x.is_empty())
        .unwrap_or(UNKNOWN_SECTOR)
        .to_string()
}

/// Незавершений намір на вхід: ще може стати ордером або вже ним є.
fn is_live_entry(i: &Intent) -> bool {
    matches!(i.kind, IntentKind::Entry { .. })
        && matches!(
            i.status,
            IntentStatus::PendingApproval | IntentStatus::Approved | IntentStatus::Submitted
        )
}

fn scan_key(r: &ScanReject) -> &'static str {
    match r {
        ScanReject::NotTradable => "not_tradable",
        ScanReject::TooLittleHistory => "too_little_history",
        ScanReject::PriceBelowMin => "price_below_min",
        ScanReject::LowLiquidity => "low_liquidity",
        ScanReject::BadData(_) => "bad_data",
    }
}

fn scan_label(key: &str) -> &'static str {
    match key {
        "not_tradable" => "недоступні для торгівлі",
        "too_little_history" => "мало історії",
        "price_below_min" => "ціна нижче мінімуму",
        "low_liquidity" => "низька ліквідність",
        _ => "некоректні дані",
    }
}

/// Результат скану: бари тікерів universe і ті, що пройшли.
struct Scanned {
    bars: HashMap<Symbol, Vec<Bar>>,
    passed: Vec<Symbol>,
    total: usize,
}

/// Спільний контекст одного прогону аналізу.
struct Run<'a> {
    d: &'a Deps,
    cfg: &'a EngineConfig,
    date: NaiveDate,
    session: NaiveDate,
    from: NaiveDate,
}

/// Крок аналізу за день `date` (D): наміри створюються на наступну сесію.
pub async fn run_analysis(
    d: &Deps,
    cfg: &EngineConfig,
    date: NaiveDate,
) -> Result<AnalysisSummary, CycleError> {
    refresh_market_data(d, cfg, date).await?;
    let cal = trading_calendar(d, date).await?;
    let session = cal.next_session_after(date).ok_or(CycleError::NoCalendar)?;
    let run = Run {
        d,
        cfg,
        date,
        session,
        from: window_start(cfg, date),
    };
    run.execute(&cal).await
}

impl Run<'_> {
    async fn execute(&self, cal: &TradingCalendar) -> Result<AnalysisSummary, CycleError> {
        let (d, cfg) = (self.d, self.cfg);
        let mut summary = AnalysisSummary::default();

        let account = d.broker.account().await?;
        let (equity, cash) = (to_f64(account.equity), to_f64(account.cash));
        d.store
            .insert_equity(&EquitySnapshot {
                at: d.clock.now(),
                kind: EquityKind::Close,
                session_date: self.date,
                equity,
                cash,
            })
            .await?;

        let universe = d.store.universe(false).await?;
        let scan = self.scan(&universe).await?;
        summary.scanned = scan.total;

        let ctx = MarketContext {
            date: self.date,
            market_filter_ok: self.market_filter().await?,
        };

        let strategies = build_all(&cfg.strategies);
        let positions = d.store.positions().await?;
        let mut held = self
            .exits(&ctx, &strategies, &positions, &mut summary)
            .await?;

        // Наміри сесії (уже з новими виходами). Незавершений вхід на тікер (зокрема від
        // іншої стратегії з попереднього прогону) не дублюється і займає місце в лімітах.
        let session_intents = d.store.intents_for_session(self.session).await?;
        let live_entries: Vec<&Intent> = session_intents
            .iter()
            .filter(|i| is_live_entry(i))
            .collect();
        let mut planned: BTreeMap<&Symbol, &Intent> = BTreeMap::new();
        for i in &live_entries {
            planned.entry(&i.symbol).or_insert(i);
        }

        let names: HashMap<&Symbol, &UniverseEntry> =
            universe.iter().map(|e| (&e.symbol, e)).collect();
        let held_symbols: HashSet<&Symbol> = positions.iter().map(|p| &p.position.symbol).collect();
        let mut candidates: Vec<&Symbol> = Vec::with_capacity(scan.passed.len());
        for s in scan.passed.iter().filter(|s| !held_symbols.contains(s)) {
            match planned.get(s) {
                Some(i) => self.already_planned(i).await?,
                None => candidates.push(s),
            }
        }
        summary.candidates = candidates.len();

        let mut signals = Vec::new();
        for sym in candidates {
            let Some(bars) = scan.bars.get(sym) else {
                continue;
            };
            for st in strategies
                .iter()
                .filter(|s| entries_enabled(&cfg.strategies, &s.id()))
            {
                signals.extend(st.evaluate_entry(&ctx, sym, bars));
            }
        }
        let signals = pick_best_per_symbol(signals);
        summary.signals = signals.len();

        // Завжди: порушення тижневого ліміту ставить паузу навіть без сигналів.
        let blocked = entries_block_reason(d, cfg, self.date).await?;
        let entries = self
            .entries(cal, &names, signals, blocked.is_none())
            .await?;

        // Заплановані входи рахуються як позиції (вартість qty × ліміт) і витрачають кошти,
        // так само як прийняті в цьому раунді кандидати.
        let mut cash = cash;
        for i in live_entries
            .iter()
            .filter(|i| !held_symbols.contains(&i.symbol))
        {
            if let IntentKind::Entry {
                qty, limit_price, ..
            } = &i.kind
            {
                let value = f64::from(*qty) * to_f64(*limit_price);
                cash -= value;
                held.push(HeldPosition {
                    symbol: i.symbol.clone(),
                    strategy: i.strategy.clone(),
                    sector: sector_of(names.get(&i.symbol).copied()),
                    market_value: value,
                });
            }
        }
        let orders_planned = session_intents
            .iter()
            .filter(|i| match i.kind {
                IntentKind::Exit { .. } | IntentKind::MoveStop { .. } => true,
                IntentKind::Entry { .. } => matches!(
                    i.status,
                    IntentStatus::PendingApproval | IntentStatus::Approved
                ),
            })
            .count();
        let snapshot = PortfolioSnapshot {
            equity,
            cash,
            positions: held,
            orders_planned: u32::try_from(orders_planned).unwrap_or(u32::MAX),
            entries_blocked: blocked,
            allocations: allocations(&cfg.strategies),
        };
        let (created, pending) = self
            .persist_decisions(
                evaluate(entries.candidates, &snapshot, &cfg.risk),
                entries.news,
            )
            .await?;
        summary.entries_created = created;
        if pending > 0 {
            d.bus.publish(EngineEventDto::IntentsPending {
                session: self.session,
                count: pending,
            });
        }

        journal(
            d,
            EventKind::Cycle,
            None,
            None,
            format!(
                "Аналіз {} завершено: сигналів {}, нових намірів — входів {}, виходів {}, \
                 підтягувань стопа {} (сесія {})",
                self.date,
                summary.signals,
                summary.entries_created,
                summary.exits_created,
                summary.stop_moves_created,
                self.session
            ),
            json!({
                "date": self.date,
                "session": self.session,
                "scanned": summary.scanned,
                "candidates": summary.candidates,
                "signals": summary.signals,
                "entries_created": summary.entries_created,
                "exits_created": summary.exits_created,
                "stop_moves_created": summary.stop_moves_created,
            }),
        )
        .await?;
        Ok(summary)
    }

    /// Сканер для universe; одна підсумкова подія з кількістю відсіяних за причинами.
    async fn scan(&self, universe: &[UniverseEntry]) -> Result<Scanned, CycleError> {
        let d = self.d;
        let mut results = Vec::with_capacity(universe.len());
        let mut bars = HashMap::with_capacity(universe.len());
        for e in universe {
            let b = d.store.bars(&e.symbol, self.from, self.date).await?;
            let tradable = d
                .store
                .asset(&e.symbol)
                .await?
                .is_some_and(|a| a.tradable && a.active);
            let reject = scan_symbol(&b, tradable, self.date, &self.cfg.scanner).err();
            results.push((e.symbol.clone(), reject));
            bars.insert(e.symbol.clone(), b);
        }
        d.store.put_scan_results(self.date, &results).await?;

        let mut rejected: BTreeMap<&'static str, usize> = BTreeMap::new();
        for r in results.iter().filter_map(|(_, r)| r.as_ref()) {
            *rejected.entry(scan_key(r)).or_default() += 1;
        }
        let passed: Vec<Symbol> = results
            .iter()
            .filter(|(_, r)| r.is_none())
            .map(|(s, _)| s.clone())
            .collect();
        let mut message = format!("Сканер: пройшли {} з {}", passed.len(), results.len());
        if !rejected.is_empty() {
            let detail: Vec<String> = rejected
                .iter()
                .map(|(k, n)| format!("{} — {n}", scan_label(k)))
                .collect();
            message.push_str(&format!("; відсіяно: {}", detail.join(", ")));
        }
        journal(
            d,
            EventKind::Scan,
            None,
            None,
            message,
            json!({
                "date": self.date,
                "total": results.len(),
                "passed": passed.len(),
                "rejected": rejected,
            }),
        )
        .await?;
        Ok(Scanned {
            bars,
            passed,
            total: results.len(),
        })
    }

    /// Ринковий фільтр: свіжі коректні дані символу і close вище SMA.
    async fn market_filter(&self) -> Result<bool, CycleError> {
        let mf = &self.cfg.strategies.market_filter;
        let bars = self.d.store.bars(&mf.symbol, self.from, self.date).await?;
        let valid = validate_bars(&bars, self.date);
        let ok = valid.is_ok() && market_filter_ok(&bars, mf);
        let message = match (&valid, ok) {
            (Err(why), _) => format!(
                "Ринковий фільтр ({}): некоректні дані ({why}), нових входів немає",
                mf.symbol
            ),
            (Ok(()), true) => format!(
                "Ринковий фільтр ({}): закриття вище SMA{}, входи дозволено",
                mf.symbol, mf.sma
            ),
            (Ok(()), false) => format!(
                "Ринковий фільтр ({}): закриття не вище SMA{}, нових входів немає",
                mf.symbol, mf.sma
            ),
        };
        journal(
            self.d,
            EventKind::Filter,
            Some(&mf.symbol),
            None,
            message,
            json!({ "ok": ok, "symbol": mf.symbol, "sma": mf.sma }),
        )
        .await?;
        Ok(ok)
    }

    fn intent(
        &self,
        id: String,
        symbol: &Symbol,
        strategy: &StrategyId,
        kind: IntentKind,
        status: IntentStatus,
        explanation: String,
    ) -> Intent {
        let now = self.d.clock.now();
        Intent {
            id,
            session_date: self.session,
            signal_date: self.date,
            symbol: symbol.clone(),
            strategy: strategy.clone(),
            kind,
            status,
            explanation,
            created_at: now,
            updated_at: now,
            broker_order_id: None,
            error: None,
        }
    }

    /// Виходи й підтягування стопів для відстежуваних позицій (завжди `Approved`).
    /// Повертає позиції з ринковою вартістю за останнім close для ризик-менеджера.
    async fn exits(
        &self,
        ctx: &MarketContext,
        strategies: &[Box<dyn Strategy>],
        positions: &[TrackedPosition],
        summary: &mut AnalysisSummary,
    ) -> Result<Vec<HeldPosition>, CycleError> {
        let d = self.d;
        let mut held = Vec::with_capacity(positions.len());
        for tp in positions {
            let p = &tp.position;
            let bars = d.store.bars(&p.symbol, self.from, self.date).await?;
            let last_close = bars.last().map_or(p.entry_price, |b| b.close);
            held.push(HeldPosition {
                symbol: p.symbol.clone(),
                strategy: p.strategy.clone(),
                sector: p.sector.clone(),
                market_value: f64::from(p.qty) * last_close,
            });

            let Some(strategy) = strategies.iter().find(|s| s.id() == p.strategy) else {
                journal(
                    d,
                    EventKind::Error,
                    Some(&p.symbol),
                    None,
                    format!("Невідома стратегія «{}»: вихід не оцінено", p.strategy),
                    json!({ "strategy": p.strategy }),
                )
                .await?;
                continue;
            };
            if let Err(why) = validate_bars(&bars, self.date) {
                journal(
                    d,
                    EventKind::Error,
                    Some(&p.symbol),
                    None,
                    format!("Некоректні дані ({why}): вихід не оцінено"),
                    json!({ "reason": why }),
                )
                .await?;
                continue;
            }
            match strategy.evaluate_exit(ctx, p, &bars) {
                ExitDecision::Hold => {}
                ExitDecision::Exit { reason } => {
                    if self.exit_intent(tp, reason).await? {
                        summary.exits_created += 1;
                    }
                }
                ExitDecision::MoveStop { new_stop, reason } => {
                    if self.move_stop_intent(tp, new_stop, reason).await? {
                        summary.stop_moves_created += 1;
                    }
                }
            }
        }
        Ok(held)
    }

    async fn exit_intent(&self, tp: &TrackedPosition, reason: String) -> Result<bool, CycleError> {
        let p = &tp.position;
        let id = intent_id(self.session, &p.strategy, &p.symbol, IntentTag::Exit);
        let kind = IntentKind::Exit {
            qty: p.qty,
            reason: reason.clone(),
        };
        let intent = self.intent(
            id.clone(),
            &p.symbol,
            &p.strategy,
            kind,
            IntentStatus::Approved,
            reason.clone(),
        );
        let created = self.d.store.insert_intent(&intent).await?;
        if created {
            journal(
                self.d,
                EventKind::Intent,
                Some(&p.symbol),
                Some(&id),
                format!("Намір на вихід: {} шт. — {reason}", p.qty),
                json!({
                    "kind": "exit",
                    "strategy": p.strategy,
                    "qty": p.qty,
                    "session": self.session,
                    "status": IntentStatus::Approved,
                }),
            )
            .await?;
        }
        Ok(created)
    }

    async fn skip_stop(
        &self,
        tp: &TrackedPosition,
        new_stop: f64,
        message: String,
    ) -> Result<bool, CycleError> {
        let p = &tp.position;
        journal(
            self.d,
            EventKind::Error,
            Some(&p.symbol),
            None,
            message,
            json!({ "new_stop": new_stop, "stop": p.stop_price }),
        )
        .await?;
        Ok(false)
    }

    /// Лише вгору і лише для позиції зі стоп-ордером у брокера.
    async fn move_stop_intent(
        &self,
        tp: &TrackedPosition,
        new_stop: f64,
        reason: String,
    ) -> Result<bool, CycleError> {
        let p = &tp.position;
        let current = to_price(p.stop_price).unwrap_or(Decimal::ZERO);
        let new_price = match to_price(new_stop) {
            Ok(x) if new_stop > p.stop_price && x > current => x,
            _ => {
                let message = format!(
                    "Стоп не підтягнуто: новий рівень {new_stop:.2} не вище поточного {:.2}",
                    p.stop_price
                );
                return self.skip_stop(tp, new_stop, message).await;
            }
        };
        let Some(stop_order_id) = tp.stop_order_id.clone() else {
            let message = format!("Стоп не підтягнуто до {new_price}: у позиції немає стоп-ордера");
            return self.skip_stop(tp, new_stop, message).await;
        };

        let id = intent_id(self.session, &p.strategy, &p.symbol, IntentTag::Stop);
        let kind = IntentKind::MoveStop {
            new_stop: new_price,
            stop_order_id,
        };
        let intent = self.intent(
            id.clone(),
            &p.symbol,
            &p.strategy,
            kind,
            IntentStatus::Approved,
            reason.clone(),
        );
        let created = self.d.store.insert_intent(&intent).await?;
        if created {
            journal(
                self.d,
                EventKind::Intent,
                Some(&p.symbol),
                Some(&id),
                format!("Намір підтягнути стоп до {new_price} — {reason}"),
                json!({
                    "kind": "move_stop",
                    "strategy": p.strategy,
                    "new_stop": new_price.to_string(),
                    "session": self.session,
                    "status": IntentStatus::Approved,
                }),
            )
            .await?;
        }
        Ok(created)
    }

    /// Фільтри звітів і новин (новини — лише для тих, хто пройшов звіти). Коли входи
    /// заблоковано, фільтри не викликаються: ризик-менеджер усе одно відхилить сигнали.
    async fn entries(
        &self,
        cal: &TradingCalendar,
        names: &HashMap<&Symbol, &UniverseEntry>,
        signals: Vec<EntrySignal>,
        run_filters: bool,
    ) -> Result<Entries, CycleError> {
        let mut out = Entries {
            candidates: Vec::with_capacity(signals.len()),
            news: HashMap::new(),
        };
        for s in signals {
            let entry = names.get(&s.symbol).copied();
            if run_filters {
                if let FilterOutcome::Reject(why) = earnings_filter(self.d, self.cfg, cal, &s).await
                {
                    self.reject_signal(&s, EventKind::Filter, why).await?;
                    continue;
                }
                let company = entry.map_or(s.symbol.as_str(), |e| e.name.as_str());
                match news_filter(self.d, self.cfg, &s, company).await {
                    FilterOutcome::Reject(why) => {
                        self.reject_signal(&s, EventKind::Filter, why).await?;
                        continue;
                    }
                    FilterOutcome::Pass { news } => {
                        out.news.insert(s.symbol.clone(), news);
                    }
                }
            }
            let sector = sector_of(entry);
            out.candidates.push(Candidate { signal: s, sector });
        }
        Ok(out)
    }

    /// Кандидат уже має незавершений намір на вхід у цій сесії.
    async fn already_planned(&self, i: &Intent) -> Result<(), CycleError> {
        journal(
            self.d,
            EventKind::Intent,
            Some(&i.symbol),
            Some(&i.id),
            format!("Вже заплановано: {}", i.id),
            json!({ "strategy": i.strategy, "status": i.status, "session": self.session }),
        )
        .await
    }

    async fn reject_signal(
        &self,
        s: &EntrySignal,
        kind: EventKind,
        why: String,
    ) -> Result<(), CycleError> {
        let id = intent_id(self.session, &s.strategy, &s.symbol, IntentTag::Entry);
        self.d
            .store
            .insert_signal(&SignalRecord {
                id,
                signal: s.clone(),
                status: SignalStatus::Rejected,
                reject_reason: Some(why.clone()),
            })
            .await?;
        journal(
            self.d,
            kind,
            Some(&s.symbol),
            None,
            format!("Сигнал {} відхилено: {why}", s.strategy),
            json!({ "strategy": s.strategy, "strength": s.strength, "reason": why }),
        )
        .await
    }

    /// Зберігає рішення ризик-менеджера. Повертає (створено входів, з них очікують схвалення).
    async fn persist_decisions(
        &self,
        decisions: Vec<RiskDecision>,
        mut news: HashMap<Symbol, Option<NewsVerdict>>,
    ) -> Result<(usize, u32), CycleError> {
        let status = match self.cfg.execution_mode {
            ExecutionMode::Approval => IntentStatus::PendingApproval,
            ExecutionMode::Auto => IntentStatus::Approved,
        };
        let (mut created, mut pending) = (0, 0);
        for decision in decisions {
            match decision {
                RiskDecision::Rejected { signal, reason } => {
                    self.reject_signal(&signal, EventKind::Risk, reject_text(&reason))
                        .await?;
                }
                RiskDecision::Accepted {
                    signal,
                    sector,
                    qty,
                    limit_price,
                    stop_price,
                    take_profit,
                    risk_usd,
                } => {
                    let verdict = news.remove(&signal.symbol).flatten();
                    let state = if status == IntentStatus::PendingApproval {
                        "очікує схвалення"
                    } else {
                        "схвалено автоматично"
                    };
                    let note = Note {
                        message: format!(
                            "Намір на вхід ({state}): {qty} шт. за лімітом {limit_price}, \
                             стоп {stop_price}, ризик ${risk_usd:.2}"
                        ),
                        data: json!({
                            "kind": "entry",
                            "strategy": signal.strategy,
                            "sector": sector,
                            "qty": qty,
                            "limit_price": limit_price.to_string(),
                            "stop_price": stop_price.to_string(),
                            "take_profit": take_profit.map(|t| t.to_string()),
                            "risk_usd": risk_usd,
                            "news": verdict,
                            "session": self.session,
                            "status": status,
                        }),
                    };
                    let kind = IntentKind::Entry {
                        qty,
                        limit_price,
                        stop_price,
                        take_profit,
                        risk_usd,
                        news: verdict,
                    };
                    if self.accept_signal(&signal, kind, status, note).await? {
                        created += 1;
                        if status == IntentStatus::PendingApproval {
                            pending += 1;
                        }
                    }
                }
            }
        }
        Ok((created, pending))
    }

    /// `true`, якщо намір на вхід створено (повторний прогін нічого не дублює).
    async fn accept_signal(
        &self,
        s: &EntrySignal,
        kind: IntentKind,
        status: IntentStatus,
        note: Note,
    ) -> Result<bool, CycleError> {
        let d = self.d;
        let id = intent_id(self.session, &s.strategy, &s.symbol, IntentTag::Entry);
        d.store
            .insert_signal(&SignalRecord {
                id: id.clone(),
                signal: s.clone(),
                status: SignalStatus::Accepted,
                reject_reason: None,
            })
            .await?;
        let Note { message, data } = note;
        let intent = self.intent(
            id.clone(),
            &s.symbol,
            &s.strategy,
            kind,
            status,
            s.explanation.clone(),
        );
        let created = d.store.insert_intent(&intent).await?;
        if created {
            journal(
                d,
                EventKind::Intent,
                Some(&s.symbol),
                Some(&id),
                message,
                data,
            )
            .await?;
        } else {
            journal(
                d,
                EventKind::Signal,
                Some(&s.symbol),
                Some(&id),
                format!("Сигнал {} прийнято повторно: намір уже існує", s.strategy),
                data,
            )
            .await?;
        }
        Ok(created)
    }
}

/// Кандидати після фільтрів і вердикти новин для них.
struct Entries {
    candidates: Vec<Candidate>,
    news: HashMap<Symbol, Option<NewsVerdict>>,
}

/// Текст і дані події журналу.
struct Note {
    message: String,
    data: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::Duration;
    use fond_api::config::{CautionPolicy, UnavailablePolicy};
    use fond_api::events::EngineEventDto;
    use fond_core::error::{DataError, JudgeError};
    use fond_core::indicators::lowest_low;
    use fond_core::intent::{Intent, IntentKind, IntentStatus};
    use fond_core::portfolio::Account;
    use fond_core::price::to_price;
    use fond_core::scanner::ScanReject;
    use fond_core::testkit::{FakeMarketData, uptrend_then_pullback};
    use fond_core::traits::{AssetInfo, Clock, MarketData, Session, Verdict};
    use fond_core::types::{Bar, EngineMode, ExecutionMode, Symbol};
    use fond_store::{
        EquityKind, EquitySnapshot, EventKind, EventQuery, SignalRecord, SignalStatus,
    };
    use fond_strategies::{BREAKOUT, TREND_PULLBACK};
    use rust_decimal_macros::dec;
    use tokio::sync::broadcast;

    use super::*;
    use crate::cycle::refresh::refresh_market_data;
    use crate::cycle::test_support::{
        N_BARS, falling_bars, fixture, flat_bars, judgment, sym, tracked, weekday_sessions,
    };
    use crate::cycle::trading_calendar;

    /// Сесія після дня аналізу стенда (середа 2026-11-04 -> четвер).
    fn session() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 5).unwrap()
    }

    fn drain(rx: &mut broadcast::Receiver<EngineEventDto>) -> Vec<EngineEventDto> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    async fn signal(d: &Deps, date: NaiveDate, s: &str) -> SignalRecord {
        let all = d.store.signals_on(date).await.unwrap();
        all.into_iter()
            .find(|r| r.signal.symbol.as_str() == s)
            .unwrap_or_else(|| panic!("немає сигналу для {s}"))
    }

    async fn intents(d: &Deps) -> Vec<Intent> {
        d.store.intents_for_session(session()).await.unwrap()
    }

    fn by_id<'a>(all: &'a [Intent], id: &str) -> &'a Intent {
        all.iter()
            .find(|i| i.id == id)
            .unwrap_or_else(|| panic!("немає наміру {id}: {all:?}"))
    }

    async fn events(d: &Deps, kind: EventKind, s: Option<&str>) -> Vec<fond_store::StoredEvent> {
        d.store
            .events(&EventQuery {
                kinds: vec![kind],
                symbol: s.map(sym),
                ..Default::default()
            })
            .await
            .unwrap()
    }

    async fn hold(d: &Deps, p: fond_core::portfolio::TrackedPosition) {
        d.store.upsert_position(&p, d.clock.now()).await.unwrap();
    }

    fn close_snapshot(date: NaiveDate, equity: f64) -> EquitySnapshot {
        EquitySnapshot {
            at: fond_core::calendar::et_to_utc(
                date,
                chrono::NaiveTime::from_hms_opt(16, 5, 0).unwrap(),
            ),
            kind: EquityKind::Close,
            session_date: date,
            equity,
            cash: equity,
        }
    }

    #[tokio::test]
    async fn creates_pending_entry_in_approval_mode() {
        let (d, h) = fixture().await;
        let cfg = EngineConfig::default();
        let mut rx = d.bus.subscribe();
        let s = run_analysis(&d, &cfg, h.last_date()).await.unwrap();
        assert_eq!(s.entries_created, 1);
        assert_eq!((s.scanned, s.candidates, s.signals), (3, 3, 1));
        assert_eq!((s.exits_created, s.stop_moves_created), (0, 0));

        let i = d
            .store
            .intents_with_status(&[IntentStatus::PendingApproval])
            .await
            .unwrap();
        assert_eq!(i.len(), 1);
        assert_eq!(i[0].symbol.as_str(), "AAPL");
        assert!(matches!(i[0].kind, IntentKind::Entry { qty, .. } if qty > 0));
        assert_eq!(i[0].id, "20261105-trend_pullback-AAPL-entry");
        assert_eq!(
            (i[0].session_date, i[0].signal_date),
            (session(), h.last_date())
        );
        assert_eq!(i[0].strategy.as_str(), TREND_PULLBACK);
        assert!(i[0].explanation.contains("RSI(2)"), "{}", i[0].explanation);
        assert!(matches!(
            &i[0].kind,
            IntentKind::Entry { news: Some(v), limit_price, stop_price, .. }
                if v.verdict == Verdict::Allow && limit_price > stop_price
        ));

        let sig = signal(&d, h.last_date(), "AAPL").await;
        assert_eq!(sig.status, SignalStatus::Accepted);
        assert_eq!(sig.id, i[0].id);
        assert_eq!(sig.reject_reason, None);

        assert_eq!(
            d.store.close_equity(h.last_date()).await.unwrap(),
            Some(100_000.0)
        );
        assert_eq!((h.earnings.calls(), h.judge.calls()), (1, 1));
        let ev = drain(&mut rx);
        assert!(ev.contains(&EngineEventDto::IntentsPending {
            session: session(),
            count: 1
        }));
        assert!(ev.iter().any(|e| matches!(
            e,
            EngineEventDto::Journal { event } if event.kind == "intent"
                && event.intent_id.as_deref() == Some("20261105-trend_pullback-AAPL-entry")
        )));
    }

    #[tokio::test]
    async fn auto_mode_creates_approved_entry() {
        let (d, h) = fixture().await;
        let cfg = EngineConfig {
            execution_mode: ExecutionMode::Auto,
            ..EngineConfig::default()
        };
        let mut rx = d.bus.subscribe();
        let s = run_analysis(&d, &cfg, h.last_date()).await.unwrap();
        assert_eq!(s.entries_created, 1);

        let approved = d
            .store
            .intents_with_status(&[IntentStatus::Approved])
            .await
            .unwrap();
        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].symbol.as_str(), "AAPL");
        assert!(matches!(approved[0].kind, IntentKind::Entry { qty, .. } if qty > 0));
        assert!(
            d.store
                .intents_with_status(&[IntentStatus::PendingApproval])
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            !drain(&mut rx)
                .iter()
                .any(|e| matches!(e, EngineEventDto::IntentsPending { .. }))
        );
    }

    #[tokio::test]
    async fn market_filter_down_blocks_entries_but_not_exits() {
        let (d, h) = fixture().await;
        h.data
            .set_bars(&sym("SPY"), falling_bars(h.start_date(), N_BARS));
        // MSFT у чистому підйомі: close > SMA5 -> вихід trend_pullback.
        h.data.set_bars(
            &sym("MSFT"),
            uptrend_then_pullback(h.start_date(), N_BARS, 0),
        );
        hold(
            &d,
            tracked("MSFT", TREND_PULLBACK, 40, 90.0, Some("ord-1-stop")),
        )
        .await;

        let s = run_analysis(&d, &EngineConfig::default(), h.last_date())
            .await
            .unwrap();
        assert_eq!(s.signals, 0);
        assert_eq!(s.entries_created, 0);
        assert_eq!(s.exits_created, 1);
        assert_eq!(s.candidates, 2, "MSFT утримується і не є кандидатом");

        let all = intents(&d).await;
        assert_eq!(all.len(), 1);
        let exit = by_id(&all, "20261105-trend_pullback-MSFT-exit");
        assert_eq!(exit.status, IntentStatus::Approved);
        assert!(matches!(
            &exit.kind,
            IntentKind::Exit { qty: 40, reason } if reason.contains("SMA5")
        ));
        assert!(d.store.signals_on(h.last_date()).await.unwrap().is_empty());
        let filt = events(&d, EventKind::Filter, None).await;
        assert!(
            filt.iter()
                .any(|e| e.message.contains("Ринковий фільтр") && e.data["ok"] == false),
            "{filt:?}"
        );
    }

    #[tokio::test]
    async fn earnings_within_window_rejects() {
        let (d, h) = fixture().await;
        let cal = trading_calendar(&d, h.last_date()).await.unwrap();
        let report = cal.nth_session_after(h.last_date(), 2).unwrap();
        assert_eq!(report, NaiveDate::from_ymd_opt(2026, 11, 6).unwrap());
        h.earnings.set(&sym("AAPL"), Ok(Some(report)));

        let s = run_analysis(&d, &EngineConfig::default(), h.last_date())
            .await
            .unwrap();
        assert_eq!((s.signals, s.entries_created), (1, 0));
        let sig = signal(&d, h.last_date(), "AAPL").await;
        assert_eq!(sig.status, SignalStatus::Rejected);
        assert_eq!(
            sig.reject_reason.as_deref(),
            Some("Звіт 2026-11-06: у межах 3 торгових днів")
        );
        assert_eq!(h.judge.calls(), 0, "новинний фільтр не викликається");
        assert!(intents(&d).await.is_empty());
    }

    #[tokio::test]
    async fn news_block_rejects_and_verdict_is_cached() {
        let (d, h) = fixture().await;
        h.judge.set_result(Ok(judgment(
            Verdict::Block,
            &["Розслідування SEC", "Відкликання продукту"],
        )));
        let cfg = EngineConfig::default();
        for run in 0..2 {
            let s = run_analysis(&d, &cfg, h.last_date()).await.unwrap();
            assert_eq!((s.signals, s.entries_created), (1, 0), "прогін {run}");
            let sig = signal(&d, h.last_date(), "AAPL").await;
            assert_eq!(sig.status, SignalStatus::Rejected);
            assert_eq!(
                sig.reject_reason.as_deref(),
                Some("Новини: Розслідування SEC")
            );
        }
        assert_eq!(h.judge.calls(), 1);
        assert_eq!(h.news.calls(), 1);
        let cached = d
            .store
            .news_verdict(&sym("AAPL"), h.last_date())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cached.verdict, Verdict::Block);
        let rejected = events(&d, EventKind::Filter, Some("AAPL")).await;
        assert_eq!(
            rejected
                .iter()
                .filter(|e| e.message.contains("Новини: Розслідування SEC"))
                .count(),
            2
        );
        // Витрати записано один раз: (2000 × 0.10 + 300 × 0.50) / 1e6.
        let now = h.clock.now();
        let spent = d
            .store
            .llm_spend_between(now - Duration::hours(1), now + Duration::hours(1))
            .await
            .unwrap();
        assert!((spent - 0.000_35).abs() < 1e-12, "{spent}");
        assert!(intents(&d).await.is_empty());
    }

    #[tokio::test]
    async fn news_unavailable_blocks_by_default() {
        let (d, h) = fixture().await;
        h.judge
            .set_result(Err(JudgeError::Transient("timeout".into())));
        let s = run_analysis(&d, &EngineConfig::default(), h.last_date())
            .await
            .unwrap();
        assert_eq!(s.entries_created, 0);
        let sig = signal(&d, h.last_date(), "AAPL").await;
        assert_eq!(sig.status, SignalStatus::Rejected);
        let why = sig.reject_reason.unwrap();
        assert!(why.starts_with("Фільтр новин недоступний"), "{why}");
        assert!(
            d.store
                .news_verdict(&sym("AAPL"), h.last_date())
                .await
                .unwrap()
                .is_none()
        );

        let (d, h) = fixture().await;
        h.judge
            .set_result(Err(JudgeError::Transient("timeout".into())));
        let mut cfg = EngineConfig::default();
        cfg.news_filter.on_unavailable = UnavailablePolicy::Allow;
        let s = run_analysis(&d, &cfg, h.last_date()).await.unwrap();
        assert_eq!(s.entries_created, 1);
        let sig = signal(&d, h.last_date(), "AAPL").await;
        assert_eq!(sig.status, SignalStatus::Accepted);
        let all = intents(&d).await;
        assert!(matches!(
            &by_id(&all, "20261105-trend_pullback-AAPL-entry").kind,
            IntentKind::Entry { news: None, .. }
        ));
    }

    #[tokio::test]
    async fn budget_exhausted_skips_judge() {
        let (d, h) = fixture().await;
        d.store
            .add_llm_usage(h.clock.now(), "claude-haiku-5-5", 0, 0, 1.0)
            .await
            .unwrap();
        let s = run_analysis(&d, &EngineConfig::default(), h.last_date())
            .await
            .unwrap();
        assert_eq!(s.entries_created, 0);
        assert_eq!(h.judge.calls(), 0);
        assert_eq!(h.news.calls(), 0);
        let sig = signal(&d, h.last_date(), "AAPL").await;
        assert_eq!(sig.status, SignalStatus::Rejected);
        assert_eq!(sig.reject_reason.as_deref(), Some("Вичерпано бюджет LLM"));

        // Учорашні витрати (за ET) денний бюджет не зачіпають.
        let (d, h) = fixture().await;
        d.store
            .add_llm_usage(h.clock.now() - Duration::days(1), "m", 0, 0, 1.0)
            .await
            .unwrap();
        let s = run_analysis(&d, &EngineConfig::default(), h.last_date())
            .await
            .unwrap();
        assert_eq!((s.entries_created, h.judge.calls()), (1, 1));
    }

    #[tokio::test]
    async fn exit_and_stop_intents_are_auto_approved() {
        let (d, h) = fixture().await;
        h.data.set_bars(
            &sym("MSFT"),
            uptrend_then_pullback(h.start_date(), N_BARS, 0),
        );
        hold(
            &d,
            tracked("MSFT", TREND_PULLBACK, 40, 90.0, Some("ord-1-stop")),
        )
        .await;
        hold(&d, tracked("SPY", BREAKOUT, 10, 50.0, Some("ord-2-stop"))).await;
        // Без стоп-ордера підтягувати нічого: намір не створюється.
        hold(&d, tracked("AAPL", BREAKOUT, 5, 50.0, None)).await;

        let cfg = EngineConfig::default();
        assert_eq!(cfg.execution_mode, ExecutionMode::Approval);
        let s = run_analysis(&d, &cfg, h.last_date()).await.unwrap();
        assert_eq!(
            (s.exits_created, s.stop_moves_created, s.entries_created),
            (1, 1, 0)
        );

        let all = intents(&d).await;
        assert_eq!(all.len(), 2, "{all:?}");
        let exit = by_id(&all, "20261105-trend_pullback-MSFT-exit");
        assert_eq!(exit.status, IntentStatus::Approved);
        assert!(matches!(exit.kind, IntentKind::Exit { qty: 40, .. }));

        let mv = by_id(&all, "20261105-breakout-SPY-stop");
        assert_eq!(mv.status, IntentStatus::Approved);
        let spy = uptrend_then_pullback(h.start_date(), N_BARS, 0);
        let want = to_price(lowest_low(&spy, 20).unwrap()).unwrap();
        match &mv.kind {
            IntentKind::MoveStop {
                new_stop,
                stop_order_id,
            } => {
                assert_eq!(*new_stop, want);
                assert!(*new_stop > dec!(50));
                assert_eq!(stop_order_id, "ord-2-stop");
            }
            other => panic!("очікувався MoveStop, є {other:?}"),
        }
        assert!(
            d.store
                .intents_with_status(&[IntentStatus::PendingApproval])
                .await
                .unwrap()
                .is_empty()
        );
        let errs = events(&d, EventKind::Error, Some("AAPL")).await;
        assert!(
            errs.iter().any(|e| e.message.contains("стоп-ордер")),
            "{errs:?}"
        );
    }

    #[tokio::test]
    async fn rerun_is_idempotent() {
        let (d, h) = fixture().await;
        h.data.set_bars(
            &sym("MSFT"),
            uptrend_then_pullback(h.start_date(), N_BARS, 0),
        );
        hold(
            &d,
            tracked("MSFT", TREND_PULLBACK, 40, 90.0, Some("ord-1-stop")),
        )
        .await;
        let cfg = EngineConfig::default();

        let first = run_analysis(&d, &cfg, h.last_date()).await.unwrap();
        assert_eq!((first.entries_created, first.exits_created), (1, 1));
        let second = run_analysis(&d, &cfg, h.last_date()).await.unwrap();
        // AAPL уже має намір на вхід: повторно не оцінюється (MSFT утримується).
        assert_eq!((second.candidates, second.signals), (1, 0));
        assert_eq!((second.entries_created, second.exits_created), (0, 0));

        let all = intents(&d).await;
        assert_eq!(all.len(), 2);
        for s in ["AAPL", "MSFT"] {
            assert_eq!(all.iter().filter(|i| i.symbol.as_str() == s).count(), 1);
        }
        assert_eq!(d.store.signals_on(h.last_date()).await.unwrap().len(), 1);
        assert_eq!(h.judge.calls(), 1);
        let planned = events(&d, EventKind::Intent, Some("AAPL")).await;
        assert_eq!(
            planned[0].message,
            "Вже заплановано: 20261105-trend_pullback-AAPL-entry"
        );
    }

    #[tokio::test]
    async fn existing_entry_intent_blocks_duplicate_and_counts_toward_limits() {
        let cases = [
            (1, 20, "Ризик: досягнуто максимум відкритих позицій"),
            (10, 1, "Ризик: досягнуто ліміт ордерів на день"),
        ];
        for (max_positions, max_orders, why) in cases {
            let (d, h) = fixture().await;
            // MSFT теж дає сигнал trend_pullback.
            h.data.set_bars(
                &sym("MSFT"),
                uptrend_then_pullback(h.start_date(), N_BARS - 3, 3),
            );
            // З попереднього прогону: на AAPL уже схвалено вхід від іншої стратегії.
            let now = h.clock.now();
            let existing = Intent {
                id: "20261105-breakout-AAPL-entry".into(),
                session_date: session(),
                signal_date: h.last_date(),
                symbol: sym("AAPL"),
                strategy: StrategyId::new(BREAKOUT),
                kind: IntentKind::Entry {
                    qty: 40,
                    limit_price: dec!(300.00),
                    stop_price: dec!(290.00),
                    take_profit: None,
                    risk_usd: 400.0,
                    news: None,
                },
                status: IntentStatus::Approved,
                explanation: "попередній прогін".into(),
                created_at: now,
                updated_at: now,
                broker_order_id: None,
                error: None,
            };
            assert!(d.store.insert_intent(&existing).await.unwrap());
            let mut cfg = EngineConfig {
                execution_mode: ExecutionMode::Auto,
                ..EngineConfig::default()
            };
            cfg.risk.max_open_positions = max_positions;
            cfg.risk.max_orders_per_day = max_orders;

            let s = run_analysis(&d, &cfg, h.last_date()).await.unwrap();
            assert_eq!(
                (s.candidates, s.signals, s.entries_created),
                (2, 1, 0),
                "{why}"
            );

            let aapl_entries: Vec<Intent> = intents(&d)
                .await
                .into_iter()
                .filter(|i| i.symbol.as_str() == "AAPL")
                .collect();
            assert_eq!(
                aapl_entries,
                vec![existing.clone()],
                "лише попередній намір"
            );
            assert!(
                d.store
                    .signals_on(h.last_date())
                    .await
                    .unwrap()
                    .iter()
                    .all(|r| r.signal.symbol.as_str() != "AAPL")
            );
            let ev = events(&d, EventKind::Intent, Some("AAPL")).await;
            assert_eq!(ev.len(), 1);
            assert_eq!(
                ev[0].message,
                "Вже заплановано: 20261105-breakout-AAPL-entry"
            );
            assert_eq!(ev[0].intent_id.as_deref(), Some(existing.id.as_str()));

            // Без урахування запланованого входу MSFT пройшов би обидва ліміти.
            let msft = signal(&d, h.last_date(), "MSFT").await;
            assert_eq!(msft.status, SignalStatus::Rejected);
            assert_eq!(msft.reject_reason.as_deref(), Some(why));
        }
    }

    #[tokio::test]
    async fn terminal_entry_intent_does_not_block_new_signal() {
        let (d, h) = fixture().await;
        let now = h.clock.now();
        let rejected = Intent {
            id: "20261105-breakout-AAPL-entry".into(),
            session_date: session(),
            signal_date: h.last_date(),
            symbol: sym("AAPL"),
            strategy: StrategyId::new(BREAKOUT),
            kind: IntentKind::Entry {
                qty: 40,
                limit_price: dec!(300.00),
                stop_price: dec!(290.00),
                take_profit: None,
                risk_usd: 400.0,
                news: None,
            },
            status: IntentStatus::Rejected,
            explanation: String::new(),
            created_at: now,
            updated_at: now,
            broker_order_id: None,
            error: None,
        };
        assert!(d.store.insert_intent(&rejected).await.unwrap());
        let s = run_analysis(&d, &EngineConfig::default(), h.last_date())
            .await
            .unwrap();
        assert_eq!((s.candidates, s.entries_created), (3, 1));
    }

    #[tokio::test]
    async fn weekly_loss_breach_pauses_engine() {
        let (d, h) = fixture().await;
        let friday = NaiveDate::from_ymd_opt(2026, 10, 30).unwrap();
        let tuesday = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        d.store
            .insert_equity(&close_snapshot(friday, 100_000.0))
            .await
            .unwrap();
        d.store
            .insert_equity(&close_snapshot(tuesday, 94_000.0))
            .await
            .unwrap();
        h.broker.set_account(Account {
            equity: dec!(93000),
            cash: dec!(93000),
            buying_power: dec!(93000),
            last_equity: dec!(94000),
        });
        let mut rx = d.bus.subscribe();

        let s = run_analysis(&d, &EngineConfig::default(), h.last_date())
            .await
            .unwrap();
        assert_eq!((s.signals, s.entries_created), (1, 0));
        assert_eq!(
            d.store.engine_mode().await.unwrap(),
            EngineMode::Paused {
                reason: "Тижневий ліміт збитку".into()
            }
        );
        assert!(drain(&mut rx).contains(&EngineEventDto::AutoPaused {
            reason: "Тижневий ліміт збитку".into()
        }));
        let sig = signal(&d, h.last_date(), "AAPL").await;
        assert_eq!(sig.status, SignalStatus::Rejected);
        let why = sig.reject_reason.unwrap();
        assert!(why.contains("Тижневий ліміт збитку"), "{why}");
        assert_eq!(
            h.judge.calls(),
            0,
            "заблоковані входи не витрачають бюджет LLM"
        );
        assert!(intents(&d).await.is_empty());
    }

    #[tokio::test]
    async fn stale_symbol_is_skipped_with_scan_reason() {
        let (d, h) = fixture().await;
        let mut msft = flat_bars(h.start_date(), N_BARS);
        msft.pop();
        h.data.set_bars(&sym("MSFT"), msft);

        let s = run_analysis(&d, &EngineConfig::default(), h.last_date())
            .await
            .unwrap();
        assert_eq!((s.scanned, s.candidates), (3, 2));
        let scan = d.store.scan_results(h.last_date()).await.unwrap();
        let get = |s: &str| {
            scan.iter()
                .find(|(x, _)| x.as_str() == s)
                .map(|(_, r)| r.clone())
                .unwrap()
        };
        match get("MSFT") {
            Some(ScanReject::BadData(m)) => assert!(m.starts_with("stale"), "{m}"),
            other => panic!("очікувався BadData, є {other:?}"),
        }
        assert_eq!(get("AAPL"), None);
        assert_eq!(get("SPY"), None);

        let ev = events(&d, EventKind::Scan, None).await;
        assert_eq!(ev.len(), 1, "одна підсумкова подія скану");
        assert_eq!(ev[0].data["total"], 3);
        assert_eq!(ev[0].data["passed"], 2);
        assert_eq!(ev[0].data["rejected"]["bad_data"], 1);
    }

    /// Обгортка, що запам'ятовує запити свічок.
    struct RecordingData {
        inner: Arc<FakeMarketData>,
        calls: Mutex<Vec<(Vec<Symbol>, NaiveDate, NaiveDate)>>,
    }

    #[async_trait]
    impl MarketData for RecordingData {
        async fn daily_bars(
            &self,
            symbols: &[Symbol],
            from: NaiveDate,
            to: NaiveDate,
        ) -> Result<std::collections::HashMap<Symbol, Vec<Bar>>, DataError> {
            self.calls
                .lock()
                .unwrap()
                .push((symbols.to_vec(), from, to));
            self.inner.daily_bars(symbols, from, to).await
        }
        async fn calendar(
            &self,
            from: NaiveDate,
            to: NaiveDate,
        ) -> Result<Vec<Session>, DataError> {
            self.inner.calendar(from, to).await
        }
        async fn latest_trade(&self, symbol: &Symbol) -> Result<Option<f64>, DataError> {
            self.inner.latest_trade(symbol).await
        }
        async fn assets(&self) -> Result<Vec<AssetInfo>, DataError> {
            self.inner.assets().await
        }
    }

    fn bar(date: NaiveDate, close: f64) -> Bar {
        Bar {
            date,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1_000.0,
        }
    }

    #[tokio::test]
    async fn refresh_rewrites_adjusted_window() {
        let (mut d, h) = fixture().await;
        let date = h.last_date();
        let cfg = EngineConfig::default();
        let from = date - Duration::days(600);
        let aapl = sym("AAPL");

        // Старі (нескориговані) значення у store: до вікна, на межі і всередині.
        d.store
            .upsert_bars(
                &aapl,
                &[
                    bar(from - Duration::days(1), 7.0),
                    bar(from, 7.0),
                    bar(date, 7.0),
                ],
            )
            .await
            .unwrap();
        // Брокер віддає скориговані значення, зокрема й поза вікном.
        let series = uptrend_then_pullback(h.start_date(), N_BARS - 3, 3);
        let last_close = series.last().unwrap().close;
        let mut adjusted = vec![bar(from - Duration::days(1), 50.0), bar(from, 60.0)];
        adjusted.extend(series);
        h.data.set_bars(&aapl, adjusted);

        // Набір символів: MSFT і SPY виключені, SPY повертається як символ фільтра,
        // TSLA — утримувана позиція поза universe.
        d.store.set_excluded(&sym("MSFT"), true).await.unwrap();
        d.store.set_excluded(&sym("SPY"), true).await.unwrap();
        hold(&d, tracked("TSLA", TREND_PULLBACK, 1, 1.0, Some("s"))).await;

        // Календар: субота D+3 (у вікні ±40) і субота D+45 (поза ним).
        let mut sessions = weekday_sessions(h.start_date(), date + Duration::days(120));
        let t = |h: u32| chrono::NaiveTime::from_hms_opt(h, 0, 0).unwrap();
        for x in [date + Duration::days(3), date + Duration::days(45)] {
            sessions.push(Session {
                date: x,
                open: t(9),
                close: t(13),
            });
        }
        h.data.set_sessions(sessions);

        let rec = Arc::new(RecordingData {
            inner: h.data.clone(),
            calls: Mutex::new(Vec::new()),
        });
        d.data = rec.clone();
        let summary = refresh_market_data(&d, &cfg, date).await.unwrap();

        let calls = rec.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![(vec![aapl.clone(), sym("SPY"), sym("TSLA")], from, date)]
        );
        assert_eq!(summary.symbols, 3);
        assert_eq!(summary.bars, (N_BARS + 1) + N_BARS, "AAPL (+межа) і SPY");

        let stored = d
            .store
            .bars(&aapl, from - Duration::days(10), date)
            .await
            .unwrap();
        let close_on = |x: NaiveDate| stored.iter().find(|b| b.date == x).unwrap().close;
        assert_eq!(
            close_on(from - Duration::days(1)),
            7.0,
            "поза вікном не чіпаємо"
        );
        assert_eq!(close_on(from), 60.0, "межа вікна включна");
        assert_eq!(close_on(date), last_close, "старе значення перезаписано");

        assert_eq!(
            d.store.asset(&aapl).await.unwrap(),
            Some(AssetInfo {
                symbol: aapl.clone(),
                tradable: true,
                active: true
            })
        );
        let sat = date + Duration::days(3);
        assert_eq!(d.store.sessions(sat, sat).await.unwrap().len(), 1);
        let far = date + Duration::days(45);
        assert!(d.store.sessions(far, far).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn first_run_without_equity_history_creates_intents() {
        let (d, h) = fixture().await;
        let cfg = EngineConfig::default();
        let date = h.last_date();
        assert_eq!(
            d.store
                .last_close_equity_before(date + Duration::days(1))
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            crate::cycle::entries_block_reason(&d, &cfg, date)
                .await
                .unwrap(),
            None
        );

        let s = run_analysis(&d, &cfg, date).await.unwrap();
        assert_eq!(s.entries_created, 1);
        assert_eq!(d.store.close_equity(date).await.unwrap(), Some(100_000.0));
        assert_eq!(d.store.last_close_equity_before(date).await.unwrap(), None);
        assert_eq!(d.store.engine_mode().await.unwrap(), EngineMode::Running);
    }

    #[tokio::test]
    async fn caution_in_auto_respects_policy() {
        let cases = [
            (ExecutionMode::Auto, CautionPolicy::Block, None),
            (
                ExecutionMode::Auto,
                CautionPolicy::Allow,
                Some(IntentStatus::Approved),
            ),
            // Політика стосується лише режиму «авто».
            (
                ExecutionMode::Approval,
                CautionPolicy::Block,
                Some(IntentStatus::PendingApproval),
            ),
        ];
        for (mode, policy, want) in cases {
            let (d, h) = fixture().await;
            h.judge.set_result(Ok(judgment(
                Verdict::Caution,
                &["Підвищена волатильність після новин"],
            )));
            let mut cfg = EngineConfig {
                execution_mode: mode,
                ..EngineConfig::default()
            };
            cfg.news_filter.caution_in_auto = policy;
            let s = run_analysis(&d, &cfg, h.last_date()).await.unwrap();
            let sig = signal(&d, h.last_date(), "AAPL").await;
            match want {
                None => {
                    assert_eq!(s.entries_created, 0);
                    assert_eq!(sig.status, SignalStatus::Rejected);
                    assert_eq!(
                        sig.reject_reason.as_deref(),
                        Some("Новини: обережно (політика режиму авто)")
                    );
                }
                Some(status) => {
                    assert_eq!(s.entries_created, 1, "{mode:?} {policy:?}");
                    assert_eq!(sig.status, SignalStatus::Accepted);
                    let all = intents(&d).await;
                    let i = by_id(&all, "20261105-trend_pullback-AAPL-entry");
                    assert_eq!(i.status, status);
                    assert!(matches!(
                        &i.kind,
                        IntentKind::Entry { news: Some(v), .. } if v.verdict == Verdict::Caution
                    ));
                }
            }
        }
    }

    #[test]
    fn reject_reasons_are_short_ukrainian_texts() {
        assert_eq!(
            reject_text(&RejectReason::MaxOpenPositions),
            "Ризик: досягнуто максимум відкритих позицій"
        );
        assert_eq!(
            reject_text(&RejectReason::EntriesBlocked("Денний ліміт збитку".into())),
            "Ризик: входи заблоковано (Денний ліміт збитку)"
        );
        let all = [
            RejectReason::InvalidStop,
            RejectReason::AlreadyHolding,
            RejectReason::DuplicateSymbol,
            RejectReason::MaxOrdersPerDay,
            RejectReason::QtyBelowOne,
            RejectReason::SectorLimit,
            RejectReason::GrossExposure,
            RejectReason::StrategyAllocation,
        ];
        let texts: std::collections::HashSet<String> = all.iter().map(reject_text).collect();
        assert_eq!(texts.len(), all.len(), "тексти не повторюються");
        assert!(texts.iter().all(|t| t.starts_with("Ризик: ")));
    }

    #[tokio::test]
    async fn entry_sector_falls_back_to_unknown() {
        let (d, h) = fixture().await;
        d.store
            .add_manual(&fond_store::UniverseEntry {
                symbol: sym("AAPL"),
                name: "Apple Inc.".into(),
                sector: " ".into(),
                source: fond_store::UniverseSource::Manual,
                excluded: false,
            })
            .await
            .unwrap();
        let s = run_analysis(&d, &EngineConfig::default(), h.last_date())
            .await
            .unwrap();
        assert_eq!(s.entries_created, 1);
        let ev = events(&d, EventKind::Intent, Some("AAPL")).await;
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].data["sector"], "Unknown");
        assert_eq!(ev[0].data["status"], "pending_approval");
    }

    #[tokio::test]
    async fn missing_next_session_is_no_calendar() {
        let (d, h) = fixture().await;
        let far = h.last_date() + Duration::days(200);
        let err = run_analysis(&d, &EngineConfig::default(), far)
            .await
            .unwrap_err();
        assert!(matches!(err, CycleError::NoCalendar), "{err:?}");
    }
}
