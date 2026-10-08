//! Checkpoints, pruning, the horizon signal and rebootstrap.
use super::super::replica::checkpoint::Stage;
use super::*;
use crate::storage::rows;
use network::NativeSession;
use std::collections::BTreeMap;
use std::path::Path;
use tokio::net::{TcpListener, TcpStream};

type Replica = EncryptedReplica<Employees>;

fn id(index: u16) -> [u8; 32] {
    let mut row = [7; 32];
    row[..2].copy_from_slice(&index.to_be_bytes());
    row
}
fn handle(index: u16) -> [u8; 32] {
    let mut handle = [9; 32];
    handle[..2].copy_from_slice(&index.to_be_bytes());
    handle
}
fn insert(f: &Fixture, index: u16, name: &str) -> Vec<RowChange> {
    vec![RowChange {
        scope: f.public,
        row: id(index),
        before: None,
        after: Some(f.app().name(name)),
    }]
}
fn update(f: &Fixture, index: u16, before: &str, after: Option<&str>) -> Vec<RowChange> {
    vec![RowChange {
        scope: f.public,
        row: id(index),
        before: Some(f.app().name(before)),
        after: after.map(|name| f.app().name(name)),
    }]
}
async fn open(f: &Fixture, path: &Path, node: &PrivateKey) -> (Replica, CredentialChain) {
    let config = f.config(node, f.full(), &[f.public]);
    let credentials = config.credentials.clone();
    (
        EncryptedReplica::open(path, f.app(), config).await.unwrap(),
        credentials,
    )
}
/// One complete pass from `source` to `target`; the target acknowledges what it applied.
async fn sync(
    f: &Fixture,
    source: &Replica,
    target: (&Replica, &PrivateKey, &CredentialChain),
) -> Vec<ResourceScope> {
    let (replica, key, credentials) = target;
    let mut cursor = None;
    loop {
        let page = source
            .transfer_page(
                key.public_key(),
                credentials,
                &[f.public],
                cursor,
                MAX_SECURE_FRAME,
            )
            .await
            .unwrap();
        let pruned = page.pruned;
        let ids: Vec<_> = page
            .frames
            .iter()
            .map(|frame| frame.id().unwrap())
            .collect();
        for (id, status) in ids.into_iter().zip(replica.receive_all(page.frames).await) {
            // A duplicate was acknowledged by the pass that delivered it.
            let applied = match status.unwrap() {
                ReceiveStatus::Duplicate => continue,
                status => matches!(status, ReceiveStatus::Applied | ReceiveStatus::Pruned),
            };
            source
                .acknowledge(key.public_key(), credentials, id, applied)
                .await
                .unwrap();
        }
        cursor = page.next;
        if cursor.is_none() {
            return pruned;
        }
    }
}
async fn all_rows(f: &Fixture, replica: &Replica) -> BTreeMap<Commitment, RowFields> {
    let mut rows = BTreeMap::new();
    let mut cursor = None;
    loop {
        let page = replica
            .snapshot_page(f.public, cursor, MAX_SNAPSHOT_ROWS)
            .await
            .unwrap();
        assert!(page.complete);
        rows.extend(page.rows);
        cursor = page.next;
        if cursor.is_none() {
            return rows;
        }
    }
}
async fn count(replica: &Replica, sql: &str) -> i64 {
    match rows(&replica.inner.lock().await.conn, sql).await.unwrap()[0][0] {
        rocky_core::SqlValue::Integer(count) => count,
        _ => panic!("not a count"),
    }
}
fn policy(required: &[&PrivateKey], behind: &[&PrivateKey]) -> RetentionPolicy {
    RetentionPolicy {
        required: required.iter().map(|key| key.public_key()).collect(),
        behind: behind.iter().map(|key| key.public_key()).collect(),
        copies: 1,
        window: 0,
    }
}

#[tokio::test]
async fn checkpoint_binds_the_scope_and_survives_a_failure_at_every_step() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a.db");
    let node = PrivateKey::new();
    let (mut replica, _) = open(&f, &path, &node).await;
    for index in 0..40 {
        replica
            .author_buffered(handle(index), insert(&f, index, "new"), vec![])
            .await
            .unwrap();
    }
    replica
        .author_buffered(handle(1000), update(&f, 3, "new", Some("edited")), vec![])
        .await
        .unwrap();
    replica
        .author(handle(1001), update(&f, 4, "new", None), vec![])
        .await
        .unwrap();
    let expected = all_rows(&f, &replica).await;
    // Each fault rolls its step back; the file must reopen, pass the audit
    // and continue from the last committed step.
    for (stage, skip) in [
        (Stage::Begin, 0),
        (Stage::Replay, 0),
        (Stage::Replay, 2),
        (Stage::Seal, 0),
        (Stage::Install, 1),
    ] {
        replica.inject_stage_fault(stage, skip).await;
        assert!(
            matches!(
                replica.checkpoint(f.public).await,
                Err(Error::InjectedFailure)
            ),
            "{stage:?}"
        );
        drop(replica);
        replica = open(&f, &path, &node).await.0;
        replica.audit(f.public).await.unwrap();
        assert_eq!(all_rows(&f, &replica).await, expected);
    }
    let signed = replica.checkpoint(f.public).await.unwrap();
    let statement = &signed.statement;
    assert_eq!(signed.signer(), node.public_key());
    assert_eq!(statement.definition, f.app().definition().hash());
    assert_eq!((statement.schema, statement.handler), (1, 1));
    assert_eq!((statement.policy_epoch, statement.key_epoch), (1, 1));
    assert_eq!((statement.sequence, statement.previous), (1, None));
    assert_eq!(statement.frontier.len(), 1);
    assert_eq!(statement.frontier[0].author, node.public_key());
    assert_eq!(statement.frontier[0].sequence, 41);
    assert_eq!((statement.operations, statement.clock), (42, 42));
    assert_eq!((statement.rows.count, statement.tombstones), (40, 1));
    assert_eq!(statement.creations.count, 40);
    assert_eq!(statement.handles.count, 42);
    assert_eq!(statement.strong.head, None);
    // Nobody acknowledged anything: every covered operation is a sole copy.
    assert_eq!(statement.custody.superseded, 0);
    assert_eq!(statement.custody.sole.count, 42);
    let report = replica.audit(f.public).await.unwrap();
    assert_eq!(report.checkpoint, Some(signed.id().unwrap()));
    assert_eq!(
        (report.operations, report.rows, report.replayed),
        (42, 40, 0)
    );
    // Nothing new: the same checkpoint. One more operation: the next in the chain.
    assert_eq!(
        replica.checkpoint(f.public).await.unwrap().id().unwrap(),
        signed.id().unwrap()
    );
    replica
        .author(handle(1002), insert(&f, 400, "later"), vec![])
        .await
        .unwrap();
    assert_eq!(replica.audit(f.public).await.unwrap().replayed, 1);
    // A sealed checkpoint whose install was interrupted is finished by open.
    replica.inject_stage_fault(Stage::Install, 0).await;
    assert!(replica.checkpoint(f.public).await.is_err());
    assert!(
        replica
            .checkpoint_status(f.public)
            .await
            .unwrap()
            .in_progress
    );
    drop(replica);
    replica = open(&f, &path, &node).await.0;
    assert!(
        !replica
            .checkpoint_status(f.public)
            .await
            .unwrap()
            .in_progress
    );
    let next = replica.checkpoint(f.public).await.unwrap();
    assert_eq!(next.statement.sequence, 2);
    assert_eq!(next.statement.previous, Some(signed.id().unwrap()));
    assert_eq!(next.statement.rows.count, 41);
    drop(replica);
    let replica = open(&f, &path, &node).await.0;
    assert_eq!(replica.audit(f.public).await.unwrap().replayed, 0);
    assert_eq!(all_rows(&f, &replica).await.len(), 40);
    assert_eq!(
        replica.companion(f.public, id(4)).await.unwrap(),
        Companion::Deleted
    );
}

