//! Помилки брокера, даних і LLM-судді.

use thiserror::Error;

/// Чи варто повторити операцію.
pub trait IsTransient {
    fn is_transient(&self) -> bool;
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum BrokerError {
    #[error("transient broker error: {0}")]
    Transient(String),
    #[error("order rejected ({status}): {message}")]
    Rejected { status: u16, message: String },
    #[error("duplicate client order id")]
    DuplicateClientOrderId,
    #[error("not found")]
    NotFound,
    #[error("unauthorized")]
    Unauthorized,
    #[error("broker error: {0}")]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum DataError {
    #[error("transient data error: {0}")]
    Transient(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("not available: {0}")]
    NotAvailable(String),
    #[error("data error: {0}")]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum JudgeError {
    #[error("transient judge error: {0}")]
    Transient(String),
    #[error("model refused")]
    Refused,
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("judge error: {0}")]
    Other(String),
}

impl IsTransient for BrokerError {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

impl IsTransient for DataError {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

impl IsTransient for JudgeError {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_transient_variants_are_transient() {
        assert!(BrokerError::Transient("x".into()).is_transient());
        assert!(!BrokerError::DuplicateClientOrderId.is_transient());
        assert!(DataError::Transient("x".into()).is_transient());
        assert!(!DataError::Unauthorized.is_transient());
        assert!(JudgeError::Transient("x".into()).is_transient());
        assert!(!JudgeError::Refused.is_transient());
    }
}
