//! Credentials verify, issue, enroll and renew with no database and no
//! endpoint: everything here is keys and bytes.
use rocky_core::{
    AppId, ApplicationManifest, AuthorLogScope, DefinitionItem, DomainId, DomainRule,
    FieldSelection, MeshId, PrincipalKind, ResourceGrant, ResourceScope, Right, RowSelection,
    Scope,
};
use rocky_credentials::*;
use std::collections::BTreeSet;

const NOW: u64 = 1_800_000_000;
const VALIDITY: (u64, u64, u64) = (NOW, 30 * DAY, 300);

struct Domain {
    publisher: PrivateKey,
    manifest: ApplicationManifest,
    resources: Vec<Resource>,
}
impl Domain {
    fn new() -> Self {
        let publisher = PrivateKey::new();
        let key = *publisher.public_key().as_bytes();
        let mut manifest = ApplicationManifest {
            publisher: key,
            name: "example".into(),
            app: ApplicationManifest::app_id(key, "example"),
            schema: 1,
            projection: 1,
            entities: Vec::new(),
            fields: Vec::new(),
            operations: Vec::new(),
            domains: vec![DomainRule {
                domain: DomainId([2; 16]),
                author_log: AuthorLogScope::Domain,
                rule: "independent".into(),
            }],
        };
        let mut resources = Vec::new();
        for (index, name) in ["notes", "payroll"].into_iter().enumerate() {
            let table = manifest.item_id("entity", name);
            let field = manifest.item_id("field", name);
            for (items, id) in [
                (&mut manifest.entities, table),
                (&mut manifest.fields, field),
            ] {
                items.push(DefinitionItem {
                    id,
                    name: name.into(),
                    shape: "record".into(),
                });
            }
            resources.push(Resource {
                name: name.into(),
                scope: ResourceScope {
                    domain: Scope {
                        mesh: MeshId([1; 16]),
                        app: AppId(manifest.app.0),
                        domain: DomainId([2; 16]),
                    },
                    encryption: [index as u8 + 1; 32],
                    table,
                },
                columns: [("text".to_owned(), field)].into(),
            });
        }
        Self {
            publisher,
            manifest,
            resources,
        }
    }
    fn authority(&self) -> Authority<'_> {
        Authority::owner(&self.publisher)
    }
    fn policy(&self) -> RevocationSet {
        RevocationSet::new()
    }
    /// A set whose one statement revokes `subjects` as identities.
    fn revoking(&self, subjects: &[PublicKey]) -> RevocationSet {
        let statement = Revocation {
            app: self.manifest.app,
            issuer: self.publisher.public_key(),
            time: NOW,
            nodes: BTreeSet::new(),
            identities: subjects.iter().copied().collect(),
            capabilities: BTreeSet::new(),
        };
        let signed = self.authority().revoke(statement).expect("revocation");
        RevocationSet::try_from(vec![signed]).expect("set")
    }
    fn names(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }
    /// A device enrolled with `profile`: its user and node keys and chain.
    fn device(&self, profile: &Profile) -> (PrivateKey, PrivateKey, CredentialChain) {
        let (user, node) = (PrivateKey::new(), PrivateKey::new());
        let root = self
            .authority()
            .issue(
                user.public_key(),
                profile.grants(&self.resources).expect("grants"),
                rocky_credentials::AuthorRange::ALL,
                VALIDITY,
            )
            .expect("capability");
        let chain = device_chain(root, &user, node.public_key(), 1).expect("chain");
        (user, node, chain)
    }
    fn verify(
        &self,
        chain: &CredentialChain,
        node: &PrivateKey,
        policy: &RevocationSet,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        chain.verify(self.publisher.public_key(), node.public_key(), policy, time)
    }
}
fn rights(grants: &[ResourceGrant]) -> BTreeSet<Right> {
    grants
        .iter()
        .flat_map(|grant| grant.rights.iter().copied())
        .collect()
}