#[tokio::test]
async fn pruning_needs_a_verified_checkpoint_acknowledgments_and_custody() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a.db");
    let (a_key, b_key) = (PrivateKey::new(), PrivateKey::new());
    let (mut a, _) = open(&f, &path, &a_key).await;
    let (b, b_credentials) = open(&f, &dir.path().join("b.db"), &b_key).await;
    for index in 0..20 {
        a.author_buffered(handle(index), insert(&f, index, "new"), vec![])
            .await
            .unwrap();
    }
    let blocked = |block| PruneReport {
        pruned: 0,
        blocked: Some(block),
    };
    assert_eq!(
        a.prune(f.public).await.unwrap(),
        blocked(PruneBlock::NoCheckpoint)
    );
    a.checkpoint(f.public).await.unwrap();
    assert_eq!(
        a.prune(f.public).await.unwrap(),
        blocked(PruneBlock::NoPolicy)
    );
    // Missing custody evidence: nobody holds another copy.
    a.set_retention_policy(f.public, policy(&[], &[]))
        .await
        .unwrap();
    assert!(matches!(
        a.prune(f.public).await.unwrap(),
        PruneReport {
            pruned: 0,
            blocked: Some(PruneBlock::SoleCopy { .. })
        }
    ));
    // The checkpoint is flushed before any operation it covers may go.
    let (commit, durable, _) = a.watermark().await;
    assert_eq!(commit, durable);
    a.set_retention_policy(f.public, policy(&[&b_key], &[]))
        .await
        .unwrap();
    assert!(matches!(
        a.prune(f.public).await.unwrap().blocked,
        Some(PruneBlock::Unacknowledged { peer, .. }) if peer == b_key.public_key()
    ));
    assert!(sync(&f, &a, (&b, &b_key, &b_credentials)).await.is_empty());
    // A domain that requires recovery custodians keeps complete history.
    let durability = [
        encode(&f.public.domain).unwrap(),
        encode(&DurabilityPolicy::RequiredCustodians {
            nodes: [b_key.public_key()].into(),
            minimum: 1,
        })
        .unwrap(),
    ];
    a.inner
        .lock()
        .await
        .conn
        .execute("INSERT INTO secure_durability VALUES(?,?)", durability)
        .await
        .unwrap();
    assert_eq!(
        a.prune(f.public).await.unwrap(),
        blocked(PruneBlock::RecoveryCustody)
    );
    a.inner
        .lock()
        .await
        .conn
        .execute("DELETE FROM secure_durability", ())
        .await
        .unwrap();
    // A checkpoint that no longer verifies blocks pruning.
    let signed = {
        let inner = a.inner.lock().await;
        let stored = rows(&inner.conn, "SELECT signed FROM secure_checkpoints")
            .await
            .unwrap();
        let rocky_core::SqlValue::Blob(signed) = stored[0][0].clone() else {
            panic!("missing checkpoint");
        };
        let mut tampered = SignedCheckpoint::from_bytes(&signed).unwrap();
        tampered.statement.operations += 1;
        inner
            .conn
            .execute(
                "UPDATE secure_checkpoints SET signed=?",
                [tampered.to_bytes().unwrap()],
            )
            .await
            .unwrap();
        signed
    };
    assert!(matches!(a.prune(f.public).await, Err(Error::Corrupt(_))));
    assert!(matches!(a.audit(f.public).await, Err(Error::Corrupt(_))));
    assert_eq!(
        count(&a, "SELECT COUNT(*) FROM secure_operations").await,
        20
    );
    a.inner
        .lock()
        .await
        .conn
        .execute("UPDATE secure_checkpoints SET signed=?", [signed])
        .await
        .unwrap();
    // A failure in the second batch keeps the first one pruned and the file whole.
    a.inject_stage_fault(Stage::Prune, 1).await;
    assert!(matches!(
        a.prune(f.public).await,
        Err(Error::InjectedFailure)
    ));
    drop(a);
    a = open(&f, &path, &a_key).await.0;
    assert_eq!(
        count(&a, "SELECT COUNT(*) FROM secure_operations").await,
        12
    );
    a.audit(f.public).await.unwrap();
    assert_eq!(
        a.prune(f.public).await.unwrap(),
        PruneReport {
            pruned: 12,
            blocked: None
        }
    );
    assert_eq!(
        a.prune(f.public).await.unwrap(),
        PruneReport {
            pruned: 0,
            blocked: None
        }
    );
    for table in [
        "secure_operations",
        "secure_outbox",
        "secure_ack",
        "secure_applied",
    ] {
        assert_eq!(count(&a, &format!("SELECT COUNT(*) FROM {table}")).await, 0);
    }
    let status = a.checkpoint_status(f.public).await.unwrap();
    assert_eq!(status.horizon, vec![(a_key.public_key(), 19)]);
    assert_eq!(
        (status.covered, status.uncovered, status.retained_bytes),
        (0, 0, 0)
    );
    // The pruned file reopens, audits, keeps its rows and continues its log.
    drop(a);
    a = open(&f, &path, &a_key).await.0;
    assert_eq!(a.audit(f.public).await.unwrap().operations, 0);
    assert_eq!(all_rows(&f, &a).await.len(), 20);
    let frame = a
        .author(handle(500), update(&f, 7, "new", Some("after")), vec![])
        .await
        .unwrap();
    assert_eq!(frame.operation().unwrap().header.seq_num, 20);
    let late = frame.id().unwrap();
    assert_eq!(b.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    a.acknowledge(b_key.public_key(), &b_credentials, late, true)
        .await
        .unwrap();
    assert_eq!(all_rows(&f, &a).await, all_rows(&f, &b).await);
    a.rebuild_projection(f.public).await.unwrap();
    assert_eq!(all_rows(&f, &a).await, all_rows(&f, &b).await);
    a.audit(f.public).await.unwrap();
    // The retained window keeps the newest covered operations.
    let mut windowed = policy(&[], &[]);
    windowed.window = 1;
    a.set_retention_policy(f.public, windowed).await.unwrap();
    a.author(handle(501), update(&f, 8, "new", Some("after")), vec![])
        .await
        .unwrap();
    sync(&f, &a, (&b, &b_key, &b_credentials)).await;
    a.checkpoint(f.public).await.unwrap();
    assert_eq!(a.prune(f.public).await.unwrap().pruned, 1);
    assert_eq!(count(&a, "SELECT COUNT(*) FROM secure_operations").await, 1);
    a.audit(f.public).await.unwrap();
}

