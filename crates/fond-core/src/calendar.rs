//! Торговий календар і перетворення часових зон (розклад рушія — America/New_York).

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};

use crate::traits::Session;

pub const NEW_YORK: chrono_tz::Tz = chrono_tz::America::New_York;
pub const KYIV: chrono_tz::Tz = chrono_tz::Europe::Kyiv;

/// Календар торгових сесій, відсортований за датою, без дублікатів.
#[derive(Debug, Clone, Default)]
pub struct TradingCalendar {
    sessions: Vec<Session>,
}

impl TradingCalendar {
    pub fn new(mut sessions: Vec<Session>) -> Self {
        sessions.sort_by_key(|s| s.date);
        sessions.dedup_by_key(|s| s.date);
        Self { sessions }
    }

    pub fn session(&self, d: NaiveDate) -> Option<&Session> {
        self.sessions
            .binary_search_by_key(&d, |s| s.date)
            .ok()
            .map(|i| &self.sessions[i])
    }

    /// Перша сесія строго після `d`.
    pub fn next_session_after(&self, d: NaiveDate) -> Option<NaiveDate> {
        let i = self.sessions.partition_point(|s| s.date <= d);
        self.sessions.get(i).map(|s| s.date)
    }

    /// Остання сесія на дату `d` або раніше.
    pub fn last_session_on_or_before(&self, d: NaiveDate) -> Option<NaiveDate> {
        let i = self.sessions.partition_point(|s| s.date <= d);
        i.checked_sub(1).map(|i| self.sessions[i].date)
    }

    /// `n`-та сесія після `d` (`n = 1` — те саме, що `next_session_after`); `n = 0` — `None`.
    pub fn nth_session_after(&self, d: NaiveDate, n: usize) -> Option<NaiveDate> {
        if n == 0 {
            return None;
        }
        let i = self.sessions.partition_point(|s| s.date <= d);
        self.sessions.get(i + n - 1).map(|s| s.date)
    }

    /// Дати сесій у `[from, to]` включно.
    pub fn sessions_between(&self, from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
        self.sessions
            .iter()
            .map(|s| s.date)
            .filter(|d| *d >= from && *d <= to)
            .collect()
    }
}

/// Локальний час Нью-Йорка -> UTC. Не панікує: неоднозначний час бере раніший
/// варіант, неіснуючий (пропуск при переході на літній час) зсувається на годину вперед.
pub fn et_to_utc(date: NaiveDate, time: NaiveTime) -> DateTime<Utc> {
    let naive = date.and_time(time);
    let local = NEW_YORK
        .from_local_datetime(&naive)
        .earliest()
        .unwrap_or_else(|| {
            NEW_YORK
                .from_local_datetime(&(naive + Duration::hours(1)))
                .earliest()
                .unwrap_or_else(|| NEW_YORK.from_utc_datetime(&naive))
        });
    local.with_timezone(&Utc)
}

pub fn utc_to_et(t: DateTime<Utc>) -> DateTime<chrono_tz::Tz> {
    t.with_timezone(&NEW_YORK)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::Datelike;

    pub(crate) fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }
    fn utc(y: i32, mo: u32, day: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, day, h, mi, 0).unwrap()
    }

    /// Будні листопада 2026 (09:30–16:00) без 26-го; 27-го закриття о 13:00.
    pub(crate) fn cal_nov_2026() -> TradingCalendar {
        let sessions = (1..=30)
            .map(|day| d(2026, 11, day))
            .filter(|x| x.weekday().num_days_from_monday() < 5 && x.day() != 26)
            .map(|date| Session {
                date,
                open: t(9, 30),
                close: if date.day() == 27 { t(13, 0) } else { t(16, 0) },
            })
            .collect();
        TradingCalendar::new(sessions)
    }

    #[test]
    fn analysis_time_is_new_york_based() {
        assert_eq!(
            et_to_utc(d(2026, 11, 4), t(16, 30)),
            utc(2026, 11, 4, 21, 30)
        );
        assert_eq!(
            et_to_utc(d(2026, 10, 28), t(16, 30)),
            utc(2026, 10, 28, 20, 30)
        );
    }

    #[test]
    fn et_to_utc_handles_dst_gap_and_overlap_without_panic() {
        // 2026-03-08 02:30 ET не існує -> 03:30 EDT = 07:30 UTC.
        assert_eq!(et_to_utc(d(2026, 3, 8), t(2, 30)), utc(2026, 3, 8, 7, 30));
        // 2026-11-01 01:30 ET неоднозначний -> раніший (EDT) = 05:30 UTC.
        assert_eq!(et_to_utc(d(2026, 11, 1), t(1, 30)), utc(2026, 11, 1, 5, 30));
    }

    #[test]
    fn utc_to_et_roundtrip() {
        let e = utc_to_et(utc(2026, 11, 4, 21, 30));
        assert_eq!(e.naive_local(), d(2026, 11, 4).and_time(t(16, 30)));
    }

    #[test]
    fn next_session_skips_weekend_and_thanksgiving() {
        let cal = cal_nov_2026();
        assert_eq!(cal.next_session_after(d(2026, 11, 6)), Some(d(2026, 11, 9)));
        assert_eq!(
            cal.next_session_after(d(2026, 11, 25)),
            Some(d(2026, 11, 27))
        );
        assert_eq!(
            cal.nth_session_after(d(2026, 11, 24), 3),
            Some(d(2026, 11, 30))
        );
        assert_eq!(
            cal.nth_session_after(d(2026, 11, 24), 1),
            cal.next_session_after(d(2026, 11, 24))
        );
        assert_eq!(cal.next_session_after(d(2026, 11, 30)), None);
        assert_eq!(cal.nth_session_after(d(2026, 11, 24), 0), None);
    }

    #[test]
    fn last_session_between_and_lookup() {
        let cal = cal_nov_2026();
        assert_eq!(
            cal.last_session_on_or_before(d(2026, 11, 26)),
            Some(d(2026, 11, 25))
        );
        assert_eq!(
            cal.last_session_on_or_before(d(2026, 11, 28)),
            Some(d(2026, 11, 27))
        );
        assert_eq!(cal.last_session_on_or_before(d(2026, 10, 31)), None);
        assert_eq!(
            cal.sessions_between(d(2026, 11, 24), d(2026, 11, 30)),
            vec![
                d(2026, 11, 24),
                d(2026, 11, 25),
                d(2026, 11, 27),
                d(2026, 11, 30)
            ]
        );
        assert_eq!(cal.session(d(2026, 11, 27)).unwrap().close, t(13, 0));
        assert!(cal.session(d(2026, 11, 26)).is_none());
    }

    #[test]
    fn new_sorts_and_dedups() {
        let s = |date| Session {
            date,
            open: t(9, 30),
            close: t(16, 0),
        };
        let cal = TradingCalendar::new(vec![
            s(d(2026, 11, 3)),
            s(d(2026, 11, 2)),
            s(d(2026, 11, 3)),
        ]);
        assert_eq!(
            cal.sessions_between(d(2026, 11, 1), d(2026, 11, 30)).len(),
            2
        );
        assert_eq!(cal.next_session_after(d(2026, 11, 1)), Some(d(2026, 11, 2)));
    }
}