#[test]
fn a_supplied_set_verifies_with_keys_and_bytes_alone() {
    let domain = Domain::new();
    let writer = Profile::Writer {
        scopes: Domain::names(&["notes"]),
    };
    let (user, node, chain) = domain.device(&writer);
    let credentials = Credentials {
        publisher: domain.publisher.public_key(),
        revocations: domain.policy(),
        chain,
    };
    // Any channel may carry the bytes; they verify on arrival.
    let received = Credentials::from_bytes(&credentials.to_bytes().expect("bytes")).expect("set");
    let verified = received
        .verify(node.public_key(), domain.manifest.app, NOW)
        .expect("verified");
    assert_eq!(verified.principal, user.public_key());
    assert_eq!(verified.device, node.public_key());
    assert_eq!(
        verified.revocations,
        RevocationSet::new().digest(NOW).expect("digest")
    );
    assert_eq!(verified.expires, NOW + 30 * DAY);
    assert_eq!(verified.grants.len(), 1);
    assert_eq!(verified.grants[0].scope, domain.resources[0].scope);
    // Another device, a later time and another publisher are refused.
    assert!(
        received
            .verify(PrivateKey::new().public_key(), domain.manifest.app, NOW)
            .is_err()
    );
    assert!(
        received
            .verify(node.public_key(), domain.manifest.app, NOW + 31 * DAY)
            .is_err()
    );
    // Another application's credentials are refused.
    assert!(
        received
            .verify(node.public_key(), AppId([9; 16]), NOW)
            .is_err()
    );
    let mut foreign = received.clone();
    foreign.publisher = PrivateKey::new().public_key();
    assert!(
        foreign
            .verify(node.public_key(), domain.manifest.app, NOW)
            .is_err()
    );
    let mut bytes = credentials.to_bytes().expect("bytes");
    bytes.push(0);
    assert!(Credentials::from_bytes(&bytes).is_err());
}

#[test]
fn chains_keep_publisher_revocations_expiry_attenuation_and_paired_rights() {
    let domain = Domain::new();
    let policy = domain.policy();
    let writer = Profile::Writer {
        scopes: Domain::names(&["notes"]),
    };
    let (user, node, chain) = domain.device(&writer);
    assert!(domain.verify(&chain, &node, &policy, NOW).is_ok());
    assert!(domain.verify(&chain, &node, &policy, NOW - 301).is_err());
    assert!(
        domain
            .verify(&chain, &node, &policy, NOW + 30 * DAY)
            .is_err()
    );
    // Another publisher and a revocation of any link end the chain; a
    // revocation of someone else does not. No generation is named: the
    // chain holds in each.
    assert!(
        chain
            .verify(
                PrivateKey::new().public_key(),
                node.public_key(),
                &policy,
                NOW
            )
            .is_err()
    );
    for key in [user.public_key(), node.public_key()] {
        assert!(
            domain
                .verify(&chain, &node, &domain.revoking(&[key]), NOW)
                .is_err()
        );
    }
    let unrelated = domain.revoking(&[PrivateKey::new().public_key()]);
    assert!(domain.verify(&chain, &node, &unrelated, NOW).is_ok());
    // A link valid for longer than a chain may live is refused.
    let mut long = chain.0[0].capability.clone();
    long.expires = long.not_before + MAX_CHAIN_LIFETIME + LIFETIME_SKEW + 1;
    let long = device_chain(
        SignedCapability::sign(long, &domain.publisher).expect("signed"),
        &user,
        node.public_key(),
        1,
    )
    .expect("chain");
    assert!(domain.verify(&long, &node, &policy, NOW).is_err());
    assert!(
        domain
            .authority()
            .issue(
                user.public_key(),
                chain.0[0].capability.grants.clone(),
                rocky_credentials::AuthorRange::ALL,
                (NOW, MAX_CHAIN_LIFETIME + 1, 300)
            )
            .is_err()
    );
    // A leaf cannot add a scope, a right or a field its parent lacks.
    let root = chain.0[0].clone();
    let widen = |change: &dyn Fn(&mut ResourceGrant)| {
        let mut leaf = Capability {
            subject: node.public_key(),
            kind: PrincipalKind::Node,
            delegations: 0,
            extension: Extension::new(Extended {
                authors: Some(AuthorRange::new(1, 1).unwrap()),
                node: Some(rocky_credentials::PeerId::of(node.public_key())),
                ..Default::default()
            })
            .expect("node binding"),
            ..root.capability.clone()
        };
        change(&mut leaf.grants[0]);
        CredentialChain(vec![
            root.clone(),
            SignedCapability::sign(leaf, &user).expect("leaf"),
        ])
    };
    let other = domain.resources[1].scope;
    assert!(
        domain
            .verify(&widen(&|grant| grant.scope = other), &node, &policy, NOW)
            .is_err()
    );
    assert!(
        domain
            .verify(
                &widen(&|grant| {
                    grant.rights.insert(Right::PublishSchema);
                }),
                &node,
                &policy,
                NOW
            )
            .is_err()
    );
    // Narrowing is delegation: fewer rights on selected rows.
    let narrowed = widen(&|grant| {
        grant.rights = [Right::Read].into();
        grant.rows = RowSelection::Only([[1; 32]].into());
    });
    let grants = domain
        .verify(&narrowed, &node, &policy, NOW)
        .expect("narrowed");
    assert_eq!(rights(&grants), [Right::Read].into());
}