#[tokio::test]
async fn late_frames_and_retried_handles_from_before_the_horizon_do_nothing() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (a_key, b_key) = (PrivateKey::new(), PrivateKey::new());
    let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &a_key).await;
    let (b, b_credentials) = open(&f, &dir.path().join("b.db"), &b_key).await;
    let created = a
        .author(handle(1), insert(&f, 1, "alive"), vec![])
        .await
        .unwrap();
    a.author(handle(2), insert(&f, 2, "kept"), vec![])
        .await
        .unwrap();
    sync(&f, &a, (&b, &b_key, &b_credentials)).await;
    // B edits the row while A deletes it; B's edit reaches A only after the prune.
    let concurrent = b
        .author(handle(3), update(&f, 1, "alive", Some("edited")), vec![])
        .await
        .unwrap();
    let deleted = a
        .author(handle(4), update(&f, 1, "alive", None), vec![])
        .await
        .unwrap();
    sync(&f, &a, (&b, &b_key, &b_credentials)).await;
    a.set_retention_policy(f.public, policy(&[&b_key], &[]))
        .await
        .unwrap();
    a.checkpoint(f.public).await.unwrap();
    assert_eq!(a.prune(f.public).await.unwrap().pruned, 3);
    let before = all_rows(&f, &a).await;
    // The original creation and deletion frames come back late.
    for frame in [&created, &deleted] {
        assert_eq!(
            a.receive(frame.clone()).await.unwrap(),
            ReceiveStatus::Pruned
        );
    }
    // An edit that predates the deletion applies to the tombstone only.
    assert_eq!(a.receive(concurrent).await.unwrap(), ReceiveStatus::Applied);
    assert_eq!(all_rows(&f, &a).await, before);
    assert_eq!(
        a.companion(f.public, id(1)).await.unwrap(),
        Companion::Deleted
    );
    assert_eq!(count(&a, "SELECT COUNT(*) FROM secure_operations").await, 1);
    // A retried handle from before the horizon never authors again.
    let operations = count(&a, "SELECT COUNT(*) FROM secure_handles").await;
    assert!(matches!(
        a.author(handle(1), insert(&f, 1, "alive"), vec![]).await,
        Err(Error::Rejected(message)) if message.contains("before the pruning horizon")
    ));
    assert_eq!(
        a.author_buffered(handle(1), insert(&f, 1, "alive"), vec![])
            .await
            .unwrap()
            .value,
        created.id().unwrap()
    );
    assert_eq!(
        a.lookup_mutation(handle(1)).await.unwrap(),
        Some(created.id().unwrap())
    );
    assert!(matches!(
        a.author_buffered(handle(1), insert(&f, 1, "other"), vec![])
            .await,
        Err(Error::Conflict)
    ));
    assert_eq!(
        count(&a, "SELECT COUNT(*) FROM secure_handles").await,
        operations
    );
    assert_eq!(count(&a, "SELECT COUNT(*) FROM secure_operations").await, 1);
    assert_eq!(all_rows(&f, &a).await, before);
    // An acknowledgment for a pruned operation is accepted and records nothing.
    a.acknowledge(
        b_key.public_key(),
        &b_credentials,
        created.id().unwrap(),
        true,
    )
    .await
    .unwrap();
    // B converges with the pruned replica.
    sync(&f, &b, (&a, &a_key, &a_credentials)).await;
    assert_eq!(all_rows(&f, &a).await, all_rows(&f, &b).await);
    a.audit(f.public).await.unwrap();
}

