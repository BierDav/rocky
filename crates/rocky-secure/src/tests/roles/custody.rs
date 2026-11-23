use super::*;
use rocky_credentials::PeerId;

pub(super) async fn policy(node: &Node, scope: ResourceScope, peers: &[&Node], copies: usize) {
    node.replica
        .set_partition_custody(
            scope,
            PartitionCustody {
                peers: peers
                    .iter()
                    .map(|node| PeerId::of(node.key.public_key()))
                    .collect(),
                copies,
            },
        )
        .await
        .unwrap();
}
async fn keyless(f: &Fixture, path: &Path, key: &PrivateKey, scopes: &[ResourceScope]) -> Node {
    let credentials = f.credentials(
        key,
        scopes
            .iter()
            .map(|scope| grant(*scope, &[Right::Custody], &[]))
            .collect(),
    );
    let replica = EncryptedReplica::open(
        path,
        f.app(),
        SecureConfig {
            publisher: f.publisher.public_key(),
            manifest: f.manifest.clone(),
            revocations: f.policy.clone(),
            node: key.clone(),
            credentials: credentials.clone(),
            keys: vec![],
        },
    )
    .await
    .unwrap();
    Node {
        key: key.clone(),
        credentials,
        replica,
    }
}
async fn count(node: &Node, scope: ResourceScope) -> usize {
    node.replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .custody_coverage(scope)
        .await
        .unwrap()
}
async fn push(node: &Node, keeper: &Node, scope: ResourceScope) {
    let (mut session, server) = connect(node, keeper).await;
    session.push(&node.replica, scope, QUOTA).await.unwrap();
    drop(session);
    server.await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn registry_and_chat_have_independent_durable_custody_and_loss_reporting() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let scopes = departments(&f, 2);
    let (registry, chat) = (scopes[0], scopes[1]);
    let owner_key = PrivateKey::new();
    let owner_path = dir.path().join("owner.db");
    let owner = open_node(&f, &owner_path, &owner_key, &scopes, &scopes).await;
    let keeper = keyless(
        &f,
        &dir.path().join("keeper.db"),
        &PrivateKey::new(),
        &scopes,
    )
    .await;
    owner.replica.set_role(partial()).await.unwrap();
    keeper.replica.set_role(partial()).await.unwrap();
    for scope in &scopes {
        owner.replica.subscribe(*scope).await.unwrap();
        keeper.replica.subscribe(*scope).await.unwrap();
        insert(&f, &owner, *scope, 0..6, 2).await;
        owner
            .replica
            .set_retention_policy(*scope, keep_one_copy())
            .await
            .unwrap();
    }
    policy(&owner, registry, &[&keeper], 1).await;
    owner
        .replica
        .set_partition_custody(chat, PartitionCustody::default())
        .await
        .unwrap();
    assert!(owner.replica.evict(registry).await.is_err());
    assert!(owner.replica.unsubscribe(registry).await.is_err());
    push(&owner, &keeper, chat).await;
    compacted_base(&owner.replica, &f.publisher, chat).await;
    push(&owner, &keeper, chat).await;
    let report = owner.replica.prune(chat).await.unwrap();
    assert_eq!(report.blocked, None, "{report:?}");
    let retained = owner.replica.checkpoint_status(chat).await.unwrap();
    assert_eq!(retained.covered, 0, "{retained:?}");
    assert_eq!(retained.pruned_prefix.len(), 1, "{retained:?}");
    assert_eq!(
        owner.replica.subscription(chat).await.unwrap(),
        Some(Holding::Held)
    );
    compacted_base(&owner.replica, &f.publisher, registry).await;
    assert_eq!(owner.replica.prune(registry).await.unwrap().pruned, 0);
    push(&owner, &keeper, registry).await;
    assert_eq!(count(&owner, registry).await, 1);
    assert_eq!(owner.replica.prune(registry).await.unwrap().pruned, 3);
    assert_eq!(stored_rows(&keeper.replica, registry).await, 0);
    assert!(keeper.replica.key_epochs(registry).await.is_empty());
    assert_eq!(
        keeper
            .replica
            .frontier(registry)
            .await
            .unwrap()
            .logs
            .iter()
            .map(|log| log.applied)
            .sum::<u64>(),
        0
    );
    assert!(
        keeper
            .replica
            .evict(chat)
            .await
            .unwrap()
            .recovery_unconfirmed,
        "custody zero never evicts on its own, so chat is discarded explicitly"
    );
    let report = owner.replica.evict(chat).await.unwrap();
    assert!(report.recovery_unconfirmed);
    let policy_before = owner.replica.partition_custody(registry).await.unwrap();
    owner.replica.flush().await.unwrap();
    drop(owner);
    let owner = open_node(&f, &owner_path, &owner_key, &scopes, &scopes).await;
    assert_eq!(
        owner.replica.partition_custody(registry).await.unwrap(),
        policy_before
    );
    assert_eq!(
        owner.replica.partition_custody(chat).await.unwrap(),
        PartitionCustody::default()
    );
    assert_eq!(count(&owner, registry).await, 1);
    assert!(
        !owner
            .replica
            .evict(registry)
            .await
            .unwrap()
            .recovery_unconfirmed
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn receipts_are_bound_to_challenges_partitions_and_live_custodians() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let scopes = departments(&f, 2);
    let scope = scopes[0];
    let owner = open_node(
        &f,
        &dir.path().join("owner.db"),
        &PrivateKey::new(),
        &scopes,
        &scopes,
    )
    .await;
    let keeper = keyless(
        &f,
        &dir.path().join("keeper.db"),
        &PrivateKey::new(),
        &scopes,
    )
    .await;
    insert(&f, &owner, scope, 0..2, 1).await;
    policy(&owner, scope, &[&keeper], 1).await;
    push(&owner, &keeper, scope).await;
    let (binding, challenge) = ([3; 32], [4; 32]);
    owner
        .replica
        .begin_custody_challenge(keeper.key.public_key(), scope, binding, challenge)
        .await
        .unwrap();
    let receipt = keeper
        .replica
        .custody_receipt(
            owner.key.public_key(),
            &owner.credentials,
            scope,
            binding,
            challenge,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        owner
            .replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .check_receipt_at(&receipt, now().unwrap() + 301 * rocky_credentials::DAY)
            .is_err()
    );
    assert!(
        owner
            .replica
            .record_custody_receipt(
                keeper.key.public_key(),
                scope,
                binding,
                challenge,
                receipt.forged()
            )
            .await
            .is_err()
    );
    assert!(
        owner
            .replica
            .record_custody_receipt(
                keeper.key.public_key(),
                scopes[1],
                binding,
                challenge,
                receipt.clone()
            )
            .await
            .is_err()
    );
    owner
        .replica
        .record_custody_receipt(
            keeper.key.public_key(),
            scope,
            binding,
            challenge,
            receipt.clone(),
        )
        .await
        .unwrap();
    assert!(
        owner
            .replica
            .record_custody_receipt(
                keeper.key.public_key(),
                scope,
                binding,
                challenge,
                receipt.clone()
            )
            .await
            .is_err()
    );
    insert(&f, &owner, scope, 2..3, 1).await;
    assert_eq!(
        count(&owner, scope).await,
        0,
        "old coverage never covers a new write"
    );
    push(&owner, &keeper, scope).await;
    assert_eq!(count(&owner, scope).await, 1);
    policy(&owner, scope, &[&keeper], 1).await;
    assert_eq!(
        count(&owner, scope).await,
        0,
        "a new policy requires fresh challenges, even for the same peer"
    );
    push(&owner, &keeper, scope).await;
    assert_eq!(count(&owner, scope).await, 1);
    owner
        .replica
        .learn_revocations(&crate::tests::revoking(
            &f.publisher,
            f.app().definition().app,
            &[keeper.key.public_key()],
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(count(&owner, scope).await, 0);
    assert!(
        owner
            .replica
            .record_custody_receipt(keeper.key.public_key(), scope, binding, challenge, receipt)
            .await
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn quota_refusals_and_applied_frontiers_earn_no_durable_credit() {
    let f = Fixture::new();
    let dir = Scratch::new().await;
    let scope = department(&f, 0);
    let owner = open_node(
        &f,
        &dir.path().join("owner.db"),
        &PrivateKey::new(),
        &[scope],
        &[scope],
    )
    .await;
    let keeper = keyless(
        &f,
        &dir.path().join("keeper.db"),
        &PrivateKey::new(),
        &[scope],
    )
    .await;
    insert(&f, &owner, scope, 0..2, 1).await;
    policy(&owner, scope, &[&keeper], 1).await;
    owner
        .replica
        .acknowledge_frontier(
            keeper.key.public_key(),
            &keeper.credentials,
            &owner.replica.frontier(scope).await.unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(count(&owner, scope).await, 0);
    keeper
        .replica
        .set_retention_limits(RetentionLimits {
            hard_bytes: 1,
            ..RetentionLimits::default()
        })
        .await
        .unwrap();
    let (mut session, server) = connect(&owner, &keeper).await;
    assert!(session.push(&owner.replica, scope, QUOTA).await.is_err());
    drop(session);
    server.await.unwrap();
    assert_eq!(count(&owner, scope).await, 0);
    keeper
        .replica
        .set_retention_limits(RetentionLimits::default())
        .await
        .unwrap();
    push(&owner, &keeper, scope).await;
    assert_eq!(count(&owner, scope).await, 1);
}
