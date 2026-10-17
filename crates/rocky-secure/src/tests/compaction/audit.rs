//! Guarded multi-scope audits use the production single-scope checks.
use super::*;
use crate::storage::{bindings, rows};
mod failures;
mod fixture;
mod global;
pub(in crate::tests) use fixture::snapshot;
use fixture::*;

#[tokio::test]
async fn reports_match_single_audits_with_bases_suffixes_and_no_base() {
    for based in [false, true] {
        let f = AuditFixture::new(based).await;
        let scopes = [f.lineage.scope("tags"), f.lineage.scope("items")];
        let expected = singles(&f.replica, &scopes).await;
        for (scope, report) in &expected {
            let has_base = based && *scope == f.lineage.scope("items");
            assert_eq!((report.operations, report.rows), (2, 2));
            assert_eq!(report.replayed, if has_base { 1 } else { 2 });
            assert_eq!(report.checkpoint.is_some(), has_base);
        }
        let before = snapshot(&f.replica).await;
        let passes = f.replica.inner.lock().await.audit_global_passes;
        assert_eq!(f.replica.audit_scopes(&scopes).await.unwrap(), expected);
        assert_eq!(f.replica.inner.lock().await.audit_global_passes - passes, 2);
        assert_clean(&f.replica).await;
        assert_eq!(snapshot(&f.replica).await, before);
        for (scope, report) in &expected {
            if report.checkpoint.is_some() {
                assert_eq!(
                    report.checkpoint,
                    Some(f.replica.checkpoint(*scope).await.unwrap().id().unwrap())
                );
            }
        }
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
        assert_eq!(singles(&reopened, &scopes).await, expected);
        assert_eq!(reopened.audit_scopes(&scopes).await.unwrap(), expected);
    }
}

#[tokio::test]
async fn keyless_unknown_slots_and_invalid_inputs_are_honest() {
    let f = AuditFixture::new(false).await;
    let items = f.lineage.scope("items");
    let tags = f.lineage.scope("tags");
    let unknown = ResourceScope {
        table: [199; 32],
        ..items
    };
    f.replica
        .inner
        .lock()
        .await
        .config
        .keys
        .retain(|key| key.scope != tags);
    let before = snapshot(&f.replica).await;
    let scopes = [tags, unknown, items];
    let expected = singles(&f.replica, &scopes).await;
    assert_eq!(
        (
            expected[0].1.operations,
            expected[0].1.rows,
            expected[0].1.checkpoint,
            expected[0].1.replayed
        ),
        (2, 0, None, 0)
    );
    assert_eq!(
        (
            expected[1].1.operations,
            expected[1].1.rows,
            expected[1].1.checkpoint,
            expected[1].1.replayed
        ),
        (0, 0, None, 0)
    );
    assert_eq!(f.replica.audit_scopes(&scopes).await.unwrap(), expected);
    let passes = f.replica.inner.lock().await.audit_global_passes;
    for invalid in [vec![], vec![items, items]] {
        assert!(matches!(
            f.replica.audit_scopes(&invalid).await,
            Err(Error::Rejected(_))
        ));
    }
    assert_eq!(f.replica.inner.lock().await.audit_global_passes, passes);
    assert_eq!(snapshot(&f.replica).await, before);
    assert_clean(&f.replica).await;
}