#[test]
fn a_login_principal_alone_grants_nothing() {
    let domain = Domain::new();
    let policy = domain.policy();
    // A provider vouches for a user. That names who asks; it is no grant.
    struct Provider;
    impl LoginProvider for Provider {
        fn verify(&self, assertion: &[u8], _time: u64) -> Result<ExternalLogin, Error> {
            Ok(ExternalLogin {
                provider: "example".into(),
                subject: String::from_utf8_lossy(assertion).into_owned(),
            })
        }
    }
    let login = Provider.verify(b"alice", NOW).expect("login");
    assert_eq!(login.subject, "alice");
    // The user signs itself the capability it would like to hold.
    let (user, node) = (PrivateKey::new(), PrivateKey::new());
    let grants = Profile::Issuer.grants(&domain.resources).expect("grants");
    let claimed = Authority::owner(&user)
        .issue(
            user.public_key(),
            grants,
            rocky_credentials::AuthorRange::ALL,
            VALIDITY,
        )
        .expect("self-signed");
    let chain = device_chain(claimed, &user, node.public_key(), 1).expect("chain");
    assert!(domain.verify(&chain, &node, &policy, NOW).is_err());
    assert!(
        chain
            .provenance(domain.publisher.public_key(), node.public_key())
            .is_err()
    );
    // Distinct identities are distinct types over distinct keys.
    let principal = Identity(user.public_key());
    let device = SigningDevice(node.public_key());
    assert_ne!(principal.0, device.0);
    assert_ne!(Publisher(domain.publisher.public_key()).0, principal.0);
}

#[test]
fn profiles_are_scoped_and_hold_no_schema_right() {
    let domain = Domain::new();
    let custody = Profile::Custody {
        scopes: Domain::names(&["notes"]),
    };
    let grants = custody.grants(&domain.resources).expect("custody");
    assert_eq!(rights(&grants), [Right::Custody].into());
    assert!(grants[0].fields.is_none());
    assert!(!custody.reads("notes") && !custody.issues());
    let reader = Profile::Reader {
        scopes: Domain::names(&["notes"]),
    };
    let writer = Profile::Writer {
        scopes: Domain::names(&["payroll"]),
    };
    for profile in [&reader, &writer, &Profile::Issuer] {
        let grants = profile.grants(&domain.resources).expect("grants");
        assert!(!rights(&grants).contains(&Right::PublishSchema));
        assert!(grants.iter().all(|grant| grant.rows == RowSelection::All));
    }
    let grants = reader.grants(&domain.resources).expect("reader");
    assert_eq!(rights(&grants), [Right::Read, Right::Custody].into());
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].scope, domain.resources[0].scope);
    assert!(reader.reads("notes") && !reader.reads("payroll") && !reader.issues());
    let grants = writer.grants(&domain.resources).expect("writer");
    assert_eq!(grants[0].scope, domain.resources[1].scope);
    assert!(rights(&grants).contains(&Right::Insert));
    assert_eq!(
        Profile::Issuer
            .grants(&domain.resources)
            .expect("issuer")
            .len(),
        2
    );
    // No default and no unknown scope.
    for names in [&[][..], &["missing"][..], &["notes", "missing"][..]] {
        let scopes = Domain::names(names);
        assert!(
            Profile::Reader {
                scopes: scopes.clone()
            }
            .grants(&domain.resources)
            .is_err()
        );
        assert!(
            Profile::Writer { scopes }
                .grants(&domain.resources)
                .is_err()
        );
    }
}

