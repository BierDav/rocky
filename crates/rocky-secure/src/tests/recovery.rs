use super::*;
use rocky_coordinator::MemoryCoordinator;
use rocky_core::{Confirmation, CoordinationFuture, Coordinator, Scope, StrongHead, StrongReceipt};
use std::sync::{Arc, Mutex};
pub(super) fn grants(f: &Fixture) -> Vec<ResourceGrant> {
    let mut grants = f.full();
    for grant in &mut grants {
        grant.rights.extend([Right::Invoke, Right::Execute]);
        grant
            .fields
            .insert(f.app().definition().item_id("method", "SetEmployee"));
    }
    grants
}
pub(super) fn coordination(
    f: &Fixture,
    configs: &[&SecureConfig],
) -> (MemoryCoordinator, rocky_core::PrincipalId) {
    let coordinator = MemoryCoordinator::default();
    let owner = secure_principal_id(configs[0].credentials.0[0].capability.subject);
    let nodes = configs
        .iter()
        .map(|c| secure_node_id(c.node.public_key()))
        .collect();
    coordinator
        .register(f.public.domain, owner, [1; 16], nodes, 1, 1)
        .unwrap();
    coordinator
        .set_invokers(
            f.public.domain,
            owner,
            configs
                .iter()
                .map(|c| {
                    (
                        secure_node_id(c.node.public_key()),
                        secure_principal_id(c.credentials.0[0].capability.subject),
                    )
                })
                .collect(),
        )
        .unwrap();
    coordinator
        .acquire(
            f.public.domain,
            owner,
            secure_node_id(configs[0].node.public_key()),
        )
        .unwrap();
    (coordinator, owner)
}
#[tokio::test]
async fn phone_recovers_confirmed_unapplied_and_pending_work_to_new_identity() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let source_key = PrivateKey::new();
    let phone_key = PrivateKey::new();
    let new_key = PrivateKey::new();
    let source_config = f.config(&source_key, grants(&f), &[f.public, f.private]);
    let phone_config = f.config(&phone_key, grants(&f), &[f.public, f.private]);
    let new_config = f.config(&new_key, grants(&f), &[f.public, f.private]);
    let phone_credentials = phone_config.credentials.clone();
    let new_credentials = new_config.credentials.clone();
    let (c, owner) = coordination(&f, &[&source_config, &phone_config, &new_config]);
    let source = EncryptedReplica::open(dir.path().join("source.db"), f.app(), source_config)
        .await
        .unwrap();
    let phone = EncryptedReplica::open(dir.path().join("phone.db"), f.app(), phone_config)
        .await
        .unwrap();
    let create = source
        .author(
            [8; 32],
            vec![change(f.public, 8, None, Some(f.app().name("offline")))],
            vec![],
        )
        .await
        .unwrap();
    let command = source
        .invoke_encrypted([1; 32], f.private, "SetEmployee", b"id\nAlice\n99")
        .await
        .unwrap();
    let preparation = source.prepare_encrypted(command.clone(), &c).await.unwrap();
    let checkpoint = source
        .export_recovery(
            phone_key.public_key(),
            &phone_credentials,
            vec![f.public, f.private],
            &[9; 32],
            &c,
        )
        .await
        .unwrap();
    phone.retain_recovery(&checkpoint, &[9; 32]).await.unwrap();
    source.inject_failure(0, 1).await;
    assert!(
        source
            .confirm_encrypted(preparation, &c, &DurabilityPolicy::AsyncReplication, &[])
            .await
            .is_err()
    );
    assert_eq!(c.head(f.public.domain).unwrap().sequence, 1);
    drop(source);
    let report = phone
        .import_recovery(&checkpoint, &[9; 32], &c)
        .await
        .unwrap();
    assert!(!report.ready()); // The source still owns the execution role.
    let pending = phone
        .invoke_encrypted([2; 32], f.private, "SetEmployee", b"second\nPending\n10")
        .await
        .unwrap();
    let replacement = phone
        .export_recovery(
            new_key.public_key(),
            &new_credentials,
            vec![f.public, f.private],
            &[9; 32],
            &c,
        )
        .await
        .unwrap();
    let fresh = EncryptedReplica::open(dir.path().join("fresh.db"), f.app(), new_config)
        .await
        .unwrap();
    fresh
        .author(
            [7; 32],
            vec![change(
                f.public,
                7,
                None,
                Some(f.app().name("fresh offline edit")),
            )],
            vec![],
        )
        .await
        .unwrap();
    fresh
        .import_recovery(&replacement, &[9; 32], &c)
        .await
        .unwrap();
    fresh
        .import_recovery(&replacement, &[9; 32], &c)
        .await
        .unwrap();
    assert_eq!(
        fresh.receipt_status(create.id().unwrap()).await.unwrap(),
        ReceiveStatus::Applied
    );
    let retry = fresh
        .author(
            [8; 32],
            vec![change(f.public, 8, None, Some(f.app().name("offline")))],
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(retry.id().unwrap(), create.id().unwrap());
    assert_eq!(
        fresh
            .invoke_encrypted([2; 32], f.private, "SetEmployee", b"second\nPending\n10")
            .await
            .unwrap()
            .to_bytes()
            .unwrap(),
        pending.to_bytes().unwrap()
    );
    c.takeover(
        f.public.domain,
        owner,
        secure_node_id(new_key.public_key()),
        1,
    )
    .unwrap();
    assert!(
        fresh
            .recovery_status(&[f.public, f.private], &c)
            .await
            .unwrap()
            .ready()
    );
    fresh.execute_encrypted(pending, &c).await.unwrap();
    assert_eq!(c.head(f.public.domain).unwrap().sequence, 2);
    assert_eq!(
        fresh
            .execute_encrypted(command, &c)
            .await
            .unwrap()
            .receipt()
            .unwrap()
            .sequence,
        1
    );
    let authored = fresh
        .author(
            [9; 32],
            vec![change(
                f.public,
                9,
                None,
                Some(f.app().name("new credential")),
            )],
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        authored.operation().unwrap().header.public_key,
        new_key.public_key()
    );
    assert_eq!(authored.operation().unwrap().header.seq_num, 1);
    assert_eq!(
        fresh.snapshot(f.public).await.unwrap().rows[&[7; 32]],
        f.app().name("fresh offline edit")
    );
}
#[tokio::test]
async fn lost_newest_payload_and_lost_data_keys_block_old_domain() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a = PrivateKey::new();
    let b = PrivateKey::new();
    let ac = f.config(&a, grants(&f), &[f.public, f.private]);
    let bc = f.config(&b, grants(&f), &[]);
    let creds = bc.credentials.clone();
    let (c, owner) = coordination(&f, &[&ac, &bc]);
    let source = EncryptedReplica::open(dir.path().join("a.db"), f.app(), ac)
        .await
        .unwrap();
    let old = source
        .export_recovery(
            b.public_key(),
            &creds,
            vec![f.public, f.private],
            &[7; 32],
            &c,
        )
        .await
        .unwrap();
    let cmd = source
        .invoke_encrypted([1; 32], f.private, "SetEmployee", b"id\nAlice\n77")
        .await
        .unwrap();
    source.execute_encrypted(cmd, &c).await.unwrap();
    let latest = source
        .export_recovery(
            b.public_key(),
            &creds,
            vec![f.public, f.private],
            &[7; 32],
            &c,
        )
        .await
        .unwrap();
    let target = EncryptedReplica::open(dir.path().join("b.db"), f.app(), bc)
        .await
        .unwrap();
    c.takeover(f.public.domain, owner, secure_node_id(b.public_key()), 1)
        .unwrap();
    let report = target.import_recovery(&old, &[7; 32], &c).await.unwrap();
    assert!(report.blocked.contains(&RecoveryBlock::MissingPayload {
        domain: f.public.domain,
        sequence: 1
    }));
    assert!(
        target
            .import_recovery(&latest, &[99; 32], &c)
            .await
            .is_err()
    );
    let report = target.import_recovery(&latest, &[7; 32], &c).await.unwrap();
    assert!(
        report
            .blocked
            .contains(&RecoveryBlock::MissingKey(f.private))
    );
    assert!(!target.command_snapshot(f.private).await.unwrap().complete);
    for scope in [f.public, f.private] {
        target
            .install_key(KeyMaterial::new(
                scope,
                1,
                if scope == f.public {
                    [11; 32]
                } else {
                    [22; 32]
                },
            ))
            .await
            .unwrap();
    }
    target.reconcile_encrypted_results(&c).await.unwrap();
    assert!(
        target
            .recovery_status(&[f.public, f.private], &c)
            .await
            .unwrap()
            .ready()
    );
}
#[tokio::test]
async fn checkpoint_requires_dependency_closure_and_excludes_private_companions() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a = PrivateKey::new();
    let b = PrivateKey::new();
    let ac = f.config(&a, grants(&f), &[f.public, f.private]);
    let bc = f.config(
        &b,
        vec![grant(f.public, &[Right::Read, Right::Custody], &[])],
        &[f.public],
    );
    let creds = bc.credentials.clone();
    let (c, _) = coordination(&f, &[&ac]);
    let source = EncryptedReplica::open(dir.path().join("a.db"), f.app(), ac)
        .await
        .unwrap();
    let first = source
        .author(
            [1; 32],
            vec![change(f.public, 1, None, Some(f.app().name("base")))],
            vec![],
        )
        .await
        .unwrap();
    let second = source
        .author(
            [2; 32],
            vec![change(
                f.public,
                1,
                Some(f.app().name("base")),
                Some(f.app().name("new")),
            )],
            vec![],
        )
        .await
        .unwrap();
    source
        .author(
            [3; 32],
            vec![change(
                f.private,
                1,
                None,
                Some(f.app().salary("987654321")),
            )],
            vec![],
        )
        .await
        .unwrap();
    let target = EncryptedReplica::open(dir.path().join("b.db"), f.app(), bc)
        .await
        .unwrap();
    target.receive(second).await.unwrap();
    assert!(
        target
            .export_recovery(
                a.public_key(),
                &f.credentials(&a, grants(&f)),
                vec![f.public],
                &[7; 32],
                &c
            )
            .await
            .is_err()
    );
    let package = source
        .export_recovery(b.public_key(), &creds, vec![f.public], &[7; 32], &c)
        .await
        .unwrap();
    let decoded =
        super::super::recovery::RecoveryPackage::from_bytes(&package.to_bytes().unwrap()).unwrap();
    assert_eq!(decoded.inventory.frontier.len(), 2);
    target
        .import_recovery(&decoded, &[7; 32], &c)
        .await
        .unwrap();
    assert_eq!(
        target.receipt_status(first.id().unwrap()).await.unwrap(),
        ReceiveStatus::Applied
    );
    assert!(target.snapshot(f.private).await.is_err());
    assert!(
        source
            .export_recovery(b.public_key(), &creds, vec![f.private], &[7; 32], &c)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn required_custody_precedes_confirmation_and_survives_restart() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a = PrivateKey::new();
    let b = PrivateKey::new();
    let ac = f.config(&a, grants(&f), &[f.public, f.private]);
    let bc = f.config(&b, grants(&f), &[]);
    let creds = bc.credentials.clone();
    let (c, owner) = coordination(&f, &[&ac, &bc]);
    let source = EncryptedReplica::open(dir.path().join("a.db"), f.app(), ac)
        .await
        .unwrap();
    let custodian = EncryptedReplica::open(dir.path().join("b.db"), f.app(), bc)
        .await
        .unwrap();
    let policy = DurabilityPolicy::RequiredCustodians {
        nodes: [b.public_key()].into(),
        minimum: 1,
    };
    source
        .set_recovery_durability(f.public.domain, policy.clone())
        .await
        .unwrap();
    let command = source
        .invoke_encrypted([1; 32], f.private, "SetEmployee", b"id\nAlice\n22")
        .await
        .unwrap();
    assert!(source.execute_encrypted(command.clone(), &c).await.is_err());
    assert_eq!(c.head(f.public.domain).unwrap().sequence, 0);
    let prepared = source.prepare_encrypted(command, &c).await.unwrap();
    let package = source
        .export_recovery(
            b.public_key(),
            &creds,
            vec![f.public, f.private],
            &[8; 32],
            &c,
        )
        .await
        .unwrap();
    let ack = custodian.retain_recovery(&package, &[8; 32]).await.unwrap();
    assert!(
        custodian
            .retained_recovery(package.commitment().unwrap())
            .await
            .unwrap()
            .is_some()
    );
    drop(custodian);
    let custodian = EncryptedReplica::open(
        dir.path().join("b.db"),
        f.app(),
        f.config(&b, grants(&f), &[]),
    )
    .await
    .unwrap();
    assert!(
        custodian
            .retained_recovery(package.commitment().unwrap())
            .await
            .unwrap()
            .is_some()
    );
    source
        .confirm_encrypted(prepared, &c, &policy, &[ack])
        .await
        .unwrap();
    assert_eq!(c.head(f.public.domain).unwrap().sequence, 1);
    drop(source);
    drop(custodian);
    let lost = EncryptedReplica::open(
        dir.path().join("lost-custody.db"),
        f.app(),
        f.config(&b, grants(&f), &[]),
    )
    .await
    .unwrap();
    assert!(
        lost.retained_recovery(package.commitment().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    c.takeover(f.public.domain, owner, secure_node_id(b.public_key()), 1)
        .unwrap();
    assert!(
        lost.recovery_status(&[f.public, f.private], &c)
            .await
            .unwrap()
            .blocked
            .contains(&RecoveryBlock::MissingPayload {
                domain: f.public.domain,
                sequence: 1
            })
    );
}
struct Advancing {
    c: Arc<MemoryCoordinator>,
    proposal: Mutex<Option<Confirmation>>,
}
impl Coordinator for Advancing {
    fn head(&self, scope: Scope) -> CoordinationFuture<'_, StrongHead> {
        Box::pin(async move {
            let head = self.c.head(scope)?;
            if let Some(proposal) = self.proposal.lock().unwrap().take() {
                self.c.confirm(proposal)?;
            }
            Ok(head)
        })
    }
    fn lookup(
        &self,
        scope: Scope,
        id: rocky_core::RequestId,
    ) -> CoordinationFuture<'_, Option<StrongReceipt>> {
        Box::pin(async move { self.c.lookup(scope, id) })
    }
    fn confirm(&self, p: Confirmation) -> CoordinationFuture<'_, StrongReceipt> {
        Box::pin(async move { self.c.confirm(p) })
    }
    fn verify<'a>(&'a self, r: &'a StrongReceipt) -> CoordinationFuture<'a, ()> {
        Box::pin(async move { self.c.verify(r) })
    }
    fn retire(
        &self,
        scope: Scope,
        node: rocky_core::NodeId,
        through: u64,
    ) -> CoordinationFuture<'_, ()> {
        Coordinator::retire(&*self.c, scope, node, through)
    }
}
#[tokio::test]
async fn head_advance_during_import_is_visible_and_takeover_does_not_hide_it() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a = PrivateKey::new();
    let b = PrivateKey::new();
    let ac = f.config(&a, grants(&f), &[f.public, f.private]);
    let bc = f.config(&b, grants(&f), &[f.public, f.private]);
    let creds = bc.credentials.clone();
    let (c, owner) = coordination(&f, &[&ac, &bc]);
    let c = Arc::new(c);
    let source = EncryptedReplica::open(dir.path().join("a.db"), f.app(), ac)
        .await
        .unwrap();
    let target = EncryptedReplica::open(dir.path().join("b.db"), f.app(), bc)
        .await
        .unwrap();
    let package = source
        .export_recovery(
            b.public_key(),
            &creds,
            vec![f.public, f.private],
            &[8; 32],
            c.as_ref(),
        )
        .await
        .unwrap();
    let command = source
        .invoke_encrypted([1; 32], f.private, "SetEmployee", b"id\nAlice\n44")
        .await
        .unwrap();
    let prepared = source.prepare_encrypted(command, c.as_ref()).await.unwrap();
    let advancing = Advancing {
        c: c.clone(),
        proposal: Mutex::new(Some(
            prepared.strong_package().unwrap().confirmation().unwrap(),
        )),
    };
    let report = target
        .import_recovery(&package, &[8; 32], &advancing)
        .await
        .unwrap();
    assert!(
        report
            .blocked
            .contains(&RecoveryBlock::HeadChanged(f.public.domain))
    );
    c.takeover(f.public.domain, owner, secure_node_id(b.public_key()), 1)
        .unwrap();
    assert!(
        !target
            .recovery_status(&[f.public, f.private], c.as_ref())
            .await
            .unwrap()
            .ready()
    );
    let latest = source
        .export_recovery(
            b.public_key(),
            &creds,
            vec![f.public, f.private],
            &[8; 32],
            c.as_ref(),
        )
        .await
        .unwrap();
    assert!(
        target
            .import_recovery(&latest, &[8; 32], c.as_ref())
            .await
            .unwrap()
            .ready()
    );
}

