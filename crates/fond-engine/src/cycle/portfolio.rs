//! Прохід ребалансування портфеля: цільові ваги → дельти → наміри конвеєра.
//!
//! Викликається всередині вечірнього кроку аналізу, коли сесія відправки —
//! перший торговий день місяця або кварталу. Наміри живуть у тому самому
//! життєвому циклі, що й swing: схвалення → submit → звірка. Позиції
//! swing-стратегій не торкаються: тікер під swing-керуванням виключається
//! з цілей з журналюванням.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::Datelike;
use fond_api::config::RebalanceMode;
use fond_api::events::EngineEventDto;
use fond_core::calendar::TradingCalendar;
use fond_core::ids::{IntentTag, intent_id};
use fond_core::intent::{IntentKind, IntentStatus};
use fond_core::orders::Side;
use fond_core::portfolio::{Position, TrackedPosition};
use fond_core::price::to_f64;
use fond_core::rebalance::{PORTFOLIO_STRATEGY, RebalanceDelta, RebalanceParams, compute_deltas};
use fond_core::types::{ExecutionMode, StrategyId, Symbol};
use fond_store::EventKind;
use serde_json::json;

use super::analysis::Run;
use super::{CycleError, journal};

/// Індекс кварталу дати: 0–3.
fn quarter(d: chrono::NaiveDate) -> u32 {
    (d.month() - 1) / 3
}

/// Чи сесія `session` — перший торговий день місяця (`Monthly`) або кварталу
/// (`Quarterly`) за календарем. Для `Off` — ні. Попередня сесія визначається
/// за календарем (не «перше число» наївно); якщо попередньої сесії в календарі
/// немає, день ребалансом не вважається.
pub fn is_rebalance_day(
    session: chrono::NaiveDate,
    mode: RebalanceMode,
    cal: &TradingCalendar,
) -> bool {
    if mode == RebalanceMode::Off {
        return false;
    }
    let Some(prev) = session
        .checked_sub_signed(chrono::Duration::days(1))
        .and_then(|d| cal.last_session_on_or_before(d))
    else {
        return false;
    };
    match mode {
        RebalanceMode::Off => false,
        // Зміна місяця покриває й перехід року (груень → січень).
        RebalanceMode::Monthly => prev.month() != session.month(),
        RebalanceMode::Quarterly => quarter(prev) != quarter(session),
    }
}