#[test]
fn an_invitation_binds_its_profile_counts_wrong_tokens_and_keeps_its_retry_identity() {
    let token = [5; 32];
    let profile = Profile::Reader {
        scopes: Domain::names(&["notes"]),
    };
    let mut invitation = Invitation::open(&token, profile.clone(), NOW + 600);
    assert_eq!(invitation.profile, profile);
    assert!(invitation.is_open(&token, NOW));
    assert!(invitation.is_open(&token, NOW + 600));
    assert!(!invitation.is_open(&token, NOW + 601));
    assert!(!invitation.is_open(&[6; 32], NOW));
    let request = |token: &[u8; 32], nonce: u8| {
        PairRequest::new(
            token,
            format!("{nonce:02x}").repeat(32),
            PrivateKey::new().public_key(),
            PrivateKey::new().public_key(),
            [3; 32],
        )
    };
    let first = request(&token, 1);
    assert_eq!(
        invitation.admit(&token, &first, NOW).expect("first"),
        Admission::First
    );
    // Until claimed, nothing is consumed: a refused context costs nothing.
    assert_eq!(
        invitation.admit(&token, &first, NOW).expect("again"),
        Admission::First
    );
    invitation.claim(&first);
    assert!(!invitation.is_open(&token, NOW));
    assert_eq!(
        invitation.admit(&token, &first, NOW).expect("retry"),
        Admission::Retry
    );
    assert!(invitation.admit(&token, &request(&token, 2), NOW).is_err());
    assert!(invitation.admit(&token, &first, NOW + 601).is_err());
    // The sealed enrollment opens for that request and token only.
    let sealed = seal_enrollment(&token, &first, b"enrollment").expect("sealed");
    let round = PairRequest::from_bytes(&first.to_bytes().expect("bytes")).expect("request");
    assert_eq!(
        open_enrollment(&token, &round, &sealed).expect("opened"),
        b"enrollment"
    );
    assert!(open_enrollment(&[6; 32], &first, &sealed).is_err());
    assert!(open_enrollment(&token, &request(&token, 1), &sealed).is_err());
    // Ten wrong tokens lock the invitation, also for the right one.
    let mut locked = Invitation::open(&token, profile, NOW + 600);
    for attempt in 0..10 {
        assert!(
            locked
                .admit(&token, &request(&[attempt + 100; 32], 1), NOW)
                .is_err()
        );
    }
    assert!(locked.admit(&token, &first, NOW).is_err());
    assert!(!locked.is_open(&token, NOW));
}

#[test]
fn a_joining_issuers_routes_are_bound_by_its_proof() {
    let token = [5; 32];
    let mut invitation = Invitation::open(&token, Profile::Issuer, NOW + 600);
    let routes = vec![("tcp".to_owned(), "127.0.0.1:4000".to_owned())];
    let request = PairRequest::new(
        &token,
        "01".repeat(32),
        PrivateKey::new().public_key(),
        PrivateKey::new().public_key(),
        [3; 32],
    )
    .with_routes(&token, routes.clone());
    let round = PairRequest::from_bytes(&request.to_bytes().expect("bytes")).expect("request");
    assert_eq!(round.routes, routes);
    let mut moved = round.clone();
    moved.routes[0].1 = "127.0.0.1:4001".into();
    assert!(invitation.admit(&token, &moved, NOW).is_err());
    assert_eq!(
        invitation.admit(&token, &round, NOW).expect("admitted"),
        Admission::First
    );
}