#[tokio::test]
async fn native_phone_publication_recovers_after_executor_exit() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a = PrivateKey::new();
    let phone_key = PrivateKey::new();
    let new_key = PrivateKey::new();
    let ac = f.config(&a, grants(&f), &[f.public, f.private]);
    let pc = f.config(&phone_key, grants(&f), &[f.public, f.private]);
    let nc = f.config(&new_key, grants(&f), &[f.public, f.private]);
    let acred = ac.credentials.clone();
    let pcred = pc.credentials.clone();
    let ncred = nc.credentials.clone();
    let (c, owner) = coordination(&f, &[&ac, &pc, &nc]);
    let source = EncryptedReplica::open(dir.path().join("a.db"), f.app(), ac)
        .await
        .unwrap();
    let phone = EncryptedReplica::open(dir.path().join("phone.db"), f.app(), pc)
        .await
        .unwrap();
    let command = source
        .invoke_encrypted([1; 32], f.private, "SetEmployee", b"id\nAlice\n88")
        .await
        .unwrap();
    source.execute_encrypted(command, &c).await.unwrap();
    let (near, far) = network::link::testing::tcp().await;
    let (session, mut served) = network::link::testing::sessions_over(
        near,
        far,
        (&phone, phone_key.clone(), pcred),
        (&source, a.clone(), acred),
    )
    .await
    .unwrap();
    let server = async {
        served.serve_command_once(&source, &c).await.unwrap();
    };
    let client = async {
        let mut session = session;
        assert!(
            session
                .pull_result(&phone, f.public.domain, 1, vec![f.public, f.private], &c)
                .await
                .unwrap()
        );
    };
    tokio::join!(server, client);
    drop(source);
    let package = phone
        .export_recovery(
            new_key.public_key(),
            &ncred,
            vec![f.public, f.private],
            &[8; 32],
            &c,
        )
        .await
        .unwrap();
    let fresh = EncryptedReplica::open(dir.path().join("fresh.db"), f.app(), nc)
        .await
        .unwrap();
    fresh.import_recovery(&package, &[8; 32], &c).await.unwrap();
    c.takeover(
        f.public.domain,
        owner,
        secure_node_id(new_key.public_key()),
        1,
    )
    .unwrap();
    assert!(
        fresh
            .recovery_status(&[f.public, f.private], &c)
            .await
            .unwrap()
            .ready()
    );
    let command = fresh
        .invoke_encrypted(
            [2; 32],
            f.private,
            "SetEmployee",
            b"new
Recovered
99",
        )
        .await
        .unwrap();
    fresh.execute_encrypted(command, &c).await.unwrap();
    assert_eq!(c.head(f.public.domain).unwrap().sequence, 2);
}

