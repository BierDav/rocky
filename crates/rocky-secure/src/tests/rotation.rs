//! Key epochs: a rotation seals later writes under a fresh key, earlier
//! ciphertext stays readable to those who hold its key, and an interrupted
//! rotation completes at the next open.
use super::*;

const OLD: [u8; 32] = [11; 32];
const NEW: [u8; 32] = [77; 32];

fn key(scope: ResourceScope, generation: u64, key: [u8; 32]) -> KeyMaterial {
    KeyMaterial {
        scope,
        generation,
        key,
    }
}
fn reader(f: &Fixture) -> Vec<ResourceGrant> {
    vec![grant(f.public, &[Right::Read, Right::Custody], &[])]
}
/// A device's chain: publisher, user, node.
fn chain(f: &Fixture, node: &PrivateKey, grants: Vec<ResourceGrant>) -> CredentialChain {
    let user = PrivateKey::new();
    let capability = |subject, kind, delegations| Capability {
        manifest: f.app().definition().hash(),
        subject,
        kind,
        grants: grants.clone(),
        not_before: crate::tests::lifetime().0,
        expires: crate::tests::lifetime().1,
        delegations,
        extension: rocky_credentials::Extension::new(rocky_credentials::Extended::default())
            .unwrap(),
    };
    CredentialChain(vec![
        SignedCapability::sign(
            capability(user.public_key(), PrincipalKind::User, 1),
            &f.publisher,
        )
        .unwrap(),
        SignedCapability::sign(capability(node.public_key(), PrincipalKind::Node, 0), &user)
            .unwrap(),
    ])
}
/// A statement revoking `revoked` as nodes and users.
fn revoking(f: &Fixture, revoked: &[PublicKey]) -> RevocationSet {
    crate::tests::revoking(&f.publisher, f.app().definition().app, revoked, &[])
}
fn config(
    f: &Fixture,
    node: &PrivateKey,
    grants: Vec<ResourceGrant>,
    keys: Vec<KeyMaterial>,
) -> SecureConfig {
    SecureConfig {
        publisher: f.publisher.public_key(),
        manifest: f.manifest.clone(),
        revocations: RevocationSet::new(),
        node: node.clone(),
        credentials: chain(f, node, grants),
        keys,
    }
}
async fn insert(replica: &EncryptedReplica<Employees>, f: &Fixture, row: u8) -> EncryptedFrame {
    replica
        .author(
            [row; 32],
            vec![change(f.public, row, None, Some(f.app().name("Row")))],
            vec![],
        )
        .await
        .unwrap()
}
fn generation(frame: &EncryptedFrame) -> u64 {
    frame.operation().unwrap().header.extensions.generation
}
async fn rows(replica: &EncryptedReplica<Employees>, f: &Fixture) -> usize {
    replica.snapshot(f.public).await.unwrap().rows.len()
}