#[test]
fn renewal_keeps_the_grants_refuses_the_revoked_and_seals_to_the_device() {
    let domain = Domain::new();
    let policy = domain.policy();
    let issuer = PrivateKey::new().public_key();
    let reader = Profile::Reader {
        scopes: Domain::names(&["notes"]),
    };
    let (user, node, chain) = domain.device(&reader);
    let (request, secret) = RenewRequest::sign(&node, issuer, chain.clone(), NOW).expect("request");
    let request = RenewRequest::from_bytes(&request.to_bytes().expect("bytes")).expect("request");
    let prior = request
        .verify(issuer, domain.publisher.public_key(), (NOW, 300))
        .expect("verified");
    let renewal = Renewal::seal(
        &domain.authority(),
        &request,
        prior,
        policy.clone(),
        VALIDITY,
        b"keys",
    )
    .expect("renewal");
    let renewal = Renewal::from_bytes(&renewal.to_bytes().expect("bytes")).expect("renewal");
    assert_eq!(
        renewal
            .open(domain.publisher.public_key(), &request, &secret)
            .expect("payload"),
        b"keys"
    );
    // The same grants: a reader stays a reader.
    assert_eq!(
        renewal.capability.capability.grants,
        chain.0[0].capability.grants
    );
    let renewed =
        device_chain(renewal.capability.clone(), &user, node.public_key(), 1).expect("chain");
    assert_eq!(
        rights(
            &domain
                .verify(&renewed, &node, &policy, NOW)
                .expect("grants")
        ),
        [Right::Read, Right::Custody].into()
    );

    // Another issuer, a stale request and another device's secret are refused.
    let other = PrivateKey::new().public_key();
    let anchor = (domain.publisher.public_key(),);
    assert!(request.verify(other, anchor.0, (NOW, 300)).is_err());
    assert!(request.verify(issuer, anchor.0, (NOW + 301, 300)).is_err());
    let (_, stranger) = RenewRequest::sign(&node, issuer, chain.clone(), NOW).expect("request");
    assert!(renewal.open(anchor.0, &request, &stranger).is_err());
    // An answer for another request, or signed by anyone else, is refused.
    let (again, again_secret) =
        RenewRequest::sign(&node, issuer, chain.clone(), NOW).expect("request");
    assert!(renewal.open(anchor.0, &again, &again_secret).is_err());
    let impostor = PrivateKey::new();
    let forged = Renewal::seal(
        &Authority::owner(&impostor),
        &request,
        prior,
        policy.clone(),
        VALIDITY,
        b"keys",
    )
    .expect("forged");
    assert!(forged.open(anchor.0, &request, &secret).is_err());

    // A chain the device widened itself has no provenance.
    let mut widened = chain.0[0].capability.clone();
    widened.grants = Profile::Issuer.grants(&domain.resources).expect("grants");
    let claimed = CredentialChain(vec![
        SignedCapability::sign(widened, &user).expect("self-signed"),
        chain.0[1].clone(),
    ]);
    let (escalating, _) = RenewRequest::sign(&node, issuer, claimed, NOW).expect("request");
    assert!(escalating.verify(issuer, anchor.0, (NOW, 300)).is_err());

    // An expired chain still proves who asks; another device's revocation
    // does not stop it.
    let later = NOW + 90 * DAY;
    let next = domain.revoking(&[PrivateKey::new().public_key()]);
    let (late, late_secret) =
        RenewRequest::sign(&node, issuer, chain.clone(), later).expect("request");
    let prior = late
        .verify(issuer, anchor.0, (later, 300))
        .expect("provenance");
    let renewal = Renewal::seal(
        &domain.authority(),
        &late,
        prior,
        next.clone(),
        (later, 30 * DAY, 300),
        b"",
    )
    .expect("renewal");
    renewal.open(anchor.0, &late, &late_secret).expect("opened");
    assert_eq!(renewal.revocations, next);

    // Revoking the user or the device ends renewal and every chain naming it.
    for subject in [user.public_key(), node.public_key()] {
        let revoked = domain.revoking(&[subject]);
        assert!(
            Renewal::seal(
                &domain.authority(),
                &request,
                prior,
                revoked.clone(),
                VALIDITY,
                b""
            )
            .is_err()
        );
        let reissued = domain
            .authority()
            .issue(
                user.public_key(),
                chain.0[0].capability.grants.clone(),
                rocky_credentials::AuthorRange::ALL,
                VALIDITY,
            )
            .expect("capability");
        let chain = device_chain(reissued, &user, node.public_key(), 1).expect("chain");
        assert!(domain.verify(&chain, &node, &revoked, NOW).is_err());
    }
}

