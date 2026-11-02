//! Restrictions are transition checks: a posted entry never changes or
//! goes, the audit log only grows. Author commits refuse a violation;
//! replay drops it with reason `Restricted` wherever it orders after the
//! posting, whatever the delivery schedule.
use super::convergence::{History, deliver_all, runtime, unit};
use super::*;

/// Entries of batches (cascade) whose posted rows are immutable, ignoring
/// case as the status column compares, and an append-only audit log.
fn ledger() -> Constrained {
    let json = serde_json::json!({
        "format": 6,
        "application": "restricted-ledger",
        "tables": [
            {"name": "batches", "class": "replayed",
             "columns": [{"name": "name", "type": "text", "not_null": true}],
             "primary_key": ["id"]},
            {"name": "entries", "class": "replayed",
             "columns": [{"name": "batch", "type": "integer", "not_null": true},
                         {"name": "status", "type": "text", "not_null": true, "collation": "nocase"},
                         {"name": "amount", "type": "integer", "not_null": true}],
             "checks": ["status IN ('draft', 'posted')"],
             "primary_key": ["id"],
             "foreign_keys": [{"columns": ["batch"], "table": "batches", "on_delete": "cascade"}]},
            {"name": "audit", "class": "replayed",
             "columns": [{"name": "note", "type": "text", "not_null": true}],
             "primary_key": ["id"]}
        ],
        "restrictions": [
            {"name": "posted_entries", "table": "entries", "when": "status = 'posted'",
             "refuse_update": true, "refuse_delete": true},
            {"name": "audit_log", "table": "audit", "refuse_update": true, "refuse_delete": true}
        ]
    });
    Constrained::with(json, |_| {})
}

const UPDATE_POSTED: &str = "restriction posted_entries refuses this update of entries";
const DELETE_POSTED: &str = "restriction posted_entries refuses this delete of entries";

#[tokio::test]
async fn restrictions_refuse_local_updates_and_deletes_of_protected_rows() {
    let c = ledger();
    let root = tempfile::tempdir().unwrap();
    let (mut devices, ids, _) = devices(
        &c,
        root.path(),
        1,
        &[
            "INSERT INTO batches(name,id) VALUES('may',#id)",
            "INSERT INTO batches(name,id) VALUES('june',#id)",
            "INSERT INTO audit(note,id) VALUES('opened',#id)",
        ],
    )
    .await;
    let device = &mut devices[0];
    let (may, june, opened) = (ids[0], ids[1], ids[2]);
    let entries = device
        .commit(
            &c,
            &[
                &format!("INSERT INTO entries(batch,status,amount,id) VALUES({may},'draft',5,#id)"),
                &format!(
                    "INSERT INTO entries(batch,status,amount,id) VALUES({june},'draft',7,#id)"
                ),
            ],
        )
        .await
        .unwrap();
    let (posted, draft) = (entries[0], entries[1]);
    // Posting is a change of a row that did not match yet; the status
    // column compares ignoring case, and so does the restriction.
    device
        .commit(
            &c,
            &[&format!(
                "UPDATE entries SET status='POSTED' WHERE id={posted}"
            )],
        )
        .await
        .unwrap();
    device.fresh().await;
    let refused = async |statements: &[&str]| {
        constraint_error(device.commit(&c, statements).await.unwrap_err())
    };
    assert_eq!(
        refused(&[&format!("UPDATE entries SET amount=6 WHERE id={posted}")]).await,
        UPDATE_POSTED
    );
    assert_eq!(
        refused(&[&format!("DELETE FROM entries WHERE id={posted}")]).await,
        DELETE_POSTED
    );
    // Reopening and reposting within one transaction still changed it.
    assert_eq!(
        refused(&[
            &format!("UPDATE entries SET status='draft' WHERE id={posted}"),
            &format!("UPDATE entries SET amount=9 WHERE id={posted}"),
            &format!("UPDATE entries SET status='posted' WHERE id={posted}"),
        ])
        .await,
        UPDATE_POSTED
    );
    // A delete action reaches the posted child: the parent's delete fails.
    assert_eq!(
        refused(&[&format!("DELETE FROM batches WHERE id={may}")]).await,
        DELETE_POSTED
    );
    // A row keeps its id; moving it is deleting it and inserting a copy.
    assert_eq!(
        refused(&[
            &format!("DELETE FROM entries WHERE id={posted}"),
            &format!("INSERT INTO entries(batch,status,amount,id) VALUES({june},'posted',5,#id)"),
        ])
        .await,
        DELETE_POSTED
    );
    // Append-only: no update or delete of any audit row.
    for statement in [
        format!("UPDATE audit SET note='edited' WHERE id={opened}"),
        format!("DELETE FROM audit WHERE id={opened}"),
    ] {
        assert!(
            refused(&[&statement])
                .await
                .starts_with("restriction audit_log refuses this "),
            "{statement}"
        );
    }
    assert!(
        device.fresh().await.is_empty(),
        "a refusal publishes nothing"
    );
    // Unprotected rows and changes that keep every value commit.
    device
        .commit(
            &c,
            &[
                &format!("UPDATE entries SET amount=amount WHERE id={posted}"),
                &format!("UPDATE entries SET amount=8 WHERE id={draft}"),
                "INSERT INTO audit(note,id) VALUES('closed',#id)",
            ],
        )
        .await
        .unwrap();
    device
        .commit(&c, &[&format!("DELETE FROM batches WHERE id={june}")])
        .await
        .unwrap();
    let rows = device.rows(&c).await;
    assert_eq!(
        rows["entries"],
        [vec![
            SqlValue::Integer(may),
            SqlValue::Text("POSTED".into()),
            SqlValue::Integer(5),
            SqlValue::Integer(posted),
        ]]
    );
    assert_eq!(rows["audit"].len(), 2);
}

