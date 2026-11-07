//! Фільтри входу: правило звітів та вартість LLM.

use chrono::{Duration, NaiveDate};

use crate::calendar::TradingCalendar;

/// `true`, якщо `next_earnings` ∈ `[signal_date, nth_session_after(signal_date, days_ahead)]`.
///
/// Якщо календар закороткий і `nth_session_after` дає `None`, кінець вікна —
/// `signal_date + 2 * days_ahead` календарних днів (консервативно, з запасом).
pub fn earnings_blocks(
    signal_date: NaiveDate,
    next_earnings: Option<NaiveDate>,
    cal: &TradingCalendar,
    days_ahead: usize,
) -> bool {
    let Some(e) = next_earnings else {
        return false;
    };
    let end = cal
        .nth_session_after(signal_date, days_ahead)
        .unwrap_or_else(|| {
            signal_date + Duration::days(i64::try_from(days_ahead).unwrap_or(0).saturating_mul(2))
        });
    e >= signal_date && e <= end
}

/// Вартість виклику LLM у USD за ціною на 1 млн токенів.
pub fn llm_cost_usd(
    input_tokens: u64,
    output_tokens: u64,
    usd_per_mtok_in: f64,
    usd_per_mtok_out: f64,
) -> f64 {
    (input_tokens as f64 * usd_per_mtok_in + output_tokens as f64 * usd_per_mtok_out) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::tests::{cal_nov_2026, d};

    #[test]
    fn earnings_window_includes_signal_day_and_three_sessions() {
        let cal = cal_nov_2026();
        assert!(earnings_blocks(
            d(2026, 11, 24),
            Some(d(2026, 11, 24)),
            &cal,
            3
        ));
        assert!(earnings_blocks(
            d(2026, 11, 24),
            Some(d(2026, 11, 30)),
            &cal,
            3
        ));
        assert!(!earnings_blocks(
            d(2026, 11, 24),
            Some(d(2026, 12, 1)),
            &cal,
            3
        ));
        assert!(!earnings_blocks(d(2026, 11, 24), None, &cal, 3));
        assert!(!earnings_blocks(
            d(2026, 11, 24),
            Some(d(2026, 11, 23)),
            &cal,
            3
        ));
    }

    #[test]
    fn short_calendar_falls_back_to_double_calendar_days() {
        let cal = cal_nov_2026();
        // 30.11 — остання сесія; вікно = 30.11 + 6 днів = 6.12.
        assert!(earnings_blocks(
            d(2026, 11, 30),
            Some(d(2026, 12, 6)),
            &cal,
            3
        ));
        assert!(!earnings_blocks(
            d(2026, 11, 30),
            Some(d(2026, 12, 7)),
            &cal,
            3
        ));
    }

    #[test]
    fn llm_cost() {
        assert!((llm_cost_usd(1_000_000, 200_000, 0.10, 0.50) - 0.20).abs() < 1e-12);
    }
}