#[test]
fn raw_grants_name_rights_and_columns_and_profiles_are_built_on_them() {
    let mut resources = Domain::new().resources;
    resources[0].columns.insert("title".into(), [6; 32]);
    let parse = |value: serde_json::Value| serde_json::from_value::<Vec<Grant>>(value);
    // Any rights to whoever the issuer names; columns narrow fields.
    let raw = parse(serde_json::json!([
        {"scope": "notes", "rights": ["read", "update", "custody"], "columns": ["title"]},
        {"scope": "payroll", "rights": ["delete", "custody"]},
    ]))
    .expect("grants");
    let grants = Grant::resolve_all(&raw, &resources).expect("resolved");
    assert_eq!(grants[0].fields, FieldSelection::Only([[6; 32]].into()));
    assert_eq!(rights(&grants[1..]), [Right::Delete, Right::Custody].into());
    assert_eq!(
        serde_json::to_value(&raw[0]).expect("json")["rights"],
        serde_json::json!(["read", "update", "custody"])
    );
    // Each signed grant describes back to the raw grant that made it.
    let described: Vec<Grant> = grants
        .iter()
        .filter_map(|grant| Grant::describe(grant, &resources))
        .collect();
    assert_eq!(described, raw);
    assert_eq!(
        Profile::of(&grants, &resources),
        Profile::Grants { grants: raw }
    );
    // Unknown names, empty sets and the schema right are refused.
    for refused in [
        serde_json::json!([{"scope": "notes", "rights": ["read"], "columns": ["x"]}]),
        serde_json::json!([{"scope": "x", "rights": ["read"]}]),
        serde_json::json!([{"scope": "notes", "rights": []}]),
        serde_json::json!([{"scope": "notes", "rights": ["publish_schema"]}]),
        serde_json::json!([]),
    ] {
        let parsed = parse(refused.clone()).expect("parsed");
        assert!(
            Grant::resolve_all(&parsed, &resources).is_err(),
            "{refused}"
        );
    }
    // So are names that are no right, the removed command rights included.
    for name in ["fly", "invoke", "execute"] {
        assert!(parse(serde_json::json!([{"scope": "notes", "rights": [name]}])).is_err());
    }
    assert!(parse(serde_json::json!([{"scope": "notes", "rights": [], "rows": "all"}])).is_err());
    // The starter profiles are functions over raw grants.
    let writer = Profile::Writer {
        scopes: Domain::names(&["payroll"]),
    };
    let raw = writer.raw(&resources).expect("raw");
    assert_eq!(raw.len(), 1);
    assert!(raw[0].rights.contains(&Right::Insert) && raw[0].columns.is_none());
    assert_eq!(
        Profile::Grants { grants: raw }
            .grants(&resources)
            .expect("grants"),
        writer.grants(&resources).expect("writer")
    );
    let reader = Profile::Reader {
        scopes: Domain::names(&["notes"]),
    };
    let grants = reader.grants(&resources).expect("reader");
    assert_eq!(Profile::of(&grants, &resources), reader);
    assert!(
        Profile::Grants { grants: Vec::new() }
            .grants(&resources)
            .is_err()
    );
}