#[tokio::test]
#[ignore = "invoked by abrupt_executor_exit_recovers_each_confirmation_boundary"]
async fn recovery_crash_child() {
    let path = std::path::PathBuf::from(std::env::var("ROCKY_RECOVERY_CRASH_DIR").unwrap());
    let phase = std::env::var("ROCKY_RECOVERY_CRASH_PHASE").unwrap();
    let f = Fixture::with_publisher(PrivateKey::from_bytes(&[41; 32]));
    let source_key = PrivateKey::from_bytes(&[42; 32]);
    let phone_key = PrivateKey::from_bytes(&[43; 32]);
    let mut ac = f.config(&source_key, grants(&f), &[f.public, f.private]);
    ac.credentials = decode(&std::fs::read(path.join("source-credentials")).unwrap()).unwrap();
    let mut pc = f.config(&phone_key, grants(&f), &[f.public, f.private]);
    pc.credentials = decode(&std::fs::read(path.join("phone-credentials")).unwrap()).unwrap();
    let (c, _) = coordination(&f, &[&ac, &pc]);
    let source = EncryptedReplica::open(path.join("pc.db"), f.app(), ac)
        .await
        .unwrap();
    source
        .author(
            [8; 32],
            vec![change(
                f.public,
                8,
                None,
                Some(f.app().name("offline edit")),
            )],
            vec![],
        )
        .await
        .unwrap();
    let command = source
        .invoke_encrypted([1; 32], f.private, "SetEmployee", b"event\nAlice\n88")
        .await
        .unwrap();
    let prepared = source.prepare_encrypted(command, &c).await.unwrap();
    if phase != "before-confirmation" {
        let receipt = c
            .confirm(prepared.strong_package().unwrap().confirmation().unwrap())
            .unwrap();
        std::fs::write(path.join("receipt"), receipt.encode()).unwrap();
        if phase == "after-publication" {
            source
                .confirm_encrypted(prepared, &c, &DurabilityPolicy::AsyncReplication, &[])
                .await
                .unwrap();
        }
    }
    let package = source
        .export_recovery(
            phone_key.public_key(),
            &pc.credentials,
            vec![f.public, f.private],
            &[9; 32],
            &c,
        )
        .await
        .unwrap();
    std::fs::write(path.join("phone-package"), package.to_bytes().unwrap()).unwrap();
    std::process::exit(77);
}
#[tokio::test]
async fn abrupt_executor_exit_recovers_each_confirmation_boundary() {
    for phase in [
        "before-confirmation",
        "confirmed-before-application",
        "after-publication",
    ] {
        let dir = TempDir::new().unwrap();
        let f = Fixture::with_publisher(PrivateKey::from_bytes(&[41; 32]));
        let source_key = PrivateKey::from_bytes(&[42; 32]);
        let phone_key = PrivateKey::from_bytes(&[43; 32]);
        let new_key = PrivateKey::new();
        let ac = f.config(&source_key, grants(&f), &[f.public, f.private]);
        let pc = f.config(&phone_key, grants(&f), &[f.public, f.private]);
        let nc = f.config(&new_key, grants(&f), &[f.public, f.private]);
        let ncred = nc.credentials.clone();
        std::fs::write(
            dir.path().join("source-credentials"),
            encode(&ac.credentials).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("phone-credentials"),
            encode(&pc.credentials).unwrap(),
        )
        .unwrap();
        let (c, owner) = coordination(&f, &[&ac, &pc, &nc]);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::recovery::recovery_crash_child",
                "--ignored",
            ])
            .env("ROCKY_RECOVERY_CRASH_DIR", dir.path())
            .env("ROCKY_RECOVERY_CRASH_PHASE", phase)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(77));
        if phase != "before-confirmation" {
            let receipt =
                StrongReceipt::decode(&std::fs::read(dir.path().join("receipt")).unwrap()).unwrap();
            c.confirm(receipt.confirmation).unwrap();
        }
        let phone = EncryptedReplica::open(dir.path().join("phone.db"), f.app(), pc)
            .await
            .unwrap();
        let package =
            RecoveryPackage::from_bytes(&std::fs::read(dir.path().join("phone-package")).unwrap())
                .unwrap();
        phone.import_recovery(&package, &[9; 32], &c).await.unwrap();
        let package = phone
            .export_recovery(
                new_key.public_key(),
                &ncred,
                vec![f.public, f.private],
                &[9; 32],
                &c,
            )
            .await
            .unwrap();
        let fresh = EncryptedReplica::open(dir.path().join("new.db"), f.app(), nc)
            .await
            .unwrap();
        let report = fresh.import_recovery(&package, &[9; 32], &c).await.unwrap();
        c.takeover(
            f.public.domain,
            owner,
            secure_node_id(new_key.public_key()),
            1,
        )
        .unwrap();
        assert!(
            fresh
                .recovery_status(&[f.public, f.private], &c)
                .await
                .unwrap()
                .ready()
        );
        for command in report.unresolved {
            fresh.execute_encrypted(command, &c).await.unwrap();
        }
        assert_eq!(c.head(f.public.domain).unwrap().sequence, 1);
        assert_eq!(
            fresh.snapshot(f.public).await.unwrap().rows[&[8; 32]],
            f.app().name("offline edit")
        );
        let command = fresh
            .invoke_encrypted([2; 32], f.private, "SetEmployee", b"after\nRecovered\n99")
            .await
            .unwrap();
        fresh.execute_encrypted(command, &c).await.unwrap();
        assert_eq!(c.head(f.public.domain).unwrap().sequence, 2);
    }
}