/// Serves `source` until the client closes the session.
#[tokio::test]
async fn handles_beyond_the_window_are_forgotten_once_pruned() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (a_key, b_key) = (PrivateKey::new(), PrivateKey::new());
    let (a, _) = open(&f, &dir.path().join("a.db"), &a_key).await;
    let (b, b_credentials) = open(&f, &dir.path().join("b.db"), &b_key).await;
    a.set_retention_limits(RetentionLimits {
        handles: 1,
        ..RetentionLimits::default()
    })
    .await
    .unwrap();
    a.author(handle(1), insert(&f, 1, "first"), vec![])
        .await
        .unwrap();
    a.author(handle(2), insert(&f, 2, "second"), vec![])
        .await
        .unwrap();
    // Both handles stay while their operations are retained.
    a.author(handle(3), insert(&f, 3, "third"), vec![])
        .await
        .unwrap();
    assert_eq!(count(&a, "SELECT COUNT(*) FROM secure_handles").await, 3);
    sync(&f, &a, (&b, &b_key, &b_credentials)).await;
    a.set_retention_policy(f.public, policy(&[&b_key], &[]))
        .await
        .unwrap();
    a.checkpoint(f.public).await.unwrap();
    assert_eq!(a.prune(f.public).await.unwrap().pruned, 3);
    // Only the newest handle survives; the audit checks the handle digest.
    assert_eq!(count(&a, "SELECT COUNT(*) FROM secure_handles").await, 1);
    a.audit(f.public).await.unwrap();
    assert_eq!(a.lookup_mutation(handle(1)).await.unwrap(), None);
    assert!(matches!(
        a.author(handle(3), insert(&f, 3, "third"), vec![]).await,
        Err(Error::Rejected(message)) if message.contains("before the pruning horizon")
    ));
    // A forgotten handle authors again.
    a.author(handle(1), insert(&f, 4, "fourth"), vec![])
        .await
        .unwrap();
    assert_eq!(count(&a, "SELECT COUNT(*) FROM secure_operations").await, 1);
}

async fn serve(
    listener: TcpListener,
    source: &Replica,
    identity: (PrivateKey, CredentialChain),
    peer: PublicKey,
) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut session =
        NativeSession::authenticate(stream, identity.0, identity.1, peer, MAX_SECURE_BYTES)
            .await
            .unwrap();
    while session.serve_once(source).await.is_ok() {}
}

#[tokio::test]
async fn offline_replica_learns_it_is_behind_and_rebootstraps_with_its_local_edits() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    let (b, b_credentials) = open(&f, &dir.path().join("b.db"), &keys[1]).await;
    let c_path = dir.path().join("c.db");
    let (mut c, c_credentials) = open(&f, &c_path, &keys[2]).await;
    for index in 0..5 {
        a.author_buffered(handle(index), insert(&f, index, "base"), vec![])
            .await
            .unwrap();
    }
    sync(&f, &a, (&b, &keys[1], &b_credentials)).await;
    sync(&f, &a, (&c, &keys[2], &c_credentials)).await;
    // C is offline from here. A moves on, checkpoints and prunes.
    for index in 5..45 {
        a.author_buffered(handle(index), insert(&f, index, "base"), vec![])
            .await
            .unwrap();
    }
    a.author_buffered(handle(100), update(&f, 2, "base", None), vec![])
        .await
        .unwrap();
    a.author_buffered(handle(101), insert(&f, 300, "from a"), vec![])
        .await
        .unwrap();
    sync(&f, &a, (&b, &keys[1], &b_credentials)).await;
    a.set_retention_policy(f.public, policy(&[&keys[1]], &[&keys[2]]))
        .await
        .unwrap();
    let signed = a.checkpoint(f.public).await.unwrap();
    assert_eq!(a.prune(f.public).await.unwrap().pruned, 47);
    a.author(handle(102), update(&f, 0, "base", Some("after")), vec![])
        .await
        .unwrap();
    // C's unsent edits: an update, a new row, an edit of a row A deleted, and
    // an insert that collides with a row A created meanwhile.
    let kept: Vec<_> = [
        (handle(200), update(&f, 3, "base", Some("local"))),
        (handle(201), insert(&f, 301, "from c")),
        (handle(202), update(&f, 2, "base", Some("lost"))),
    ]
    .into_iter()
    .collect();
    let mut local = Vec::new();
    for (handle, changes) in kept {
        local.push(
            c.author(handle, changes, vec![])
                .await
                .unwrap()
                .id()
                .unwrap(),
        );
    }
    let collision = c
        .author(handle(203), insert(&f, 300, "from c"), vec![])
        .await
        .unwrap()
        .id()
        .unwrap();
    // B was online and acknowledged everything: it is not behind and converges.
    let horizon = a
        .horizon_page(keys[1].public_key(), &b_credentials, f.public, None)
        .await
        .unwrap();
    assert_eq!(horizon.logs, vec![(keys[0].public_key(), 46)]);
    assert!(!b.behind_horizon(&horizon).await.unwrap());
    assert_eq!(
        sync(&f, &a, (&b, &keys[1], &b_credentials)).await,
        vec![f.public]
    );
    assert_eq!(all_rows(&f, &a).await, all_rows(&f, &b).await);
    assert!(c.behind_horizon(&horizon).await.unwrap());
    // Over the native session C is told so, and no frame is received into a gap.
    let quota = 8192;
    let before = all_rows(&f, &c).await;
    for fault in [Some((Stage::Import, 1)), Some((Stage::Switch, 0)), None] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = async {
            let stream = TcpStream::connect(address).await.unwrap();
            let mut session = NativeSession::authenticate(
                stream,
                keys[2].clone(),
                c_credentials.clone(),
                keys[0].public_key(),
                MAX_SECURE_BYTES,
            )
            .await
            .unwrap();
            assert!(
                session
                    .pull(&c, vec![f.public], None, MAX_SECURE_FRAME / 2)
                    .await
                    .is_err()
            );
            let pulled = session
                .pull_page(&c, vec![f.public], None, MAX_SECURE_FRAME / 2)
                .await
                .unwrap();
            assert_eq!(pulled.rebootstrap, vec![f.public]);
            assert_eq!(all_rows(&f, &c).await, before);
            if let Some((stage, skip)) = fault {
                c.inject_stage_fault(stage, skip).await;
            }
            session.rebootstrap(&c, f.public, quota).await
        };
        let (report, ()) = tokio::join!(
            client,
            serve(
                listener,
                &a,
                (keys[0].clone(), a_credentials.clone()),
                keys[2].public_key()
            )
        );
        let Some(_) = fault else {
            let report = report.unwrap();
            assert_eq!(report.checkpoint, signed.id().unwrap());
            assert_eq!(report.superseded, 5);
            assert_eq!(report.reapplied, local);
            assert_eq!(report.pending, vec![]);
            assert_eq!(report.rejected.len(), 1);
            assert_eq!(report.rejected[0].0, collision);
            assert!(report.rejected[0].1.contains("row identity already exists"));
            break;
        };
        // A failed import leaves the old state; the file reopens and audits.
        assert!(matches!(report, Err(Error::InjectedFailure)));
        drop(c);
        c = open(&f, &c_path, &keys[2]).await.0;
        c.audit(f.public).await.unwrap();
        assert_eq!(all_rows(&f, &c).await, before);
        assert!(c.rebootstrap_progress(f.public).await.unwrap().is_some());
    }
    assert_eq!(c.rebootstrap_progress(f.public).await.unwrap(), None);
    c.audit(f.public).await.unwrap();
    // C now syncs the frames after the checkpoint and A receives C's edits.
    assert!(!c.behind_horizon(&horizon).await.unwrap());
    sync(&f, &a, (&c, &keys[2], &c_credentials)).await;
    sync(&f, &c, (&a, &keys[0], &a_credentials)).await;
    let rows = all_rows(&f, &a).await;
    assert_eq!(rows, all_rows(&f, &c).await);
    assert_eq!(rows.len(), 46);
    assert_eq!(rows[&id(0)], f.app().name("after"));
    assert_eq!(rows[&id(3)], f.app().name("local"));
    assert_eq!(rows[&id(301)], f.app().name("from c"));
    assert_eq!(rows[&id(300)], f.app().name("from a"));
    assert!(!rows.contains_key(&id(2)));
    // C's handles survive: none of its local edits can be authored twice.
    assert_eq!(
        c.author_buffered(handle(200), update(&f, 3, "base", Some("local")), vec![])
            .await
            .unwrap()
            .value,
        local[0]
    );
    assert_eq!(
        c.lookup_mutation(handle(203)).await.unwrap(),
        Some(collision)
    );
    drop(c);
    let c = open(&f, &c_path, &keys[2]).await.0;
    c.audit(f.public).await.unwrap();
    a.audit(f.public).await.unwrap();
    assert_eq!(all_rows(&f, &c).await, rows);
    // C checkpoints on top of the imported one and can prune in turn.
    let next = c.checkpoint(f.public).await.unwrap();
    assert_eq!(next.statement.sequence, 2);
    assert_eq!(next.statement.previous, Some(signed.id().unwrap()));
    assert_eq!(next.signer(), keys[2].public_key());
}

