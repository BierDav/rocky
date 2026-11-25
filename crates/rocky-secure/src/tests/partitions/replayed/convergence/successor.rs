use super::*;
use crate::CrashPoint;
use rocky_credentials::{BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SignedBookmarkMove};

async fn follow_main(f: &Fixture, replica: &EncryptedReplica<TypedRows>) {
    let created = TipStatement::Bookmark(
        SignedBookmarkMove::sign(
            BookmarkMoveStatement {
                format: BOOKMARK_MOVE_FORMAT,
                scope: f.domain,
                name: "main".into(),
                from: None,
                to: Some(replica.followed_tip().await),
            },
            &f.publisher,
        )
        .unwrap(),
    );
    replica.publish_tip_statements(&[created]).await.unwrap();
    replica.follow_bookmark(f.domain, "main").await.unwrap();
}

fn raise(by: i64) -> (&'static str, Vec<SqlValue>) {
    (
        "UPDATE items SET amount=amount+? WHERE list='A' AND key='x'",
        vec![SqlValue::Integer(by)],
    )
}

fn intents(f: &Fixture, frames: &[EncryptedFrame]) -> usize {
    frames
        .iter()
        .filter(|frame| payload(frame, key_of(f, frame), f).intent().is_some())
        .count()
}

struct Offline {
    owner: Author,
    a: Author,
    b: Author,
    sealed: Vec<EncryptedFrame>,
    offline: Vec<Vec<EncryptedFrame>>,
}

async fn offline_history(f: &Fixture, root: &Path) -> Offline {
    let mut owner = Author::open(f, root, 1).await;
    let mut a = Author::open(f, root, 2).await;
    let mut b = Author::open(f, root, 3).await;
    follow_main(f, &owner.replica).await;
    for device in [&a, &b] {
        exchange_graphs(&owner.replica, &device.replica, f.domain).await;
    }
    owner.create(f, &[("A", "x", 1)], 1).await;
    let created = owner.fresh().await;
    a.receive(&created).await;
    b.receive(&created).await;

    a.run(f, &[raise(1)], 1).await;
    let a1 = a.fresh().await;
    b.receive(&a1).await;
    b.run(f, &[raise(10)], 1).await;
    let b1 = b.fresh().await;
    b.create(f, &[("A", "k", 3)], 2).await;
    let b2 = b.fresh().await;
    a.receive(&b1).await;
    a.receive(&b2).await;
    assert_eq!(
        keys(f, &a.replica, &["A"]).await,
        vec![
            vec![text("A"), text("k"), SqlValue::Integer(3)],
            vec![text("A"), text("x"), SqlValue::Integer(12)],
        ]
    );

    owner
        .run(
            f,
            &[(
                "UPDATE items SET amount=amount*2 WHERE list='A' AND key='x'",
                vec![],
            )],
            2,
        )
        .await;
    owner.create(f, &[("A", "k", 5)], 3).await;
    let sealed = created.iter().cloned().chain(owner.fresh().await).collect();
    seal_main(&owner.replica, &f.publisher, f.domain).await;
    owner.replica.finalize_path(f.domain).await.unwrap();
    Offline {
        owner,
        a,
        b,
        sealed,
        offline: vec![a1, b1, b2],
    }
}

impl Offline {
    fn offline(&self) -> Vec<EncryptedFrame> {
        self.offline.iter().flatten().cloned().collect()
    }

    fn expected(&self, f: &Fixture) -> Canonical {
        canonical_sealed(f, &[&self.sealed, &self.offline()], 1)
    }

    async fn reconnect_a(&self, f: &Fixture) {
        exchange_graphs(&self.owner.replica, &self.a.replica, f.domain).await;
        self.a.receive(&self.sealed).await;
    }
}

