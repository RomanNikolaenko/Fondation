//! Фейкові реалізації портів. Стан — у `std::sync::Mutex` (без `await` під блокуванням).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use rust_decimal::Decimal;

use crate::error::{BrokerError, DataError, JudgeError};
use crate::orders::{BrokerOrder, OrderClass, OrderRequest, OrderStatus, OrderType, Side};
use crate::portfolio::{Account, BrokerPosition};
use crate::traits::{
    AiReview, AiStrategist, AiTickerContext, AssetInfo, Broker, Clock, EarningsCalendar,
    MarketData, NewsItem, NewsJudge, NewsJudgeRequest, NewsJudgment, NewsSource, Session,
};
use crate::types::{Bar, Symbol};

// ---------------------------------------------------------------- FakeClock

pub struct FakeClock {
    now: Mutex<DateTime<Utc>>,
}

impl FakeClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self.now.lock().unwrap() = now;
    }

    pub fn advance(&self, by: Duration) {
        let mut g = self.now.lock().unwrap();
        *g += by;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap()
    }
}

// ---------------------------------------------------------------- FakeBroker

struct BrokerState {
    next_id: u64,
    /// Верхньорівневі ордери; ноги вкладені в `legs`.
    orders: Vec<BrokerOrder>,
    account: Account,
    positions: BTreeMap<Symbol, (i64, Decimal)>,
    submitted: Vec<OrderRequest>,
    cancels: Vec<String>,
    fail_next: Option<BrokerError>,
    fail_cancel: HashSet<String>,
    fail_lookup: HashSet<String>,
}

/// Брокер у пам'яті. `open_orders` повертає верхньорівневі ордери (з вкладеними ногами),
/// якщо відкритий сам ордер або будь-яка з його ніг.
pub struct FakeBroker {
    state: Mutex<BrokerState>,
    clock: Arc<dyn Clock>,
}

/// Час виконання за замовчуванням: 2026-01-02T15:00:00Z (детермінований).
pub fn default_fill_time() -> DateTime<Utc> {
    DateTime::from_timestamp(1_767_366_000, 0).expect("valid timestamp")
}

impl Default for FakeBroker {
    fn default() -> Self {
        Self::with_clock(Arc::new(FakeClock::new(default_fill_time())))
    }
}

impl FakeBroker {
    /// Брокер, що бере час виконання (`filled_at`) з переданого годинника.
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        let hundred_k = Decimal::from(100_000);
        Self {
            state: Mutex::new(BrokerState {
                next_id: 1,
                orders: Vec::new(),
                account: Account {
                    equity: hundred_k,
                    cash: hundred_k,
                    buying_power: hundred_k,
                    last_equity: hundred_k,
                },
                positions: BTreeMap::new(),
                submitted: Vec::new(),
                cancels: Vec::new(),
                fail_next: None,
                fail_cancel: HashSet::new(),
                fail_lookup: HashSet::new(),
            }),
            clock,
        }
    }
}

fn find<'a>(orders: &'a [BrokerOrder], id: &str) -> Option<&'a BrokerOrder> {
    for o in orders {
        if o.id == id {
            return Some(o);
        }
        if let Some(f) = find(&o.legs, id) {
            return Some(f);
        }
    }
    None
}

fn find_mut<'a>(orders: &'a mut [BrokerOrder], id: &str) -> Option<&'a mut BrokerOrder> {
    for o in orders.iter_mut() {
        if o.id == id {
            return Some(o);
        }
        if let Some(f) = find_mut(&mut o.legs, id) {
            return Some(f);
        }
    }
    None
}

fn find_by_client<'a>(orders: &'a [BrokerOrder], cid: &str) -> Option<&'a BrokerOrder> {
    for o in orders {
        if o.client_order_id == cid {
            return Some(o);
        }
        if let Some(f) = find_by_client(&o.legs, cid) {
            return Some(f);
        }
    }
    None
}

fn any_open(o: &BrokerOrder) -> bool {
    o.status.is_open() || o.legs.iter().any(any_open)
}

