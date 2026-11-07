//! Ліміти збитків і перевірка адекватності ціни.

/// Допуск для порівняння меж у `f64`.
pub(crate) const EPS: f64 = 1e-9;

fn loss_breached(start: Option<f64>, current: Option<f64>, limit: f64) -> bool {
    match (start, current) {
        (Some(s), Some(c)) if s.is_finite() && c.is_finite() && s > 0.0 => {
            (c / s - 1.0) <= -limit + EPS
        }
        _ => false,
    }
}

/// Денний збиток досяг ліміту. Без історії — не блокує.
pub fn daily_loss_breached(prev_close: Option<f64>, last_close: Option<f64>, limit: f64) -> bool {
    loss_breached(prev_close, last_close, limit)
}

/// Тижневий збиток досяг ліміту. Без історії — не блокує.
pub fn weekly_loss_breached(week_start: Option<f64>, current: Option<f64>, limit: f64) -> bool {
    loss_breached(week_start, current, limit)
}

/// `|limit/last - 1| <= pct`.
pub fn price_sane(limit_price: f64, last_price: f64, pct: f64) -> bool {
    if !limit_price.is_finite() || !last_price.is_finite() || last_price <= 0.0 {
        return false;
    }
    (limit_price / last_price - 1.0).abs() <= pct + EPS
}