#[tokio::test]
async fn a_writer_resigns_an_imported_checkpoint_before_offering_it_to_its_original_signer() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (a_key, b_key) = (PrivateKey::new(), PrivateKey::new());
    let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &a_key).await;
    let (b, b_credentials) = open(&f, &dir.path().join("b.db"), &b_key).await;
    a.author(handle(1), insert(&f, 1, "retained"), vec![])
        .await
        .unwrap();
    let original = a.checkpoint(f.public).await.unwrap();
    b.begin_rebootstrap(original.clone()).await.unwrap();
    let page = a
        .checkpoint_page(
            b_key.public_key(),
            &b_credentials,
            f.public,
            original.id().unwrap(),
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert!(page.next.is_none());
    b.stage_checkpoint_page(page).await.unwrap();
    b.finish_rebootstrap(f.public).await.unwrap();

    let offered = b
        .checkpoint_offer(a_key.public_key(), &a_credentials, f.public)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(offered.signer(), b_key.public_key());
    assert_eq!(offered.statement.rows, original.statement.rows);
    let c_key = PrivateKey::new();
    let reader_grants = f
        .full()
        .into_iter()
        .map(|mut grant| {
            grant
                .rights
                .retain(|right| matches!(right, Right::Read | Right::Custody));
            grant
        })
        .collect();
    let config = f.config(&c_key, reader_grants, &[f.public]);
    let c_credentials = config.credentials.clone();
    let c = EncryptedReplica::open(dir.path().join("c.db"), f.app(), config)
        .await
        .unwrap();
    c.begin_rebootstrap(original.clone()).await.unwrap();
    let page = a
        .checkpoint_page(
            c_key.public_key(),
            &c_credentials,
            f.public,
            original.id().unwrap(),
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    c.stage_checkpoint_page(page).await.unwrap();
    c.finish_rebootstrap(f.public).await.unwrap();
    // A read-only peer cannot replace the signature, but may serve the cut to others.
    assert!(
        c.checkpoint_offer(a_key.public_key(), &a_credentials, f.public)
            .await
            .unwrap()
            .is_none()
    );
    let forwarded = c
        .checkpoint_offer(b_key.public_key(), &b_credentials, f.public)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(forwarded.id().unwrap(), original.id().unwrap());

    // A retries against the same stable offer; own-signed imports remain refused.
    let retry = b
        .checkpoint_offer(a_key.public_key(), &a_credentials, f.public)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(offered.id().unwrap(), retry.id().unwrap());
    assert!(a.begin_rebootstrap(original).await.is_err());
    a.begin_rebootstrap(offered).await.unwrap();
}

#[tokio::test]
async fn rebootstrap_refuses_unverifiable_checkpoints() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (a_key, c_key) = (PrivateKey::new(), PrivateKey::new());
    let (a, _) = open(&f, &dir.path().join("a.db"), &a_key).await;
    let (c, c_credentials) = open(&f, &dir.path().join("c.db"), &c_key).await;
    for index in 0..6 {
        a.author_buffered(handle(index), insert(&f, index, "base"), vec![])
            .await
            .unwrap();
    }
    c.author(handle(50), insert(&f, 50, "mine"), vec![])
        .await
        .unwrap();
    let signed = a.checkpoint(f.public).await.unwrap();
    let before = all_rows(&f, &c).await;
    // A statement that was changed after signing.
    let mut forged = signed.clone();
    forged.statement.rows.count += 1;
    assert!(c.begin_rebootstrap(forged).await.is_err());
    // Its own checkpoint, and a page that belongs to no import.
    assert!(a.begin_rebootstrap(signed.clone()).await.is_err());
    let checkpoint = signed.id().unwrap();
    let page = a
        .checkpoint_page(
            c_key.public_key(),
            &c_credentials,
            f.public,
            checkpoint,
            None,
            8192,
        )
        .await
        .unwrap();
    assert!(page.next.is_some());
    assert!(c.stage_checkpoint_page(page.clone()).await.is_err());
    c.begin_rebootstrap(signed.clone()).await.unwrap();
    assert!(c.finish_rebootstrap(f.public).await.is_err());
    // A page out of order, then a row set that stops early.
    let mut skipped = page.clone();
    skipped.after = page.next;
    assert!(c.stage_checkpoint_page(skipped).await.is_err());
    let mut truncated = page.clone();
    truncated.next = None;
    assert!(c.stage_checkpoint_page(truncated).await.is_err());
    c.stage_checkpoint_page(page).await.unwrap();
    assert!(c.finish_rebootstrap(f.public).await.is_err());
    assert_eq!(all_rows(&f, &c).await, before);
    c.audit(f.public).await.unwrap();
    // A superseded checkpoint is not served any more.
    a.author(handle(60), insert(&f, 60, "newer"), vec![])
        .await
        .unwrap();
    a.checkpoint(f.public).await.unwrap();
    assert!(
        a.checkpoint_page(
            c_key.public_key(),
            &c_credentials,
            f.public,
            checkpoint,
            None,
            4096
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn retained_bytes_trigger_checkpoints_and_pruning_then_backpressure() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (a_key, b_key) = (PrivateKey::new(), PrivateKey::new());
    let (a, _) = open(&f, &dir.path().join("a.db"), &a_key).await;
    let (b, b_credentials) = open(&f, &dir.path().join("b.db"), &b_key).await;
    let frame = a
        .author(handle(0), insert(&f, 0, "first"), vec![])
        .await
        .unwrap();
    let size = frame.operation().unwrap().header.to_bytes().len() + frame.body.len();
    a.set_retention_limits(RetentionLimits {
        checkpoint_operations: 8,
        soft_bytes: 30 * size,
        hard_bytes: 60 * size,
        ..RetentionLimits::default()
    })
    .await
    .unwrap();
    a.set_retention_policy(f.public, policy(&[&b_key], &[]))
        .await
        .unwrap();
    // With an acknowledging peer, maintenance keeps history inside the bound.
    let mut peak = 0;
    for index in 1..160 {
        a.author_buffered(handle(index), insert(&f, index, "row"), vec![])
            .await
            .unwrap();
        if index % 8 == 0 {
            sync(&f, &a, (&b, &b_key, &b_credentials)).await;
        }
        peak = peak.max(a.checkpoint_status(f.public).await.unwrap().retained_bytes);
    }
    let status = a.checkpoint_status(f.public).await.unwrap();
    assert_eq!(status.maintenance_error, None);
    assert!(peak <= 60 * size as u64, "{peak}");
    assert!(status.checkpoint.unwrap().statement.operations > 100);
    assert!(!status.horizon.is_empty());
    assert!(count(&a, "SELECT COUNT(*) FROM secure_operations").await < 60);
    a.audit(f.public).await.unwrap();
    // Without acknowledgments nothing may be pruned: the hard bound answers
    // with a retryable error instead of losing anything.
    let mut refused = None;
    for index in 160..300 {
        if let Err(error) = a
            .author_buffered(handle(index), insert(&f, index, "row"), vec![])
            .await
        {
            refused = Some((index, error));
            break;
        }
    }
    let (index, error) = refused.expect("backpressure");
    assert!(matches!(error, Error::Backpressure) && error.is_retryable());
    assert_eq!(a.lookup_mutation(handle(index)).await.unwrap(), None);
    assert!(a.checkpoint_status(f.public).await.unwrap().retained_bytes <= 60 * size as u64);
    // Acknowledgments release it; the same handle then commits once.
    sync(&f, &a, (&b, &b_key, &b_credentials)).await;
    a.checkpoint(f.public).await.unwrap();
    assert!(a.prune(f.public).await.unwrap().pruned > 0);
    a.author(handle(index), insert(&f, index, "row"), vec![])
        .await
        .unwrap();
    sync(&f, &a, (&b, &b_key, &b_credentials)).await;
    assert_eq!(all_rows(&f, &a).await, all_rows(&f, &b).await);
    assert_eq!(all_rows(&f, &a).await.len(), index as usize + 1);
    a.audit(f.public).await.unwrap();
    b.audit(f.public).await.unwrap();
}

#[tokio::test]
async fn retried_command_from_before_the_horizon_keeps_its_one_result() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (a_key, b_key) = (PrivateKey::new(), PrivateKey::new());
    let config = f.config(&a_key, commands::command_grants(&f), &[f.public, f.private]);
    let coordinator = commands::coordinator(&f, &config);
    let a = EncryptedReplica::open(dir.path().join("a.db"), f.app(), config)
        .await
        .unwrap();
    let (b, b_credentials) = open(&f, &dir.path().join("b.db"), &b_key).await;
    a.author(handle(1), insert(&f, 1, "merged"), vec![])
        .await
        .unwrap();
    let arguments = b"employee-1\nAlice\n731994";
    let command = a
        .invoke_encrypted([1; 32], f.private, "SetEmployee", arguments)
        .await
        .unwrap();
    let result = a
        .execute_encrypted(command.clone(), &coordinator)
        .await
        .unwrap();
    let before = a.command_snapshot(f.public).await.unwrap().rows;
    assert!(!before.is_empty());
    sync(&f, &a, (&b, &b_key, &b_credentials)).await;
    a.set_retention_policy(f.public, policy(&[&b_key], &[]))
        .await
        .unwrap();
    // The checkpoint binds the strong head and every retained strong artifact.
    let strong = a.checkpoint(f.public).await.unwrap().statement.strong;
    let head = coordinator.head(f.public.domain).unwrap();
    assert_eq!(strong.head, Some((head.sequence, head.head)));
    assert_eq!((strong.commands.count, strong.results.count), (0, 0));
    assert_eq!(strong.base, strong.head);
    assert_eq!(a.prune(f.public).await.unwrap().pruned, 1);
    // Strong artifacts are not pruned: the retried invocation is the same
    // signed command and executing it again returns the one result.
    assert_eq!(
        a.invoke_encrypted([1; 32], f.private, "SetEmployee", arguments)
            .await
            .unwrap()
            .to_bytes()
            .unwrap(),
        command.to_bytes().unwrap()
    );
    assert_eq!(
        a.execute_encrypted(command, &coordinator)
            .await
            .unwrap()
            .to_bytes()
            .unwrap(),
        result.to_bytes().unwrap()
    );
    assert_eq!(coordinator.head(f.public.domain).unwrap(), head);
    assert_eq!(count(&a, "SELECT COUNT(*) FROM secure_results").await, 1);
    assert_eq!(a.command_snapshot(f.public).await.unwrap().rows, before);
    a.audit(f.public).await.unwrap();
}

#[tokio::test]
async fn late_copy_of_a_pruned_dependency_releases_a_waiting_frame() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (a_key, b_key) = (PrivateKey::new(), PrivateKey::new());
    let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &a_key).await;
    let (b, b_credentials) = open(&f, &dir.path().join("b.db"), &b_key).await;
    a.author(handle(1), insert(&f, 1, "one"), vec![])
        .await
        .unwrap();
    let named = a
        .author(handle(2), update(&f, 1, "one", Some("two")), vec![])
        .await
        .unwrap();
    a.author(handle(3), insert(&f, 2, "tip"), vec![])
        .await
        .unwrap();
    sync(&f, &a, (&b, &b_key, &b_credentials)).await;
    // B, which keeps its history, names an ordinary old operation explicitly.
    let dependent = b
        .author(
            handle(4),
            insert(&f, 3, "depends"),
            vec![named.id().unwrap()],
        )
        .await
        .unwrap();
    a.set_retention_policy(f.public, policy(&[&b_key], &[]))
        .await
        .unwrap();
    a.checkpoint(f.public).await.unwrap();
    assert_eq!(a.prune(f.public).await.unwrap().pruned, 3);
    // A cannot tell a pruned dependency from a missing one: the frame waits.
    assert_eq!(
        a.receive(dependent).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    assert_eq!(a.checkpoint_status(f.public).await.unwrap().pending, 1);
    // A pass from the peer that still holds it settles the frame.
    sync(&f, &b, (&a, &a_key, &a_credentials)).await;
    assert_eq!(a.checkpoint_status(f.public).await.unwrap().pending, 0);
    assert_eq!(all_rows(&f, &a).await, all_rows(&f, &b).await);
    a.audit(f.public).await.unwrap();
}

