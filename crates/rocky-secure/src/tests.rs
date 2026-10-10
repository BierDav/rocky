use super::*;
use employees_app::Employees;
use rocky_core::{
    Commitment, MeshId, PrincipalKind, ResourceGrant, ResourceScope, Right, RowChange, RowFields,
    RowSelection, RowWrite,
};
use std::collections::BTreeSet;
use tempfile::TempDir;

struct Fixture {
    publisher: PrivateKey,
    manifest: SignedManifest,
    policy: RevocationSet,
    public: ResourceScope,
    private: ResourceScope,
}
/// From a day ago for 300 days: a fixture chain's validity, within the
/// longest a chain may live. Read once per process, so a link issued after a
/// second boundary never outlives its parent.
/// The retained operations `sql` selects as header and body, each with the
/// stored chain its header names.
pub(crate) async fn stored_frames<A: rocky_core::RowApplication>(
    replica: &EncryptedReplica<A>,
    sql: &str,
) -> Vec<EncryptedFrame> {
    let inner = replica.inner.lock().await;
    let mut frames = Vec::new();
    for record in crate::storage::rows(&inner.conn, sql).await.unwrap() {
        let [
            rocky_core::SqlValue::Blob(header),
            rocky_core::SqlValue::Blob(body),
        ] = record.as_slice()
        else {
            panic!("stored operation")
        };
        frames.push(
            inner
                .stored_frame(header.clone(), body.clone())
                .await
                .unwrap(),
        );
    }
    frames
}
pub(crate) fn lifetime() -> (u64, u64) {
    static LIFETIME: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    *LIFETIME.get_or_init(|| {
        let time = now().unwrap();
        (
            time - rocky_credentials::DAY,
            time + 300 * rocky_credentials::DAY,
        )
    })
}
/// Issuance terms for a fixture: from now for 300 days.
pub(crate) fn validity() -> (u64, u64, u64) {
    (
        now().unwrap(),
        300 * rocky_credentials::DAY,
        rocky_credentials::DAY,
    )
}
/// The digest of an empty revocation set.
pub(crate) fn unrevoked() -> Commitment {
    RevocationSet::new().digest(0).unwrap()
}
/// A one-statement set that revokes `users` and `capabilities`.
pub(crate) fn revoking(
    publisher: &PrivateKey,
    app: rocky_core::AppId,
    users: &[PublicKey],
    capabilities: &[Commitment],
) -> RevocationSet {
    let issuer = PrivateKey::new();
    let statement = Revocation {
        app,
        issuer: issuer.public_key(),
        time: now().unwrap(),
        nodes: BTreeSet::new(),
        users: users.iter().copied().collect(),
        capabilities: capabilities.iter().copied().collect(),
        pseudonyms: BTreeSet::new(),
    };
    RevocationSet::try_from(vec![
        SignedRevocation::sign(statement, publisher, &issuer).unwrap(),
    ])
    .unwrap()
}
fn grant(scope: ResourceScope, rights: &[Right], fields: &[Commitment]) -> ResourceGrant {
    ResourceGrant {
        scope,
        rows: RowSelection::All,
        rights: rights.iter().copied().collect(),
        fields: fields.iter().copied().collect(),
    }
}
impl Fixture {
    fn new() -> Self {
        Self::with_publisher(PrivateKey::new())
    }
    fn with_publisher(publisher: PrivateKey) -> Self {
        let app = Employees::new(*publisher.public_key().as_bytes());
        let body = app.definition();
        Self {
            manifest: SignedManifest::sign(&body, &publisher).unwrap(),
            policy: RevocationSet::new(),
            public: app.scope(MeshId([1; 16]), false),
            private: app.scope(MeshId([1; 16]), true),
            publisher,
        }
    }
    fn app(&self) -> Employees {
        Employees::new(*self.publisher.public_key().as_bytes())
    }
    fn credentials(&self, node: &PrivateKey, grants: Vec<ResourceGrant>) -> CredentialChain {
        self.chain(node, grants, &self.publisher, lifetime())
    }
    /// A node chain whose root `root` signs, valid from `not_before` to `expires`.
    fn chain(
        &self,
        node: &PrivateKey,
        grants: Vec<ResourceGrant>,
        root: &PrivateKey,
        (not_before, expires): (u64, u64),
    ) -> CredentialChain {
        let user = PrivateKey::new();
        let deployment = PrivateKey::new();
        let capability = |subject, kind, delegations| Capability {
            manifest: self.app().definition().hash(),
            subject,
            kind,
            grants: grants.clone(),
            not_before,
            expires,
            delegations,
            extension: rocky_credentials::Extension::new(rocky_credentials::Extended::default())
                .unwrap(),
        };
        CredentialChain(vec![
            SignedCapability::sign(capability(user.public_key(), PrincipalKind::User, 2), root)
                .unwrap(),
            SignedCapability::sign(
                capability(deployment.public_key(), PrincipalKind::Deployment, 1),
                &user,
            )
            .unwrap(),
            SignedCapability::sign(
                capability(node.public_key(), PrincipalKind::Node, 0),
                &deployment,
            )
            .unwrap(),
        ])
    }
    fn config(
        &self,
        node: &PrivateKey,
        grants: Vec<ResourceGrant>,
        scopes: &[ResourceScope],
    ) -> SecureConfig {
        SecureConfig {
            publisher: self.publisher.public_key(),
            manifest: self.manifest.clone(),
            revocations: self.policy.clone(),
            node: node.clone(),
            credentials: self.credentials(node, grants),
            keys: scopes
                .iter()
                .map(|scope| {
                    KeyMaterial::new(
                        *scope,
                        1,
                        if *scope == self.public {
                            [11; 32]
                        } else {
                            [22; 32]
                        },
                    )
                })
                .collect(),
        }
    }
    fn full(&self) -> Vec<ResourceGrant> {
        let fields = [
            self.app().field("display_name"),
            self.app().field("salary"),
            self.app().field("owner"),
        ];
        [self.public, self.private]
            .into_iter()
            .map(|scope| {
                grant(
                    scope,
                    &[
                        Right::Read,
                        Right::Insert,
                        Right::Update,
                        Right::Delete,
                        Right::Custody,
                    ],
                    &fields,
                )
            })
            .collect()
    }
}
fn change(
    scope: ResourceScope,
    row: u8,
    before: Option<RowFields>,
    after: Option<RowFields>,
) -> RowChange {
    RowChange {
        scope,
        row: [row; 32],
        before,
        after,
    }
}

