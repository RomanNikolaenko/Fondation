//! Дедлайн схвалення і пропущене вікно відправки.

use super::*;

#[tokio::test]
async fn expire_pending_at_deadline() {
    let (dp, _h) = setup().await;
    let entry = || entry_kind(10, dec!(101.00), dec!(95.50), None);
    let pending = |s: &str| {
        intent_with(
            s,
            IntentTag::Entry,
            entry(),
            IntentStatus::PendingApproval,
            None,
        )
    };
    let today = pending("AAPL");
    let earlier = on(d(2026, 11, 4), pending("MSFT"));
    // Наміри наступної сесії (щойно створені аналізом) не чіпаються.
    let next = on(d(2026, 11, 6), pending("SPY"));
    let approved = intent_with(
        "NVDA",
        IntentTag::Entry,
        entry(),
        IntentStatus::Approved,
        None,
    );
    insert(&dp, &[&today, &earlier, &next, &approved]).await;

    let n = expire_pending(&dp, session()).await.unwrap();

    assert_eq!(n, 2);
    for i in [&today, &earlier] {
        let stored = intent(&dp, &i.id).await;
        assert_eq!(stored.status, IntentStatus::Expired, "{}", i.id);
        assert_eq!(stored.error.as_deref(), Some("Не схвалено до дедлайну"));
    }
    assert_eq!(
        intent(&dp, &next.id).await.status,
        IntentStatus::PendingApproval
    );
    assert_eq!(
        intent(&dp, &approved.id).await.status,
        IntentStatus::Approved
    );
    assert_eq!(
        journal_messages(&dp).await,
        vec!["Дедлайн схвалення минув (сесія 2026-11-05): прострочено намірів 2".to_string()]
    );
}

#[tokio::test]
async fn mark_missed_expires_approved_and_pending() {
    let (dp, _h) = setup().await;
    let entry = || entry_kind(10, dec!(101.00), dec!(95.50), None);
    let pending = intent_with(
        "AAPL",
        IntentTag::Entry,
        entry(),
        IntentStatus::PendingApproval,
        None,
    );
    let approved = approved_entry("MSFT", None);
    let old_exit = on(d(2026, 11, 4), approved_exit("SPY", 5));
    let next = on(d(2026, 11, 6), approved_entry("NVDA", None));
    let submitted = intent_with(
        "XOM",
        IntentTag::Entry,
        entry(),
        IntentStatus::Submitted,
        Some("ord-9"),
    );
    insert(&dp, &[&pending, &approved, &old_exit, &next, &submitted]).await;

    let n = mark_missed(&dp, session()).await.unwrap();

    assert_eq!(n, 3);
    for i in [&pending, &approved, &old_exit] {
        let stored = intent(&dp, &i.id).await;
        assert_eq!(stored.status, IntentStatus::Expired, "{}", i.id);
        assert_eq!(stored.error.as_deref(), Some("Вікно відправки минуло"));
    }
    assert_eq!(intent(&dp, &next.id).await.status, IntentStatus::Approved);
    assert_eq!(
        intent(&dp, &submitted.id).await.status,
        IntentStatus::Submitted
    );
    assert_eq!(
        journal_messages(&dp).await,
        vec!["Вікно відправки минуло (сесія 2026-11-05): прострочено намірів 3".to_string()]
    );
}

// ---------------------------------------------------------------- ручне закриття
