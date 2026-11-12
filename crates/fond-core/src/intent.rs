//! Наміри (intents) та їхній життєвий цикл.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::orders::Side;
use crate::traits::NewsVerdict;
use crate::types::{StrategyId, Symbol};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum IntentStatus {
    PendingApproval,
    Approved,
    Rejected,
    Expired,
    Submitted,
    Filled,
    Cancelled,
    Failed,
}

impl IntentStatus {
    pub fn can_transition_to(self, next: IntentStatus) -> bool {
        use IntentStatus::*;
        matches!(
            (self, next),
            (PendingApproval, Approved | Rejected | Expired | Cancelled)
                | (Approved, Submitted | Expired | Failed | Cancelled)
                | (Submitted, Filled | Cancelled | Failed)
        )
    }

    pub fn is_terminal(self) -> bool {
        use IntentStatus::*;
        !matches!(self, PendingApproval | Approved | Submitted)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum IntentKind {
    Entry {
        qty: u32,
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        limit_price: Decimal,
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        stop_price: Decimal,
        #[cfg_attr(feature = "ts", ts(type = "string | null"))]
        take_profit: Option<Decimal>,
        risk_usd: f64,
        news: Option<NewsVerdict>,
    },
    Exit {
        qty: u32,
        reason: String,
    },
    MoveStop {
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        new_stop: Decimal,
        stop_order_id: String,
    },
    /// Ребаланс портфеля: купівля/продаж до цільової ваги. Лімітний ордер на
    /// день без стоп-леги; виконання відстежує звірка.
    Rebalance {
        side: Side,
        qty: u32,
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        limit_price: Decimal,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Intent {
    pub id: String,
    pub session_date: NaiveDate,
    pub signal_date: NaiveDate,
    pub symbol: Symbol,
    pub strategy: StrategyId,
    pub kind: IntentKind,
    pub status: IntentStatus,
    pub explanation: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub broker_order_id: Option<String>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn intent_status_transitions() {
        use IntentStatus::*;
        assert!(PendingApproval.can_transition_to(Approved));
        assert!(PendingApproval.can_transition_to(Cancelled));
        assert!(Approved.can_transition_to(Submitted));
        assert!(Submitted.can_transition_to(Filled));
        assert!(!Filled.can_transition_to(Approved));
        assert!(!Rejected.can_transition_to(Submitted));
        assert!(!Expired.can_transition_to(Approved));
        assert!(Filled.is_terminal() && !Submitted.is_terminal());
    }

    #[test]
    fn status_and_kind_serde() {
        assert_eq!(
            serde_json::to_string(&IntentStatus::PendingApproval).unwrap(),
            "\"pending_approval\""
        );
        let k = IntentKind::MoveStop {
            new_stop: dec!(9.50),
            stop_order_id: "ord-1".into(),
        };
        let v = serde_json::to_value(&k).unwrap();
        assert_eq!(v["type"], "move_stop");
        assert_eq!(v["new_stop"], "9.50");
        assert_eq!(serde_json::from_value::<IntentKind>(v).unwrap(), k);
    }
}
