use super::*;
use crate::network::NativeSession;
use rocky_credentials::{HorizonAck, Revocation, SignedRevocation};
type Replica = EncryptedReplica<TypedRows>;
fn partition() -> i64 {
    RowIdentity::new(1, 1).unwrap().integer()
}

async fn exchange(from: &Replica, to: &Replica, scope: ResourceScope) {
    let (c, s) = tokio::io::duplex(1024 * 1024);
    let (cn, cc) = {
        let i = from.inner.lock().await;
        (i.config.node.clone(), i.config.credentials.clone())
    };
    let (sn, sc) = {
        let i = to.inner.lock().await;
        (i.config.node.clone(), i.config.credentials.clone())
    };
    let (c, s) = tokio::join!(
        NativeSession::authenticate(c, from, cn, cc, sn.public_key(), MAX_SECURE_BYTES),
        NativeSession::accept(s, to, sn, sc, MAX_SECURE_BYTES)
    );
    let mut c = c.unwrap();
    let mut s = s.unwrap();
    let (result, served) = tokio::join!(
        async {
            c.relay_mergeable_horizon(from, scope).await?;
            c.close().await
        },
        s.serve(to)
    );
    result.unwrap();
    served.unwrap();
}
async fn reader(f: &Fixture, root: &Path, name: &str) -> Replica {
    let r = f
        .open(
            &root.join(name),
            PrivateKey::new(),
            f.grants(
                None,
                &[Right::Read, Right::Insert, Right::Update, Right::Delete],
            ),
            Some((PrivateKey::new(), 1)),
        )
        .await;
    f.keys(&r, partition(), &["lists"]).await;
    r
}
async fn custody(f: &Fixture, root: &Path, node: PrivateKey) -> Replica {
    let r = f
        .open(
            &root.join("custody"),
            node,
            f.grants(None, &[Right::Custody]),
            None,
        )
        .await;
    r.register_partition(f.scope("lists", partition()), f.tuple("lists", partition()))
        .await
        .unwrap();
    r
}
#[tokio::test]
async fn custody_relays_signatures_and_restart_without_any_issuer_participant_session() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let issuer = reader(&f, root.path(), "issuer").await;
    let participant = reader(&f, root.path(), "participant").await;
    let node = PrivateKey::new();
    let relay = custody(&f, root.path(), node.clone()).await;
    let scope = f.scope("lists", partition());
    let signed = declare(&f, &issuer, scope, &[peer(&participant).await]).await;
    exchange(&issuer, &relay, scope).await;
    assert!(relay.acknowledge_horizon(scope).await.is_err());
    exchange(&participant, &relay, scope).await;
    let ack = participant.acknowledge_horizon(scope).await.unwrap();
    exchange(&participant, &relay, scope).await;
    assert_eq!(
        relay.horizon_progress(scope).await.unwrap().acknowledged,
        vec![ack.node]
    );
    let mut altered = ack.clone();
    altered.horizon[0] ^= 1;
    assert!(relay.receive_horizon_ack(scope, altered).await.is_err());
    let pk = participant.inner.lock().await.config.node.clone();
    assert!(
        relay
            .receive_horizon_ack(scope, HorizonAck::sign([5; 32], &pk).unwrap())
            .await
            .is_err()
    );
    assert!(
        relay
            .receive_horizon_ack(
                scope,
                HorizonAck::sign(signed.id().unwrap(), &PrivateKey::new()).unwrap()
            )
            .await
            .is_err()
    );
    relay.flush().await.unwrap();
    drop(relay);
    let relay = custody(&f, root.path(), node).await;
    assert_eq!(
        relay.admitted_horizon(scope).await.unwrap().as_deref(),
        Some(&signed)
    );
    assert_eq!(
        relay.horizon_progress(scope).await.unwrap().acknowledged,
        vec![ack.node]
    );
    participant.finalize_horizon(scope).await.unwrap();
    exchange(&participant, &relay, scope).await;
    let mut altered = *relay.relay_finalized_horizon(scope).await.unwrap().unwrap();
    altered.acks[0].horizon[0] ^= 1;
    assert!(
        issuer
            .receive_relay_finalized_horizon(altered)
            .await
            .is_err()
    );
    exchange(&issuer, &relay, scope).await;
    assert_eq!(
        issuer.horizon_progress(scope).await.unwrap().acknowledged,
        vec![ack.node]
    );
    issuer.finalize_horizon(scope).await.unwrap();
    exchange(&issuer, &relay, scope).await;
    assert!(relay.horizon_relay(scope).await.unwrap().0.is_none());
    assert!(relay.horizon_relay(scope).await.unwrap().1.is_empty());
    for r in [&issuer, &participant] {
        assert_eq!(
            r.finalized_horizon(scope).await.unwrap().as_deref(),
            Some(&signed)
        );
        r.author_bundle(
            [7; 32],
            vec![f.write("lists", partition(), 1, None)],
            vec![],
        )
        .await
        .unwrap();
    }
}
async fn siblings(high_first: bool, reverse: bool, ack_reverse: bool) {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let a = reader(&f, root.path(), "a").await;
    let b = reader(&f, root.path(), "b").await;
    let scope = f.scope("lists", partition());
    let participants = if high_first {
        vec![peer(&b).await]
    } else {
        vec![peer(&a).await, peer(&b).await]
    };
    let first = a.horizon_statement(scope, &participants).await.unwrap();
    let mut second = first.clone();
    second.issued -= 1;
    let first = rocky_credentials::SignedHorizon::sign(first, &f.publisher).unwrap();
    let second = rocky_credentials::SignedHorizon::sign(second, &f.publisher).unwrap();
    let (low, high) = if first.id().unwrap() < second.id().unwrap() {
        (first, second)
    } else {
        (second, first)
    };
    for r in [&a, &b] {
        r.receive_horizon(high.clone()).await.unwrap();
    }
    let old_a = if high_first {
        b.acknowledge_horizon(scope).await.unwrap()
    } else {
        a.acknowledge_horizon(scope).await.unwrap()
    };
    let old_b = b.acknowledge_horizon(scope).await.unwrap();
    if high_first {
        a.receive_horizon_ack(scope, old_b.clone()).await.unwrap();
        a.finalize_horizon(scope).await.unwrap();
    }
    for r in if reverse { [&b, &a] } else { [&a, &b] } {
        r.receive_horizon(low.clone()).await.unwrap();
        r.receive_horizon(high.clone()).await.unwrap();
        assert_eq!(
            r.admitted_horizon(scope).await.unwrap().as_deref(),
            Some(&low)
        );
        assert_eq!(
            r.horizon_progress(scope).await.unwrap().waiting.len(),
            if high_first { 1 } else { 2 }
        );
        assert!(r.receive_horizon_ack(scope, old_a.clone()).await.is_err());
    }
    let ack = b.acknowledge_horizon(scope).await.unwrap();
    if high_first {
        assert!(a.horizon_rebootstrap_required(scope).await.unwrap());
        b.finalize_horizon(scope).await.unwrap();
        restore_scope(&b, &a, scope).await;
    } else {
        let aa = a.acknowledge_horizon(scope).await.unwrap();
        if ack_reverse {
            b.receive_horizon_ack(scope, aa).await.unwrap();
            a.receive_horizon_ack(scope, ack).await.unwrap();
        } else {
            a.receive_horizon_ack(scope, ack).await.unwrap();
            b.receive_horizon_ack(scope, aa).await.unwrap();
        }
        for r in if reverse { [&b, &a] } else { [&a, &b] } {
            r.finalize_horizon(scope).await.unwrap();
        }
    }
    for r in [&a, &b] {
        assert!(!r.horizon_rebootstrap_required(scope).await.unwrap());
        r.author_bundle(
            [9; 32],
            vec![f.write("lists", partition(), 1, None)],
            vec![],
        )
        .await
        .unwrap();
    }
}
#[tokio::test]
async fn concurrent_statements_and_acknowledgments_in_forward_order_choose_lower() {
    siblings(false, false, false).await;
}
#[tokio::test]
async fn concurrent_statements_and_acknowledgments_in_reverse_order_choose_lower() {
    siblings(false, true, true).await;
}
#[tokio::test]
async fn a_finalized_higher_sibling_rebootstraps_to_the_lower() {
    siblings(true, false, false).await;
}
#[tokio::test]
async fn revocation_during_a_pending_cut_removes_the_required_participant() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let a = reader(&f, root.path(), "a").await;
    let b = reader(&f, root.path(), "b").await;
    let scope = f.scope("lists", partition());
    let node = peer(&b).await.0;
    declare(&f, &a, scope, &[peer(&b).await]).await;
    assert_eq!(a.horizon_progress(scope).await.unwrap().waiting, vec![node]);
    let issuer = PrivateKey::new();
    let signed = SignedRevocation::sign(
        Revocation {
            app: f.domain.app,
            issuer: issuer.public_key(),
            time: now().unwrap(),
            nodes: [node].into(),
            users: Default::default(),
            capabilities: Default::default(),
            pseudonyms: Default::default(),
        },
        &f.publisher,
        &issuer,
    )
    .unwrap();
    let set: RevocationSet = vec![signed].try_into().unwrap();
    a.learn_revocations(&set).await.unwrap();
    assert!(a.horizon_progress(scope).await.unwrap().waiting.is_empty());
    a.finalize_horizon(scope).await.unwrap();
}