#[tokio::test]
async fn archive_retention_requires_durable_complete_successor() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a = PrivateKey::new();
    let b = PrivateKey::new();
    let ac = f.config(&a, grants(&f), &[f.public, f.private]);
    let bc = f.config(&b, grants(&f), &[]);
    let creds = bc.credentials.clone();
    let (c, _) = coordination(&f, &[&ac, &bc]);
    let source = EncryptedReplica::open(dir.path().join("a.db"), f.app(), ac)
        .await
        .unwrap();
    let custodian = EncryptedReplica::open(dir.path().join("b.db"), f.app(), bc)
        .await
        .unwrap();
    source
        .author(
            [1; 32],
            vec![change(f.public, 1, None, Some(f.app().name("base")))],
            vec![],
        )
        .await
        .unwrap();
    let old = source
        .export_recovery(b.public_key(), &creds, vec![f.public], &[7; 32], &c)
        .await
        .unwrap();
    custodian.retain_recovery(&old, &[7; 32]).await.unwrap();
    source
        .author(
            [2; 32],
            vec![change(f.public, 2, None, Some(f.app().name("new")))],
            vec![],
        )
        .await
        .unwrap();
    let new = source
        .export_recovery(b.public_key(), &creds, vec![f.public], &[7; 32], &c)
        .await
        .unwrap();
    let old_id = old.commitment().unwrap();
    let new_id = new.commitment().unwrap();
    assert!(
        custodian
            .prune_redundant_recovery(old_id, new_id, &[7; 32])
            .await
            .is_err()
    );
    assert!(custodian.retained_recovery(old_id).await.unwrap().is_some());
    custodian.retain_recovery(&new, &[7; 32]).await.unwrap();
    assert!(
        custodian
            .prune_redundant_recovery(new_id, old_id, &[7; 32])
            .await
            .is_err()
    );
    assert!(
        custodian
            .prune_redundant_recovery(old_id, new_id, &[8; 32])
            .await
            .is_err()
    );
    assert!(
        custodian
            .prune_redundant_recovery(old_id, new_id, &[7; 32])
            .await
            .unwrap()
    );
    assert!(
        !custodian
            .prune_redundant_recovery(old_id, new_id, &[7; 32])
            .await
            .unwrap()
    );
    assert!(custodian.retained_recovery(old_id).await.unwrap().is_none());
    drop(custodian);
    let custodian = EncryptedReplica::open(
        dir.path().join("b.db"),
        f.app(),
        f.config(&b, grants(&f), &[]),
    )
    .await
    .unwrap();
    assert!(custodian.retained_recovery(new_id).await.unwrap().is_some());
    assert!(
        custodian
            .prune_redundant_recovery(new_id, [99; 32], &[7; 32])
            .await
            .is_err()
    );
}