fn opposite(side: Side) -> Side {
    match side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    }
}

fn leg(
    id: String,
    parent: &OrderRequest,
    suffix: &str,
    order_type: OrderType,
    limit_price: Option<Decimal>,
    stop_price: Option<Decimal>,
) -> BrokerOrder {
    BrokerOrder {
        id,
        client_order_id: format!("{}-{suffix}", parent.client_order_id),
        symbol: parent.symbol.clone(),
        side: opposite(parent.side),
        order_type,
        qty: parent.qty,
        filled_qty: 0,
        filled_avg_price: None,
        limit_price,
        stop_price,
        status: OrderStatus::Held,
        legs: Vec::new(),
        filled_at: None,
    }
}

impl FakeBroker {
    fn lock(&self) -> std::sync::MutexGuard<'_, BrokerState> {
        self.state.lock().unwrap()
    }

    fn take_failure(st: &mut BrokerState) -> Result<(), BrokerError> {
        match st.fail_next.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub fn set_account(&self, account: Account) {
        self.lock().account = account;
    }

    pub fn set_position(&self, symbol: &Symbol, qty: i64, avg: Decimal) {
        let mut st = self.lock();
        if qty == 0 {
            st.positions.remove(symbol);
        } else {
            st.positions.insert(symbol.clone(), (qty, avg));
        }
    }

    /// Виконує `qty` акцій ордера за `price` (приріст до вже виконаного). Часткове виконання
    /// дає `PartiallyFilled`; повне — `Filled`, і тоді стоп-нога стає `New`. Позиції оновлюються.
    pub fn fill(&self, id: &str, price: Decimal, qty: u32) {
        self.fill_at(id, price, qty, self.clock.now());
    }

    /// Як `fill`, але з явним часом виконання.
    pub fn fill_at(&self, id: &str, price: Decimal, qty: u32, at: DateTime<Utc>) {
        let mut guard = self.lock();
        let st = &mut *guard;
        let order = find_mut(&mut st.orders, id).expect("fill: unknown order id");
        let prev = Decimal::from(order.filled_qty);
        let add = Decimal::from(qty);
        let total = order.filled_qty + qty;
        let avg = (order.filled_avg_price.unwrap_or(Decimal::ZERO) * prev + price * add)
            / Decimal::from(total.max(1));
        order.filled_qty = total;
        order.filled_avg_price = Some(avg);
        if total < order.qty {
            order.status = OrderStatus::PartiallyFilled;
        } else {
            order.status = OrderStatus::Filled;
            order.filled_at = Some(at);
            for l in order.legs.iter_mut().filter(|l| l.stop_price.is_some()) {
                if l.status == OrderStatus::Held {
                    l.status = OrderStatus::New;
                }
            }
        }
        let (symbol, side) = (order.symbol.clone(), order.side);
        let entry = st
            .positions
            .entry(symbol.clone())
            .or_insert((0, Decimal::ZERO));
        match side {
            Side::Buy => {
                let old = Decimal::from(entry.0);
                entry.1 = (entry.1 * old + price * add) / (old + add);
                entry.0 += i64::from(qty);
            }
            Side::Sell => entry.0 -= i64::from(qty),
        }
        if entry.0 == 0 {
            st.positions.remove(&symbol);
        }
    }

    pub fn expire(&self, id: &str) {
        let mut st = self.lock();
        find_mut(&mut st.orders, id)
            .expect("expire: unknown order id")
            .status = OrderStatus::Expired;
    }

    /// Наступний виклик будь-якого методу брокера поверне цю помилку.
    pub fn fail_next(&self, err: BrokerError) {
        self.lock().fail_next = Some(err);
    }

    /// `cancel(id)` завжди повертатиме відмову.
    pub fn fail_cancel(&self, id: &str) {
        self.lock().fail_cancel.insert(id.to_string());
    }

    /// Наступний `order_by_client_id` саме для цього id поверне `Transient` (один раз).
    pub fn fail_next_lookup(&self, client_order_id: &str) {
        self.lock().fail_lookup.insert(client_order_id.to_string());
    }

    /// Задає стан ордера (наприклад, `Other("pending_cancel")`).
    pub fn set_status(&self, id: &str, status: OrderStatus) {
        let mut st = self.lock();
        find_mut(&mut st.orders, id)
            .expect("set_status: unknown order id")
            .status = status;
    }

    pub fn submitted(&self) -> Vec<OrderRequest> {
        self.lock().submitted.clone()
    }

    pub fn cancels(&self) -> Vec<String> {
        self.lock().cancels.clone()
    }
}