fn outcome(dropped: Option<DropReason>, sealed: bool) -> TransactionOutcome {
    match dropped {
        None => TransactionOutcome::Applied { sealed },
        Some(reason) => TransactionOutcome::Dropped { reason, sealed },
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn offline_peers_transactions_replay_after_the_seal_in_its_successor() {
    let f = Fixture::build_class(false, PrivateKey::new(), "replayed");
    let root = rocky_platform::fs::Scratch::new().await;
    let mut h = offline_history(&f, root.path()).await;
    let late_k = transaction(&h.offline[2][0], [1; 32], &f);
    assert_eq!(
        h.a.replica.transaction_outcome(late_k).await.unwrap(),
        Some(outcome(None, false))
    );

    h.reconnect_a(&f).await;
    h.a.replica.finalize_path(f.domain).await.unwrap();
    let expected = h.expected(&f);
    assert_eq!(
        expected.rows,
        vec![
            vec![text("A"), text("k"), SqlValue::Integer(5)],
            vec![text("A"), text("x"), SqlValue::Integer(13)],
        ]
    );
    assert_eq!(
        expected.outcomes[&late_k],
        outcome(Some(DropReason::Constraint), false),
        "B's `k`, applied before, drops behind the owner's once A finalizes the seal"
    );
    assert_eq!(state(&f, &h.a.replica, &expected).await, expected);
    assert_eq!(
        intents(&f, &h.a.fresh().await),
        0,
        "A reauthors nothing: it relays B's original frames"
    );
    assert!(h.a.replica.held_changes().await.unwrap().is_empty());

    let offline_frames_relayed_by_a = h.offline();
    for _ in 0..2 {
        h.owner.receive(&offline_frames_relayed_by_a).await;
        assert_eq!(
            state(&f, &h.owner.replica, &expected).await,
            expected,
            "the owner converges on frames relayed by A, received twice, before B reconnects"
        );
    }
    let node = h.a.node.clone();
    drop(h.a);
    h.a = Author::at(&f, root.path(), 2, node).await;
    assert_eq!(
        state(&f, &h.a.replica, &expected).await,
        expected,
        "a restart keeps the finalized state"
    );

    seal_main(&h.owner.replica, &f.publisher, f.domain).await;
    h.owner.replica.finalize_path(f.domain).await.unwrap();
    let last = canonical_sealed(&f, &[&h.sealed, &offline_frames_relayed_by_a], 2);
    assert_eq!(
        last.outcomes[&late_k],
        outcome(Some(DropReason::Constraint), true),
        "the owner's next seal covers B's drop and makes it final"
    );
    exchange_graphs(&h.owner.replica, &h.b.replica, f.domain).await;
    h.b.receive(&h.sealed).await;
    h.b.replica.finalize_path(f.domain).await.unwrap();
    for replica in [&h.owner.replica, &h.b.replica] {
        assert_eq!(state(&f, replica, &last).await, last);
        assert_eq!(replica.unsealed_replayed_transactions().await.unwrap(), 0);
    }
    assert_eq!(
        intents(&f, &h.b.fresh().await),
        0,
        "B, back at last, converges without authoring anything again"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn every_order_around_the_successor_converges_to_the_sealed_replay() {
    let f = Fixture::build_class(false, PrivateKey::new(), "replayed");
    let root = rocky_platform::fs::Scratch::new().await;
    let h = offline_history(&f, root.path()).await;
    let expected = h.expected(&f);
    let units: Vec<Vec<EncryptedFrame>> = std::iter::once(h.sealed.clone())
        .chain(h.offline.iter().cloned())
        .collect();
    for (index, order) in permutations(units.len()).into_iter().enumerate() {
        let graph = index % (units.len() + 1);
        let path = root.path().join(format!("r{index}.sqlite"));
        let node = PrivateKey::new();
        let replica = receiver(&f, &path, &node).await;
        for (position, unit) in order.iter().enumerate() {
            if position == graph {
                exchange_graphs(&h.owner.replica, &replica, f.domain).await;
            }
            for status in replica.receive_all(units[*unit].clone()).await {
                assert_ne!(status.unwrap(), ReceiveStatus::Rejected);
            }
        }
        if graph == units.len() {
            exchange_graphs(&h.owner.replica, &replica, f.domain).await;
        }
        replica.finalize_path(f.domain).await.unwrap();
        assert_eq!(
            state(&f, &replica, &expected).await,
            expected,
            "{order:?} {graph}"
        );
        drop(replica);
        rocky_platform::fs::remove_file(&path).ok();
    }
}

async fn finalization_crash(point: CrashPoint) {
    let f = Fixture::build_class(false, PrivateKey::new(), "replayed");
    let root = rocky_platform::fs::Scratch::new().await;
    let mut h = offline_history(&f, root.path()).await;
    let unsealed = h.a.replica.unsealed_replayed_transactions().await.unwrap();
    h.a.replica.arm_crash(Some(point)).await;
    h.reconnect_a(&f).await;
    assert_eq!(
        h.a.replica.crashed().await,
        Some(point),
        "the receive completing the seal's members finalizes it after its group commits, and that step crashes"
    );
    let node = h.a.node.clone();
    drop(h.a);

    h.a = Author::at(&f, root.path(), 2, node).await;
    assert!(
        h.a.replica
            .compaction_status(f.domain)
            .await
            .unwrap()
            .finalized
            .is_none()
    );
    let every_received_transaction_open_in_stamp_order = canonical(
        &f,
        &h.sealed
            .iter()
            .cloned()
            .chain(h.offline())
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        state(
            &f,
            &h.a.replica,
            &every_received_transaction_open_in_stamp_order
        )
        .await,
        every_received_transaction_open_in_stamp_order
    );
    assert_eq!(
        h.a.replica.unsealed_replayed_transactions().await.unwrap(),
        unsealed + 2
    );
    h.a.replica.finalize_path(f.domain).await.unwrap();
    let expected = h.expected(&f);
    assert_eq!(state(&f, &h.a.replica, &expected).await, expected);
    assert_eq!(
        h.a.replica.unsealed_replayed_transactions().await.unwrap(),
        3,
        "only the transactions the seal left out keep undo records"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_crash_while_a_seal_deletes_undo_records_reopens_and_converges() {
    finalization_crash(CrashPoint::UndoDeletion).await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_crash_while_a_seal_rewinds_its_late_transactions_reopens_and_converges() {
    finalization_crash(CrashPoint::Rewind).await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_crash_while_a_seal_redoes_its_late_transactions_reopens_and_converges() {
    finalization_crash(CrashPoint::Redo).await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn compaction_signs_a_replayed_base_and_a_later_member_replays_the_frames() {
    let f = Fixture::build_class(false, PrivateKey::new(), "replayed");
    let root = rocky_platform::fs::Scratch::new().await;
    let mut owner = Author::open(&f, root.path(), 1).await;
    follow_main(&f, &owner.replica).await;
    owner.create(&f, &[("A", "x", 1)], 1).await;
    owner.run(&f, &[raise(4)], 2).await;
    let frames = owner.fresh().await;
    seal_main(&owner.replica, &f.publisher, f.domain).await;
    let report = owner.replica.compact(f.domain, 0).await.unwrap();
    assert!(report.compacted.is_some());
    assert_eq!(
        owner.replica.prune(f.scope("A")).await.unwrap().blocked,
        Some(crate::PruneBlock::NoPolicy)
    );
    let later = Author::open(&f, root.path(), 2).await;
    exchange_graphs(&owner.replica, &later.replica, f.domain).await;
    later.receive(&frames).await;
    assert!(
        later
            .replica
            .seal_rebootstraps(f.domain)
            .await
            .unwrap()
            .is_empty()
    );
    let expected = canonical_sealed(&f, &[&frames], 1);
    for replica in [&owner.replica, &later.replica] {
        assert_eq!(state(&f, replica, &expected).await, expected);
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_list_held_after_a_seal_publishes_no_outcome_before_its_sealed_rows_arrive() {
    let f = Fixture::build_class(false, PrivateKey::new(), "replayed");
    let root = rocky_platform::fs::Scratch::new().await;
    let mut owner = Author::open(&f, root.path(), 1).await;
    let mut writer = Author::open(&f, root.path(), 2).await;
    follow_main(&f, &owner.replica).await;
    owner.create(&f, &[("A", "x", 1)], 1).await;
    let sealed = owner.fresh().await;
    writer.create(&f, &[("A", "x", 2), ("B", "y", 1)], 1).await;
    let late = writer.fresh().await;
    seal_main(&owner.replica, &f.publisher, f.domain).await;
    owner.replica.finalize_path(f.domain).await.unwrap();

    let node = PrivateKey::new();
    let granted = f
        .open(
            &root.path().join("granted.sqlite"),
            node.clone(),
            f.grant(None, &all()),
        )
        .await;
    f.key(&granted, "B", [2; 32]).await;
    exchange_graphs(&owner.replica, &granted, f.domain).await;
    granted.follow_bookmark(f.domain, "main").await.unwrap();
    let credentials = granted
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .credentials
        .clone();
    for request in granted.wanted_placements().await.unwrap() {
        let page = owner
            .replica
            .placement_page(node.public_key(), &credentials, request)
            .await
            .unwrap();
        granted.receive_placements(request, page).await.unwrap();
    }
    granted.finalize_path(f.domain).await.unwrap();
    assert!(
        granted
            .compaction_status(f.domain)
            .await
            .unwrap()
            .finalized
            .is_some(),
        "holding only B, the device finalizes the seal over A's rows it never saw"
    );
    f.key(&granted, "A", [1; 32]).await;
    for status in granted.receive_all(late.clone()).await {
        assert_ne!(status.unwrap(), ReceiveStatus::Rejected);
    }
    let txn = transaction(&late[0], key_of(&f, &late[0]), &f);
    let published = |name: &'static str| {
        let (f, granted, node) = (&f, &granted, &node);
        async move {
            records(f, granted, node, name)
                .await
                .iter()
                .map(|frame| {
                    payload(frame, key_of(f, frame), f)
                        .outcome()
                        .unwrap()
                        .dropped
                })
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        published("A").await,
        vec![],
        "without the seal's rows of A the device cannot know the transaction's outcome"
    );
    for status in granted.receive_all(sealed.clone()).await {
        assert_ne!(status.unwrap(), ReceiveStatus::Rejected);
    }
    granted.finalize_path(f.domain).await.unwrap();
    assert_eq!(
        granted.transaction_outcome(txn).await.unwrap(),
        Some(TransactionOutcome::Dropped {
            reason: DropReason::Constraint,
            sealed: false
        })
    );
    for name in ["A", "B"] {
        assert!(
            published(name).await.iter().all(Option::is_some),
            "every record the device published drops the transaction"
        );
    }
    let mut records_of_granted = records(&f, &granted, &node, "A").await;
    records_of_granted.extend(records(&f, &granted, &node, "B").await);
    owner.receive(&late).await;
    owner.receive(&records_of_granted).await;
    assert!(
        owner
            .replica
            .outcome_divergences()
            .await
            .unwrap()
            .is_empty()
    );
}