#[tokio::test]
async fn encrypted_offline_publisher_public_private_and_opaque_relay() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let alice = PrivateKey::new();
    let bob = PrivateKey::new();
    let relay_key = PrivateKey::new();
    let bob_grants = vec![grant(
        fixture.public,
        &[Right::Read, Right::Update, Right::Custody],
        &[fixture.app().field("display_name")],
    )];
    let bob_config = fixture.config(&bob, bob_grants.clone(), &[fixture.public]);
    let bob_credentials = bob_config.credentials.clone();
    let relay_config = fixture.config(
        &relay_key,
        [fixture.public, fixture.private]
            .into_iter()
            .map(|scope| grant(scope, &[Right::Custody], &[]))
            .collect(),
        &[],
    );
    let relay_credentials = relay_config.credentials.clone();
    let source = EncryptedReplica::open(
        dir.path().join("alice.db"),
        fixture.app(),
        fixture.config(&alice, fixture.full(), &[fixture.public, fixture.private]),
    )
    .await
    .unwrap();
    let target = EncryptedReplica::open(dir.path().join("bob.db"), fixture.app(), bob_config)
        .await
        .unwrap();
    let relay = EncryptedReplica::open(dir.path().join("relay.db"), fixture.app(), relay_config)
        .await
        .unwrap();
    let public = source
        .author(
            [1; 32],
            vec![change(
                fixture.public,
                1,
                None,
                Some(fixture.app().name("Alice")),
            )],
            vec![],
        )
        .await
        .unwrap();
    let private = source
        .author(
            [2; 32],
            vec![change(
                fixture.private,
                1,
                None,
                Some(fixture.app().salary("987654321")),
            )],
            vec![],
        )
        .await
        .unwrap();
    assert!(
        !private
            .to_bytes()
            .unwrap()
            .windows(9)
            .any(|bytes| bytes == b"987654321")
    );
    let mut committed_frames = relay.observe_frames().await;
    assert_eq!(
        relay.receive(private.clone()).await.unwrap(),
        ReceiveStatus::Custody
    );
    assert_eq!(
        relay.receive(public.clone()).await.unwrap(),
        ReceiveStatus::Custody
    );
    assert!(committed_frames.has_changed().unwrap());
    committed_frames.borrow_and_update();
    assert_eq!(
        relay.receive(public.clone()).await.unwrap(),
        ReceiveStatus::Duplicate
    );
    assert!(!committed_frames.has_changed().unwrap());
    assert!(relay.snapshot(fixture.private).await.is_err());
    for cursor in [None, Some([0; 32])] {
        assert!(
            relay
                .snapshot_page(fixture.private, cursor, 10)
                .await
                .is_err()
        );
    }
    assert!(relay.companion(fixture.private, [1; 32]).await.is_err());
    assert!(
        relay
            .author(
                [3; 32],
                vec![change(
                    fixture.public,
                    1,
                    None,
                    Some(fixture.app().name("Relay"))
                )],
                vec![]
            )
            .await
            .is_err()
    );
    assert!(
        relay
            .transfer_page(
                bob.public_key(),
                &bob_credentials,
                &[fixture.private],
                None,
                MAX_SECURE_FRAME
            )
            .await
            .is_err()
    );
    let page = relay
        .transfer_page(
            bob.public_key(),
            &bob_credentials,
            &[fixture.public],
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert_eq!(page.frames.len(), 1);
    assert_eq!(
        page.frames[0].to_bytes().unwrap(),
        public.to_bytes().unwrap()
    );
    target.receive(page.frames[0].clone()).await.unwrap();
    target
        .author(
            [4; 32],
            vec![change(
                fixture.public,
                1,
                Some(fixture.app().name("Alice")),
                Some(fixture.app().name("Alice B")),
            )],
            vec![],
        )
        .await
        .unwrap();
    assert!(target.snapshot(fixture.private).await.is_err());
    for cursor in [None, Some([0; 32])] {
        assert!(
            target
                .snapshot_page(fixture.private, cursor, 10)
                .await
                .is_err()
        );
    }
    assert!(target.companion(fixture.private, [1; 32]).await.is_err());
    assert!(
        target
            .author(
                [5; 32],
                vec![change(
                    fixture.private,
                    1,
                    Some(fixture.app().salary("987654321")),
                    Some(fixture.app().salary("1"))
                )],
                vec![]
            )
            .await
            .is_err()
    );
    assert!(
        target
            .author(
                [6; 32],
                vec![change(
                    fixture.public,
                    2,
                    None,
                    Some(fixture.app().name("New"))
                )],
                vec![]
            )
            .await
            .is_err()
    );
    assert!(
        target
            .author(
                [7; 32],
                vec![change(
                    fixture.public,
                    1,
                    Some(fixture.app().name("Alice B")),
                    None
                )],
                vec![]
            )
            .await
            .is_err()
    );
    assert_eq!(
        source.companion(fixture.private, [1; 32]).await.unwrap(),
        Companion::Present(fixture.app().salary("987654321"))
    );
    let page = source
        .transfer_page(
            relay_key.public_key(),
            &relay_credentials,
            &[fixture.public, fixture.private],
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert_eq!(page.frames.len(), 2);
    drop(relay);
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("relay")
        {
            let bytes = std::fs::read(path).unwrap();
            assert!(!bytes.windows(9).any(|bytes| bytes == b"987654321"));
            assert!(!bytes.windows(5).any(|bytes| bytes == b"Alice"));
        }
    }
}