const CRASH_STEPS: [&str; 7] = [
    "begin", "replay", "seal", "install", "prune", "import", "switch",
];
/// Fixed keys, so that the parent reopens what the child wrote.
fn crash_keys() -> (Fixture, [PrivateKey; 3]) {
    (
        Fixture::with_publisher(PrivateKey::from_bytes(&[41; 32])),
        [42, 43, 44].map(|byte| PrivateKey::from_bytes(&[byte; 32])),
    )
}
/// Imports `source`'s checkpoint into `target`, resuming a staged import as
/// the native session does.
async fn crash_rebootstrap(
    f: &Fixture,
    source: &Replica,
    target: (&Replica, &PrivateKey, &CredentialChain),
) -> Result<RebootstrapReport, Error> {
    let (replica, key, credentials) = target;
    let signed = source.checkpoint(f.public).await?;
    let checkpoint = signed.id()?;
    let (mut after, mut complete) = match replica.rebootstrap_progress(f.public).await? {
        Some((staged, cursor, complete)) if staged == checkpoint => (cursor, complete),
        _ => {
            replica.begin_rebootstrap(signed).await?;
            (None, false)
        }
    };
    while !complete {
        let page = source
            .checkpoint_page(
                key.public_key(),
                credentials,
                f.public,
                checkpoint,
                after,
                4096,
            )
            .await?;
        (after, complete) = (page.next, page.next.is_none());
        replica.stage_checkpoint_page(page).await?;
    }
    replica.finish_rebootstrap(f.public).await
}

