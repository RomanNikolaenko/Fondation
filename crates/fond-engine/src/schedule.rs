//! Чистий планувальник кроків циклу: що саме треба виконати зараз.
//! Враховує догін після перезапуску та час America/New_York (включно з DST).

// Споживачі (цикл рушія) з'являються в наступних задачах.
#![allow(dead_code)]

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use fond_api::config::ScheduleConfig;
use fond_core::calendar::{TradingCalendar, et_to_utc, utc_to_et};
use fond_store::{StepName, StepStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DueStep {
    Reconcile,
    Analysis { date: NaiveDate },
    ApprovalDeadline { session: NaiveDate },
    PreOpenSubmit { session: NaiveDate },
    MissedSubmit { session: NaiveDate },
}

#[derive(Debug, Clone)]
pub struct StepRecord {
    pub status: StepStatus,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ScheduleInput<'a> {
    pub now: DateTime<Utc>,
    pub cal: &'a TradingCalendar,
    pub cfg: &'a ScheduleConfig,
    pub records: &'a HashMap<(NaiveDate, StepName), StepRecord>,
    pub last_reconcile: Option<DateTime<Utc>>,
    pub stale_after: Duration,
}

/// Крок вважається виконаним, якщо запис фінальний або `Running` ще не протух.
fn is_done(i: &ScheduleInput, date: NaiveDate, step: StepName) -> bool {
    match i.records.get(&(date, step)) {
        None => false,
        Some(r) => match r.status {
            StepStatus::Done | StepStatus::Failed | StepStatus::Missed => true,
            StepStatus::Running => i.now - r.started_at <= i.stale_after,
        },
    }
}

fn today_session(i: &ScheduleInput) -> Option<NaiveDate> {
    let today = utc_to_et(i.now).date_naive();
    i.cal.session(today).map(|s| s.date)
}

/// Остання сесія, час аналізу якої вже настав.
pub fn latest_analysis_date(i: &ScheduleInput) -> Option<NaiveDate> {
    let today = utc_to_et(i.now).date_naive();
    let d = i.cal.last_session_on_or_before(today)?;
    if et_to_utc(d, i.cfg.analysis) <= i.now {
        Some(d)
    } else {
        // Дата d мінус один день: шукаємо попередню сесію.
        i.cal.last_session_on_or_before(d.pred_opt()?)
    }
}

fn reconcile_due(i: &ScheduleInput, session: Option<NaiveDate>) -> bool {
    let Some(last) = i.last_reconcile else {
        return true;
    };
    let Some(s) = session.and_then(|d| i.cal.session(d)) else {
        return false;
    };
    let every = Duration::minutes(i64::from(i.cfg.reconcile_every_minutes));
    let in_session = et_to_utc(s.date, s.open) <= i.now && i.now <= et_to_utc(s.date, s.close);
    in_session && i.now - last >= every
}

pub fn due_steps(i: &ScheduleInput) -> Vec<DueStep> {
    let mut out = Vec::new();
    let session = today_session(i);

    if reconcile_due(i, session) {
        out.push(DueStep::Reconcile);
    }
    if let Some(date) = latest_analysis_date(i)
        && !is_done(i, date, StepName::Analysis)
    {
        out.push(DueStep::Analysis { date });
    }
    if let Some(s) = session {
        if i.now >= et_to_utc(s, i.cfg.approval_deadline)
            && !is_done(i, s, StepName::ApprovalDeadline)
        {
            out.push(DueStep::ApprovalDeadline { session: s });
        }
        if !is_done(i, s, StepName::PreOpenSubmit) {
            if i.now >= et_to_utc(s, i.cfg.submit_window_end) {
                out.push(DueStep::MissedSubmit { session: s });
            } else if i.now >= et_to_utc(s, i.cfg.submit_window_start) {
                out.push(DueStep::PreOpenSubmit { session: s });
            }
        }
    }
    out
}

/// Найближчий майбутній невиконаний крок розкладу (для статусу в UI).
pub fn next_step_at(i: &ScheduleInput) -> Option<(StepName, DateTime<Utc>)> {
    let today = utc_to_et(i.now).date_naive();
    let mut sessions = Vec::new();
    sessions.extend(today_session(i));
    sessions.extend(i.cal.next_session_after(today));

    sessions
        .into_iter()
        .flat_map(|s| {
            [
                (StepName::ApprovalDeadline, s, i.cfg.approval_deadline),
                (StepName::PreOpenSubmit, s, i.cfg.submit_window_start),
                (StepName::Analysis, s, i.cfg.analysis),
            ]
        })
        .map(|(step, s, t)| (step, s, et_to_utc(s, t)))
        .filter(|&(step, s, at)| at > i.now && !is_done(i, s, step))
        .map(|(step, _, at)| (step, at))
        .min_by_key(|&(_, at)| at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone};
    use fond_core::traits::Session;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    fn utc(y: i32, mo: u32, day: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, day, h, mi, 0).unwrap()
    }
    fn hm(h: u32, m: u32) -> chrono::NaiveTime {
        chrono::NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    /// Будні березня, жовтня й листопада 2026 (09:30–16:00); 26.11 вихідний, 27.11 до 13:00.
    fn cal() -> TradingCalendar {
        let mut sessions = Vec::new();
        for month in [3u32, 10, 11] {
            for day in 1..=31 {
                let Some(date) = NaiveDate::from_ymd_opt(2026, month, day) else {
                    continue;
                };
                if date.weekday().num_days_from_monday() >= 5 {
                    continue;
                }
                if month == 11 && day == 26 {
                    continue;
                }
                let close = if month == 11 && day == 27 {
                    hm(13, 0)
                } else {
                    hm(16, 0)
                };
                sessions.push(Session {
                    date,
                    open: hm(9, 30),
                    close,
                });
            }
        }
        TradingCalendar::new(sessions)
    }

    type Records = HashMap<(NaiveDate, StepName), StepRecord>;

    fn rec(status: StepStatus, started_at: DateTime<Utc>) -> StepRecord {
        StepRecord { status, started_at }
    }

    fn done(r: &mut Records, date: NaiveDate, step: StepName) {
        r.insert((date, step), rec(StepStatus::Done, utc(2026, 1, 1, 0, 0)));
    }

    fn run(
        now: DateTime<Utc>,
        records: &Records,
        last_reconcile: Option<DateTime<Utc>>,
    ) -> Vec<DueStep> {
        let cal = cal();
        let cfg = ScheduleConfig::default();
        due_steps(&ScheduleInput {
            now,
            cal: &cal,
            cfg: &cfg,
            records,
            last_reconcile,
            stale_after: Duration::minutes(15),
        })
    }

    /// Підтвердження й відправку сесії `s` виконано (аналіз — ні).
    fn session_done(r: &mut Records, s: NaiveDate) {
        done(r, s, StepName::ApprovalDeadline);
        done(r, s, StepName::PreOpenSubmit);
    }

    #[test]
    fn analysis_due_after_1630_et() {
        let mut r = Records::new();
        done(&mut r, d(2026, 11, 3), StepName::Analysis);
        session_done(&mut r, d(2026, 11, 4));
        let now = utc(2026, 11, 4, 21, 31);
        assert_eq!(
            run(now, &r, Some(now)),
            vec![DueStep::Analysis {
                date: d(2026, 11, 4)
            }]
        );
        let now = utc(2026, 11, 4, 21, 29);
        assert_eq!(run(now, &r, Some(now)), vec![]);
    }

    #[test]
    fn analysis_not_repeated_when_done() {
        let mut r = Records::new();
        session_done(&mut r, d(2026, 11, 4));
        let now = utc(2026, 11, 4, 23, 0);
        for status in [StepStatus::Done, StepStatus::Failed, StepStatus::Missed] {
            r.insert(
                (d(2026, 11, 4), StepName::Analysis),
                rec(status, utc(2026, 11, 4, 21, 31)),
            );
            assert_eq!(run(now, &r, Some(now)), vec![]);
        }
    }

    #[test]
    fn stale_running_step_is_due_again() {
        let mut r = Records::new();
        session_done(&mut r, d(2026, 11, 4));
        let now = utc(2026, 11, 4, 21, 50);
        r.insert(
            (d(2026, 11, 4), StepName::Analysis),
            rec(StepStatus::Running, now - Duration::minutes(20)),
        );
        assert_eq!(
            run(now, &r, Some(now)),
            vec![DueStep::Analysis {
                date: d(2026, 11, 4)
            }]
        );
        r.insert(
            (d(2026, 11, 4), StepName::Analysis),
            rec(StepStatus::Running, now - Duration::minutes(5)),
        );
        assert_eq!(run(now, &r, Some(now)), vec![]);
    }

    #[test]
    fn catch_up_runs_only_latest_session() {
        let r = Records::new();
        let now = utc(2026, 11, 9, 13, 0);
        assert_eq!(
            run(now, &r, Some(now)),
            vec![DueStep::Analysis {
                date: d(2026, 11, 6)
            }]
        );
    }

    #[test]
    fn weekend_and_holiday_have_no_session_steps() {
        let mut r = Records::new();
        done(&mut r, d(2026, 11, 6), StepName::Analysis);
        done(&mut r, d(2026, 11, 25), StepName::Analysis);
        assert_eq!(
            run(utc(2026, 11, 7, 15, 0), &r, None),
            vec![DueStep::Reconcile]
        );
        assert_eq!(
            run(utc(2026, 11, 26, 14, 1), &r, None),
            vec![DueStep::Reconcile]
        );
        let now = utc(2026, 11, 7, 15, 0);
        assert_eq!(run(now, &r, Some(now)), vec![]);
    }

    #[test]
    fn approval_deadline_then_submit_window() {
        let mut r = Records::new();
        done(&mut r, d(2026, 11, 4), StepName::Analysis);
        let s = d(2026, 11, 5);
        let now = utc(2026, 11, 5, 13, 56);
        assert_eq!(
            run(now, &r, Some(now)),
            vec![DueStep::ApprovalDeadline { session: s }]
        );

        done(&mut r, s, StepName::ApprovalDeadline);
        let now = utc(2026, 11, 5, 14, 1);
        assert_eq!(
            run(now, &r, Some(now)),
            vec![DueStep::PreOpenSubmit { session: s }]
        );

        let now = utc(2026, 11, 5, 14, 26);
        assert_eq!(
            run(now, &r, Some(now)),
            vec![DueStep::MissedSubmit { session: s }]
        );
    }

    #[test]
    fn reconcile_on_start_and_hourly_in_session() {
        let mut r = Records::new();
        done(&mut r, d(2026, 11, 4), StepName::Analysis);
        done(&mut r, d(2026, 11, 5), StepName::Analysis);
        session_done(&mut r, d(2026, 11, 5));
        assert_eq!(
            run(utc(2026, 11, 5, 15, 0), &r, None),
            vec![DueStep::Reconcile]
        );
        let last = Some(utc(2026, 11, 5, 14, 30));
        assert_eq!(run(utc(2026, 11, 5, 15, 0), &r, last), vec![]);
        assert_eq!(
            run(utc(2026, 11, 5, 15, 31), &r, last),
            vec![DueStep::Reconcile]
        );
        assert_eq!(run(utc(2026, 11, 5, 22, 0), &r, last), vec![]);
    }

    #[test]
    fn early_close_limits_reconcile() {
        let mut r = Records::new();
        done(&mut r, d(2026, 11, 25), StepName::Analysis);
        session_done(&mut r, d(2026, 11, 27));
        let last = Some(utc(2026, 11, 27, 14, 0));
        // 13:30 ET, після скороченого закриття о 13:00.
        assert_eq!(run(utc(2026, 11, 27, 18, 30), &r, last), vec![]);
        // 12:30 ET, ще в сесії.
        assert_eq!(
            run(utc(2026, 11, 27, 17, 30), &r, last),
            vec![DueStep::Reconcile]
        );
    }

    #[test]
    fn dst_transition_weeks_use_new_york_time() {
        // 2026-03-09: у США вже EDT, у Європі ще ні; 2026-10-28: у Європі вже зимовий час.
        for (prev, day) in [
            (d(2026, 3, 6), d(2026, 3, 9)),
            (d(2026, 10, 27), d(2026, 10, 28)),
        ] {
            let mut r = Records::new();
            done(&mut r, prev, StepName::Analysis);
            session_done(&mut r, day);
            let (y, m, dd) = (day.year(), day.month(), day.day());
            let now = utc(y, m, dd, 20, 31);
            assert_eq!(
                run(now, &r, Some(now)),
                vec![DueStep::Analysis { date: day }]
            );
            let now = utc(y, m, dd, 20, 29);
            assert_eq!(run(now, &r, Some(now)), vec![]);
        }
    }

    #[test]
    fn next_step_at_picks_earliest_undone_future_step() {
        let cal = cal();
        let cfg = ScheduleConfig::default();
        let mut r = Records::new();
        session_done(&mut r, d(2026, 11, 4));
        let now = utc(2026, 11, 4, 15, 0);
        let go = |r: &Records| {
            next_step_at(&ScheduleInput {
                now,
                cal: &cal,
                cfg: &cfg,
                records: r,
                last_reconcile: Some(now),
                stale_after: Duration::minutes(15),
            })
        };
        assert_eq!(go(&r), Some((StepName::Analysis, utc(2026, 11, 4, 21, 30))));
        done(&mut r, d(2026, 11, 4), StepName::Analysis);
        assert_eq!(
            go(&r),
            Some((StepName::ApprovalDeadline, utc(2026, 11, 5, 13, 55)))
        );
        done(&mut r, d(2026, 11, 5), StepName::ApprovalDeadline);
        assert_eq!(
            go(&r),
            Some((StepName::PreOpenSubmit, utc(2026, 11, 5, 14, 0)))
        );
    }

    #[test]
    fn latest_analysis_date_steps_back_before_analysis_time() {
        let r = Records::new();
        let cal = cal();
        let cfg = ScheduleConfig::default();
        let at = |now| {
            latest_analysis_date(&ScheduleInput {
                now,
                cal: &cal,
                cfg: &cfg,
                records: &r,
                last_reconcile: None,
                stale_after: Duration::minutes(15),
            })
        };
        assert_eq!(at(utc(2026, 11, 4, 21, 31)), Some(d(2026, 11, 4)));
        assert_eq!(at(utc(2026, 11, 4, 21, 29)), Some(d(2026, 11, 3)));
        assert_eq!(at(utc(2026, 11, 9, 13, 0)), Some(d(2026, 11, 6)));
    }
}