#[tokio::test]
async fn rollback_retry_restart_and_missing_encrypted_dependencies() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    let config = fixture.config(&node, fixture.full(), &[fixture.public]);
    let credentials = config.credentials.clone();
    let path = dir.path().join("source.db");
    let source = EncryptedReplica::open(&path, fixture.app(), config)
        .await
        .unwrap();
    let create = vec![change(
        fixture.public,
        1,
        None,
        Some(fixture.app().name("One")),
    )];
    source.inject_failure(0, 1).await;
    let mut observer = source.observe_revision();
    assert!(
        source
            .author([1; 32], create.clone(), vec![])
            .await
            .is_err()
    );
    assert!(!observer.has_changed().unwrap());
    assert!(
        source
            .snapshot(fixture.public)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    source.inject_failure(2, 0).await;
    let first = source
        .author([1; 32], create.clone(), vec![])
        .await
        .unwrap();
    assert_eq!(first.operation().unwrap().header.seq_num, 0);
    observer.changed().await.unwrap();
    assert_eq!(
        source
            .author([1; 32], create.clone(), vec![])
            .await
            .unwrap()
            .to_bytes()
            .unwrap(),
        first.to_bytes().unwrap()
    );
    assert!(
        source
            .author(
                [1; 32],
                vec![change(
                    fixture.public,
                    2,
                    None,
                    Some(fixture.app().name("Other"))
                )],
                vec![]
            )
            .await
            .is_err()
    );
    let second = source
        .author(
            [2; 32],
            vec![change(
                fixture.public,
                1,
                Some(fixture.app().name("One")),
                Some(fixture.app().name("Two")),
            )],
            vec![],
        )
        .await
        .unwrap();
    let target_key = PrivateKey::new();
    let target = EncryptedReplica::open(
        dir.path().join("target.db"),
        fixture.app(),
        fixture.config(&target_key, fixture.full(), &[fixture.public]),
    )
    .await
    .unwrap();
    assert_eq!(
        target.receive(second.clone()).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    assert!(!target.snapshot(fixture.public).await.unwrap().complete);
    assert_eq!(
        target.companion(fixture.public, [1; 32]).await.unwrap(),
        Companion::Unavailable
    );
    target.receive(first.clone()).await.unwrap();
    assert_eq!(
        target.snapshot(fixture.public).await.unwrap().rows[&[1; 32]],
        fixture.app().name("Two")
    );
    assert_eq!(
        target.receive(first.clone()).await.unwrap(),
        ReceiveStatus::Duplicate
    );
    drop(source);
    let source = EncryptedReplica::open(
        &path,
        fixture.app(),
        SecureConfig {
            credentials,
            ..fixture.config(&node, fixture.full(), &[fixture.public])
        },
    )
    .await
    .unwrap();
    assert_eq!(source.snapshot(fixture.public).await.unwrap().revision, 2);
    let third = source
        .author(
            [3; 32],
            vec![change(
                fixture.public,
                1,
                Some(fixture.app().name("Two")),
                None,
            )],
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(third.operation().unwrap().header.seq_num, 2);
    assert_ne!(&first.body[..24], &third.body[..24]);
    target.receive(third).await.unwrap();
    assert_eq!(
        target.companion(fixture.public, [1; 32]).await.unwrap(),
        Companion::Deleted
    );
}

#[test]
fn signatures_attenuation_expiry_revocation_and_cross_app_replay() {
    let fixture = Fixture::new();
    let key = PrivateKey::new();
    let chain = fixture.credentials(&key, fixture.full());
    let time = now().unwrap();
    let verify = |chain: &CredentialChain, revocations: &RevocationSet, manifest| {
        chain.verify(
            fixture.publisher.public_key(),
            manifest,
            key.public_key(),
            revocations,
            time,
        )
    };
    assert!(verify(&chain, &fixture.policy, fixture.app().definition().hash()).is_ok());
    assert!(verify(&chain, &fixture.policy, [0; 32]).is_err());
    let mut tampered = chain.clone();
    tampered.0[2].capability.grants[0].fields.insert([99; 32]);
    assert!(
        verify(
            &tampered,
            &fixture.policy,
            fixture.app().definition().hash()
        )
        .is_err()
    );
    let app = fixture.app().definition().app;
    let ended = revoking(&fixture.publisher, app, &[], &[chain.0[0].id().unwrap()]);
    assert!(verify(&chain, &ended, fixture.app().definition().hash()).is_err());
    // A statement naming someone else leaves the chain current.
    let other = revoking(
        &fixture.publisher,
        app,
        &[PrivateKey::new().public_key()],
        &[],
    );
    assert!(verify(&chain, &other, fixture.app().definition().hash()).is_ok());
    assert!(
        chain
            .verify(
                fixture.publisher.public_key(),
                fixture.app().definition().hash(),
                key.public_key(),
                &fixture.policy,
                u64::MAX
            )
            .is_err()
    );
    let bytes = fixture.manifest.to_bytes().unwrap();
    assert!(
        SignedManifest::from_bytes(&bytes)
            .unwrap()
            .verify(PrivateKey::new().public_key())
            .is_err()
    );
}

#[test]
fn row_and_field_pairing_cannot_be_combined_and_mixed_patches_are_atomic() {
    let fixture = Fixture::new();
    let field_a = fixture.app().field("display_name");
    let field_b = fixture.app().field("salary");
    let mut a = grant(fixture.public, &[Right::Update], &[field_a]);
    a.rows = RowSelection::Only([[1; 32]].into());
    let mut b = grant(fixture.public, &[Right::Update], &[field_b]);
    b.rows = RowSelection::Only([[2; 32]].into());
    let patch = change(
        fixture.public,
        1,
        Some([(field_a, b"old".to_vec())].into()),
        Some([(field_a, b"new".to_vec()), (field_b, b"private".to_vec())].into()),
    );
    assert!(!patch.permitted_by(&[a.clone(), b.clone()]));
    assert!(a.includes(&a));
    assert!(!a.includes(&b));
}

#[tokio::test]
async fn native_tcp_sessions_forward_original_ciphertext_and_named_acknowledgments() {
    use network::NativeSession;
    use tokio::net::{TcpListener, TcpStream};
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a = PrivateKey::new();
    let b = PrivateKey::new();
    let ac = fixture.config(&a, fixture.full(), &[fixture.public]);
    let bc = fixture.config(&b, fixture.full(), &[fixture.public]);
    let acred = ac.credentials.clone();
    let bcred = bc.credentials.clone();
    let source = EncryptedReplica::open(dir.path().join("a.db"), fixture.app(), ac)
        .await
        .unwrap();
    let target = EncryptedReplica::open(dir.path().join("b.db"), fixture.app(), bc)
        .await
        .unwrap();
    source
        .author(
            [1; 32],
            vec![change(
                fixture.public,
                1,
                None,
                Some(fixture.app().name("Native")),
            )],
            vec![],
        )
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut session =
            NativeSession::accept(stream, &source, a.clone(), acred, MAX_SECURE_FRAME * 4)
                .await
                .unwrap();
        session.serve_once(&source).await.unwrap();
        session.serve_once(&source).await.unwrap();
    };
    let client = async {
        let stream = TcpStream::connect(address).await.unwrap();
        let mut session = NativeSession::authenticate(
            stream,
            &target,
            b.clone(),
            bcred,
            a.public_key(),
            MAX_SECURE_FRAME * 4,
        )
        .await
        .unwrap();
        assert_eq!(
            session
                .pull(&target, vec![fixture.public], None, MAX_SECURE_FRAME / 2)
                .await
                .unwrap(),
            None
        );
    };
    tokio::join!(server, client);
    assert_eq!(
        target.snapshot(fixture.public).await.unwrap().rows[&[1; 32]],
        fixture.app().name("Native")
    );
    assert_eq!(source.applied_ack_count().await, 1);
}

#[tokio::test]
async fn learned_revocation_lost_keys_wrong_generations_and_package_subset() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let source_key = PrivateKey::new();
    let target_key = PrivateKey::new();
    let source_config = fixture.config(
        &source_key,
        fixture.full(),
        &[fixture.public, fixture.private],
    );
    let root_grant = source_config.credentials.0[0].id().unwrap();
    let source = EncryptedReplica::open(dir.path().join("source.db"), fixture.app(), source_config)
        .await
        .unwrap();
    let target_config = fixture.config(&target_key, fixture.full(), &[]);
    let target_credentials = target_config.credentials.clone();
    let target_path = dir.path().join("target.db");
    let target = EncryptedReplica::open(&target_path, fixture.app(), target_config)
        .await
        .unwrap();
    let frame = source
        .author(
            [1; 32],
            vec![change(
                fixture.private,
                1,
                None,
                Some(fixture.app().salary("987654321")),
            )],
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        target.receive(frame.clone()).await.unwrap(),
        ReceiveStatus::MissingKey
    );
    assert_eq!(
        target.companion(fixture.private, [1; 32]).await.unwrap(),
        Companion::Unavailable
    );
    target
        .install_key(KeyMaterial::new(fixture.private, 1, [22; 32]))
        .await
        .unwrap();
    assert_eq!(
        target.receipt_status(frame.id().unwrap()).await.unwrap(),
        ReceiveStatus::Applied
    );
    target.evict_key(fixture.private).await;
    assert_eq!(
        target.companion(fixture.private, [1; 32]).await.unwrap(),
        Companion::Unavailable
    );
    assert!(
        target
            .install_key(KeyMaterial::new(fixture.private, 1, [23; 32]))
            .await
            .is_err()
    );
    assert!(
        target
            .install_key(KeyMaterial::new(fixture.private, 2, [22; 32]))
            .await
            .is_err()
    );
    target
        .install_key(KeyMaterial::new(fixture.private, 1, [22; 32]))
        .await
        .unwrap();
    for (index, kind) in [
        PackageKind::CommandResult,
        PackageKind::Job,
        PackageKind::Blob,
        PackageKind::Checkpoint,
    ]
    .into_iter()
    .enumerate()
    {
        let public = EncryptedPackage::seal(
            fixture.public,
            rocky_core::KeyEpoch::new(1, &[11; 32]),
            kind.clone(),
            &[11; 32],
            b"public",
        )
        .unwrap();
        let private = EncryptedPackage::seal(
            fixture.private,
            rocky_core::KeyEpoch::new(1, &[22; 32]),
            kind,
            &[22; 32],
            b"SECRET SALARY",
        )
        .unwrap();
        let set = PackageSet::bind(&[public.clone(), private.clone()]).unwrap();
        assert!(
            !encode(&private)
                .unwrap()
                .windows(13)
                .any(|bytes| bytes == b"SECRET SALARY")
        );
        assert!(set.verify_subset(std::slice::from_ref(&public)).is_ok());
        assert_eq!(
            target.open_package(&private, &set).await.unwrap(),
            b"SECRET SALARY"
        );
        let other = EncryptedPackage::seal(
            fixture.private,
            rocky_core::KeyEpoch::new(1, &[22; 32]),
            PackageKind::Blob,
            &[22; 32],
            b"other",
        )
        .unwrap();
        assert!(set.verify_subset(&[other]).is_err());
        let public_config = fixture.config(
            &PrivateKey::new(),
            vec![grant(fixture.public, &[Right::Read], &[])],
            &[fixture.public],
        );
        let public_reader = EncryptedReplica::open(
            dir.path().join(format!("public-{index}.db")),
            fixture.app(),
            public_config,
        )
        .await
        .unwrap();
        assert_eq!(
            public_reader.open_package(&public, &set).await.unwrap(),
            b"public"
        );
        assert!(public_reader.open_package(&private, &set).await.is_err());
    }
    let revoked = revoking(
        &fixture.publisher,
        fixture.app().definition().app,
        &[],
        &[root_grant],
    );
    target.learn_revocations(&revoked).await.unwrap();
    // The revoked author's new frames are refused; the target's own chain,
    // which no statement names, stays current.
    let later = source
        .author(
            [2; 32],
            vec![change(
                fixture.public,
                2,
                None,
                Some(fixture.app().name("Later")),
            )],
            vec![],
        )
        .await
        .unwrap();
    assert!(target.receive(later).await.is_err());
    assert!(target.snapshot(fixture.private).await.is_ok());
    // Learning less removes nothing.
    assert!(
        target
            .learn_revocations(&RevocationSet::new())
            .await
            .unwrap()
            .is_empty()
    );
    drop(target);
    // Reopened with an empty supplied set, the file keeps what it learned.
    let mut config = fixture.config(&target_key, fixture.full(), &[]);
    config.credentials = target_credentials;
    assert_eq!(
        EncryptedReplica::open(&target_path, fixture.app(), config)
            .await
            .unwrap()
            .revocations()
            .await,
        revoked
    );
    assert!(
        EncryptedReplica::open(&target_path, fixture.app(), {
            let mut config = fixture.config(&target_key, fixture.full(), &[]);
            config.credentials = fixture.credentials(&target_key, fixture.full());
            config.revocations = revoking(
                &fixture.publisher,
                fixture.app().definition().app,
                &[config.credentials.0[0].capability.subject],
                &[],
            );
            config
        })
        .await
        .is_err()
    );
}