#[async_trait]
impl Broker for FakeBroker {
    async fn account(&self) -> Result<Account, BrokerError> {
        let mut st = self.lock();
        Self::take_failure(&mut st)?;
        Ok(st.account.clone())
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, BrokerError> {
        let mut st = self.lock();
        Self::take_failure(&mut st)?;
        Ok(st
            .positions
            .iter()
            .map(|(symbol, &(qty, avg))| BrokerPosition {
                symbol: symbol.clone(),
                qty,
                avg_entry_price: avg,
                market_value: avg * Decimal::from(qty),
                current_price: avg,
            })
            .collect())
    }

    async fn open_orders(&self) -> Result<Vec<BrokerOrder>, BrokerError> {
        let mut st = self.lock();
        Self::take_failure(&mut st)?;
        Ok(st.orders.iter().filter(|o| any_open(o)).cloned().collect())
    }

    async fn order(&self, id: &str) -> Result<BrokerOrder, BrokerError> {
        let mut st = self.lock();
        Self::take_failure(&mut st)?;
        find(&st.orders, id).cloned().ok_or(BrokerError::NotFound)
    }

    async fn order_by_client_id(
        &self,
        client_order_id: &str,
    ) -> Result<Option<BrokerOrder>, BrokerError> {
        let mut st = self.lock();
        Self::take_failure(&mut st)?;
        if st.fail_lookup.remove(client_order_id) {
            return Err(BrokerError::Transient("lookup failed".into()));
        }
        Ok(find_by_client(&st.orders, client_order_id).cloned())
    }

    async fn submit(&self, req: &OrderRequest) -> Result<BrokerOrder, BrokerError> {
        let mut st = self.lock();
        Self::take_failure(&mut st)?;
        if find_by_client(&st.orders, &req.client_order_id).is_some() {
            return Err(BrokerError::DuplicateClientOrderId);
        }
        if req.class != OrderClass::Simple && req.stop_loss.is_none() {
            return Err(BrokerError::Rejected {
                status: 422,
                message: "stop_loss required for oto/bracket".into(),
            });
        }
        if req.class == OrderClass::Bracket && req.take_profit.is_none() {
            return Err(BrokerError::Rejected {
                status: 422,
                message: "take_profit required for bracket".into(),
            });
        }
        let n = st.next_id;
        st.next_id += 1;
        let mut legs = Vec::new();
        if req.class != OrderClass::Simple {
            legs.push(leg(
                format!("ord-{n}-stop"),
                req,
                "stop",
                OrderType::Stop,
                None,
                req.stop_loss,
            ));
        }
        if req.class == OrderClass::Bracket {
            legs.push(leg(
                format!("ord-{n}-tp"),
                req,
                "tp",
                OrderType::Limit,
                req.take_profit,
                None,
            ));
        }
        let order = BrokerOrder {
            id: format!("ord-{n}"),
            client_order_id: req.client_order_id.clone(),
            symbol: req.symbol.clone(),
            side: req.side,
            order_type: req.order_type.clone(),
            qty: req.qty,
            filled_qty: 0,
            filled_avg_price: None,
            limit_price: req.limit_price,
            stop_price: req.stop_price,
            status: OrderStatus::Accepted,
            legs,
            filled_at: None,
        };
        st.submitted.push(req.clone());
        st.orders.push(order.clone());
        Ok(order)
    }

    async fn cancel(&self, order_id: &str) -> Result<(), BrokerError> {
        let mut st = self.lock();
        Self::take_failure(&mut st)?;
        st.cancels.push(order_id.to_string());
        if st.fail_cancel.contains(order_id) {
            return Err(BrokerError::Rejected {
                status: 422,
                message: "cannot cancel".into(),
            });
        }
        let o = find_mut(&mut st.orders, order_id).ok_or(BrokerError::NotFound)?;
        if !o.status.is_open() {
            return Err(BrokerError::Rejected {
                status: 422,
                message: "order is not cancelable".into(),
            });
        }
        o.status = OrderStatus::Canceled;
        for l in o.legs.iter_mut().filter(|l| l.status.is_open()) {
            l.status = OrderStatus::Canceled;
        }
        Ok(())
    }

    async fn replace_stop(
        &self,
        order_id: &str,
        new_stop: Decimal,
    ) -> Result<BrokerOrder, BrokerError> {
        let mut st = self.lock();
        Self::take_failure(&mut st)?;
        let m = st.next_id;
        let old = find_mut(&mut st.orders, order_id).ok_or(BrokerError::NotFound)?;
        if !old.status.is_open() || old.stop_price.is_none() {
            return Err(BrokerError::Rejected {
                status: 422,
                message: "order is not a replaceable stop".into(),
            });
        }
        let new_status = old.status.clone();
        old.status = OrderStatus::Replaced;
        let replacement = BrokerOrder {
            id: format!("ord-{m}"),
            client_order_id: format!("{}-r{m}", old.client_order_id),
            stop_price: Some(new_stop),
            status: new_status,
            legs: Vec::new(),
            filled_qty: 0,
            filled_avg_price: None,
            filled_at: None,
            ..old.clone()
        };
        st.next_id += 1;
        st.orders.push(replacement.clone());
        Ok(replacement)
    }
}

// ---------------------------------------------------------------- FakeMarketData

#[derive(Default)]
struct MarketState {
    bars: HashMap<Symbol, Vec<Bar>>,
    sessions: Vec<Session>,
    trades: HashMap<Symbol, f64>,
    assets: Vec<AssetInfo>,
    daily_bars_calls: usize,
    fail_next: Option<DataError>,
}

#[derive(Default)]
pub struct FakeMarketData {
    state: Mutex<MarketState>,
}

impl FakeMarketData {
    pub fn set_bars(&self, symbol: &Symbol, bars: Vec<Bar>) {
        self.state.lock().unwrap().bars.insert(symbol.clone(), bars);
    }