#[tokio::test]
async fn concurrent_forward_statements_and_reverse_acks_choose_lower() {
    siblings(false, false, true).await;
}
#[tokio::test]
async fn concurrent_reverse_statements_and_forward_acks_choose_lower() {
    siblings(false, true, false).await;
}
#[tokio::test]
async fn a_finalized_higher_sibling_in_reverse_order_rebootstraps_to_lower() {
    siblings(true, true, true).await;
}

#[tokio::test]
async fn a_finalized_higher_sibling_with_a_different_cut_restores_the_winner() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let a = reader(&f, root.path(), "a").await;
    let b = reader(&f, root.path(), "b").await;
    let scope = f.scope("lists", partition());
    let low = rocky_credentials::SignedHorizon::sign(
        b.horizon_statement(scope, &[peer(&b).await]).await.unwrap(),
        &f.publisher,
    )
    .unwrap();
    a.author_bundle(
        [1; 32],
        vec![f.write("lists", partition(), 1, None)],
        vec![],
    )
    .await
    .unwrap();
    let high = a.horizon_statement(scope, &[peer(&a).await]).await.unwrap();
    let high = (0..128)
        .find_map(|offset| {
            let mut statement = high.clone();
            statement.issued -= offset;
            let signed = rocky_credentials::SignedHorizon::sign(statement, &f.publisher).unwrap();
            (signed.id().unwrap() > low.id().unwrap()).then_some(signed)
        })
        .expect("a higher sibling");
    a.receive_horizon(high).await.unwrap();
    a.acknowledge_horizon(scope).await.unwrap();
    a.finalize_horizon(scope).await.unwrap();
    b.receive_horizon(low.clone()).await.unwrap();
    b.acknowledge_horizon(scope).await.unwrap();
    b.finalize_horizon(scope).await.unwrap();
    a.receive_horizon(low.clone()).await.unwrap();
    assert!(a.horizon_rebootstrap_required(scope).await.unwrap());
    assert!(a.acknowledge_horizon(scope).await.is_err());
    restore_scope(&b, &a, scope).await;
    assert_eq!(
        a.finalized_horizon(scope).await.unwrap().as_deref(),
        Some(&low)
    );
    assert_eq!(visible(&a, "lists").await, visible(&b, "lists").await);
    for r in [&a, &b] {
        assert!(!r.horizon_rebootstrap_required(scope).await.unwrap());
        let next = r.next_partition_identity("lists").await.unwrap();
        if next.integer() != partition() {
            f.keys(r, next.integer(), &["lists"]).await;
        }
        r.author_bundle(
            [9; 32],
            vec![f.write("lists", next.integer(), next.sequence(), None)],
            vec![],
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn a_verified_pending_policy_survives_revocation_set_expiry() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let a = reader(&f, root.path(), "a").await;
    let issuer = PrivateKey::new();
    let revoked = PrivateKey::new().public_key();
    let signed = SignedRevocation::sign(
        Revocation {
            app: f.domain.app,
            issuer: issuer.public_key(),
            time: now().unwrap(),
            nodes: [revoked].into(),
            users: Default::default(),
            capabilities: Default::default(),
            pseudonyms: Default::default(),
        },
        &f.publisher,
        &issuer,
    )
    .unwrap();
    let set: RevocationSet = vec![signed].try_into().unwrap();
    a.learn_revocations(&set).await.unwrap();
    let scope = f.scope("lists", partition());
    let cut = declare(&f, &a, scope, &[]).await;
    assert_eq!(cut.statement.policy.len(), 1);
    // Exercise the same removal expiry performs, without a year-long clock wait.
    a.inner.lock().await.config.revocations = set.pruned(u64::MAX);
    a.finalize_horizon(scope).await.unwrap();
}

#[tokio::test]
async fn finalized_relay_evidence_guards_a_stale_nonparticipant_until_checkpoint_import() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new();
    let issuer = reader(&f, root.path(), "issuer").await;
    let offline = reader(&f, root.path(), "offline").await;
    let relay = custody(&f, root.path(), PrivateKey::new()).await;
    let scope = f.scope("lists", partition());
    issuer
        .author_bundle(
            [1; 32],
            vec![f.write("lists", partition(), 1, None)],
            vec![],
        )
        .await
        .unwrap();
    let signed = declare(&f, &issuer, scope, &[]).await;
    issuer.finalize_horizon(scope).await.unwrap();
    exchange(&issuer, &relay, scope).await;
    exchange(&offline, &relay, scope).await;
    assert!(offline.horizon_rebootstrap_required(scope).await.unwrap());
    assert!(offline.finalize_horizon(scope).await.is_err());
    restore_scope(&issuer, &offline, scope).await;
    assert!(!offline.horizon_rebootstrap_required(scope).await.unwrap());
    assert_eq!(
        offline.finalized_horizon(scope).await.unwrap().as_deref(),
        Some(&signed)
    );
    assert_eq!(
        visible(&issuer, "lists").await,
        visible(&offline, "lists").await
    );
}