#[test]
fn validly_signed_delegation_cannot_expand_rows_fields_actions_or_depth() {
    let fixture = Fixture::new();
    let user = PrivateKey::new();
    let node = PrivateKey::new();
    let narrow = grant(
        fixture.public,
        &[Right::Update],
        &[fixture.app().field("display_name")],
    );
    let root = Capability {
        manifest: fixture.app().definition().hash(),
        subject: user.public_key(),
        kind: PrincipalKind::User,
        grants: vec![narrow.clone()],
        not_before: 0,
        expires: 100,
        delegations: 1,
        extension: rocky_credentials::Extension::new(rocky_credentials::Extended::default())
            .unwrap(),
    };
    let base = Capability {
        subject: node.public_key(),
        kind: PrincipalKind::Node,
        delegations: 0,
        ..root.clone()
    };
    for mutant in 0..4 {
        let mut child = base.clone();
        match mutant {
            0 => {
                child.grants[0].fields.insert(fixture.app().field("salary"));
            }
            1 => {
                child.grants[0].rights.insert(Right::Delete);
            }
            2 => {
                child.grants[0].scope = fixture.private;
            }
            _ => child.delegations = 1,
        }
        let chain = CredentialChain(vec![
            SignedCapability::sign(root.clone(), &fixture.publisher).unwrap(),
            SignedCapability::sign(child, &user).unwrap(),
        ]);
        assert!(
            chain
                .verify(
                    fixture.publisher.public_key(),
                    root.manifest,
                    node.public_key(),
                    &fixture.policy,
                    1
                )
                .is_err()
        );
    }
    let chain = CredentialChain(vec![
        SignedCapability::sign(root, &fixture.publisher).unwrap(),
        SignedCapability::sign(base, &user).unwrap(),
    ]);
    assert!(
        chain
            .verify(
                fixture.publisher.public_key(),
                fixture.app().definition().hash(),
                node.public_key(),
                &fixture.policy,
                1
            )
            .is_ok()
    );
}

