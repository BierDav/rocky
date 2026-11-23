pub(crate) mod live_publication;
use super::*;
pub(crate) const PARTITION_NAME: rocky_core::PartitionName = [0x5a; 32];
use employees_app::Employees;
use rocky_core::{
    Commitment, FieldSelection, MeshId, PrincipalKind, ResourceGrant, ResourceScope, Right,
    RowChange, RowFields, RowSelection, RowWrite,
};
use rocky_platform::fs::Scratch;
use std::collections::BTreeSet;

pub(crate) struct Fixture {
    pub(crate) publisher: PrivateKey,
    manifest: SignedManifest,
    policy: RevocationSet,
    pub(crate) public: ResourceScope,
    pub(crate) private: ResourceScope,
}

pub(crate) async fn check_frontier_cache<A: rocky_core::RowApplication>(
    replica: &EncryptedReplica<A>,
    scope: ResourceScope,
) {
    let first = replica.frontier(scope).await.unwrap();
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    let before = inner.frontier_cache_stats();
    assert_eq!(inner.frontier(scope).await.unwrap(), first);
    assert_eq!(inner.frontier_cache_stats(), before);
    assert_eq!(inner.compute_tip_frontier(scope).await.unwrap(), first);
}
pub(crate) async fn stored_frames<A: rocky_core::RowApplication>(
    replica: &EncryptedReplica<A>,
    sql: &str,
) -> Vec<EncryptedFrame> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
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
pub(crate) async fn seal_main<A: rocky_core::RowApplication>(
    replica: &EncryptedReplica<A>,
    owner: &PrivateKey,
    domain: rocky_core::Scope,
) -> Vec<TipStatement> {
    use rocky_credentials::{
        BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SEAL_FORMAT, SealAction, SealStatement,
        SignedBookmarkMove, SignedSeal,
    };
    let bookmark = |from, to| {
        TipStatement::Bookmark(
            SignedBookmarkMove::sign(
                BookmarkMoveStatement {
                    format: BOOKMARK_MOVE_FORMAT,
                    scope: domain,
                    name: "main".into(),
                    from,
                    to: Some(to),
                },
                owner,
            )
            .unwrap(),
        )
    };
    let mut statements = Vec::new();
    if replica.followed_bookmark().await.is_none() {
        let created = bookmark(None, replica.followed_tip().await);
        replica
            .publish_tip_statements(std::slice::from_ref(&created))
            .await
            .unwrap();
        replica.follow_bookmark(domain, "main").await.unwrap();
        statements.push(created);
    }
    let tip = replica.followed_tip().await;
    let schema = replica
        .tip_graph(domain)
        .await
        .unwrap()
        .schema(tip)
        .unwrap();
    let seal = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: domain,
            tip,
            frontier: replica.seal_frontier(domain, false).await.unwrap(),
            action: SealAction::Continue { schema },
        },
        owner,
    )
    .unwrap();
    let next = seal.successor().unwrap().unwrap();
    let advanced = [TipStatement::Seal(seal), bookmark(Some(tip), next)];
    replica.publish_tip_statements(&advanced).await.unwrap();
    statements.extend(advanced);
    statements
}
pub(crate) async fn seal_and_compact<A: rocky_core::RowApplication>(
    replica: &EncryptedReplica<A>,
    owner: &PrivateKey,
    domain: rocky_core::Scope,
) -> Vec<TipStatement> {
    let statements = seal_main(replica, owner, domain).await;
    let report = replica.compact(domain, 0).await.unwrap();
    if report.compacted.is_none() {
        let status = replica.compaction_status(domain).await;
        panic!("{report:?} {status:?}");
    }
    statements
}
pub(crate) async fn compacted_base<A: rocky_core::RowApplication>(
    replica: &EncryptedReplica<A>,
    owner: &PrivateKey,
    scope: ResourceScope,
) -> crate::SignedCheckpoint {
    seal_and_compact(replica, owner, scope.domain).await;
    replica.checkpoint(scope).await.unwrap()
}
pub(crate) async fn import_needed_bases<A: rocky_core::RowApplication>(
    source: &EncryptedReplica<A>,
    target: &EncryptedReplica<A>,
    key: &PrivateKey,
    credentials: &CredentialChain,
    domain: rocky_core::Scope,
) -> Vec<crate::SignedCheckpoint> {
    source
        .admit_peer(key.public_key(), credentials, target.followed_tip().await)
        .await
        .unwrap();
    let mut imported = Vec::new();
    for need in target.seal_rebootstraps(domain).await.unwrap() {
        let offered = source
            .checkpoint_offer(key.public_key(), credentials, need.scope)
            .await
            .unwrap()
            .expect("a verified base at a seal");
        offered.verify_signature().unwrap();
        let checkpoint = offered.id().unwrap();
        target.begin_rebootstrap(offered.clone()).await.unwrap();
        let (_, mut after, complete) = target
            .rebootstrap_progress(need.scope)
            .await
            .unwrap()
            .unwrap();
        if !complete {
            loop {
                let page = source
                    .checkpoint_page(
                        key.public_key(),
                        credentials,
                        need.scope,
                        checkpoint,
                        after,
                        MAX_SECURE_FRAME,
                    )
                    .await
                    .unwrap();
                after = page.next;
                target.stage_checkpoint_page(page).await.unwrap();
                if after.is_none() {
                    break;
                }
            }
        }
        assert_eq!(
            target
                .finish_rebootstrap(need.scope)
                .await
                .unwrap()
                .checkpoint,
            checkpoint
        );
        imported.push(offered);
    }
    assert!(target.seal_rebootstraps(domain).await.unwrap().is_empty());
    imported
}
pub(crate) fn assert_signed_empty(signed: &crate::SignedCheckpoint) {
    signed.verify_signature().unwrap();
    let statement = &signed.statement;
    assert!(statement.seal.is_some());
    assert_eq!(
        (
            statement.operations,
            statement.rows.count,
            statement.tombstones,
            statement.creations.count
        ),
        (0, 0, 0, 0)
    );
}
pub(crate) async fn exchange_graphs<A: rocky_core::RowApplication>(
    a: &EncryptedReplica<A>,
    b: &EncryptedReplica<A>,
    domain: rocky_core::Scope,
) {
    let (forward, back) = (all_tip_statements(a).await, all_tip_statements(b).await);
    for (replica, statements) in [(b, &forward), (a, &back)] {
        replica.receive_tip_statements(statements).await.unwrap();
    }
    for replica in [a, b] {
        if replica.followed_bookmark().await.is_none()
            && replica
                .tip_graph(domain)
                .await
                .unwrap()
                .bookmark("main")
                .unwrap()
                .is_some()
        {
            replica.follow_bookmark(domain, "main").await.unwrap();
        }
    }
}
pub(crate) async fn link_up<A: rocky_core::RowApplication>(
    a: &EncryptedReplica<A>,
    b: &EncryptedReplica<A>,
    domain: rocky_core::Scope,
) {
    exchange_graphs(a, b, domain).await;
    for (server, client) in [(a, b), (b, a)] {
        let (node, credentials) = {
            let inner = client.inner.lock(crate::replica::Class::Background).await;
            (
                inner.config.node.public_key(),
                inner.config.credentials.clone(),
            )
        };
        server
            .admit_peer(node, &credentials, client.followed_tip().await)
            .await
            .unwrap();
    }
}
pub(crate) async fn all_tip_statements<A: rocky_core::RowApplication>(
    replica: &EncryptedReplica<A>,
) -> Vec<TipStatement> {
    let mut statements = Vec::new();
    let mut after = None;
    loop {
        let page = replica.tip_statements(after, 32).await.unwrap();
        let Some(last) = page.last() else {
            return statements;
        };
        after = Some(last.id().unwrap());
        statements.extend(page);
    }
}
pub(crate) fn identity_of(node: &PrivateKey) -> PrivateKey {
    PrivateKey::from_bytes(&rocky_core::commitment(
        "rocky.test.node-identity",
        node.as_bytes(),
    ))
}
pub(crate) fn proven(
    capability: Capability,
    node: &PrivateKey,
    identity: &PrivateKey,
) -> Capability {
    let mut extended = capability.extended().unwrap();
    extended.possession = Some(
        rocky_credentials::NodePossession::sign(
            node,
            identity.public_key(),
            extended.database.unwrap(),
        )
        .unwrap(),
    );
    Capability {
        extension: rocky_credentials::Extension::new(extended).unwrap(),
        ..capability
    }
}
pub(crate) const LIFETIME_START: &str = "ROCKY_TEST_LIFETIME_START";
pub(crate) fn lifetime() -> (u64, u64) {
    static LIFETIME: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    *LIFETIME.get_or_init(|| {
        let start = std::env::var(LIFETIME_START).ok().map_or_else(
            || now().unwrap() - rocky_credentials::DAY,
            |start| start.parse().unwrap(),
        );
        (start, start + 301 * rocky_credentials::DAY)
    })
}
pub(crate) fn validity() -> rocky_credentials::Validity {
    rocky_credentials::Validity {
        issued_at: now().unwrap(),
        lifetime: std::time::Duration::from_secs(300 * rocky_credentials::DAY),
        clock_skew: std::time::Duration::from_secs(rocky_credentials::DAY),
    }
}
pub(crate) fn unrevoked() -> Commitment {
    RevocationSet::new().digest(0).unwrap()
}
pub(crate) fn revoking(
    publisher: &PrivateKey,
    app: rocky_core::AppId,
    users: &[PublicKey],
    capabilities: &[Commitment],
) -> RevocationSet {
    let statement = Revocation {
        app,
        issuer: publisher.public_key(),
        time: now().unwrap(),
        nodes: BTreeSet::new(),
        identities: users.iter().copied().collect(),
        capabilities: capabilities.iter().copied().collect(),
        cut: Default::default(),
        mode: rocky_credentials::RevocationMode::Lost,
    };
    RevocationSet::try_from(vec![
        SignedRevocation::sign_in(
            statement,
            publisher,
            &CredentialChain::default(),
            rocky_credentials::DatabaseContext {
                publisher: publisher.public_key(),
                scope: rocky_core::Scope {
                    mesh: MeshId([1; 16]),
                    app,
                    domain: rocky_core::DomainId([1; 16]),
                },
            },
        )
        .unwrap(),
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
    pub(crate) fn new() -> Self {
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
    pub(crate) fn app(&self) -> Employees {
        Employees::in_domain(
            *self.publisher.public_key().as_bytes(),
            self.public.domain.domain,
        )
    }
    pub(crate) fn credentials(
        &self,
        node: &PrivateKey,
        grants: Vec<ResourceGrant>,
    ) -> CredentialChain {
        self.chain(node, grants, &self.publisher, lifetime())
    }
    fn chain(
        &self,
        node: &PrivateKey,
        grants: Vec<ResourceGrant>,
        root: &PrivateKey,
        lifetime: (u64, u64),
    ) -> CredentialChain {
        self.chain_under(node, grants, root, &identity_of(node), lifetime)
    }
    fn chain_under(
        &self,
        node: &PrivateKey,
        grants: Vec<ResourceGrant>,
        root: &PrivateKey,
        user: &PrivateKey,
        (not_before, expires): (u64, u64),
    ) -> CredentialChain {
        let capability = |subject, kind, delegations| Capability {
            subject,
            kind,
            grants: if kind != PrincipalKind::Node {
                grants.clone()
            } else {
                Vec::new()
            },
            not_before,
            expires: if kind == PrincipalKind::Node {
                rocky_credentials::NEVER
            } else {
                expires
            },
            delegations,
            extension: rocky_credentials::Extension::new(rocky_credentials::Extended {
                node: (kind == PrincipalKind::Node).then(|| rocky_credentials::PeerId::of(subject)),
                database: Some(rocky_credentials::DatabaseContext {
                    publisher: root.public_key(),
                    scope: self.public.domain,
                }),
                sequence: (kind == PrincipalKind::Node).then_some(0),
                ..Default::default()
            })
            .unwrap(),
        };
        CredentialChain(vec![
            SignedCapability::sign(capability(user.public_key(), PrincipalKind::User, 1), root)
                .unwrap(),
            SignedCapability::sign(
                crate::tests::proven(
                    capability(node.public_key(), PrincipalKind::Node, 0),
                    node,
                    user,
                ),
                user,
            )
            .unwrap(),
        ])
    }
    pub(crate) fn config(
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
    pub(crate) fn full(&self) -> Vec<ResourceGrant> {
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
pub(crate) fn change(
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn encrypted_offline_publisher_public_private_and_opaque_relay() {
    let fixture = Fixture::new();
    let dir = Scratch::new().await;
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
    for path in rocky_platform::fs::read_dir(dir.path()).unwrap() {
        if path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("relay")
        {
            let bytes = rocky_platform::fs::read(path).unwrap();
            assert!(!bytes.windows(9).any(|bytes| bytes == b"987654321"));
            assert!(!bytes.windows(5).any(|bytes| bytes == b"Alice"));
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn rollback_retry_restart_and_missing_encrypted_dependencies() {
    let fixture = Fixture::new();
    let dir = Scratch::new().await;
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
    assert_ne!(&first.body_bytes()[..24], &third.body_bytes()[..24]);
    target.receive(third).await.unwrap();
    assert_eq!(
        target.companion(fixture.public, [1; 32]).await.unwrap(),
        Companion::Deleted
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn signatures_attenuation_expiry_revocation_and_cross_app_replay() {
    let fixture = Fixture::new();
    let key = PrivateKey::new();
    let chain = fixture.credentials(&key, fixture.full());
    let time = now().unwrap();
    let verify = |chain: &CredentialChain, revocations: &RevocationSet| {
        chain.verify(
            fixture.publisher.public_key(),
            key.public_key(),
            revocations,
            time,
        )
    };
    assert!(verify(&chain, &fixture.policy).is_ok());
    let mut tampered = chain.clone();
    tampered.0[0].capability.grants[0].fields = FieldSelection::All;
    assert!(verify(&tampered, &fixture.policy).is_err());
    let app = fixture.app().definition().app;
    let ended = revoking(&fixture.publisher, app, &[], &[chain.0[0].id().unwrap()]);
    assert!(verify(&chain, &ended).is_err());
    let other = revoking(
        &fixture.publisher,
        app,
        &[PrivateKey::new().public_key()],
        &[],
    );
    assert!(
        verify(&chain, &other).is_ok(),
        "a statement naming someone else leaves the chain current"
    );
    assert!(
        chain
            .verify(
                fixture.publisher.public_key(),
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
            .verify_historic(PrivateKey::new().public_key())
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn link_tcp_sessions_forward_original_ciphertext_and_named_acknowledgments() {
    let fixture = Fixture::new();
    let dir = Scratch::new().await;
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
    let (near, far) = network::link::testing::connected().await;
    let (mut session, mut served) = network::link::testing::sessions_over(
        near,
        far,
        (&target, b.clone(), bcred),
        (&source, a.clone(), acred),
    )
    .await
    .unwrap();
    let server = async {
        served.serve_once(&source).await.unwrap();
        served.serve_once(&source).await.unwrap();
    };
    let client = async {
        assert_eq!(
            session
                .catch_up(&target, fixture.public, MAX_SECURE_FRAME / 2)
                .await
                .unwrap()
                .frames,
            1
        );
    };
    tokio::join!(server, client);
    assert_eq!(
        target.snapshot(fixture.public).await.unwrap().rows[&[1; 32]],
        fixture.app().name("Native")
    );
    let acknowledged = crate::storage::rows(
        &source
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT applied FROM secure_peer_frontiers",
    )
    .await
    .unwrap();
    assert_eq!(acknowledged, vec![vec![rocky_core::SqlValue::Integer(1)]]);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn learned_revocation_lost_keys_and_wrong_generations() {
    let fixture = Fixture::new();
    let dir = Scratch::new().await;
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
    let private = EncryptedPackage::seal(
        fixture.private,
        rocky_core::KeyEpoch::new(1, &[22; 32]),
        PackageKind::Checkpoint,
        &[22; 32],
        b"SECRET SALARY",
    )
    .unwrap();
    assert!(
        !encode(&private)
            .unwrap()
            .windows(13)
            .any(|bytes| bytes == b"SECRET SALARY"),
        "a sealed package carries no plaintext"
    );
    let revoked = revoking(
        &fixture.publisher,
        fixture.app().definition().app,
        &[],
        &[root_grant],
    );
    target.learn_revocations(&revoked).await.unwrap();
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
    assert!(
        target.receive(later).await.is_err(),
        "the revoked author's new frames are refused"
    );
    assert!(
        target.snapshot(fixture.private).await.is_ok(),
        "the target's own chain, which no statement names, stays current"
    );
    assert!(
        target
            .learn_revocations(&RevocationSet::new())
            .await
            .unwrap()
            .is_empty(),
        "learning less removes nothing"
    );
    let expected = target.revocations().await;
    drop(target);
    let mut config = fixture.config(&target_key, fixture.full(), &[]);
    config.credentials = target_credentials;
    assert_eq!(
        EncryptedReplica::open(&target_path, fixture.app(), config)
            .await
            .unwrap()
            .revocations()
            .await,
        expected,
        "reopened with an empty supplied set, the file keeps what it learned"
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn validly_signed_delegation_cannot_expand_rows_fields_actions_or_depth() {
    let fixture = Fixture::new();
    let user = PrivateKey::new();
    let node = PrivateKey::new();
    let delegate = PrivateKey::new();
    let narrow = grant(
        fixture.public,
        &[Right::Update],
        &[fixture.app().field("display_name")],
    );
    let root = Capability {
        subject: user.public_key(),
        kind: PrincipalKind::User,
        grants: vec![narrow.clone()],
        not_before: 0,
        expires: 100,
        delegations: 2,
        extension: rocky_credentials::Extension::new(rocky_credentials::Extended {
            ..Default::default()
        })
        .unwrap(),
    };
    let base = Capability {
        subject: delegate.public_key(),
        kind: PrincipalKind::User,
        delegations: 1,
        extension: rocky_credentials::Extension::new(rocky_credentials::Extended {
            ..Default::default()
        })
        .unwrap(),
        ..root.clone()
    };
    for mutant in 0..4 {
        let mut child = base.clone();
        match mutant {
            0 => {
                if let FieldSelection::Only(fields) = &mut child.grants[0].fields {
                    fields.insert(fixture.app().field("salary"));
                }
            }
            1 => {
                child.grants[0].rights.insert(Right::Delete);
            }
            2 => {
                child.grants[0].scope = fixture.private;
            }
            _ => child.delegations = 2,
        }
        let chain = CredentialChain(vec![
            SignedCapability::sign(root.clone(), &fixture.publisher).unwrap(),
            SignedCapability::sign(child, &user).unwrap(),
        ]);
        assert!(
            chain
                .certify(&delegate, &node)
                .and_then(|chain| chain.verify(
                    fixture.publisher.public_key(),
                    node.public_key(),
                    &fixture.policy,
                    1
                ))
                .is_err()
        );
    }
    let chain = CredentialChain(vec![
        SignedCapability::sign(root, &fixture.publisher).unwrap(),
        SignedCapability::sign(base, &user).unwrap(),
    ]);
    assert!(
        chain
            .certify(&delegate, &node)
            .and_then(|chain| chain.verify(
                fixture.publisher.public_key(),
                node.public_key(),
                &fixture.policy,
                1
            ))
            .is_ok()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn concurrent_field_updates_converge_and_no_remote_operation_is_reauthored() {
    let fixture = Fixture::new();
    let dir = Scratch::new().await;
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

#[cfg(not(target_family = "wasm"))]
const CRASH_EXIT_CODE: i32 = 77;

#[cfg(not(target_family = "wasm"))]
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
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
    std::process::exit(CRASH_EXIT_CODE);
}

#[cfg(not(target_family = "wasm"))]
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn committed_ciphertext_survives_process_exit() {
    let dir = Scratch::new().await;
    let path = dir.path().join("crash.db");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::crash_child", "--ignored"])
        .env("ROCKY_CRASH_TEST_PATH", &path)
        .env(LIFETIME_START, lifetime().0.to_string())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(CRASH_EXIT_CODE));
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
) {
    let (near, far) = network::link::testing::connected().await;
    let (mut session, mut served) = network::link::testing::sessions_over(
        near,
        far,
        (target, target_identity.0.clone(), target_identity.1),
        (source, source_identity.0.clone(), source_identity.1),
    )
    .await
    .unwrap();
    let server = served.serve_to_end(source);
    let client = async {
        for scope in scopes {
            assert!(
                !session
                    .catch_up(target, scope, MAX_SECURE_FRAME / 2)
                    .await
                    .unwrap()
                    .exhausted
            );
        }
        session.close().await.unwrap();
    };
    let (served, ()) = tokio::join!(server, client);
    served.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn two_native_clients_and_keyless_relay_with_offline_manifest() {
    let fixture = Fixture::new();
    let dir = Scratch::new().await;
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
    )
    .await;
    native_pull(&r, &b, (&relay, rcred), (&bob, bcred), vec![fixture.public]).await;
    assert_eq!(
        b.snapshot(fixture.public).await.unwrap().rows[&[1; 32]],
        fixture.app().name("Offline")
    );
    assert!(b.snapshot(fixture.private).await.is_err());
    assert!(r.snapshot(fixture.private).await.is_err());
    let artifact = rocky_platform::fs::read(dir.path().join("relay.db")).unwrap();
    assert!(!artifact.windows(9).any(|bytes| bytes == b"777777777"));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn signatures_wrong_scope_generation_and_mixed_batch_fail_without_revision() {
    let fixture = Fixture::new();
    let dir = Scratch::new().await;
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
    let mut body = frame.body_bytes().to_vec();
    body[30] ^= 1;
    let broken = frame.with_body(body);
    assert!(target.receive(broken).await.is_err());
    let mut operation = frame.operation().unwrap();
    operation.header.extensions.generation.generation = 2;
    operation.header.sign(&key);
    let wrong_generation = frame.with_header(operation.header.to_bytes());
    assert_eq!(
        target.receive(wrong_generation).await.unwrap(),
        ReceiveStatus::MissingKey,
        "a key epoch this replica does not hold waits for its key; nothing applies"
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

mod scaling;

mod checkpoint;

mod admission;
pub(crate) mod blockers;
mod bundles;
mod chains;
mod compaction;
#[cfg(not(target_family = "wasm"))]
mod delegation;
mod delivery;
mod failed_commit;
mod formats;
mod generations;
mod group;
mod header_scans;
mod partitions;
mod placements;
mod recovery;
#[cfg(unix)]
mod replayed_constraints;
pub(crate) mod revocations;
mod roles;
mod rotation;
#[cfg(not(target_family = "wasm"))]
mod slow_peer;
mod sql;
mod tip_network;
mod tip_schemas;
mod tips;

pub(crate) mod group_sync_measurement;

mod bulk;

mod membership;
mod positions;
