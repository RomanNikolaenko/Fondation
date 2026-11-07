//! Події рушія, що транслюються в UI (WebSocket `/events`).

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Запис журналу (`events` у SQLite) у вигляді для UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct JournalEventDto {
    #[ts(type = "number")]
    pub id: i64,
    pub at: DateTime<Utc>,
    pub kind: String,
    pub symbol: Option<String>,
    pub intent_id: Option<String>,
    pub message: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEventDto {
    Journal {
        event: JournalEventDto,
    },
    IntentsPending {
        session: NaiveDate,
        count: u32,
    },
    OrderFilled {
        symbol: String,
        side: String,
        qty: u32,
        price: String,
    },
    StopHit {
        symbol: String,
        price: String,
    },
    AutoPaused {
        reason: String,
    },
    Killed {
        reason: String,
    },
    StepFailed {
        step: String,
        error: String,
    },
    StatusChanged,
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    #[test]
    fn engine_events_are_internally_tagged_snake_case() {
        let e = EngineEventDto::IntentsPending {
            session: NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
            count: 2,
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({"type": "intents_pending", "session": "2026-11-05", "count": 2})
        );
        assert_eq!(
            serde_json::to_value(EngineEventDto::StatusChanged).unwrap(),
            json!({"type": "status_changed"})
        );
        assert_eq!(
            serde_json::to_value(EngineEventDto::AutoPaused { reason: "x".into() }).unwrap(),
            json!({"type": "auto_paused", "reason": "x"})
        );
    }

    #[test]
    fn journal_event_roundtrip() {
        let e = EngineEventDto::Journal {
            event: JournalEventDto {
                id: 7,
                at: Utc.with_ymd_and_hms(2026, 11, 4, 21, 30, 0).unwrap(),
                kind: "scan".into(),
                symbol: Some("AAPL".into()),
                intent_id: None,
                message: "Сканер".into(),
                data: json!({"passed": 2}),
            },
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "journal");
        assert_eq!(v["event"]["id"], 7);
        assert_eq!(v["event"]["kind"], "scan");
        assert_eq!(v["event"]["data"]["passed"], 2);
        assert_eq!(serde_json::from_value::<EngineEventDto>(v).unwrap(), e);
    }
}