#[tokio::test]
async fn concurrent_field_updates_converge_and_no_remote_operation_is_reauthored() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let a = PrivateKey::new();
    let b = PrivateKey::new();
    let ar = EncryptedReplica::open(
        dir.path().join("a.db"),
        fixture.app(),
        fixture.config(&a, fixture.full(), &[fixture.public]),
    )
    .await
    .unwrap();
    let br = EncryptedReplica::open(
        dir.path().join("b.db"),
        fixture.app(),
        fixture.config(&b, fixture.full(), &[fixture.public]),
    )
    .await
    .unwrap();
    let create = ar
        .author(
            [1; 32],
            vec![change(
                fixture.public,
                1,
                None,
                Some(fixture.app().name("Base")),
            )],
            vec![],
        )
        .await
        .unwrap();
    br.receive(create.clone()).await.unwrap();
    let aa = ar
        .author(
            [2; 32],
            vec![change(
                fixture.public,
                1,
                Some(fixture.app().name("Base")),
                Some(fixture.app().name("A")),
            )],
            vec![],
        )
        .await
        .unwrap();
    let bb = br
        .author(
            [3; 32],
            vec![change(
                fixture.public,
                1,
                Some(fixture.app().name("Base")),
                Some(fixture.app().name("B")),
            )],
            vec![],
        )
        .await
        .unwrap();
    ar.receive(bb.clone()).await.unwrap();
    br.receive(aa.clone()).await.unwrap();
    assert_eq!(
        ar.snapshot(fixture.public).await.unwrap().rows,
        br.snapshot(fixture.public).await.unwrap().rows
    );
    let creds = fixture.credentials(&b, fixture.full());
    let page = ar
        .transfer_page(
            b.public_key(),
            &creds,
            &[fixture.public],
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert_eq!(page.frames.len(), 3);
    let original: BTreeSet<_> = [create, aa, bb]
        .into_iter()
        .map(|frame| frame.to_bytes().unwrap())
        .collect();
    assert_eq!(
        page.frames
            .into_iter()
            .map(|frame| frame.to_bytes().unwrap())
            .collect::<BTreeSet<_>>(),
        original
    );
}