#[tokio::test]
#[ignore = "invoked by process_exit_inside_any_checkpoint_step_leaves_a_file_that_opens_and_audits"]
async fn checkpoint_crash_child() {
    let dir = std::env::var("ROCKY_CHECKPOINT_CRASH_DIR").unwrap();
    let step = std::env::var("ROCKY_CHECKPOINT_CRASH_STEP").unwrap();
    let dir = Path::new(&dir);
    let (f, keys) = crash_keys();
    let (a, _) = open(&f, &dir.join("a.db"), &keys[0]).await;
    let (b, b_credentials) = open(&f, &dir.join("b.db"), &keys[1]).await;
    let (c, c_credentials) = open(&f, &dir.join("c.db"), &keys[2]).await;
    for index in 0..40 {
        a.author_buffered(handle(index), insert(&f, index, "base"), vec![])
            .await
            .unwrap();
    }
    a.author(handle(100), update(&f, 3, "base", Some("edited")), vec![])
        .await
        .unwrap();
    sync(&f, &a, (&b, &keys[1], &b_credentials)).await;
    a.set_retention_policy(f.public, policy(&[&keys[1]], &[&keys[2]]))
        .await
        .unwrap();
    c.author(handle(900), insert(&f, 900, "unsent"), vec![])
        .await
        .unwrap();
    let target = (&c, &keys[2], &c_credentials);
    match step.as_str() {
        "begin" => a.inject_stage_exit(Stage::Begin, 0).await,
        "replay" => a.inject_stage_exit(Stage::Replay, 1).await,
        "seal" => a.inject_stage_exit(Stage::Seal, 0).await,
        "install" => a.inject_stage_exit(Stage::Install, 0).await,
        "prune" => {
            a.checkpoint(f.public).await.unwrap();
            a.inject_stage_exit(Stage::Prune, 1).await;
            let _ = a.prune(f.public).await;
        }
        "import" | "switch" => {
            a.checkpoint(f.public).await.unwrap();
            if step == "import" {
                c.inject_stage_exit(Stage::Import, 1).await;
            } else {
                c.inject_stage_exit(Stage::Switch, 0).await;
            }
            let _ = crash_rebootstrap(&f, &a, target).await;
        }
        other => panic!("unknown crash step {other}"),
    }
    let _ = a.checkpoint(f.public).await;
    // The fault exits the process; reaching this line is a failure.
    std::process::exit(1);
}