#[test]
fn delegation_narrows_and_a_delegated_chain_is_renewed_by_its_delegator() {
    let domain = Domain::new();
    let policy = domain.policy();
    let grants = Profile::Writer {
        scopes: Domain::names(&["notes", "payroll"]),
    }
    .grants(&domain.resources)
    .expect("grants");
    let (holder, holder_node) = (PrivateKey::new(), PrivateKey::new());
    let root = domain
        .authority()
        .issue_delegable(
            holder.public_key(),
            grants.clone(),
            2,
            rocky_credentials::AuthorRange::ALL,
            VALIDITY,
        )
        .expect("root");
    let own = device_chain(root.clone(), &holder, holder_node.public_key(), 1).expect("own");
    assert_eq!(own.delegable().0.len(), 1);
    let (delegate, node) = (PrivateKey::new(), PrivateKey::new());
    let mut narrow = grants[0].clone();
    narrow.rights = [Right::Read, Right::Custody].into();
    let link = own
        .delegable()
        .delegate(
            &holder,
            delegate.public_key(),
            PrincipalKind::User,
            vec![narrow.clone()],
            1,
            Some(AuthorRange::ALL),
            (NOW, 90 * DAY, 300),
        )
        .expect("delegated");
    // The delegation ends with its parent, however long it was asked for.
    assert_eq!(link.expires(), NOW + 30 * DAY);
    let chain = link
        .clone()
        .complete(&delegate, node.public_key(), 12)
        .expect("chain");
    assert_eq!(chain.0.len(), 3);
    assert_eq!(
        domain
            .verify(&chain, &node, &policy, NOW)
            .expect("verified"),
        [narrow.clone()]
    );
    assert_eq!(chain.grants(), [narrow.clone()]);
    // Wider grants, other scopes, more depth and holding no key are refused.
    let mut wider = narrow.clone();
    wider.rights.insert(Right::Delete);
    let mut other = narrow.clone();
    other.scope = ResourceScope {
        table: [9; 32],
        ..narrow.scope
    };
    let delegate_with = |holder: &PrivateKey, grants: Vec<ResourceGrant>, depth: u8| {
        own.delegable().delegate(
            holder,
            delegate.public_key(),
            PrincipalKind::User,
            grants,
            depth,
            None,
            VALIDITY,
        )
    };
    let mut beyond = grants[0].clone();
    beyond.rights.insert(Right::PublishSchema);
    for (holder, grants, depth) in [
        (&holder, vec![beyond], 1),
        (&holder, vec![other], 1),
        (&holder, vec![narrow.clone()], 2),
        (&holder, vec![narrow.clone()], 0),
        (&holder, Vec::new(), 1),
        (&delegate, vec![narrow.clone()], 1),
    ] {
        assert!(delegate_with(holder, grants, depth).is_err());
    }
    let mut widened = wider.clone();
    widened.rights.insert(Right::PublishSchema);
    assert!(delegate_with(&holder, vec![wider, widened], 1).is_err());
    // The delegate reaches only its own node: one link is all it allows.
    assert!(
        link.delegate(
            &delegate,
            PrivateKey::new().public_key(),
            PrincipalKind::User,
            vec![narrow],
            1,
            None,
            VALIDITY
        )
        .is_err()
    );
    // An issuer renews the delegator, never the delegate: the root is not its.
    let issuer = PrivateKey::new().public_key();
    let (request, _) = RenewRequest::sign(&node, issuer, chain, NOW).expect("request");
    let anchor = (domain.publisher.public_key(),);
    assert!(request.verify(issuer, anchor.0, (NOW, 300)).is_err());
    // A renewed delegator keeps its depth.
    let renewed = domain
        .authority()
        .renew(&root.capability, &policy, VALIDITY)
        .expect("renewed");
    assert_eq!(renewed.capability.delegations, 2);
}

#[test]
fn node_leaves_bind_one_number_inside_the_principal_range() {
    let domain = Domain::new();
    let identity = PrivateKey::new();
    let nodes = [PrivateKey::new(), PrivateKey::new()];
    let grants = Profile::Issuer.grants(&domain.resources).unwrap();
    let root = domain
        .authority()
        .issue_terms(
            identity.public_key(),
            grants.clone(),
            (1, Some(AuthorRange::new(100, 199).unwrap())),
            VALIDITY,
        )
        .unwrap();
    let prefix = CredentialChain(vec![root]);
    let chains: Vec<_> = nodes
        .iter()
        .zip([100, 101])
        .map(|(node, number)| {
            prefix
                .clone()
                .complete(&identity, node.public_key(), number)
                .unwrap()
        })
        .collect();
    for (index, chain) in chains.iter().enumerate() {
        let number = 100 + index as u32;
        for resource in &domain.resources {
            let verified = chain
                .verify_author(
                    domain.publisher.public_key(),
                    nodes[index].public_key(),
                    number,
                    resource.scope.table,
                    &domain.policy(),
                    NOW,
                )
                .unwrap();
            assert_eq!(
                verified,
                vec![
                    grants
                        .iter()
                        .find(|grant| grant.scope == resource.scope)
                        .unwrap()
                        .clone()
                ]
            );
        }
        let bytes = encode(chain).unwrap();
        let decoded: CredentialChain = decode_bounded(&bytes, MAX_CREDENTIAL_BYTES).unwrap();
        assert_eq!(decoded.author_number().unwrap(), number);
        assert!(
            decoded
                .verify_author(
                    domain.publisher.public_key(),
                    nodes[index].public_key(),
                    number + 1,
                    domain.resources[0].scope.table,
                    &domain.policy(),
                    NOW
                )
                .is_err()
        );
    }
    assert_ne!(
        rocky_core::RowIdentity::next(100, 0).unwrap(),
        rocky_core::RowIdentity::next(101, 0).unwrap()
    );
    for outside in [0, 99, 200, MAX_AUTHOR + 1] {
        assert!(
            prefix
                .clone()
                .complete(&identity, nodes[0].public_key(), outside)
                .is_err()
        );
    }
    let mut forged = chains[0].clone();
    let mut leaf = forged.0.pop().unwrap().capability;
    leaf.extension = Extension::new(Extended {
        authors: Some(AuthorRange::new(200, 200).unwrap()),
        node: Some(rocky_credentials::PeerId::of(nodes[0].public_key())),
        ..Default::default()
    })
    .unwrap();
    forged
        .0
        .push(SignedCapability::sign(leaf, &identity).unwrap());
    assert!(
        forged
            .verify(
                domain.publisher.public_key(),
                nodes[0].public_key(),
                &domain.policy(),
                NOW
            )
            .is_err()
    );
}

