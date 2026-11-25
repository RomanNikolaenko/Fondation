//! Дедлайн схвалення і пропущене вікно відправки: незавершені наміри сесії стають `Expired`.

use chrono::NaiveDate;
use fond_core::intent::IntentStatus;
use fond_store::{EventKind, StoreError};
use serde_json::json;

use crate::cycle::{CycleError, journal};
use crate::deps::Deps;

/// Дедлайн схвалення: `PendingApproval` із `session_date ≤ session` -> `Expired`.
/// Наміри наступних сесій не чіпаються.
pub async fn expire_pending(d: &Deps, session: NaiveDate) -> Result<usize, CycleError> {
    expire(
        d,
        session,
        &[IntentStatus::PendingApproval],
        "Не схвалено до дедлайну",
        "Дедлайн схвалення минув",
    )
    .await
}

/// Вікно відправки минуло: `PendingApproval` і `Approved` із `session_date ≤ session` ->
/// `Expired`. Наміри наступних сесій не чіпаються.
pub async fn mark_missed(d: &Deps, session: NaiveDate) -> Result<usize, CycleError> {
    expire(
        d,
        session,
        &[IntentStatus::PendingApproval, IntentStatus::Approved],
        "Вікно відправки минуло",
        "Вікно відправки минуло",
    )
    .await
}

async fn expire(
    d: &Deps,
    session: NaiveDate,
    statuses: &[IntentStatus],
    note: &str,
    title: &str,
) -> Result<usize, CycleError> {
    let now = d.clock.now();
    let mut expired = Vec::new();
    for i in d.store.intents_with_status(statuses).await? {
        if i.session_date > session {
            continue;
        }
        match d
            .store
            .update_intent(&i.id, IntentStatus::Expired, now, None, Some(note))
            .await
        {
            Ok(_) => expired.push(i.id),
            // Статус щойно змінився (наприклад, намір уже відправлено).
            Err(StoreError::InvalidTransition { .. }) => {}
            Err(e) => return Err(e.into()),
        }
    }
    journal(
        d,
        EventKind::Intent,
        None,
        None,
        format!(
            "{title} (сесія {session}): прострочено намірів {}",
            expired.len()
        ),
        json!({ "session": session, "expired": expired }),
    )
    .await?;
    Ok(expired.len())
}