/// Process exit inside each step of checkpoint creation, pruning and
/// rebootstrap: the step's transaction is gone, every file opens and passes
/// the audit, and the work completes afterwards.
#[tokio::test]
async fn process_exit_inside_any_checkpoint_step_leaves_a_file_that_opens_and_audits() {
    for step in CRASH_STEPS {
        let dir = TempDir::new().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::checkpoint::checkpoint_crash_child",
                "--ignored",
            ])
            .env("ROCKY_CHECKPOINT_CRASH_DIR", dir.path())
            .env("ROCKY_CHECKPOINT_CRASH_STEP", step)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(77), "{step}");
        let (f, keys) = crash_keys();
        let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
        let (c, c_credentials) = open(&f, &dir.path().join("c.db"), &keys[2]).await;
        for replica in [&a, &c] {
            replica.audit(f.public).await.unwrap();
        }
        let rows = all_rows(&f, &a).await;
        assert_eq!(rows.len(), 40, "{step}");
        assert_eq!(rows[&id(3)], f.app().name("edited"), "{step}");
        assert_eq!(all_rows(&f, &c).await.len(), 1, "{step}");
        assert_eq!(
            c.rebootstrap_progress(f.public).await.unwrap().is_some(),
            matches!(step, "import" | "switch"),
            "{step}"
        );
        // The interrupted work completes from what was committed.
        a.checkpoint(f.public).await.unwrap();
        assert_eq!(a.prune(f.public).await.unwrap().blocked, None, "{step}");
        assert_eq!(
            count(&a, "SELECT COUNT(*) FROM secure_operations").await,
            0,
            "{step}"
        );
        let report = crash_rebootstrap(&f, &a, (&c, &keys[2], &c_credentials))
            .await
            .unwrap();
        assert_eq!(report.reapplied.len(), 1, "{step}");
        assert!(report.rejected.is_empty(), "{step}");
        sync(&f, &c, (&a, &keys[0], &a_credentials)).await;
        let rows = all_rows(&f, &a).await;
        assert_eq!(rows.len(), 41, "{step}");
        assert_eq!(rows, all_rows(&f, &c).await, "{step}");
        for replica in [&a, &c] {
            replica.audit(f.public).await.unwrap();
        }
    }
}

#[tokio::test]
async fn bootstrap_refuses_a_valid_checkpoint_signed_by_a_read_only_device() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let writer_key = PrivateKey::new();
    let reader_key = PrivateKey::new();
    let target_key = PrivateKey::new();
    let (writer, _) = open(&f, &dir.path().join("writer.db"), &writer_key).await;
    writer
        .author(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    let reader_config = f.config(
        &reader_key,
        vec![grant(f.public, &[Right::Read, Right::Custody], &[])],
        &[f.public],
    );
    let reader_credentials = reader_config.credentials.clone();
    let reader = EncryptedReplica::open(dir.path().join("reader.db"), f.app(), reader_config)
        .await
        .unwrap();
    sync(&f, &writer, (&reader, &reader_key, &reader_credentials)).await;
    let signed = reader.checkpoint(f.public).await.unwrap();
    assert_eq!(signed.signer(), reader_key.public_key());
    assert_eq!(signed.statement.rows.count, 1);
    let (target, target_credentials) = open(&f, &dir.path().join("target.db"), &target_key).await;
    let error = target.begin_rebootstrap(signed).await.unwrap_err();
    assert!(error.to_string().contains("write authority"));
    assert!(
        target
            .rebootstrap_progress(f.public)
            .await
            .unwrap()
            .is_none()
    );
    assert!(all_rows(&f, &target).await.is_empty());
    assert!(
        reader
            .checkpoint_offer(target_key.public_key(), &target_credentials, f.public)
            .await
            .is_err()
    );
    // The same rows from a writer pass authority and stage normally.
    let signed = writer.checkpoint(f.public).await.unwrap();
    target.begin_rebootstrap(signed).await.unwrap();
    assert!(
        target
            .rebootstrap_progress(f.public)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn checkpoint_signer_expiry_is_checked_at_the_controlled_admission_clock() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let writer_key = PrivateKey::new();
    let target_key = PrivateKey::new();
    let (writer, _) = open(&f, &dir.path().join("writer.db"), &writer_key).await;
    let (target, _) = open(&f, &dir.path().join("target.db"), &target_key).await;
    let signed = writer.checkpoint(f.public).await.unwrap();
    target
        .verify_checkpoint_at_for_test(&signed, u64::MAX - 1)
        .await
        .unwrap();
    assert!(
        target
            .verify_checkpoint_at_for_test(&signed, u64::MAX)
            .await
            .is_err()
    );
    assert!(
        target
            .rebootstrap_progress(f.public)
            .await
            .unwrap()
            .is_none()
    );
}
