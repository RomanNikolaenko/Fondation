//! Лічильник помилок брокера поспіль (для автопаузи).

use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Default)]
pub struct BrokerHealth {
    streak: AtomicU32,
}

impl BrokerHealth {
    pub fn record_ok(&self) {
        self.streak.store(0, Ordering::SeqCst);
    }

    /// Повертає довжину серії після цієї помилки.
    pub fn record_err(&self) -> u32 {
        let prev = self
            .streak
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                Some(n.saturating_add(1))
            })
            .unwrap_or(u32::MAX);
        prev.saturating_add(1)
    }

    pub fn streak(&self) -> u32 {
        self.streak.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streak_counts_errors_and_resets_on_ok() {
        let h = BrokerHealth::default();
        assert_eq!(h.streak(), 0);
        assert_eq!(h.record_err(), 1);
        assert_eq!(h.record_err(), 2);
        assert_eq!(h.streak(), 2);
        h.record_ok();
        assert_eq!(h.streak(), 0);
        assert_eq!(h.record_err(), 1);
    }
}