#[test]
fn ranges_narrow_by_delegation_and_unknown_extension_versions_are_refused() {
    let domain = Domain::new();
    let policy = domain.policy();
    let grants = Profile::Writer {
        scopes: Domain::names(&["notes"]),
    }
    .grants(&domain.resources)
    .expect("grants");
    let holder = PrivateKey::new();
    let range = AuthorRange::new(10, 20).expect("range");
    let root = domain
        .authority()
        .issue_terms(
            holder.public_key(),
            grants.clone(),
            (2, Some(range)),
            VALIDITY,
        )
        .expect("root");
    assert_eq!(root.capability.authors().expect("authors"), Some(range));
    let prefix = CredentialChain(vec![root.clone()]);
    let delegate = PrivateKey::new();
    let delegate_with = |authors| {
        prefix.delegate(
            &holder,
            delegate.public_key(),
            PrincipalKind::User,
            grants.clone(),
            1,
            Some(authors),
            VALIDITY,
        )
    };
    let narrowed = delegate_with(AuthorRange::new(12, 15).expect("range")).expect("narrowed");
    assert!(delegate_with(AuthorRange::new(15, 25).expect("range")).is_err());
    let node = PrivateKey::new();
    let chain = narrowed
        .clone()
        .complete(&delegate, node.public_key(), 12)
        .expect("chain");
    let verify = |chain: &CredentialChain| {
        chain.verify(
            domain.publisher.public_key(),
            node.public_key(),
            &policy,
            NOW,
        )
    };
    assert!(verify(&chain).is_ok());
    // A delegate that signs itself a wider range is refused on verification.
    let mut wide = narrowed.0[1].capability.clone();
    wide.extension = Extension::new(Extended {
        partitions: Vec::new(),
        authors: Some(AuthorRange::new(5, 15).expect("range")),
        node: None,
        issues: false,
        trusted: None,
    })
    .expect("extension");
    let widened = CredentialChain(vec![
        root.clone(),
        SignedCapability::sign(wide, &holder).expect("signed"),
    ])
    .complete(&delegate, node.public_key(), 12)
    .expect("chain");
    assert!(verify(&widened).is_err());
    // A node link cannot carry a user extension.
    let mut leaf = chain.0[2].capability.clone();
    leaf.extension = narrowed.0[1].capability.extension.clone();
    let mut tagged = chain.clone();
    tagged.0[2] = SignedCapability::sign(leaf, &delegate).expect("signed");
    assert!(verify(&tagged).is_err());
    // A later extension version is refused with a clear error.
    #[derive(serde::Serialize)]
    struct Later {
        version: u32,
        authors: AuthorRange,
        rotation: u64,
    }
    let bytes = rocky_credentials::encode(&Later {
        version: 6,
        authors: range,
        rotation: 9,
    })
    .expect("bytes");
    let later: Extension = serde_json::from_value(serde_json::json!(bytes)).expect("later");
    let mut capability = root.capability.clone();
    capability.extension = later;
    let message = SignedCapability::sign(capability, &domain.publisher)
        .expect_err("unsupported extension must refuse before signing its portable binding")
        .to_string();
    assert!(
        message.contains("unsupported credential extension version"),
        "{message}"
    );
}