/// After `setup`, the first author posts an entry. The second edits it and
/// the third deletes its batch, neither having seen the posting, which
/// orders before both; an author skipping its own checks edits the audit
/// log; a draft edit applies.
async fn posting_history(root: &Path) -> History {
    let c = ledger();
    let (mut d, ids, mut setup) = devices(
        &c,
        root,
        3,
        &[
            "INSERT INTO batches(name,id) VALUES('may',#id)",
            "INSERT INTO audit(note,id) VALUES('opened',#id)",
        ],
    )
    .await;
    let (may, opened) = (ids[0], ids[1]);
    let entries = d[0]
        .commit(
            &c,
            &[
                &format!("INSERT INTO entries(batch,status,amount,id) VALUES({may},'draft',5,#id)"),
                &format!("INSERT INTO entries(batch,status,amount,id) VALUES({may},'draft',7,#id)"),
            ],
        )
        .await
        .unwrap();
    let created = d[0].fresh().await;
    for device in &d[1..] {
        device.receive(&created).await;
    }
    setup.extend(created);
    // What the others published so far belongs to the setup.
    for device in &mut d[1..] {
        let published = device.fresh().await;
        setup.extend(published);
    }
    let (posted, draft) = (entries[0], entries[1]);
    let post = unit(
        &c,
        &mut d[0],
        &[&format!(
            "UPDATE entries SET status='posted' WHERE id={posted}"
        )],
    )
    .await;
    let stamp = |frames: &[EncryptedFrame]| frames[0].operation_ref().unwrap().header.timestamp;
    let edit = unit(
        &c,
        &mut d[1],
        &[&format!("UPDATE entries SET amount=6 WHERE id={posted}")],
    )
    .await;
    let cascade = unit(
        &c,
        &mut d[2],
        &[&format!("DELETE FROM batches WHERE id={may}")],
    )
    .await;
    assert!(stamp(&post) < stamp(&edit) && stamp(&post) < stamp(&cascade));
    d[2].replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .unchecked_replayed_admission = true;
    let audit = unit(
        &c,
        &mut d[2],
        &[&format!("UPDATE audit SET note='edited' WHERE id={opened}")],
    )
    .await;
    let applied = unit(
        &c,
        &mut d[1],
        &[&format!("UPDATE entries SET amount=8 WHERE id={draft}")],
    )
    .await;
    History {
        c,
        setup,
        units: vec![post, edit, cascade, audit, applied],
    }
}

#[test]
fn edits_ordered_after_a_posting_drop_alike_in_every_order() {
    let root = tempfile::tempdir().unwrap();
    let history = runtime().block_on(posting_history(root.path()));
    let expected = canonical(&history.c, &history.frames());
    let restricted = TransactionOutcome::Dropped {
        reason: DropReason::Restricted,
        sealed: false,
    };
    let transaction = |unit: usize| history.c.transaction(&history.units[unit]);
    for unit in 1..=3 {
        assert_eq!(
            expected.outcomes[&transaction(unit)],
            restricted,
            "unit {unit}"
        );
    }
    assert_eq!(expected.dropped(), 3, "{expected:?}");
    assert_eq!(expected.rows["entries"].len(), 2);
    deliver_all(&history, false, &expected);

    // Every replica names the same restriction; a rebuild from frames
    // restores the protected rows and keeps the drops.
    runtime().block_on(async {
        let c = &history.c;
        let mut violations = Vec::new();
        for (index, frames) in [history.frames(), latest_first(&history.frames())]
            .into_iter()
            .enumerate()
        {
            let receiver = c.open(&root.path().join(format!("v{index}.db"))).await;
            receiver.receive(&frames).await;
            let mut named = Vec::new();
            for unit in 1..=3 {
                named.push(
                    receiver
                        .replica
                        .transaction_violation(transaction(unit))
                        .await
                        .unwrap()
                        .unwrap(),
                );
            }
            assert_eq!(receiver.state(c, &expected).await, expected);
            receiver
                .replica
                .rebuild_projection(c.scope("entries"))
                .await
                .unwrap();
            assert_eq!(receiver.state(c, &expected).await, expected);
            violations.push(named);
        }
        assert_eq!(violations[0], violations[1]);
        assert_eq!(
            violations[0],
            [
                UPDATE_POSTED,
                DELETE_POSTED,
                "restriction audit_log refuses this update of audit"
            ]
        );
    });
}