#[tokio::test]
async fn a_rotation_seals_later_writes_out_of_a_revoked_readers_reach() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (alice, bob, carol) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let open = |name: &'static str, node: &PrivateKey, grants| {
        EncryptedReplica::open(
            dir.path().join(name),
            f.app(),
            config(&f, node, grants, vec![key(f.public, 1, OLD)]),
        )
    };
    let issuer = open("alice.db", &alice, f.full()).await.unwrap();
    let member = open("bob.db", &bob, reader(&f)).await.unwrap();
    let revoked = open("carol.db", &carol, reader(&f)).await.unwrap();
    let first = insert(&issuer, &f, 1).await;
    assert_eq!(generation(&first), 1);
    for replica in [&member, &revoked] {
        assert_eq!(
            replica.receive(first.clone()).await.unwrap(),
            ReceiveStatus::Applied
        );
    }

    // The issuer revokes carol: a statement, then a new key epoch. A
    // session and a paged read opened before end with the change; the
    // issuer's own chain stays current.
    let session = issuer.session([f.public].into()).await.unwrap();
    let mut staged = session.transaction();
    staged
        .insert(f.public, [9; 32], f.app().name("Staged"))
        .await
        .unwrap();
    let next = revoking(&f, &[carol.public_key()]);
    issuer.learn_revocations(&next).await.unwrap();
    let revision = *issuer.observe_revision().borrow();
    issuer
        .install_key_epoch(key(f.public, 2, NEW))
        .await
        .unwrap();
    assert_eq!(issuer.key_epochs(f.public).await, vec![1, 2]);
    assert!(staged.commit([9; 32]).await.is_err());
    assert!(session.page(f.public, None, 1).await.is_err());
    assert!(!issuer.handle_committed([9; 32]).await.unwrap());
    assert_eq!(*issuer.observe_revision().borrow(), revision);
    // The same epoch again does nothing; other material for it is refused.
    issuer
        .install_key_epoch(key(f.public, 2, NEW))
        .await
        .unwrap();
    assert!(
        issuer
            .install_key_epoch(key(f.public, 2, [78; 32]))
            .await
            .is_err()
    );

    let second = insert(&issuer, &f, 2).await;
    assert_eq!(generation(&second), 2);
    assert_eq!(rows(&issuer, &f).await, 2);
    // The earlier bytes are untouched, and the old key does not open the new.
    let operation = second.operation().unwrap();
    let aad = encode(&operation.header.extensions).unwrap();
    assert!(crate::wire::decrypt(&OLD, &aad, &second.body).is_err());
    assert!(crate::wire::decrypt(&NEW, &aad, &second.body).is_ok());
    let page = issuer
        .transfer_page(
            bob.public_key(),
            &chain(&f, &bob, reader(&f)),
            &[f.public],
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    let original = first.to_bytes().unwrap();
    assert!(
        page.frames
            .iter()
            .any(|frame| frame.to_bytes().unwrap() == original)
    );

    // Carol obtains the new ciphertext elsewhere. Whether or not she has
    // learned the statement, she holds no key for it and reads no new row.
    assert_eq!(
        revoked.receive(second.clone()).await.unwrap(),
        ReceiveStatus::MissingKey
    );
    assert_eq!(rows(&revoked, &f).await, 1);
    revoked.learn_revocations(&next).await.unwrap();
    assert!(revoked.receive(second.clone()).await.is_err());
    assert!(revoked.snapshot(f.public).await.is_err());
    assert!(revoked.session([f.public].into()).await.is_err());
    assert!(
        revoked
            .install_key_epoch(key(f.public, 2, NEW))
            .await
            .is_err()
    );

    // Bob learns the statement and keeps his chain. Until the new key
    // reaches him the write waits; then it applies.
    member.learn_revocations(&next).await.unwrap();
    assert_eq!(
        member.receive(second.clone()).await.unwrap(),
        ReceiveStatus::MissingKey
    );
    let waiting = member.snapshot(f.public).await.unwrap();
    assert_eq!((waiting.rows.len(), waiting.complete), (1, false));
    member
        .install_key_epoch(key(f.public, 2, NEW))
        .await
        .unwrap();
    let caught_up = member.snapshot(f.public).await.unwrap();
    assert_eq!((caught_up.rows.len(), caught_up.complete), (2, true));
    assert_eq!(member.audit(f.public).await.unwrap().operations, 2);
}

#[tokio::test]
async fn an_interrupted_rotation_completes_at_open_and_never_rolls_back() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let alice = PrivateKey::new();
    let path = dir.path().join("alice.db");
    let open = |keys: Vec<KeyMaterial>| {
        EncryptedReplica::open(&path, f.app(), config(&f, &alice, f.full(), keys))
    };
    let replica = open(vec![key(f.public, 1, OLD)]).await.unwrap();
    insert(&replica, &f, 1).await;
    drop(replica);

    // The key provider and the saved identity hold the new epoch; the
    // process stopped before the replica heard of either. Opening with
    // what was saved finishes the rotation.
    let both = || vec![key(f.public, 1, OLD), key(f.public, 2, NEW)];
    let replica = open(both()).await.unwrap();
    assert_eq!(replica.key_epochs(f.public).await, vec![1, 2]);
    assert_eq!(rows(&replica, &f).await, 1);
    assert_eq!(generation(&insert(&replica, &f, 2).await), 2);
    drop(replica);
    // Again, and after that with nothing left to complete.
    let replica = open(both()).await.unwrap();
    assert_eq!(rows(&replica, &f).await, 2);
    assert_eq!(replica.audit(f.public).await.unwrap().replayed, 2);
    drop(replica);

    // The key epoch does not go back, a lost earlier key is reported while
    // its operations are retained, and other material for a recorded epoch
    // is refused.
    assert!(open(vec![key(f.public, 1, OLD)]).await.is_err());
    assert!(open(vec![key(f.public, 2, NEW)]).await.is_err());
    assert!(
        open(vec![key(f.public, 1, [12; 32]), key(f.public, 2, NEW)])
            .await
            .is_err()
    );
    assert!(
        open(vec![key(f.public, 1, OLD), key(f.public, 2, [78; 32])])
            .await
            .is_err()
    );
    assert_eq!(rows(&open(both()).await.unwrap(), &f).await, 2);
}

