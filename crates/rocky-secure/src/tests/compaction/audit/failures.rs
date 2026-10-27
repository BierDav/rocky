use super::*;
use crate::replica::checkpoint::Stage;

#[tokio::test]
async fn later_scope_corruption_is_not_hidden_by_an_earlier_replay() {
    let f = AuditFixture::new(false).await;
    let scopes = [f.lineage.scope("items"), f.lineage.scope("tags")];
    trace_replay(&f.replica).await;
    let original = snapshot(&f.replica).await;
    let saved = crate::storage::collect_rows(
        f.replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .query(
                "SELECT * FROM secure_row_records WHERE scope=?",
                [encode(&scopes[1]).unwrap()],
            )
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    let keys = rows(
        &f.replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT * FROM secure_row_keys",
    )
    .await
    .unwrap();
    let candidates = rows(
        &f.replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT * FROM secure_seal_candidates",
    )
    .await
    .unwrap();
    f.replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .conn
        .execute(
            "DELETE FROM secure_row_records WHERE scope=?",
            [encode(&scopes[1]).unwrap()],
        )
        .await
        .unwrap();
    let corrupt = snapshot(&f.replica).await;
    assert!(
        f.replica
            .audit_scopes(&scopes)
            .await
            .unwrap_err()
            .to_string()
            .contains("projection differs")
    );
    assert_eq!(snapshot(&f.replica).await, corrupt);
    assert_clean(&f.replica).await;
    assert!(
        !rows(
            &f.replica
                .inner
                .lock(crate::replica::Class::Background)
                .await
                .conn,
            "SELECT 1 FROM audit_replay_seen"
        )
        .await
        .unwrap()
        .is_empty()
    );
    for row in saved {
        f.replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute(
                "INSERT INTO secure_row_records VALUES(?,?,?)",
                bindings(row),
            )
            .await
            .unwrap();
    }
    let inner = f
        .replica
        .inner
        .lock(crate::replica::Class::Background)
        .await;
    inner
        .conn
        .execute("DELETE FROM secure_row_keys", ())
        .await
        .unwrap();
    for key in keys {
        inner
            .conn
            .execute(
                format!(
                    "INSERT INTO secure_row_keys VALUES({})",
                    vec!["?"; key.len()].join(",")
                ),
                bindings(key),
            )
            .await
            .unwrap();
    }
    inner
        .conn
        .execute("DELETE FROM secure_seal_candidates", ())
        .await
        .unwrap();
    for candidate in candidates {
        inner
            .conn
            .execute(
                "INSERT INTO secure_seal_candidates VALUES(?,?)",
                bindings(candidate),
            )
            .await
            .unwrap();
    }
    drop(inner);
    assert_eq!(snapshot(&f.replica).await, original);
    assert_eq!(
        f.replica.audit_scopes(&scopes).await.unwrap(),
        singles(&f.replica, &scopes).await
    );
}

#[tokio::test]
async fn a_later_committed_replay_failure_cleans_scratch_and_retries_without_evidence_writes() {
    let f = AuditFixture::new(false).await;
    let scopes = [f.lineage.scope("items"), f.lineage.scope("tags")];
    let expected = singles(&f.replica, &scopes).await;
    let before = snapshot(&f.replica).await;
    trace_replay(&f.replica).await;
    f.replica.inject_stage_fault(Stage::AuditReplay, 1).await;
    assert!(matches!(
        f.replica.audit_scopes(&scopes).await,
        Err(Error::InjectedFailure)
    ));
    assert_clean(&f.replica).await;
    assert_eq!(snapshot(&f.replica).await, before);
    assert_eq!(
        rows(
            &f.replica
                .inner
                .lock(crate::replica::Class::Background)
                .await
                .conn,
            "SELECT count(DISTINCT scope) FROM audit_replay_seen"
        )
        .await
        .unwrap(),
        vec![vec![SqlValue::Integer(2)]]
    );
    assert_eq!(f.replica.audit_scopes(&scopes).await.unwrap(), expected);
    let AuditFixture {
        replica,
        lineage,
        directory,
        node,
    } = f;
    drop(replica);
    let reopened = lineage
        .reopen(&directory.path().join("audit.db"), &node, 3, 3)
        .await
        .unwrap();
    assert_eq!(reopened.audit_scopes(&scopes).await.unwrap(), expected);
}

#[tokio::test]
async fn the_final_global_pass_refuses_privileged_corruption_after_the_last_cleanup() {
    let f = AuditFixture::new(false).await;
    let scopes = [f.lineage.scope("items"), f.lineage.scope("tags")];
    let before = snapshot(&f.replica).await;
    let inner = f
        .replica
        .inner
        .lock(crate::replica::Class::Background)
        .await;
    let last = crate::storage::collect_rows(
        inner
            .conn
            .query(
                "SELECT id FROM secure_scope_state WHERE scope=?",
                [encode(&scopes[1]).unwrap()],
            )
            .await
            .unwrap(),
    )
    .await
    .unwrap()[0][0]
        .clone();
    let SqlValue::Integer(last) = last else {
        panic!("scope id")
    };
    // Privileged injection, not a supported concurrent writer or audit rollback promise.
    inner.conn.execute(format!("CREATE TEMP TRIGGER audit_late_corruption AFTER DELETE ON secure_checkpoint_scratch WHEN OLD.scope={last} BEGIN UPDATE secure_retained SET records=records+1; END"),()).await.unwrap();
    let passes = inner.audit_global_passes;
    drop(inner);
    let error = f.replica.audit_scopes(&scopes).await.unwrap_err();
    assert!(
        error.to_string().contains("retained history counters"),
        "{error}"
    );
    assert_eq!(
        f.replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .audit_global_passes
            - passes,
        2
    );
    assert_clean(&f.replica).await;
    let inner = f
        .replica
        .inner
        .lock(crate::replica::Class::Background)
        .await;
    inner
        .conn
        .execute("DROP TRIGGER audit_late_corruption", ())
        .await
        .unwrap();
    inner
        .conn
        .execute(
            "UPDATE secure_retained SET records=(SELECT count(*) FROM secure_operations)",
            (),
        )
        .await
        .unwrap();
    drop(inner);
    assert_eq!(snapshot(&f.replica).await, before);
    f.replica.audit_scopes(&scopes).await.unwrap();
}

#[tokio::test]
async fn cleanup_errors_never_report_success_or_replace_the_primary_replay_error() {
    for primary in [false, true] {
        let f = AuditFixture::new(false).await;
        let scope = f.lineage.scope("items");
        let before = snapshot(&f.replica).await;
        let inner = f
            .replica
            .inner
            .lock(crate::replica::Class::Background)
            .await;
        inner.conn.execute("CREATE TEMP TRIGGER audit_cleanup_failure BEFORE DELETE ON secure_checkpoint_scratch BEGIN SELECT RAISE(ABORT,'audit cleanup failed'); END",()).await.unwrap();
        drop(inner);
        if primary {
            f.replica.inject_stage_fault(Stage::AuditReplay, 0).await;
        }
        let error = f.replica.audit_scopes(&[scope]).await.unwrap_err();
        if primary {
            assert!(matches!(error, Error::InjectedFailure), "{error}");
        } else {
            assert!(
                error.to_string().contains("audit cleanup failed"),
                "{error}"
            );
        }
        assert_eq!(f.replica.audit_scratch_flags().await, (false, false));
        assert_eq!(snapshot(&f.replica).await, before);
        f.replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn
            .execute("DROP TRIGGER audit_cleanup_failure", ())
            .await
            .unwrap();
        f.replica.audit_scopes(&[scope]).await.unwrap();
        assert_clean(&f.replica).await;
    }
}

#[tokio::test]
async fn scratch_transactions_keep_the_existing_busy_retry_boundary() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let f = AuditFixture::new(false).await;
    let scopes = [f.lineage.scope("items"), f.lineage.scope("tags")];
    let expected = singles(&f.replica, &scopes).await;
    let blocker = rusqlite::Connection::open(f.directory.path().join("audit.db")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let held = Mutex::new(Some(blocker));
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = attempts.clone();
    f.replica
        .set_busy_hook(Some(Box::new(move |attempt| {
            seen.fetch_add(1, Ordering::SeqCst);
            if attempt == 1
                && let Some(blocker) = held.lock().unwrap().take()
            {
                blocker.execute_batch("COMMIT").unwrap();
            }
        })))
        .await;
    assert_eq!(f.replica.audit_scopes(&scopes).await.unwrap(), expected);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_clean(&f.replica).await;
}