    pub fn set_sessions(&self, sessions: Vec<Session>) {
        self.state.lock().unwrap().sessions = sessions;
    }

    pub fn set_latest_trade(&self, symbol: &Symbol, price: f64) {
        self.state
            .lock()
            .unwrap()
            .trades
            .insert(symbol.clone(), price);
    }

    pub fn set_assets(&self, assets: Vec<AssetInfo>) {
        self.state.lock().unwrap().assets = assets;
    }

    pub fn fail_next(&self, err: DataError) {
        self.state.lock().unwrap().fail_next = Some(err);
    }

    pub fn daily_bars_calls(&self) -> usize {
        self.state.lock().unwrap().daily_bars_calls
    }

    fn check(st: &mut MarketState) -> Result<(), DataError> {
        st.fail_next.take().map_or(Ok(()), Err)
    }
}

#[async_trait]
impl MarketData for FakeMarketData {
    async fn daily_bars(
        &self,
        symbols: &[Symbol],
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<HashMap<Symbol, Vec<Bar>>, DataError> {
        let mut st = self.state.lock().unwrap();
        st.daily_bars_calls += 1;
        Self::check(&mut st)?;
        Ok(symbols
            .iter()
            .filter_map(|s| {
                let bars: Vec<Bar> = st
                    .bars
                    .get(s)?
                    .iter()
                    .filter(|b| b.date >= from && b.date <= to)
                    .cloned()
                    .collect();
                Some((s.clone(), bars))
            })
            .collect())
    }

    async fn calendar(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<Session>, DataError> {
        let mut st = self.state.lock().unwrap();
        Self::check(&mut st)?;
        Ok(st
            .sessions
            .iter()
            .filter(|s| s.date >= from && s.date <= to)
            .cloned()
            .collect())
    }

    async fn latest_trade(&self, symbol: &Symbol) -> Result<Option<f64>, DataError> {
        let mut st = self.state.lock().unwrap();
        Self::check(&mut st)?;
        Ok(st.trades.get(symbol).copied())
    }

    async fn assets(&self) -> Result<Vec<AssetInfo>, DataError> {
        let mut st = self.state.lock().unwrap();
        Self::check(&mut st)?;
        Ok(st.assets.clone())
    }
}

// ---------------------------------------------------------------- FakeNews

#[derive(Default)]
struct NewsState {
    items: HashMap<Symbol, Vec<NewsItem>>,
    calls: usize,
    fail_next: Option<DataError>,
}

#[derive(Default)]
pub struct FakeNews {
    state: Mutex<NewsState>,
}

impl FakeNews {
    pub fn set(&self, symbol: &Symbol, items: Vec<NewsItem>) {
        self.state
            .lock()
            .unwrap()
            .items
            .insert(symbol.clone(), items);
    }

    pub fn fail_next(&self, err: DataError) {
        self.state.lock().unwrap().fail_next = Some(err);
    }

    pub fn calls(&self) -> usize {
        self.state.lock().unwrap().calls
    }
}

#[async_trait]
impl NewsSource for FakeNews {
    async fn news(
        &self,
        symbol: &Symbol,
        since: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<NewsItem>, DataError> {
        let mut st = self.state.lock().unwrap();
        st.calls += 1;
        if let Some(e) = st.fail_next.take() {
            return Err(e);
        }
        Ok(st
            .items
            .get(symbol)
            .map(|v| {
                v.iter()
                    .filter(|n| n.created_at >= since)
                    .take(limit)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }
}

// ---------------------------------------------------------------- FakeEarnings

#[derive(Default)]
struct EarningsState {
    map: HashMap<Symbol, Result<Option<NaiveDate>, DataError>>,
    calls: usize,
}

#[derive(Default)]
pub struct FakeEarnings {
    state: Mutex<EarningsState>,
}

impl FakeEarnings {
    /// Для символів без запису повертається `Ok(None)`.
    pub fn set(&self, symbol: &Symbol, result: Result<Option<NaiveDate>, DataError>) {
        self.state
            .lock()
            .unwrap()
            .map
            .insert(symbol.clone(), result);
    }

    pub fn calls(&self) -> usize {
        self.state.lock().unwrap().calls
    }
}

#[async_trait]
impl EarningsCalendar for FakeEarnings {
    async fn next_earnings(
        &self,
        symbol: &Symbol,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Option<NaiveDate>, DataError> {
        let mut st = self.state.lock().unwrap();
        st.calls += 1;
        match st.map.get(symbol) {
            Some(Ok(Some(d))) if *d >= from && *d <= to => Ok(Some(*d)),
            Some(Ok(_)) | None => Ok(None),
            Some(Err(e)) => Err(e.clone()),
        }
    }
}

// ---------------------------------------------------------------- FakeJudge

struct JudgeState {
    result: Result<NewsJudgment, JudgeError>,
    calls: usize,
}

pub struct FakeJudge {
    state: Mutex<JudgeState>,
}

impl FakeJudge {
    pub fn new(result: Result<NewsJudgment, JudgeError>) -> Self {
        Self {
            state: Mutex::new(JudgeState { result, calls: 0 }),
        }
    }

    pub fn set_result(&self, result: Result<NewsJudgment, JudgeError>) {
        self.state.lock().unwrap().result = result;
    }

    pub fn calls(&self) -> usize {
        self.state.lock().unwrap().calls
    }
}

#[async_trait]
impl NewsJudge for FakeJudge {
    async fn judge(&self, _req: &NewsJudgeRequest) -> Result<NewsJudgment, JudgeError> {
        let mut st = self.state.lock().unwrap();
        st.calls += 1;
        st.result.clone()
    }
}

// ---------------------------------------------------------- FakeStrategist

/// Відповіді віддає по черзі; остання в черзі — для всіх наступних викликів.
/// Усі контексти, які двигун передав стратегісту, зберігаються для перевірок.
struct StrategistState {
    results: Vec<Result<AiReview, JudgeError>>,
    calls: usize,
    contexts: Vec<AiTickerContext>,
}

pub struct FakeStrategist {
    state: Mutex<StrategistState>,
}

impl FakeStrategist {
    pub fn new(results: Vec<Result<AiReview, JudgeError>>) -> Self {
        Self {
            state: Mutex::new(StrategistState {
                results,
                calls: 0,
                contexts: Vec::new(),
            }),
        }
    }

    pub fn calls(&self) -> usize {
        self.state.lock().unwrap().calls
    }

    pub fn contexts(&self) -> Vec<AiTickerContext> {
        self.state.lock().unwrap().contexts.clone()
    }

    /// Перезадає відповіді; нумерує виклики заново.
    pub fn set_results(&self, results: Vec<Result<AiReview, JudgeError>>) {
        let mut st = self.state.lock().unwrap();
        st.results = results;
        st.calls = 0;
        st.contexts.clear();
    }
}

#[async_trait]
impl AiStrategist for FakeStrategist {
    async fn analyze(&self, ctx: &AiTickerContext) -> Result<AiReview, JudgeError> {
        let mut st = self.state.lock().unwrap();
        st.calls += 1;
        st.contexts.push(ctx.clone());
        if st.results.is_empty() {
            return Err(JudgeError::Other("немає заготованого результату".into()));
        }
        let idx = st.calls.min(st.results.len()) - 1;
        st.results[idx].clone()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::orders::TimeInForce;
    use crate::traits::{NewsVerdict, Verdict};

    fn sample_oto_request(cid: &str) -> OrderRequest {
        OrderRequest {
            client_order_id: cid.into(),
            symbol: Symbol::new("AAPL"),
            side: Side::Buy,
            qty: 10,
            order_type: OrderType::Limit,
            limit_price: Some(dec!(100.00)),
            stop_price: None,
            tif: TimeInForce::Day,
            class: OrderClass::Oto,
            stop_loss: Some(dec!(95.00)),
            take_profit: None,
        }
    }

    fn judgment() -> NewsJudgment {
        NewsJudgment {
            verdict: NewsVerdict {
                verdict: Verdict::Allow,
                reasons: vec![],
                key_events: vec![],
            },
            model: "m".into(),
            input_tokens: 1,
            output_tokens: 2,
        }
    }

    #[tokio::test]
    async fn fake_broker_rejects_duplicate_client_order_id() {
        let br = FakeBroker::default();
        let req = sample_oto_request("fond-x");
        let first = br.submit(&req).await.unwrap();
        assert_eq!(first.legs.len(), 1);
        assert!(matches!(
            br.submit(&req).await,
            Err(BrokerError::DuplicateClientOrderId)
        ));
        assert_eq!(br.submitted().len(), 1);
    }

    #[tokio::test]
    async fn oto_leg_is_held_stop_and_bracket_adds_take_profit() {
        let br = FakeBroker::default();
        let o = br.submit(&sample_oto_request("a")).await.unwrap();
        assert_eq!(o.id, "ord-1");
        assert_eq!(o.status, OrderStatus::Accepted);
        let stop = &o.legs[0];
        assert_eq!(stop.id, "ord-1-stop");
        assert_eq!(stop.order_type, OrderType::Stop);
        assert_eq!(stop.status, OrderStatus::Held);
        assert_eq!(
            (stop.qty, stop.stop_price, stop.side),
            (10, Some(dec!(95.00)), Side::Sell)
        );

        let mut req = sample_oto_request("b");
        req.class = OrderClass::Bracket;
        req.take_profit = Some(dec!(120.00));
        let b = br.submit(&req).await.unwrap();
        assert_eq!(b.legs.len(), 2);
        assert_eq!(b.legs[1].id, "ord-2-tp");
        assert_eq!(b.legs[1].order_type, OrderType::Limit);
        assert_eq!(b.legs[1].limit_price, Some(dec!(120.00)));
    }

    #[tokio::test]
    async fn fill_updates_status_positions_and_activates_stop() {
        let br = FakeBroker::default();
        br.submit(&sample_oto_request("a")).await.unwrap();
        br.fill("ord-1", dec!(100.00), 4);
        let o = br.order("ord-1").await.unwrap();
        assert_eq!(o.status, OrderStatus::PartiallyFilled);
        assert_eq!(o.legs[0].status, OrderStatus::Held);
        br.fill("ord-1", dec!(101.00), 6);
        let o = br.order("ord-1").await.unwrap();
        assert_eq!(o.status, OrderStatus::Filled);
        assert_eq!(o.filled_qty, 10);
        assert_eq!(o.filled_avg_price, Some(dec!(100.60)));
        assert!(o.filled_at.is_some());
        assert_eq!(o.legs[0].status, OrderStatus::New);
        let pos = br.positions().await.unwrap();
        assert_eq!(pos.len(), 1);
        assert_eq!((pos[0].qty, pos[0].avg_entry_price), (10, dec!(100.60)));

        // sell reduces the position
        let sell = br.order("ord-1-stop").await.unwrap();
        assert_eq!(sell.side, Side::Sell);
        br.fill("ord-1-stop", dec!(95.00), 10);
        assert!(br.positions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn filled_at_comes_from_injected_clock_or_explicit_time() {
        let t = Utc.with_ymd_and_hms(2026, 3, 10, 3, 59, 0).unwrap();
        let clock = Arc::new(FakeClock::new(t));
        let br = FakeBroker::with_clock(clock.clone());
        br.submit(&sample_oto_request("a")).await.unwrap();
        br.fill("ord-1", dec!(100.00), 10);
        assert_eq!(br.order("ord-1").await.unwrap().filled_at, Some(t));

        br.submit(&sample_oto_request("b")).await.unwrap();
        let at = t + Duration::minutes(5);
        br.fill_at("ord-2", dec!(100.00), 10, at);
        assert_eq!(br.order("ord-2").await.unwrap().filled_at, Some(at));

        let d = FakeBroker::default();
        d.submit(&sample_oto_request("c")).await.unwrap();
        d.fill("ord-1", dec!(100.00), 10);
        assert_eq!(
            d.order("ord-1").await.unwrap().filled_at,
            Some(default_fill_time())
        );
    }

    #[tokio::test]
    async fn replace_stop_marks_old_replaced_and_creates_new() {
        let br = FakeBroker::default();
        br.submit(&sample_oto_request("a")).await.unwrap();
        br.fill("ord-1", dec!(100.00), 10);
        let new = br.replace_stop("ord-1-stop", dec!(97.50)).await.unwrap();
        assert_eq!(new.id, "ord-2");
        assert_eq!(new.stop_price, Some(dec!(97.50)));
        assert_eq!(
            br.order("ord-1-stop").await.unwrap().status,
            OrderStatus::Replaced
        );
        assert!(br.order("ord-2").await.unwrap().status.is_open());
        assert!(matches!(
            br.replace_stop("nope", dec!(1)).await,
            Err(BrokerError::NotFound)
        ));
    }

    #[tokio::test]
    async fn cancel_expire_and_failures() {
        let br = FakeBroker::default();
        br.submit(&sample_oto_request("a")).await.unwrap();
        br.cancel("ord-1").await.unwrap();
        assert_eq!(
            br.order("ord-1").await.unwrap().status,
            OrderStatus::Canceled
        );
        assert_eq!(
            br.order("ord-1-stop").await.unwrap().status,
            OrderStatus::Canceled
        );
        assert_eq!(br.cancels(), vec!["ord-1".to_string()]);
        assert!(br.open_orders().await.unwrap().is_empty());

        br.submit(&sample_oto_request("b")).await.unwrap();
        br.fail_cancel("ord-2");
        assert!(br.cancel("ord-2").await.is_err());
        br.expire("ord-2");
        assert_eq!(
            br.order("ord-2").await.unwrap().status,
            OrderStatus::Expired
        );

        br.fail_next(BrokerError::Transient("boom".into()));
        assert!(matches!(br.account().await, Err(BrokerError::Transient(_))));
        assert!(br.account().await.is_ok());
        assert!(br.order_by_client_id("b").await.unwrap().is_some());
        assert!(br.order_by_client_id("zzz").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn set_status_and_one_shot_lookup_failure() {
        let br = FakeBroker::default();
        br.submit(&sample_oto_request("a")).await.unwrap();
        let pending = OrderStatus::Other("pending_cancel".into());
        br.set_status("ord-1-stop", pending.clone());
        assert_eq!(br.order("ord-1-stop").await.unwrap().status, pending);

        br.fail_next_lookup("a");
        assert!(br.order_by_client_id("zzz").await.unwrap().is_none());
        assert!(matches!(
            br.order_by_client_id("a").await,
            Err(BrokerError::Transient(_))
        ));
        assert!(br.order_by_client_id("a").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn fake_market_data_filters_and_counts() {
        let md = FakeMarketData::default();
        let sym = Symbol::new("AAPL");
        let start = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        md.set_bars(&sym, crate::testkit::uptrend_then_pullback(start, 10, 0));
        let to = NaiveDate::from_ymd_opt(2025, 1, 6).unwrap();
        let got = md
            .daily_bars(&[sym.clone(), Symbol::new("ZZZ")], start, to)
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[&sym].len(), 3);
        assert_eq!(md.daily_bars_calls(), 1);
        md.set_latest_trade(&sym, 12.5);
        assert_eq!(md.latest_trade(&sym).await.unwrap(), Some(12.5));
    }

    #[tokio::test]
    async fn fake_news_earnings_judge_clock() {
        let sym = Symbol::new("AAPL");
        let news = FakeNews::default();
        let t = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        news.set(
            &sym,
            vec![NewsItem {
                headline: "h".into(),
                summary: "s".into(),
                created_at: t,
                source: "x".into(),
            }],
        );
        assert_eq!(news.news(&sym, t, 5).await.unwrap().len(), 1);
        assert_eq!(
            news.news(&sym, t + Duration::days(1), 5)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(news.calls(), 2);

        let e = FakeEarnings::default();
        let day = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        e.set(&sym, Ok(Some(day)));
        e.set(
            &Symbol::new("BAD"),
            Err(DataError::NotAvailable("x".into())),
        );
        assert_eq!(e.next_earnings(&sym, day, day).await.unwrap(), Some(day));
        assert!(
            e.next_earnings(&Symbol::new("BAD"), day, day)
                .await
                .is_err()
        );
        assert_eq!(
            e.next_earnings(&Symbol::new("NONE"), day, day)
                .await
                .unwrap(),
            None
        );
        assert_eq!(e.calls(), 3);

        let j = FakeJudge::new(Ok(judgment()));
        let req = NewsJudgeRequest {
            symbol: sym,
            company: "c".into(),
            signal_summary: "s".into(),
            items: vec![],
        };
        assert!(j.judge(&req).await.is_ok());
        j.set_result(Err(JudgeError::Refused));
        assert_eq!(j.judge(&req).await, Err(JudgeError::Refused));
        assert_eq!(j.calls(), 2);

        let clock = FakeClock::new(t);
        clock.advance(Duration::hours(2));
        assert_eq!(clock.now(), t + Duration::hours(2));
        clock.set(t);
        assert_eq!(clock.now(), t);
    }
}