impl Run<'_> {
    /// Прохід ребалансу: дельти «ціль − факт» для позицій стратегії `portfolio`
    /// → наміри (статус за режимом виконання, як у swing-входів). Повертає
    /// кількість створених намірів. `manual = true` — ручний запуск з UI: без
    /// перевірки дня ребалансу. Позарозкладний виклик — тихий пропуск.
    pub(crate) async fn portfolio_pass(
        &self,
        positions: &[TrackedPosition],
        equity: f64,
        broker_cash: f64,
        cal: &TradingCalendar,
        manual: bool,
    ) -> Result<usize, CycleError> {
        let (d, cfg) = (self.d, self.cfg);
        let p = &cfg.portfolio;
        let scheduled = is_rebalance_day(self.session, p.rebalance, cal);
        if !p.enabled || (!scheduled && !manual) {
            return Ok(0);
        }
        let (intro, intro_data) = if scheduled {
            let period = match p.rebalance {
                RebalanceMode::Monthly => "місяця",
                _ => "кварталу",
            };
            (
                format!(
                    "Ребаланс портфеля: сесія {} — перший торговий день {period}",
                    self.session
                ),
                json!({ "step": "portfolio_rebalance", "session": self.session, "period": period }),
            )
        } else {
            (
                format!("Ребаланс портфеля: ручний запуск (сесія {})", self.session),
                json!({
                    "step": "portfolio_rebalance",
                    "session": self.session,
                    "manual": true,
                }),
            )
        };
        journal(d, EventKind::Cycle, None, None, intro, intro_data).await?;

        // Тікери під swing-керуванням портфель не чіпає: їх немає в цілях,
        // а цілі на них пропускаються з журналом.
        let swing: HashSet<&Symbol> = positions
            .iter()
            .filter(|tp| tp.position.strategy.as_str() != PORTFOLIO_STRATEGY)
            .map(|tp| &tp.position.symbol)
            .collect();

        // Кеш, зарезервований живими намірами сесії (входи й ребаланси):
        // вони ще не виконані, але витратять гроші при відправці.
        let live = d.store.intents_for_session(self.session).await?;
        let mut reserved = 0.0_f64;
        let mut live_symbols: HashSet<Symbol> = HashSet::new();
        for i in live.iter().filter(|i| {
            matches!(
                i.status,
                IntentStatus::PendingApproval | IntentStatus::Approved | IntentStatus::Submitted
            )
        }) {
            match &i.kind {
                IntentKind::Entry {
                    qty, limit_price, ..
                } => {
                    reserved += f64::from(*qty) * to_f64(*limit_price);
                    live_symbols.insert(i.symbol.clone());
                }
                IntentKind::Rebalance {
                    qty, limit_price, ..
                } => {
                    reserved += f64::from(*qty) * to_f64(*limit_price);
                    live_symbols.insert(i.symbol.clone());
                }
                _ => {}
            }
        }

        // Цілі: тікери зі swing-позиціями або живим swing-входом виключаються.
        let mut targets: BTreeMap<Symbol, f64> = BTreeMap::new();
        let mut skipped_tickers: Vec<(Symbol, &'static str)> = Vec::new();
        for (ticker, weight) in &p.targets {
            let symbol = Symbol::new(ticker);
            if swing.contains(&symbol) {
                skipped_tickers.push((symbol, "тікер під керуванням swing-стратегії"));
                continue;
            }
            if live_symbols.contains(&symbol) {
                skipped_tickers.push((symbol, "уже є живий намір сесії"));
                continue;
            }
            targets.insert(symbol, *weight);
        }
        for (symbol, why) in &skipped_tickers {
            journal(
                d,
                EventKind::Cycle,
                Some(symbol),
                None,
                format!("Ребаланс: ціль {symbol} пропущено — {why}"),
                json!({ "step": "portfolio_rebalance", "symbol": symbol, "reason": why }),
            )
            .await?;
        }

        // Ціни: останній close у вікні даних; тікер без барав — пропуск усередині
        // `compute_deltas` (NoPrice).
        let portfolio: Vec<Position> = positions
            .iter()
            .filter(|tp| tp.position.strategy.as_str() == PORTFOLIO_STRATEGY)
            .map(|tp| tp.position.clone())
            .collect();
        let mut prices: HashMap<Symbol, f64> = HashMap::new();
        let mut price_symbols: HashSet<Symbol> = targets.keys().cloned().collect();
        price_symbols.extend(portfolio.iter().map(|p| p.symbol.clone()));
        for symbol in &price_symbols {
            let bars = d.store.bars(symbol, self.from, self.date).await?;
            if let Some(last) = bars.last() {
                prices.insert(symbol.clone(), last.close);
            }
        }

        let params = RebalanceParams {
            drift: p.drift,
            min_order_usd: p.min_order_usd,
            max_gap: cfg.risk.max_gap,
        };
        let cash = (broker_cash - reserved).max(0.0);
        let plan = compute_deltas(&targets, &portfolio, &prices, equity, cash, &params);

        // Пропуски математики — одним журнальним записом.
        if !plan.skipped.is_empty() {
            let list: Vec<String> = plan
                .skipped
                .iter()
                .map(|s| format!("{} — {}", s.symbol, s.reason.text()))
                .collect();
            journal(
                d,
                EventKind::Cycle,
                None,
                None,
                format!("Ребаланс: пропущено {}", list.join(", ")),
                json!({
                    "step": "portfolio_rebalance",
                    "skipped": plan.skipped.iter().map(|s| {
                        json!({ "symbol": s.symbol, "reason": s.reason.text() })
                    }).collect::<Vec<_>>(),
                }),
            )
            .await?;
        }

        // Наміри: статус за режимом виконання, як у swing-входів.
        let status = match cfg.execution_mode {
            ExecutionMode::Approval => IntentStatus::PendingApproval,
            ExecutionMode::Auto => IntentStatus::Approved,
        };
        let strategy = StrategyId::new(PORTFOLIO_STRATEGY);
        let (mut created, mut pending) = (0, 0);
        for delta in &plan.deltas {
            let id = intent_id(self.session, &strategy, &delta.symbol, IntentTag::Rebalance);
            let kind = IntentKind::Rebalance {
                side: delta.side,
                qty: delta.qty,
                limit_price: delta.limit_price,
            };
            let weight = targets.get(&delta.symbol).copied().unwrap_or(0.0);
            let explanation = explanation_text(delta, weight);
            let intent = self.intent(
                id.clone(),
                &delta.symbol,
                &strategy,
                kind,
                status,
                explanation.clone(),
            );
            if d.store.insert_intent(&intent).await? {
                created += 1;
                if status == IntentStatus::PendingApproval {
                    pending += 1;
                }
                let verb = match delta.side {
                    Side::Buy => "купівля",
                    Side::Sell => "продаж",
                };
                journal(
                    d,
                    EventKind::Intent,
                    Some(&delta.symbol),
                    Some(&id),
                    format!("Ребаланс ({verb}): {explanation}"),
                    json!({
                        "kind": "rebalance",
                        "session": self.session,
                        "status": status,
                        "side": match delta.side {
                            Side::Buy => "buy",
                            Side::Sell => "sell",
                        },
                        "qty": delta.qty,
                        "limit_price": delta.limit_price.to_string(),
                        "target_weight": weight,
                        "target_value": delta.target_value,
                        "current_value": delta.current_value,
                    }),
                )
                .await?;
            }
        }

        if pending > 0 {
            d.bus.publish(EngineEventDto::IntentsPending {
                session: self.session,
                count: pending,
            });
        }
        let (buys, sells) = plan
            .deltas
            .iter()
            .fold((0, 0), |(b, s), delta| match delta.side {
                Side::Buy => (b + 1, s),
                Side::Sell => (b, s + 1),
            });
        journal(
            d,
            EventKind::Cycle,
            None,
            None,
            format!(
                "Ребаланс завершено: намірів {created} (купівель {buys}, продажів {sells}), \
                 пропущено {}",
                plan.skipped.len() + skipped_tickers.len()
            ),
            json!({
                "step": "portfolio_rebalance",
                "created": created,
                "buys": buys,
                "sells": sells,
                "skipped": plan.skipped.len() + skipped_tickers.len(),
            }),
        )
        .await?;
        Ok(created)
    }
}