#[tokio::test]
#[ignore = "invoked by committed_ciphertext_survives_process_exit"]
async fn crash_child() {
    let path = std::env::var("ROCKY_CRASH_TEST_PATH").unwrap();
    let fixture = Fixture::with_publisher(PrivateKey::from_bytes(&[41; 32]));
    let node = PrivateKey::from_bytes(&[42; 32]);
    let replica = EncryptedReplica::open(
        path,
        fixture.app(),
        fixture.config(&node, fixture.full(), &[fixture.public]),
    )
    .await
    .unwrap();
    replica
        .author(
            [1; 32],
            vec![change(
                fixture.public,
                1,
                None,
                Some(fixture.app().name("Committed")),
            )],
            vec![],
        )
        .await
        .unwrap();
    // Exit without dropping the connection or file owner.
    std::process::exit(77);
}

#[tokio::test]
async fn committed_ciphertext_survives_process_exit() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("crash.db");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::crash_child", "--ignored"])
        .env("ROCKY_CRASH_TEST_PATH", &path)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(77));
    let fixture = Fixture::with_publisher(PrivateKey::from_bytes(&[41; 32]));
    let node = PrivateKey::from_bytes(&[42; 32]);
    let replica = EncryptedReplica::open(
        path,
        fixture.app(),
        fixture.config(&node, fixture.full(), &[fixture.public]),
    )
    .await
    .unwrap();
    assert_eq!(
        replica.snapshot(fixture.public).await.unwrap().rows[&[1; 32]],
        fixture.app().name("Committed")
    );
    let next = replica
        .author(
            [2; 32],
            vec![change(
                fixture.public,
                1,
                Some(fixture.app().name("Committed")),
                Some(fixture.app().name("Recovered")),
            )],
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(next.operation().unwrap().header.seq_num, 1);
}

