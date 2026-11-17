use super::*;
use crate::{CheckpointState, TipStatement};
use rocky_credentials::SignedCheckpointRecord;

async fn list(f: &Fixture, replica: &Replica) {
    let records: Vec<TipStatement> = replica
        .unlisted_checkpoints(f.public.domain)
        .await
        .unwrap()
        .into_iter()
        .map(|statement| {
            TipStatement::Checkpoint(Box::new(
                SignedCheckpointRecord::sign(statement, &f.publisher).unwrap(),
            ))
        })
        .collect();
    assert!(!records.is_empty());
    replica.publish_tip_statements(&records).await.unwrap();
}
async fn open_keyed(f: &Fixture, path: &Path, node: &PrivateKey) -> (Replica, CredentialChain) {
    let config = f.config(node, f.full(), &[f.public, f.private]);
    let credentials = config.credentials.clone();
    (
        EncryptedReplica::open(path, f.app(), config).await.unwrap(),
        credentials,
    )
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn seal_recovery_skips_a_peer_outside_the_missing_scope() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    a.author(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &a).await;
    list(&f, &a).await;
    let (c, c_credentials) = open(&f, &dir.path().join("c.db"), &keys[1]).await;
    let config = f.config(
        &keys[2],
        f.full()
            .into_iter()
            .filter(|g| g.scope == f.public)
            .collect(),
        &[f.public],
    );
    let r_credentials = config.credentials.clone();
    let r = EncryptedReplica::open(dir.path().join("r.db"), f.app(), config)
        .await
        .unwrap();
    share_graph(&f, &a, (&r, &keys[2], &r_credentials)).await;
    let (mut client, server) = network::link::testing::sessions(
        (&c, keys[1].clone(), c_credentials.clone()),
        (&a, keys[0].clone(), a_credentials.clone()),
    )
    .await
    .unwrap();
    tokio::join!(
        async {
            client.sync_tip_graph(&c).await.unwrap();
            c.follow_bookmark(f.public.domain, "main").await.unwrap();
            client.sync_tip_graph(&c).await.unwrap();
            client.rebootstrap(&c, f.public, 8192).await.unwrap();
            client.close().await.unwrap();
        },
        serve(server, &a)
    );
    let before = c.seal_rebootstraps(f.public.domain).await.unwrap();
    assert!(before.iter().any(|need| need.scope == f.private));
    let missing =
        |kind: &crate::BlockerKind| matches!(kind, crate::BlockerKind::SealRebootstrap { .. });
    let private = crate::scope_label(&f.private);
    let blocked = c.blockers().await.unwrap();
    crate::tests::blockers::assert_named(&blocked, &private, missing, crate::ClearsBy::AnyPeer);
    let refusal = crate::tests::blockers::refused(
        c.author(
            handle(2),
            vec![crate::tests::change(
                f.private,
                2,
                None,
                Some(f.app().salary("refused")),
            )],
            vec![],
        )
        .await,
    );
    crate::tests::blockers::assert_named(&refusal, &private, missing, crate::ClearsBy::AnyPeer);
    c.author(handle(3), insert(&f, 3, "public"), vec![])
        .await
        .expect("the public scope it holds writes meanwhile");
    let (mut client, server) = network::link::testing::sessions(
        (&c, keys[1].clone(), c_credentials),
        (&r, keys[2].clone(), r_credentials),
    )
    .await
    .unwrap();
    tokio::join!(
        async {
            client.sync_tip_graph(&c).await.unwrap();
            let request_budget_disclosing_nothing = 0;
            client = client.with_budget(request_budget_disclosing_nothing);
            assert!(client.rebootstrap_at_seals(&c).await.unwrap());
            client.close().await.unwrap();
        },
        serve(server, &r)
    );
    assert_eq!(
        c.seal_rebootstraps(f.public.domain).await.unwrap(),
        before,
        "a peer outside the missing scope cannot serve it"
    );
    crate::tests::blockers::assert_named(
        &c.blockers().await.unwrap(),
        &private,
        missing,
        crate::ClearsBy::AnyPeer,
    );
}
async fn private_row(f: &Fixture, writer: &Replica, row: u8) -> EncryptedFrame {
    writer
        .author(
            [row; 32],
            vec![crate::tests::change(
                f.private,
                row,
                None,
                Some(f.app().salary("731994")),
            )],
            vec![],
        )
        .await
        .unwrap()
}
async fn compacted_and_pruned(
    f: &Fixture,
    a: &Replica,
    (b, b_key, b_credentials): (&Replica, &PrivateKey, &CredentialChain),
) -> SignedCheckpoint {
    for index in 0..10 {
        a.author_buffered(handle(index), insert(f, index, "base"), vec![])
            .await
            .unwrap();
    }
    compacted(f, a).await;
    let empty = a.checkpoint(f.private).await.unwrap();
    assert!(empty.statement.no_frames());
    crate::tests::assert_signed_empty(&empty);
    sync(f, a, (b, b_key, b_credentials)).await;
    a.set_retention_policy(f.public, policy(&[b_key], &[]))
        .await
        .unwrap();
    assert_eq!(a.prune(f.public).await.unwrap().pruned, 10);
    assert_eq!(count(a, "SELECT COUNT(*) FROM secure_operations").await, 0);
    empty
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_joiner_with_only_the_public_key_moves_on_through_a_live_session() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let keys = [
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
    ];
    let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    let (b, b_credentials) = open(&f, &dir.path().join("b.db"), &keys[1]).await;
    let empty = compacted_and_pruned(&f, &a, (&b, &keys[1], &b_credentials)).await;
    list(&f, &a).await;
    let c_path = dir.path().join("c.db");
    let (c, c_credentials) = open(&f, &c_path, &keys[2]).await;
    let (near, far) = network::link::testing::connected().await;
    let (session, served) = network::link::testing::sessions_over(
        near,
        far,
        (&c, keys[2].clone(), c_credentials.clone()),
        (&a, keys[0].clone(), a_credentials.clone()),
    )
    .await
    .unwrap();
    let client = async {
        let mut session = session;
        session.sync_tip_graph(&c).await.unwrap();
        if c.followed_bookmark().await.is_none() {
            c.follow_bookmark(f.public.domain, "main").await.unwrap();
        }
        session.sync_tip_graph(&c).await.unwrap();
        session
            .rebootstrap(&c, f.public, MAX_SECURE_FRAME / 2)
            .await
            .unwrap();
        assert!(
            session.rebootstrap_at_seals(&c).await.unwrap(),
            "the listed no-frames base is the private scope's verified state"
        );
    };
    tokio::join!(client, serve(served, &a));
    assert_eq!(
        c.checkpoint(f.private).await.unwrap().id().unwrap(),
        empty.id().unwrap()
    );
    assert!(c.key_epochs(f.private).await.is_empty());
    assert!(
        c.seal_rebootstraps(f.public.domain)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(all_rows(&f, &c).await, all_rows(&f, &a).await);
    c.author(handle(100), insert(&f, 100, "joined"), vec![])
        .await
        .unwrap();
    seal_main(&c, &f.publisher, f.public.domain).await;
    assert!(
        c.compact(f.public.domain, 0)
            .await
            .unwrap()
            .compacted
            .is_some()
    );
    let later = c.checkpoint(f.private).await.unwrap();
    assert!(
        later.statement.no_frames(),
        "the joiner compacts a later seal keyless"
    );
    assert_eq!(later.statement.sequence, 2);
    drop(c);
    let (c, _) = open(&f, &c_path, &keys[2]).await;
    c.audit(f.public).await.unwrap();
    c.audit(f.private).await.unwrap();
    let (d, d_credentials) = open(&f, &dir.path().join("d.db"), &keys[3]).await;
    share_graph(&f, &c, (&d, &keys[3], &d_credentials)).await;
    let public = c
        .checkpoint_offer(keys[3].public_key(), &d_credentials, f.public)
        .await
        .unwrap()
        .unwrap();
    d.begin_rebootstrap(public.clone()).await.unwrap();
    let mut after = None;
    loop {
        let page = page_of(&c, (&keys[3], &d_credentials), &public, after).await;
        after = page.next;
        d.stage_checkpoint_page(page).await.unwrap();
        if after.is_none() {
            break;
        }
    }
    d.finish_rebootstrap(f.public).await.unwrap();
    let imported =
        crate::tests::import_needed_bases(&c, &d, &keys[3], &d_credentials, f.public.domain).await;
    assert_eq!(imported.len(), 1);
    assert!(imported[0].statement.no_frames());
    assert_eq!(
        imported[0].id().unwrap(),
        later.id().unwrap(),
        "a later joiner starts from the first joiner's own no-frames statement"
    );
    assert_eq!(all_rows(&f, &d).await, all_rows(&f, &c).await);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn covered_keyless_ciphertext_stays_deferred_and_signs_no_empty_state() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let (a, _) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    let (e, _) = open_keyed(&f, &dir.path().join("e.db"), &keys[1]).await;
    let frame = private_row(&f, &e, 7).await;
    assert_ne!(
        a.receive(frame).await.unwrap(),
        ReceiveStatus::Applied,
        "a keyless replica holds the private frame as ciphertext only"
    );
    a.author_buffered(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &a).await;
    assert!(
        a.checkpoint(f.private).await.is_err(),
        "covered keyless ciphertext signs no empty state"
    );
    let peer = PrivateKey::new();
    let credentials = f.credentials(&peer, f.full());
    a.admit_peer(peer.public_key(), &credentials, a.followed_tip().await)
        .await
        .unwrap();
    assert!(
        a.checkpoint_offer(peer.public_key(), &credentials, f.private)
            .await
            .is_ok_and(|offered| offered.is_none())
            || a.checkpoint_offer(peer.public_key(), &credentials, f.private)
                .await
                .is_err(),
        "nothing is offered: no state was ever verified without the payload"
    );
}

fn resigned(
    base: &SignedCheckpoint,
    node: &PrivateKey,
    credentials: &CredentialChain,
    edit: impl FnOnce(&mut crate::CheckpointStatement),
) -> SignedCheckpoint {
    let mut statement = base.statement.clone();
    edit(&mut statement);
    SignedCheckpoint::sign(statement, node, credentials.clone()).unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn malformed_forged_or_unauthorized_no_frames_records_are_refused() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    let (b, b_credentials) = open(&f, &dir.path().join("b.db"), &keys[1]).await;
    let empty = compacted_and_pruned(&f, &a, (&b, &keys[1], &b_credentials)).await;
    let (c, c_credentials) = open(&f, &dir.path().join("c.db"), &keys[2]).await;
    share_graph(&f, &a, (&c, &keys[2], &c_credentials)).await;
    let refused = |signed: SignedCheckpoint| {
        let c = &c;
        async move {
            let before = rows(
                &c.inner.lock(crate::replica::Class::Background).await.conn,
                "SELECT total_changes()",
            )
            .await
            .unwrap();
            assert!(c.begin_rebootstrap(signed).await.is_err());
            assert!(c.rebootstrap_progress(f.private).await.unwrap().is_none());
            assert_eq!(
                rows(
                    &c.inner.lock(crate::replica::Class::Background).await.conn,
                    "SELECT total_changes()"
                )
                .await
                .unwrap(),
                before
            );
        }
    };
    let breaks_the_empty_shape = [
        (|s: &mut crate::CheckpointStatement| s.operations = 1) as fn(&mut _),
        |s| s.clock = 1,
        |s| s.tombstones = 1,
        |s| s.rows.count = 1,
        |s| s.creations.count = 1,
        |s| s.moves.count = 1,
        |s| s.custody.superseded = 1,
        |s| s.seal = None,
        |s| {
            s.identities = vec![crate::replica::checkpoint::CoveredMark {
                own: 1,
                author: rocky_core::WriterId::from_bytes([7; 32]),
                signer: PrivateKey::new().public_key(),
                sequence: 1,
            }]
        },
        |s| {
            s.frontier = vec![crate::LogPosition {
                author: PrivateKey::new().public_key(),
                sequence: 0,
                hash: Hash::new(b"covered"),
                clock: 0,
            }]
        },
    ];
    for edit in breaks_the_empty_shape {
        refused(resigned(&empty, &keys[0], &a_credentials, edit)).await;
    }
    let fabricated_key_epoch = |s: &mut crate::CheckpointStatement| {
        s.state = CheckpointState::Keyed {
            key_epoch: rocky_core::KeyEpoch::new(1, &[22; 32]),
        }
    };
    refused(resigned(
        &empty,
        &keys[0],
        &a_credentials,
        fabricated_key_epoch,
    ))
    .await;
    let tip_other_than_the_seals = rocky_credentials::genesis_tip([9; 32]);
    refused(resigned(&empty, &keys[0], &a_credentials, |s| {
        s.tip = tip_other_than_the_seals
    }))
    .await;
    let claims_bound_by_the_signature = [
        (|s: &mut crate::CheckpointStatement| s.scope.table = [3; 32]) as fn(&mut _),
        |s| s.scope.domain.domain = rocky_core::DomainId([5; 16]),
        |s| s.seal = Some([4; 32]),
        |s| s.tip = rocky_credentials::genesis_tip([8; 32]),
        |s| s.sequence += 1,
    ];
    for edit in claims_bound_by_the_signature {
        let mut forged = empty.clone();
        edit(&mut forged.statement);
        refused(forged).await;
    }
    let custodian = PrivateKey::new();
    let custody = f.credentials(
        &custodian,
        [f.public, f.private]
            .into_iter()
            .map(|scope| crate::tests::grant(scope, &[Right::Custody], &[]))
            .collect(),
    );
    let signed_by_custody_alone = resigned(&empty, &custodian, &custody, |_| {});
    refused(signed_by_custody_alone).await;
    import_no_frames(&f, &a, (&c, &keys[2], &c_credentials)).await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn covered_private_history_contradicts_a_no_frames_claim_now_or_later() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let keys = [
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
    ];
    let (a, a_credentials) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    a.author_buffered(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &a).await;
    let empty = a.checkpoint(f.private).await.unwrap();
    let (e, e_credentials) = open_keyed(&f, &dir.path().join("e.db"), &keys[1]).await;
    let (c, c_credentials) = open(&f, &dir.path().join("c.db"), &keys[2]).await;
    for (replica, key, credentials) in [
        (&e, &keys[1], &e_credentials),
        (&c, &keys[2], &c_credentials),
    ] {
        share_graph(&f, &a, (replica, key, credentials)).await;
    }
    let covered = private_row(&f, &e, 9).await;
    assert_ne!(
        c.receive(covered.clone()).await.unwrap(),
        ReceiveStatus::Applied,
        "a keyless holder keeps the private row as ciphertext"
    );
    sync(&f, &a, (&c, &keys[2], &c_credentials)).await;
    seal_main(&c, &f.publisher, f.public.domain).await;
    let graph = c.tip_graph(f.public.domain).await.unwrap();
    let sealed = graph
        .seal(covered.operation().unwrap().header.extensions.tip)
        .unwrap()
        .unwrap()
        .clone();
    let no_frames_claim_at_the_seal = resigned(&empty, &keys[0], &a_credentials, |s| {
        s.seal = Some(sealed.id().unwrap());
        s.tip = sealed.statement.tip;
        s.sequence += 1;
        s.previous = Some(empty.id().unwrap());
    });
    let error = c
        .begin_rebootstrap(no_frames_claim_at_the_seal.clone())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("contradicts"),
        "a no-frames claim is refused where its history is held: {error}"
    );
    let (d, d_credentials) = open(&f, &dir.path().join("d.db"), &keys[3]).await;
    for statements in [
        crate::tests::all_tip_statements(&c).await,
        crate::tests::all_tip_statements(&a).await,
    ] {
        d.receive_tip_statements(&statements).await.unwrap();
    }
    d.follow_bookmark(f.public.domain, "main").await.unwrap();
    d.begin_rebootstrap(no_frames_claim_at_the_seal)
        .await
        .expect("a replica without the history accepts the claim");
    d.finish_rebootstrap(f.private).await.unwrap();
    let error = d.receive(covered.clone()).await.unwrap_err();
    assert!(
        error.to_string().contains("contradicts"),
        "the covered frame arriving later is refused: {error}"
    );
    assert_eq!(
        count(&d, "SELECT COUNT(*) FROM secure_operations").await,
        0,
        "the contradicting frame is never stored"
    );
    e.receive_tip_statements(&crate::tests::all_tip_statements(&c).await)
        .await
        .unwrap();
    let after = private_row(&f, &e, 10).await;
    assert!(
        d.receive(after).await.is_ok(),
        "a frame after the claimed seal follows ordinary late-frame rules"
    );
    let _ = d_credentials;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_late_key_applies_the_private_suffix_past_a_no_frames_base() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let (a, _) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    let (b, b_credentials) = open(&f, &dir.path().join("b.db"), &keys[1]).await;
    let empty = compacted_and_pruned(&f, &a, (&b, &keys[1], &b_credentials)).await;
    let (e, e_credentials) = open_keyed(&f, &dir.path().join("e.db"), &keys[2]).await;
    share_graph(&f, &a, (&e, &keys[2], &e_credentials)).await;
    let frame = private_row(&f, &e, 9).await;
    assert_ne!(a.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    seal_main(&a, &f.publisher, f.public.domain).await;
    a.compact(f.public.domain, 0).await.unwrap();
    assert_eq!(
        a.checkpoint(f.private).await.unwrap().id().unwrap(),
        empty.id().unwrap(),
        "keyless, the no-frames base stays and nothing new is signed"
    );
    a.install_key_epoch(KeyMaterial::new(f.private, 1, [22; 32]))
        .await
        .unwrap();
    let report = a.compact(f.public.domain, 0).await.unwrap();
    assert_eq!(
        report.waiting, None,
        "pruned public positions below the first seal do not stall it: {report:?}"
    );
    let keyed = a.checkpoint(f.private).await.unwrap();
    assert!(keyed.statement.key_epoch().is_some());
    assert_eq!(keyed.statement.operations, 1);
    assert_eq!(keyed.statement.rows.count, 1);
    a.audit(f.private).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_tampered_stored_no_frames_base_fails_open_and_full_audit() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let path = dir.path().join("a.db");
    let (a, _) = open(&f, &path, &keys[0]).await;
    a.author_buffered(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &a).await;
    a.audit(f.private).await.unwrap();
    {
        let mut inner = a.inner.lock(crate::replica::Class::Background).await;
        inner
            .conn
            .execute(
                "INSERT INTO secure_checkpoint_rows SELECT (SELECT MAX(id) FROM secure_scope_state),row,fields FROM secure_checkpoint_rows WHERE scope=(SELECT MIN(id) FROM secure_scope_state)",
                (),
            )
            .await
            .unwrap();
        inner.flush().unwrap();
    }
    assert!(
        a.audit(f.private).await.is_err(),
        "a stored row under the keyless no-frames base fails the audit"
    );
    drop(a);
    let reopened =
        EncryptedReplica::open(&path, f.app(), f.config(&keys[0], f.full(), &[f.public])).await;
    assert!(reopened.is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_custody_relay_serves_the_original_no_frames_statement() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let (a, _) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    a.author_buffered(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &a).await;
    let empty = a.checkpoint(f.private).await.unwrap();
    let custody_only_no_key: Vec<_> = [f.public, f.private]
        .into_iter()
        .map(|scope| crate::tests::grant(scope, &[Right::Custody], &[]))
        .collect();
    let r_config = f.config(&keys[1], custody_only_no_key, &[]);
    let r_credentials = r_config.credentials.clone();
    let r = EncryptedReplica::open(dir.path().join("r.db"), f.app(), r_config)
        .await
        .unwrap();
    share_graph(&f, &a, (&r, &keys[1], &r_credentials)).await;
    import_no_frames(&f, &a, (&r, &keys[1], &r_credentials)).await;
    seal_main(&r, &f.publisher, f.public.domain).await;
    r.compact(f.public.domain, 0).await.unwrap();
    assert_eq!(
        r.checkpoint(f.private).await.unwrap().id().unwrap(),
        empty.id().unwrap(),
        "a custody relay signs nothing itself"
    );
    let (d, d_credentials) = open(&f, &dir.path().join("d.db"), &keys[2]).await;
    share_graph(&f, &r, (&d, &keys[2], &d_credentials)).await;
    let relayed = import_no_frames(&f, &r, (&d, &keys[2], &d_credentials)).await;
    assert_eq!(relayed.id().unwrap(), empty.id().unwrap());
    assert_eq!(relayed.signer(), empty.signer());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_interrupted_no_frames_switch_reopens_and_finishes_without_churn() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let (a, _) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    a.author_buffered(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &a).await;
    let empty = a.checkpoint(f.private).await.unwrap();
    let revision = a.stored_and_memory_revision().await;
    a.compact(f.public.domain, 0).await.unwrap();
    a.compact(f.public.domain, 0).await.unwrap();
    assert_eq!(
        a.stored_and_memory_revision().await,
        revision,
        "maintenance again changes nothing"
    );
    assert_eq!(
        a.checkpoint(f.private).await.unwrap().id().unwrap(),
        empty.id().unwrap()
    );
    let path = dir.path().join("b.db");
    let (b, b_credentials) = open(&f, &path, &keys[1]).await;
    share_graph(&f, &a, (&b, &keys[1], &b_credentials)).await;
    a.admit_peer(keys[1].public_key(), &b_credentials, b.followed_tip().await)
        .await
        .unwrap();
    b.begin_rebootstrap(empty.clone()).await.unwrap();
    b.inject_stage_fault(Stage::Switch, 0).await;
    assert!(matches!(
        b.finish_rebootstrap(f.private).await,
        Err(Error::InjectedFailure)
    ));
    drop(b);
    let (b, _) = open(&f, &path, &keys[1]).await;
    assert_eq!(
        b.rebootstrap_progress(f.private).await.unwrap(),
        Some((empty.id().unwrap(), None, true)),
        "the staged statement is validated at open and finishes as it was"
    );
    let report = b.finish_rebootstrap(f.private).await.unwrap();
    assert_eq!(report.superseded, 0);
    assert!(report.reapplied.is_empty());
    assert_eq!(count(&b, "SELECT COUNT(*) FROM secure_logs").await, 0);
    assert_eq!(count(&b, "SELECT COUNT(*) FROM secure_ack").await, 0);
    assert_eq!(
        count(&b, "SELECT COUNT(*) FROM secure_identity_marks").await,
        0
    );
    b.audit(f.private).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_pulled_seal_state_can_be_a_no_frames_record() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let (a, _) = open(&f, &dir.path().join("a.db"), &keys[0]).await;
    a.author_buffered(handle(1), insert(&f, 1, "base"), vec![])
        .await
        .unwrap();
    compacted(&f, &a).await;
    let empty = a.checkpoint(f.private).await.unwrap();
    let sealed = empty.statement.seal.unwrap();
    let (b, b_credentials) = open(&f, &dir.path().join("b.db"), &keys[1]).await;
    share_graph(&f, &a, (&b, &keys[1], &b_credentials)).await;
    let offered = a
        .seal_checkpoint_offer(keys[1].public_key(), &b_credentials, f.private, sealed)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(offered.id().unwrap(), empty.id().unwrap());
    b.begin_seal_pull(offered).await.unwrap();
    assert_eq!(
        b.rebootstrap_progress(f.private).await.unwrap(),
        Some((empty.id().unwrap(), None, true))
    );
    assert_eq!(b.finish_seal_pull(f.private).await.unwrap(), sealed);
    assert_eq!(
        count(&b, "SELECT COUNT(*) FROM secure_evicted_logs").await,
        0,
        "fetching a seal's state creates no restoration obligation"
    );
}

mod lifecycle;
