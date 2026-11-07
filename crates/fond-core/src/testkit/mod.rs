//! Тестові утиліти: генератори барів і фейкові порти. Лише під фічею `testkit`.

mod bars;
mod fakes;

pub use bars::{
    bars_from_closes, breakout_series, synthetic_bars, uptrend_then_pullback, weekdays_from,
};
pub use fakes::{
    FakeBroker, FakeClock, FakeEarnings, FakeJudge, FakeMarketData, FakeNews, default_fill_time,
};