async fn native_pull(
    source: &EncryptedReplica<Employees>,
    target: &EncryptedReplica<Employees>,
    source_identity: (&PrivateKey, CredentialChain),
    target_identity: (&PrivateKey, CredentialChain),
    scopes: Vec<ResourceScope>,
    count: usize,
) {
    use network::NativeSession;
    use tokio::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut session = NativeSession::accept(
            stream,
            source,
            source_identity.0.clone(),
            source_identity.1,
            MAX_SECURE_FRAME * 8,
        )
        .await
        .unwrap();
        for _ in 0..=count {
            session.serve_once(source).await.unwrap();
        }
    };
    let client = async {
        let stream = TcpStream::connect(address).await.unwrap();
        let mut session = NativeSession::authenticate(
            stream,
            target,
            target_identity.0.clone(),
            target_identity.1,
            source_identity.0.public_key(),
            MAX_SECURE_FRAME * 8,
        )
        .await
        .unwrap();
        assert!(
            session
                .pull(target, scopes, None, MAX_SECURE_FRAME / 2)
                .await
                .unwrap()
                .is_none()
        );
    };
    tokio::join!(server, client);
}

#[tokio::test]
async fn two_native_clients_and_keyless_relay_with_offline_manifest() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let alice = PrivateKey::new();
    let bob = PrivateKey::new();
    let relay = PrivateKey::new();
    let ac = fixture.config(&alice, fixture.full(), &[fixture.public, fixture.private]);
    let acred = ac.credentials.clone();
    let bc = fixture.config(
        &bob,
        vec![grant(
            fixture.public,
            &[Right::Read, Right::Update, Right::Custody],
            &[fixture.app().field("display_name")],
        )],
        &[fixture.public],
    );
    let bcred = bc.credentials.clone();
    let rc = fixture.config(
        &relay,
        [fixture.public, fixture.private]
            .into_iter()
            .map(|scope| grant(scope, &[Right::Custody], &[]))
            .collect(),
        &[],
    );
    let rcred = rc.credentials.clone();
    let a = EncryptedReplica::open(dir.path().join("a.db"), fixture.app(), ac)
        .await
        .unwrap();
    let b = EncryptedReplica::open(dir.path().join("b.db"), fixture.app(), bc)
        .await
        .unwrap();
    let r = EncryptedReplica::open(dir.path().join("relay.db"), fixture.app(), rc)
        .await
        .unwrap();
    a.author(
        [1; 32],
        vec![change(
            fixture.public,
            1,
            None,
            Some(fixture.app().name("Offline")),
        )],
        vec![],
    )
    .await
    .unwrap();
    a.author(
        [2; 32],
        vec![change(
            fixture.private,
            1,
            None,
            Some(fixture.app().salary("777777777")),
        )],
        vec![],
    )
    .await
    .unwrap();
    native_pull(
        &a,
        &r,
        (&alice, acred),
        (&relay, rcred.clone()),
        vec![fixture.public, fixture.private],
        2,
    )
    .await;
    native_pull(
        &r,
        &b,
        (&relay, rcred),
        (&bob, bcred),
        vec![fixture.public],
        1,
    )
    .await;
    assert_eq!(
        b.snapshot(fixture.public).await.unwrap().rows[&[1; 32]],
        fixture.app().name("Offline")
    );
    assert!(b.snapshot(fixture.private).await.is_err());
    assert!(r.snapshot(fixture.private).await.is_err());
    let artifact = std::fs::read(dir.path().join("relay.db")).unwrap();
    assert!(!artifact.windows(9).any(|bytes| bytes == b"777777777"));
}