#[tokio::test]
async fn earlier_epochs_retire_only_when_nothing_retained_needs_them() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (alice, bob) = (PrivateKey::new(), PrivateKey::new());
    let opened = config(&f, &alice, f.full(), vec![key(f.public, 1, OLD)]);
    let (coordinator, _) = recovery::coordination(&f, &[&opened]);
    let replica = EncryptedReplica::open(dir.path().join("alice.db"), f.app(), opened)
        .await
        .unwrap();
    insert(&replica, &f, 1).await;
    replica
        .install_key_epoch(key(f.public, 2, NEW))
        .await
        .unwrap();
    insert(&replica, &f, 2).await;
    // Recovery evidence spans both epochs: the original frames, with the
    // inventory naming the latest. A recipient holding both keys reads all.
    let both = vec![key(f.public, 1, OLD), key(f.public, 2, NEW)];
    let recipient = config(&f, &bob, f.full(), both);
    let package = replica
        .export_recovery(
            bob.public_key(),
            &recipient.credentials,
            vec![f.public],
            &[9; 32],
            &coordinator,
        )
        .await
        .unwrap();
    assert_eq!(package.inventory.key_epochs, [(f.public, 2)].into());
    assert_eq!(package.inventory.frontier.len(), 2);
    let restored = EncryptedReplica::open(dir.path().join("bob.db"), f.app(), recipient)
        .await
        .unwrap();
    restored
        .import_recovery(&package, &[9; 32], &coordinator)
        .await
        .unwrap();
    assert_eq!(rows(&restored, &f).await, 2);
    // The current epoch is not retired, and epoch 1 still opens a retained write.
    assert!(replica.retire_key_epoch(f.public, 2).await.is_err());
    assert!(replica.retire_key_epoch(f.public, 1).await.is_err());
    // A checkpoint under the new key covers both epochs.
    let signed = replica.checkpoint(f.public).await.unwrap();
    assert_eq!(signed.statement.key_epoch, 2);
    assert_eq!(signed.statement.operations, 2);
    assert_eq!(replica.audit(f.public).await.unwrap().rows, 2);
    // A rotation with no later write still seals the next checkpoint.
    replica
        .install_key_epoch(key(f.public, 3, [79; 32]))
        .await
        .unwrap();
    let resealed = replica.checkpoint(f.public).await.unwrap();
    assert_eq!(resealed.statement.key_epoch, 3);
    assert_eq!(resealed.statement.operations, 2);

    // A scope that never wrote under an earlier epoch drops it at once.
    replica
        .install_key(key(f.private, 2, [22; 32]))
        .await
        .unwrap();
    replica
        .install_key_epoch(key(f.private, 1, [21; 32]))
        .await
        .unwrap();
    assert_eq!(replica.key_epochs(f.private).await, vec![1, 2]);
    let session = replica.session([f.private].into()).await.unwrap();
    replica.retire_key_epoch(f.private, 1).await.unwrap();
    assert_eq!(replica.key_epochs(f.private).await, vec![2]);
    assert!(session.status().await.is_err());
    assert!(replica.retire_key_epoch(f.private, 1).await.is_err());
}

#[tokio::test]
async fn a_learned_revocation_alone_refreshes_the_checkpoint_without_a_new_write() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = EncryptedReplica::open(
        dir.path().join("policy.db"),
        f.app(),
        config(&f, &node, f.full(), vec![key(f.public, 1, OLD)]),
    )
    .await
    .unwrap();
    insert(&replica, &f, 1).await;
    let before = replica.checkpoint(f.public).await.unwrap();
    let learned = revoking(&f, &[PrivateKey::new().public_key()]);
    replica.learn_revocations(&learned).await.unwrap();
    // Drive bounded automatic maintenance directly, without a timer or a write.
    for _ in 0..8 {
        replica.maintain_checkpoint_for_test(f.public).await;
    }
    let status = replica.checkpoint_status(f.public).await.unwrap();
    assert!(status.maintenance_error.is_none());
    let after = status.checkpoint.unwrap();
    assert_eq!(
        after.statement.revocations,
        learned.digest(now().unwrap()).unwrap()
    );
    assert_ne!(after.statement.revocations, before.statement.revocations);
    assert_eq!(after.statement.key_epoch, 1);
    assert_eq!(after.statement.operations, before.statement.operations);
    assert_eq!(after.statement.rows, before.statement.rows);
    assert_eq!(after.statement.previous, Some(before.id().unwrap()));
    assert_eq!(replica.audit(f.public).await.unwrap().rows, 1);
}

#[tokio::test]
async fn reusing_another_scopes_key_names_the_collision_and_keeps_epochs() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let replica = EncryptedReplica::open(
        dir.path().join("keys.db"),
        f.app(),
        config(&f, &node, f.full(), vec![key(f.public, 1, OLD)]),
    )
    .await
    .unwrap();
    let error = replica
        .install_key_epoch(key(f.private, 1, OLD))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("already installed under another scope or epoch"),
        "{error}"
    );
    assert_eq!(replica.key_epochs(f.public).await, vec![1]);
    assert!(replica.key_epochs(f.private).await.is_empty());
    replica
        .install_key_epoch(key(f.private, 1, NEW))
        .await
        .unwrap();
    assert_eq!(replica.key_epochs(f.private).await, vec![1]);
}