/// Пояснення наміру українською: ціль у відсотках і дельта в доларах.
fn explanation_text(delta: &RebalanceDelta, weight: f64) -> String {
    let pct = format!("{:.0}", weight * 100.0);
    let usd = format!("{:.0}", (delta.target_value - delta.current_value).abs());
    match delta.side {
        Side::Buy => format!("докупити до цілі {pct}% (+${usd})"),
        Side::Sell => format!("продати до цілі {pct}% (−${usd})"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{NaiveDate, NaiveTime};
    use fond_api::config::{EngineConfig, RebalanceMode};
    use fond_core::calendar::TradingCalendar;
    use fond_core::ids::{IntentTag, intent_id};
    use fond_core::intent::{IntentKind, IntentStatus};
    use fond_core::orders::Side;
    use fond_core::price::to_f64;
    use fond_core::rebalance::PORTFOLIO_STRATEGY;
    use fond_core::traits::Session;
    use fond_core::types::StrategyId;
    use rust_decimal_macros::dec;

    use super::super::analysis::Run;
    use super::super::refresh::{refresh_market_data, window_start};
    use super::super::test_support::{STRATEGY, d, fixture, journal_messages, sym, tracked};
    use super::is_rebalance_day;

    fn cal(dates: &[NaiveDate]) -> TradingCalendar {
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        TradingCalendar::new(
            dates
                .iter()
                .map(|date| Session {
                    date: *date,
                    open: t(9, 30),
                    close: t(16, 0),
                })
                .collect(),
        )
    }

    /// Будні: пт 30.10, пн 2.11, вт 3.11.
    fn week_calendar() -> TradingCalendar {
        cal(&[d(2026, 10, 30), d(2026, 11, 2), d(2026, 11, 3)])
    }

    #[test]
    fn monthly_first_trading_day_of_month() {
        let c = week_calendar();
        assert!(is_rebalance_day(d(2026, 11, 2), RebalanceMode::Monthly, &c));
        // Не перший день місяця.
        assert!(!is_rebalance_day(
            d(2026, 11, 3),
            RebalanceMode::Monthly,
            &c
        ));
        // Off — ніколи.
        assert!(!is_rebalance_day(d(2026, 11, 2), RebalanceMode::Off, &c));
    }

    #[test]
    fn quarterly_quarter_boundary() {
        // Вересень → жовтень: межа кварталу.
        let c = cal(&[d(2026, 9, 30), d(2026, 10, 1)]);
        assert!(is_rebalance_day(
            d(2026, 10, 1),
            RebalanceMode::Quarterly,
            &c
        ));
        assert!(is_rebalance_day(d(2026, 10, 1), RebalanceMode::Monthly, &c));
        // Жовтень → листопад: зміна місяця, але не кварталу.
        let c = cal(&[d(2026, 10, 30), d(2026, 11, 2)]);
        assert!(!is_rebalance_day(
            d(2026, 11, 2),
            RebalanceMode::Quarterly,
            &c
        ));
        // Груень → січень: межа кварталу й року.
        let c = cal(&[d(2026, 12, 31), d(2027, 1, 4)]);
        assert!(is_rebalance_day(
            d(2027, 1, 4),
            RebalanceMode::Quarterly,
            &c
        ));
        assert!(is_rebalance_day(d(2027, 1, 4), RebalanceMode::Monthly, &c));
    }

    #[test]
    fn missing_prev_session_is_not_rebalance_day() {
        // Попередньої сесії в календарі немає — день ребалансом не вважається.
        let empty = cal(&[]);
        assert!(!is_rebalance_day(
            d(2026, 11, 2),
            RebalanceMode::Monthly,
            &empty
        ));
        let c = cal(&[d(2026, 11, 2)]);
        assert!(!is_rebalance_day(
            d(2026, 11, 2),
            RebalanceMode::Monthly,
            &c
        ));
    }

    /// Календар рушія: сесії зі сховища стенда навколо `around`.
    async fn calendar_of(dp: &crate::deps::Deps, around: NaiveDate) -> TradingCalendar {
        let span = chrono::Duration::days(super::super::CALENDAR_SPAN_DAYS);
        TradingCalendar::new(
            dp.store
                .sessions(around - span, around + span)
                .await
                .unwrap(),
        )
    }

    async fn run_pass(
        dp: &crate::deps::Deps,
        cfg: &EngineConfig,
        date: NaiveDate,
        session: NaiveDate,
        manual: bool,
    ) -> usize {
        refresh_market_data(dp, cfg, date).await.unwrap();
        let cal = calendar_of(dp, date).await;
        let run = Run {
            d: dp,
            cfg,
            date,
            session,
            from: window_start(cfg, date),
        };
        let account = dp.broker.account().await.unwrap();
        let positions = dp.store.positions().await.unwrap();
        run.portfolio_pass(
            &positions,
            to_f64(account.equity),
            to_f64(account.cash),
            &cal,
            manual,
        )
        .await
        .unwrap()
    }

    fn portfolio_targets() -> BTreeMap<String, f64> {
        BTreeMap::from([
            ("SPY".to_string(), 0.2),
            ("AAPL".to_string(), 0.3), // swing-позиція — пропуск
            ("MSFT".to_string(), 0.0), // позиція портфеля — продаж до нуля
        ])
    }

    async fn rebalance(
        dp: &crate::deps::Deps,
        session: NaiveDate,
        ticker: &str,
    ) -> Option<fond_core::intent::Intent> {
        let strategy = StrategyId::new(PORTFOLIO_STRATEGY);
        dp.store
            .intent(&intent_id(
                session,
                &strategy,
                &sym(ticker),
                IntentTag::Rebalance,
            ))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn pass_creates_intents_for_targets() {
        let (dp, _h) = fixture().await;
        let mut cfg = EngineConfig::default();
        cfg.portfolio.enabled = true;
        cfg.portfolio.rebalance = RebalanceMode::Monthly;
        cfg.portfolio.targets = portfolio_targets();
        let now = dp.clock.now();
        dp.store
            .upsert_position(&tracked("MSFT", PORTFOLIO_STRATEGY, 60, 0.0, None), now)
            .await
            .unwrap();
        dp.store
            .upsert_position(&tracked("AAPL", STRATEGY, 10, 95.0, Some("ord-1")), now)
            .await
            .unwrap();

        // Пт 30.10 — вечір; сесія пн 2.11 — перший торговий день листопада.
        let created = run_pass(&dp, &cfg, d(2026, 10, 30), d(2026, 11, 2), false).await;
        assert_eq!(created, 2);

        let session = d(2026, 11, 2);
        let sell = rebalance(&dp, session, "MSFT")
            .await
            .expect("намір продажі");
        assert_eq!(
            sell.kind,
            IntentKind::Rebalance {
                side: Side::Sell,
                qty: 60,
                limit_price: dec!(99.00), // 100 × (1 − max_gap 0.01)
            }
        );
        assert_eq!(sell.status, IntentStatus::PendingApproval);
        assert_eq!(sell.explanation, "продати до цілі 0% (−$6000)");

        let buy = rebalance(&dp, session, "SPY").await.expect("намір купівлі");
        match buy.kind {
            IntentKind::Rebalance {
                side: Side::Buy,
                qty,
                limit_price,
            } => {
                assert!(qty > 0);
                assert!(limit_price > dec!(0.00));
            }
            other => panic!("не той вид: {other:?}"),
        }
        assert_eq!(buy.status, IntentStatus::PendingApproval);

        // AAPL: swing-позиція, наміру ребалансу немає.
        assert!(rebalance(&dp, session, "AAPL").await.is_none());

        let messages = journal_messages(&dp).await;
        assert!(
            messages
                .iter()
                .any(|m| m.contains("Ребаланс завершено: намірів 2")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("ціль AAPL пропущено — тікер під керуванням swing-стратегії")),
            "{messages:?}"
        );
    }

    #[tokio::test]
    async fn pass_skips_quietly_off_schedule() {
        let (dp, _h) = fixture().await;
        let mut cfg = EngineConfig::default();
        cfg.portfolio.enabled = true;
        cfg.portfolio.rebalance = RebalanceMode::Monthly;
        cfg.portfolio.targets = BTreeMap::from([("SPY".to_string(), 0.2)]);
        // Не перший торговий день місяця — тихий пропуск, без подій ребалансу.
        let created = run_pass(&dp, &cfg, d(2026, 11, 4), d(2026, 11, 5), false).await;
        assert_eq!(created, 0);
        let messages = journal_messages(&dp).await;
        assert!(
            !messages.iter().any(|m| m.contains("Ребаланс")),
            "{messages:?}"
        );
    }

    #[tokio::test]
    async fn pass_disabled_config_is_quiet() {
        let (dp, _h) = fixture().await;
        let cfg = EngineConfig::default(); // portfolio.enabled = false
        let created = run_pass(&dp, &cfg, d(2026, 10, 30), d(2026, 11, 2), false).await;
        assert_eq!(created, 0);
        let messages = journal_messages(&dp).await;
        assert!(
            !messages.iter().any(|m| m.contains("Ребаланс")),
            "{messages:?}"
        );
    }

    #[tokio::test]
    async fn pass_manual_run_off_schedule_creates_intents() {
        let (dp, _h) = fixture().await;
        let mut cfg = EngineConfig::default();
        cfg.portfolio.enabled = true;
        // Ребаланс вимкнено за розкладом: ручний запуск іде поза розкладом.
        cfg.portfolio.rebalance = RebalanceMode::Off;
        cfg.portfolio.targets = BTreeMap::from([("SPY".to_string(), 0.5)]);
        // Не день ребалансу, але manual = true — наміри все одно створюються.
        let created = run_pass(&dp, &cfg, d(2026, 11, 4), d(2026, 11, 5), true).await;
        assert_eq!(created, 1);
        let messages = journal_messages(&dp).await;
        assert!(
            messages
                .iter()
                .any(|m| m.contains("Ребаланс портфеля: ручний запуск (сесія 2026-11-05)")),
            "{messages:?}"
        );
        let buy = rebalance(&dp, d(2026, 11, 5), "SPY")
            .await
            .expect("намір купівлі");
        assert_eq!(buy.status, IntentStatus::PendingApproval);
    }
}