#[tokio::test]
async fn confirmed_result_binds_private_subpackages_without_granting_access() {
    use rocky_core::{
        AppliedFrontier, NodeId, PrincipalId, RequestId, StrongPackage, StrongRequest,
    };
    let fixture = Fixture::new();
    let scope = fixture.public.domain;
    let caller = PrincipalId([7; 16]);
    let node = NodeId([3; 16]);
    let coordinator = rocky_coordinator::MemoryCoordinator::default();
    coordinator
        .register(scope, caller, [6; 16], vec![node], 1, 1)
        .unwrap();
    let base = coordinator.acquire(scope, caller, node).unwrap();
    let public = EncryptedPackage::seal(
        fixture.public,
        rocky_core::KeyEpoch::new(1, &[11; 32]),
        PackageKind::CommandResult,
        &[11; 32],
        b"public result",
    )
    .unwrap();
    let private = EncryptedPackage::seal(
        fixture.private,
        rocky_core::KeyEpoch::new(1, &[22; 32]),
        PackageKind::CommandResult,
        &[22; 32],
        b"private salary",
    )
    .unwrap();
    let binding = PackageSet::bind(&[public.clone(), private.clone()]).unwrap();
    let request = StrongRequest {
        scope,
        caller,
        id: RequestId {
            node,
            sequence: rocky_core::next_request_sequence(0, now().unwrap()).unwrap(),
        },
        method: "Publish".into(),
        version: 1,
        schema: 1,
        handler: 1,
        client_request: [1; 32],
        arguments: vec![],
        policy: [1; 32],
        frontier: AppliedFrontier::default(),
        digest: [1; 32],
    };
    let package = StrongPackage {
        request,
        base,
        sequence: 1,
        rows: binding.to_bytes().unwrap(),
        cut_base: Default::default(),
    };
    let receipt = coordinator
        .confirm(package.confirmation().unwrap())
        .unwrap();
    let verified = PackageSet::verify_confirmed_subset(
        &package,
        &receipt,
        &coordinator,
        std::slice::from_ref(&public),
    )
    .await
    .unwrap();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let reader = EncryptedReplica::open(
        dir.path().join("reader.db"),
        fixture.app(),
        fixture.config(
            &key,
            vec![grant(fixture.public, &[Right::Read], &[])],
            &[fixture.public],
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        reader.open_package(&public, &verified).await.unwrap(),
        b"public result"
    );
    assert!(reader.open_package(&private, &verified).await.is_err());
    let mut changed = package.clone();
    changed.rows.push(0);
    assert!(
        PackageSet::verify_confirmed_subset(&changed, &receipt, &coordinator, &[public])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn signatures_wrong_scope_generation_and_mixed_batch_fail_without_revision() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let other = PrivateKey::new();
    let source = EncryptedReplica::open(
        dir.path().join("source.db"),
        fixture.app(),
        fixture.config(&key, fixture.full(), &[fixture.public]),
    )
    .await
    .unwrap();
    let target = EncryptedReplica::open(
        dir.path().join("target.db"),
        fixture.app(),
        fixture.config(&other, fixture.full(), &[fixture.public]),
    )
    .await
    .unwrap();
    let frame = source
        .author(
            [1; 32],
            vec![change(
                fixture.public,
                1,
                None,
                Some(fixture.app().name("Public")),
            )],
            vec![],
        )
        .await
        .unwrap();
    let mut broken = frame.clone();
    broken.body[30] ^= 1;
    assert!(target.receive(broken).await.is_err());
    let mut operation = frame.operation().unwrap();
    operation.header.extensions.generation.generation = 2;
    operation.header.sign(&key);
    let wrong_generation = frame.with_header(operation.header.to_bytes());
    // A key epoch this replica does not hold waits for its key; nothing applies.
    assert_eq!(
        target.receive(wrong_generation).await.unwrap(),
        ReceiveStatus::MissingKey
    );
    operation.header.extensions.generation.generation = 1;
    operation.header.extensions.scope.domain.mesh = MeshId([99; 16]);
    operation.header.sign(&key);
    let wrong_scope = frame.with_header(operation.header.to_bytes());
    assert!(target.receive(wrong_scope).await.is_err());
    assert_eq!(target.snapshot(fixture.public).await.unwrap().revision, 0);
    let narrow_node = PrivateKey::new();
    let narrow_grants = vec![grant(
        fixture.public,
        &[Right::Read, Right::Update, Right::Custody],
        &[fixture.app().field("display_name")],
    )];
    let narrow = EncryptedReplica::open(
        dir.path().join("narrow.db"),
        fixture.app(),
        fixture.config(&narrow_node, narrow_grants, &[fixture.public]),
    )
    .await
    .unwrap();
    narrow.receive(frame).await.unwrap();
    let before = narrow.snapshot(fixture.public).await.unwrap();
    assert!(
        narrow
            .author(
                [2; 32],
                vec![
                    change(
                        fixture.public,
                        1,
                        Some(fixture.app().name("Public")),
                        Some(fixture.app().name("Allowed"))
                    ),
                    change(
                        fixture.public,
                        2,
                        None,
                        Some(fixture.app().name("Forbidden"))
                    )
                ],
                vec![]
            )
            .await
            .is_err()
    );
    let after = narrow.snapshot(fixture.public).await.unwrap();
    assert_eq!(before.rows, after.rows);
    assert_eq!(before.revision, after.revision);
}

mod commands;
mod confirmed_tables;

mod recovery;

mod effects;

mod scaling;

mod checkpoint;

mod admission;
mod bundles;
mod chains;
mod constraints;
mod delegation;
mod delivery;
mod existence;
mod exposure;
mod generations;
mod group;
mod partitions;
mod revocations;
#[cfg(unix)]
mod roles;
mod rotation;
mod sql;
mod typed_commands;

mod strong_history;

mod formats;

mod partition_foreign_keys;

pub(crate) mod group_sync_measurement;
