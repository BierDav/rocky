//! Credentials verify, issue, enroll and renew with no database and no
//! endpoint: everything here is keys and bytes.
use rocky_core::{
    AppId, ApplicationManifest, AuthorLogScope, Commitment, DefinitionItem, DomainId, DomainRule,
    MeshId, PrincipalKind, ResourceGrant, ResourceScope, Right, RowSelection, Scope,
};
use rocky_credentials::*;
use std::collections::BTreeSet;

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 24 * 60 * 60;
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
            handler: 1,
            entities: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
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
                invokes: BTreeSet::new(),
                executes: BTreeSet::new(),
            });
        }
        Self {
            publisher,
            manifest,
            resources,
        }
    }
    fn authority(&self) -> Authority<'_> {
        Authority {
            publisher: &self.publisher,
            manifest: self.manifest.hash(),
        }
    }
    fn policy(&self) -> SignedPolicy {
        SignedPolicy::sign(
            PolicyStatement {
                manifest: self.manifest.hash(),
                epoch: 1,
                revoked: BTreeSet::new(),
            },
            &self.publisher,
        )
        .expect("policy")
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
                1,
                VALIDITY,
            )
            .expect("capability");
        let chain = device_chain(root, &user, node.public_key()).expect("chain");
        (user, node, chain)
    }
    fn verify(
        &self,
        chain: &CredentialChain,
        node: &PrivateKey,
        policy: &SignedPolicy,
        time: u64,
    ) -> Result<Vec<ResourceGrant>, Error> {
        chain.verify(
            self.publisher.public_key(),
            self.manifest.hash(),
            node.public_key(),
            &policy.statement,
            time,
        )
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
        manifest: SignedManifest::sign(&domain.manifest, &domain.publisher).expect("manifest"),
        policy: domain.policy(),
        chain,
    };
    // Any channel may carry the bytes; they verify on arrival.
    let received = Credentials::from_bytes(&credentials.to_bytes().expect("bytes")).expect("set");
    let verified = received.verify(node.public_key(), NOW).expect("verified");
    assert_eq!(verified.principal, user.public_key());
    assert_eq!(verified.device, node.public_key());
    assert_eq!(verified.epoch, 1);
    assert_eq!(verified.expires, NOW + 30 * DAY);
    assert_eq!(verified.grants.len(), 1);
    assert_eq!(verified.grants[0].scope, domain.resources[0].scope);
    // Another device, a later time and another publisher are refused.
    assert!(
        received
            .verify(PrivateKey::new().public_key(), NOW)
            .is_err()
    );
    assert!(received.verify(node.public_key(), NOW + 31 * DAY).is_err());
    let mut foreign = received.clone();
    foreign.publisher = PrivateKey::new().public_key();
    assert!(foreign.verify(node.public_key(), NOW).is_err());
    let mut bytes = credentials.to_bytes().expect("bytes");
    bytes.push(0);
    assert!(Credentials::from_bytes(&bytes).is_err());
}

#[test]
fn chains_keep_manifest_epoch_expiry_attenuation_and_paired_rights() {
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
    // Another manifest and a later epoch end the chain.
    assert!(
        chain
            .verify(
                domain.publisher.public_key(),
                [9; 32],
                node.public_key(),
                &policy.statement,
                NOW
            )
            .is_err()
    );
    let next = domain
        .authority()
        .revoke(&policy.statement, [])
        .expect("policy");
    assert_eq!(next.statement.epoch, 2);
    assert!(domain.verify(&chain, &node, &next, NOW).is_err());
    // A leaf cannot add a scope, a right or a field its parent lacks.
    let root = chain.0[0].clone();
    let widen = |change: &dyn Fn(&mut ResourceGrant)| {
        let mut leaf = Capability {
            subject: node.public_key(),
            kind: PrincipalKind::Node,
            delegations: 0,
            ..root.capability.clone()
        };
        change(&mut leaf.grants[0]);
        CredentialChain(vec![
            root.clone(),
            SignedCapability::sign(leaf, &user).expect("leaf"),
        ])
    };
    let other = domain.resources[1].scope;
    let extra: Commitment = [7; 32];
    assert!(
        domain
            .verify(&widen(&|grant| grant.scope = other), &node, &policy, NOW)
            .is_err()
    );
    assert!(
        domain
            .verify(
                &widen(&|grant| {
                    grant.rights.insert(Right::Execute);
                }),
                &node,
                &policy,
                NOW
            )
            .is_err()
    );
    assert!(
        domain
            .verify(
                &widen(&|grant| {
                    grant.fields.insert(extra);
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
    let claimed = Authority {
        publisher: &user,
        manifest: domain.manifest.hash(),
    }
    .issue(user.public_key(), grants, 1, VALIDITY)
    .expect("self-signed");
    let chain = device_chain(claimed, &user, node.public_key()).expect("chain");
    assert!(domain.verify(&chain, &node, &policy, NOW).is_err());
    assert!(
        chain
            .provenance(
                domain.publisher.public_key(),
                domain.manifest.hash(),
                node.public_key()
            )
            .is_err()
    );
    // Distinct identities are distinct types over distinct keys.
    let principal = LoginPrincipal(user.public_key());
    let device = SigningDevice(node.public_key());
    assert_ne!(principal.0, device.0);
    assert_ne!(Publisher(domain.publisher.public_key()).0, principal.0);
}

#[test]
fn profiles_are_scoped_and_hold_no_executor_or_schema_right() {
    let domain = Domain::new();
    let custody = Profile::Custody {
        scopes: Domain::names(&["notes"]),
    };
    let grants = custody.grants(&domain.resources).expect("custody");
    assert_eq!(rights(&grants), [Right::Custody].into());
    assert!(grants[0].fields.is_empty());
    assert!(!custody.reads("notes") && !custody.issues());
    let reader = Profile::Reader {
        scopes: Domain::names(&["notes"]),
    };
    let writer = Profile::Writer {
        scopes: Domain::names(&["payroll"]),
    };
    let forbidden: BTreeSet<Right> = [Right::Invoke, Right::Execute, Right::PublishSchema].into();
    for profile in [&reader, &writer, &Profile::Issuer] {
        let grants = profile.grants(&domain.resources).expect("grants");
        assert!(rights(&grants).is_disjoint(&forbidden));
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
    // Command methods add paired rights: a writer of the scope arguments
    // are sealed for invokes, and only an issuer executes.
    let mut commanded = Domain::new().resources;
    let (reserve, audit) = ([7; 32], [8; 32]);
    commanded[1].invokes = [reserve].into();
    commanded[0].executes = [reserve, audit].into();
    commanded[1].executes = [reserve].into();
    let grants = writer.grants(&commanded).expect("writer");
    assert!(grants[0].rights.contains(&Right::Invoke) && grants[0].fields.contains(&reserve));
    assert!(!grants[0].rights.contains(&Right::Execute));
    let both = Profile::Writer {
        scopes: Domain::names(&["notes", "payroll"]),
    };
    let grants = both.grants(&commanded).expect("writer");
    assert!(!grants[0].rights.contains(&Right::Invoke) && !grants[0].fields.contains(&reserve));
    assert!(rights(&reader.grants(&commanded).expect("reader")).is_disjoint(&forbidden));
    let grants = Profile::Issuer.grants(&commanded).expect("issuer");
    assert!(
        grants
            .iter()
            .all(|grant| grant.rights.contains(&Right::Execute))
    );
    assert!(grants[0].fields.contains(&audit) && !grants[1].fields.contains(&audit));
    assert!(
        !grants[0].rights.contains(&Right::Invoke) && grants[1].rights.contains(&Right::Invoke)
    );
    assert!(!rights(&grants).contains(&Right::PublishSchema));
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
        .verify(
            issuer,
            domain.publisher.public_key(),
            domain.manifest.hash(),
            (NOW, 300),
        )
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
        device_chain(renewal.capability.clone(), &user, node.public_key()).expect("chain");
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
    let anchor = (domain.publisher.public_key(), domain.manifest.hash());
    assert!(
        request
            .verify(other, anchor.0, anchor.1, (NOW, 300))
            .is_err()
    );
    assert!(
        request
            .verify(issuer, anchor.0, anchor.1, (NOW + 301, 300))
            .is_err()
    );
    let (_, stranger) = RenewRequest::sign(&node, issuer, chain.clone(), NOW).expect("request");
    assert!(renewal.open(anchor.0, &request, &stranger).is_err());
    // An answer for another request, or signed by anyone else, is refused.
    let (again, again_secret) =
        RenewRequest::sign(&node, issuer, chain.clone(), NOW).expect("request");
    assert!(renewal.open(anchor.0, &again, &again_secret).is_err());
    let impostor = PrivateKey::new();
    let forged = Renewal::seal(
        &Authority {
            publisher: &impostor,
            manifest: domain.manifest.hash(),
        },
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
    assert!(
        escalating
            .verify(issuer, anchor.0, anchor.1, (NOW, 300))
            .is_err()
    );

    // An expired chain under an old epoch still proves who asks.
    let later = NOW + 90 * DAY;
    let next = domain
        .authority()
        .revoke(&policy.statement, [])
        .expect("policy");
    let (late, late_secret) =
        RenewRequest::sign(&node, issuer, chain.clone(), later).expect("request");
    let prior = late
        .verify(issuer, anchor.0, anchor.1, (later, 300))
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
    assert_eq!(renewal.capability.capability.epoch, 2);

    // Revoking the user or the device ends renewal and every chain naming it.
    for subject in [user.public_key(), node.public_key()] {
        let revoked = domain
            .authority()
            .revoke(&policy.statement, [revoked_subject(&subject)])
            .expect("policy");
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
                2,
                VALIDITY,
            )
            .expect("capability");
        let chain = device_chain(reissued, &user, node.public_key()).expect("chain");
        assert!(domain.verify(&chain, &node, &revoked, NOW).is_err());
    }
}

#[test]
fn raw_grants_name_rights_and_columns_and_profiles_are_built_on_them() {
    let mut resources = Domain::new().resources;
    let (reserve, audit) = ([7; 32], [8; 32]);
    resources[0].columns.insert("title".into(), [6; 32]);
    resources[1].invokes = [reserve].into();
    resources[1].executes = [reserve, audit].into();
    let parse = |value: serde_json::Value| serde_json::from_value::<Vec<Grant>>(value);
    // Execute and custody to whoever the issuer names; columns narrow fields.
    let raw = parse(serde_json::json!([
        {"scope": "notes", "rights": ["read", "update", "custody"], "columns": ["title"]},
        {"scope": "payroll", "rights": ["execute", "custody"]},
    ]))
    .expect("grants");
    let grants = Grant::resolve_all(&raw, &resources).expect("resolved");
    assert_eq!(grants[0].fields, [[6; 32]].into());
    assert_eq!(
        rights(&grants[1..]),
        [Right::Execute, Right::Custody].into()
    );
    assert!(grants[1].fields.contains(&audit) && grants[1].fields.contains(&reserve));
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
    // A command right where no command is, unknown names, empty sets and
    // the schema right are refused.
    for refused in [
        serde_json::json!([{"scope": "notes", "rights": ["execute"]}]),
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
    assert!(parse(serde_json::json!([{"scope": "notes", "rights": ["fly"]}])).is_err());
    assert!(parse(serde_json::json!([{"scope": "notes", "rights": [], "rows": "all"}])).is_err());
    // The starter profiles are functions over raw grants.
    let writer = Profile::Writer {
        scopes: Domain::names(&["payroll"]),
    };
    let raw = writer.raw(&resources).expect("raw");
    assert_eq!(raw.len(), 1);
    assert!(raw[0].rights.contains(&Right::Invoke) && raw[0].columns.is_none());
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
        .issue_delegable(holder.public_key(), grants.clone(), 2, 1, VALIDITY)
        .expect("root");
    let own = device_chain(root.clone(), &holder, holder_node.public_key()).expect("own");
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
            None,
            (NOW, 90 * DAY, 300),
        )
        .expect("delegated");
    // The delegation ends with its parent, however long it was asked for.
    assert_eq!(link.expires(), NOW + 30 * DAY);
    let chain = link
        .clone()
        .complete(&delegate, node.public_key())
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
    beyond.fields.insert([1; 32]);
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
    widened.rights.insert(Right::Execute);
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
    let anchor = (domain.publisher.public_key(), domain.manifest.hash());
    assert!(
        request
            .verify(issuer, anchor.0, anchor.1, (NOW, 300))
            .is_err()
    );
    // A renewed delegator keeps its depth.
    let renewed = domain
        .authority()
        .renew(&root.capability, &policy.statement, VALIDITY)
        .expect("renewed");
    assert_eq!(renewed.capability.delegations, 2);
}

/// An assigner with `authors`, its node chain, and a writer device of notes.
struct Authors {
    domain: Domain,
    policy: SignedPolicy,
    assigner: PrivateKey,
    prefix: CredentialChain,
    device: (PrivateKey, CredentialChain),
}
impl Authors {
    fn new(authors: AuthorRange) -> Self {
        let domain = Domain::new();
        let policy = domain.policy();
        let assigner = PrivateKey::new();
        let grants = Profile::Issuer.grants(&domain.resources).expect("grants");
        let root = domain
            .authority()
            .issue_terms(
                assigner.public_key(),
                grants,
                (1, Some(authors)),
                1,
                VALIDITY,
            )
            .expect("root");
        let writer = Profile::Writer {
            scopes: Domain::names(&["notes"]),
        };
        let (_, node, chain) = domain.device(&writer);
        Self {
            domain,
            policy,
            assigner,
            prefix: CredentialChain(vec![root]),
            device: (node, chain),
        }
    }
    fn table(&self) -> Commitment {
        self.domain.resources[0].scope.table
    }
    fn assign(&self, key: &PrivateKey, author: u32) -> Result<CredentialChain, Error> {
        let (node, chain) = &self.device;
        self.prefix.assign(
            &self.assigner,
            key.public_key(),
            author,
            node.public_key(),
            chain.grants().to_vec(),
            chain.expires(),
            VALIDITY,
        )
    }
    fn verify_author(
        &self,
        chain: &CredentialChain,
        signer: &PrivateKey,
        author: u32,
        table: Commitment,
    ) -> Result<Vec<ResourceGrant>, Error> {
        chain.verify_author(
            self.domain.publisher.public_key(),
            self.domain.manifest.hash(),
            signer.public_key(),
            author,
            table,
            &self.policy.statement,
            NOW,
        )
    }
}

#[test]
fn a_frame_author_must_be_the_number_its_signing_key_was_assigned() {
    let authors = Authors::new(AuthorRange::new(100, 199).expect("range"));
    let key = PrivateKey::new();
    let pseudonym = authors.assign(&key, 150).expect("assigned");
    let table = authors.table();
    let grants = authors
        .verify_author(&pseudonym, &key, 150, table)
        .expect("author");
    assert_eq!(grants, authors.device.1.grants());
    // Another number, table or signer is refused, and so is a number
    // outside the assigner's range or the reserved author 0.
    assert!(authors.verify_author(&pseudonym, &key, 151, table).is_err());
    assert!(
        authors
            .verify_author(
                &pseudonym,
                &key,
                150,
                authors.domain.resources[1].scope.table
            )
            .is_err()
    );
    assert!(
        authors
            .verify_author(&pseudonym, &PrivateKey::new(), 150, table)
            .is_err()
    );
    for outside in [0, 99, 200, MAX_AUTHOR + 1] {
        assert!(authors.assign(&key, outside).is_err(), "{outside}");
    }
    // A leaf the assigner signs past its range anyway fails verification.
    let mut forged = pseudonym.clone();
    let mut leaf = forged.0.pop().expect("leaf").capability;
    let device = leaf.extended().expect("extended").device;
    leaf.extension = Some(
        Extension::new(Extended {
            partitions: Vec::new(),
            authors: Some(AuthorRange::new(250, 250).expect("range")),
            device,
        })
        .expect("extension"),
    );
    forged
        .0
        .push(SignedCapability::sign(leaf, &authors.assigner).expect("signed"));
    assert!(authors.verify_author(&forged, &key, 250, table).is_err());
    // A pseudonym chain is not a node chain, and a node chain no pseudonym.
    let anchor = (
        authors.domain.publisher.public_key(),
        authors.domain.manifest.hash(),
    );
    assert!(
        pseudonym
            .verify(
                anchor.0,
                anchor.1,
                key.public_key(),
                &authors.policy.statement,
                NOW
            )
            .is_err()
    );
    let (node, chain) = &authors.device;
    assert!(
        chain
            .verify_pseudonym(
                anchor.0,
                anchor.1,
                node.public_key(),
                &authors.policy.statement,
                NOW
            )
            .is_err()
    );
    // A later policy epoch ends it like every other chain.
    let next = authors
        .domain
        .authority()
        .revoke(&authors.policy.statement, [])
        .expect("policy");
    assert!(
        pseudonym
            .verify_author(
                anchor.0,
                anchor.1,
                key.public_key(),
                150,
                table,
                &next.statement,
                NOW
            )
            .is_err()
    );
}

#[test]
fn only_a_holder_of_the_publisher_key_maps_a_pseudonym_to_its_device() {
    let authors = Authors::new(AuthorRange::ALL);
    let key = PrivateKey::new();
    let pseudonym = authors.assign(&key, 7).expect("assigned");
    let manifest = authors.domain.manifest.hash();
    assert_eq!(
        pseudonym
            .pseudonym_device(&authors.domain.publisher, manifest)
            .expect("opened"),
        authors.device.0.public_key()
    );
    // The chain names the assigner, never the device.
    let bytes = serde_json::to_string(&pseudonym).expect("json");
    assert!(!bytes.contains(&authors.device.0.public_key().to_hex()));
    for stranger in [
        &authors.assigner,
        &key,
        &authors.device.0,
        &PrivateKey::new(),
    ] {
        assert!(pseudonym.pseudonym_device(stranger, manifest).is_err());
    }
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
            1,
            VALIDITY,
        )
        .expect("root");
    // Old encodings are untouched: no extension, no new bytes.
    let plain = domain
        .authority()
        .issue(holder.public_key(), grants.clone(), 1, VALIDITY)
        .expect("plain");
    assert!(
        !serde_json::to_string(&plain)
            .expect("json")
            .contains("extension")
    );
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
        .complete(&delegate, node.public_key())
        .expect("chain");
    let verify = |chain: &CredentialChain| {
        chain.verify(
            domain.publisher.public_key(),
            domain.manifest.hash(),
            node.public_key(),
            &policy.statement,
            NOW,
        )
    };
    assert!(verify(&chain).is_ok());
    // A delegate that signs itself a wider range is refused on verification.
    let mut wide = narrowed.0[1].capability.clone();
    wide.extension = Some(
        Extension::new(Extended {
            partitions: Vec::new(),
            authors: Some(AuthorRange::new(5, 15).expect("range")),
            device: None,
        })
        .expect("extension"),
    );
    let widened = CredentialChain(vec![
        root.clone(),
        SignedCapability::sign(wide, &holder).expect("signed"),
    ])
    .complete(&delegate, node.public_key())
    .expect("chain");
    assert!(verify(&widened).is_err());
    // A node link carries no extension.
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
        version: 3,
        authors: range,
        rotation: 9,
    })
    .expect("bytes");
    let later: Extension = serde_json::from_value(serde_json::json!(bytes)).expect("later");
    let mut capability = root.capability.clone();
    capability.extension = Some(later);
    let refused = CredentialChain(vec![
        SignedCapability::sign(capability, &domain.publisher).expect("signed"),
    ])
    .complete(&holder, node.public_key());
    let message = match refused.map(|chain| verify(&chain)) {
        Err(error) | Ok(Err(error)) => error.to_string(),
        Ok(Ok(_)) => String::new(),
    };
    assert!(
        message.contains("newer than this runtime supports"),
        "{message}"
    );
}

#[test]
fn a_pseudonym_request_proves_both_keys_and_keeps_an_earlier_number() {
    let authors = Authors::new(AuthorRange::new(1, 1000).expect("range"));
    let (node, chain) = &authors.device;
    let key = PrivateKey::new();
    let anchor = (
        authors.domain.publisher.public_key(),
        authors.domain.manifest.hash(),
    );
    let request = PseudonymRequest::sign(node, &key, chain.clone(), authors.table(), None, NOW)
        .expect("request");
    let request =
        PseudonymRequest::from_bytes(&request.to_bytes().expect("bytes")).expect("request");
    let grants = request
        .verify(anchor.0, anchor.1, &authors.policy.statement, (NOW, 300))
        .expect("verified");
    assert_eq!(grants, chain.grants());
    // A table the device does not write, a stale or foreign request.
    let other = PseudonymRequest::sign(
        node,
        &key,
        chain.clone(),
        authors.domain.resources[1].scope.table,
        None,
        NOW,
    )
    .expect("request");
    assert!(
        other
            .verify(anchor.0, anchor.1, &authors.policy.statement, (NOW, 300))
            .is_err()
    );
    assert!(
        request
            .verify(
                anchor.0,
                anchor.1,
                &authors.policy.statement,
                (NOW + 301, 300)
            )
            .is_err()
    );
    let stolen = PseudonymRequest::sign(
        &PrivateKey::new(),
        &key,
        chain.clone(),
        authors.table(),
        None,
        NOW,
    )
    .expect("request");
    assert!(
        stolen
            .verify(anchor.0, anchor.1, &authors.policy.statement, (NOW, 300))
            .is_err()
    );
    // The same key asking again keeps its number inside the range only.
    let first = authors.assign(&key, 42).expect("assigned");
    let again = PseudonymRequest::sign(
        node,
        &key,
        chain.clone(),
        authors.table(),
        Some(first.clone()),
        NOW,
    )
    .expect("request");
    let range = AuthorRange::new(1, 1000).expect("range");
    assert_eq!(again.previous_author(anchor.0, anchor.1, range), Some(42));
    assert_eq!(
        again.previous_author(
            anchor.0,
            anchor.1,
            AuthorRange::new(100, 200).expect("range")
        ),
        None
    );
    let rekeyed = PseudonymRequest::sign(
        node,
        &PrivateKey::new(),
        chain.clone(),
        authors.table(),
        Some(first),
        NOW,
    )
    .expect("request");
    assert_eq!(rekeyed.previous_author(anchor.0, anchor.1, range), None);
}

#[test]
fn a_legacy_capability_keeps_its_canonical_bytes_and_signature() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct LegacyCapability {
        manifest: Commitment,
        subject: PublicKey,
        kind: PrincipalKind,
        grants: Vec<ResourceGrant>,
        not_before: u64,
        expires: u64,
        epoch: u64,
        delegations: u8,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    struct LegacySigned {
        issuer: PublicKey,
        capability: LegacyCapability,
        signature: p2panda_core::Signature,
    }
    let domain = Domain::new();
    let user = PrivateKey::new();
    let signed = domain
        .authority()
        .issue(
            user.public_key(),
            Profile::Issuer.grants(&domain.resources).expect("grants"),
            1,
            VALIDITY,
        )
        .expect("issued");
    let c = &signed.capability;
    let old = LegacyCapability {
        manifest: c.manifest,
        subject: c.subject,
        kind: c.kind,
        grants: c.grants.clone(),
        not_before: c.not_before,
        expires: c.expires,
        epoch: c.epoch,
        delegations: c.delegations,
    };
    let encoded = encode(&old).expect("legacy bytes");
    assert_eq!(encode(c).expect("bytes"), encoded);
    let message = encode(&("rocky.capability.v1", &old)).expect("legacy signing bytes");
    let old = LegacySigned {
        issuer: domain.publisher.public_key(),
        capability: old,
        signature: domain.publisher.sign(&message),
    };
    // Decode bytes signed using exactly the pre-extension format.
    let bytes = encode(&old).expect("legacy signed");
    let decoded: SignedCapability =
        p2panda_core::cbor::decode_cbor(bytes.as_slice()).expect("decoded");
    assert_eq!(encode(&decoded).expect("encoded"), bytes);
    let chain = device_chain(decoded, &user, PrivateKey::new().public_key()).expect("chain");
    let node = chain.0.last().expect("node").capability.subject;
    chain
        .verify(
            domain.publisher.public_key(),
            domain.manifest.hash(),
            node,
            &domain.policy().statement,
            NOW,
        )
        .expect("legacy signature verifies");
    // The old decoder ignores an extension, then its original signature
    // check refuses the new chain: unknown authority is never dropped safely.
    let extended = domain
        .authority()
        .issue_terms(
            user.public_key(),
            c.grants.clone(),
            (1, Some(AuthorRange::ALL)),
            1,
            VALIDITY,
        )
        .expect("extended");
    let bytes = encode(&extended).expect("extended bytes");
    let old: LegacySigned = p2panda_core::cbor::decode_cbor(bytes.as_slice()).expect("old decoder");
    let message = encode(&("rocky.capability.v1", &old.capability)).expect("old signing bytes");
    assert!(!old.issuer.verify(&message, &old.signature));
}

#[test]
fn horizon_statements_bind_the_cut_and_refuse_unknown_formats_and_noncanonical_order() {
    let domain = Domain::new();
    let peer = PrivateKey::new();
    let statement = HorizonStatement {
        format: HORIZON_FORMAT,
        manifest: domain.manifest.hash(),
        policy_epoch: 1,
        scope: domain.resources[0].scope,
        sequence: 1,
        previous: None,
        issued: NOW,
        logs: vec![HorizonLog {
            signer: peer.public_key(),
            sequence: 0,
            head: [1; 32],
        }],
        state: [2; 32],
        existence: vec![],
        bases: vec![],
        related: vec![],
        peers: vec![HorizonPeer {
            node: peer.public_key(),
            expires: NOW + DAY,
        }],
    };
    let signed = SignedHorizon::sign(statement.clone(), &domain.publisher).unwrap();
    signed.verify(domain.publisher.public_key()).unwrap();
    let encoded = signed.to_bytes().unwrap();
    assert_eq!(SignedHorizon::from_bytes(&encoded).unwrap(), signed);
    let mut trailing = encoded;
    trailing.push(0);
    assert!(SignedHorizon::from_bytes(&trailing).is_err());
    let mut corrupt = signed;
    corrupt.statement.state = [3; 32];
    assert!(corrupt.verify(domain.publisher.public_key()).is_err());
    assert!(corrupt.verify(peer.public_key()).is_err());
    let mut unsupported = statement.clone();
    unsupported.format += 1;
    assert!(SignedHorizon::sign(unsupported, &domain.publisher).is_err());
    let mut duplicate = statement.clone();
    duplicate.logs.push(duplicate.logs[0].clone());
    assert!(SignedHorizon::sign(duplicate, &domain.publisher).is_err());
    let mut based = statement.clone();
    let base = rocky_credentials::ExistenceBase {
        scope: statement.scope,
        rows: 0,
        digest: [4; 32],
        identities: vec![],
    };
    based.existence.push(rocky_credentials::HorizonCut {
        scope: base.scope,
        logs: vec![],
        state: rocky_core::commitment("rocky.horizon-existence-state.v2", &encode(&base).unwrap()),
    });
    based.bases.push(base);
    SignedHorizon::sign(based.clone(), &domain.publisher)
        .unwrap()
        .verify(domain.publisher.public_key())
        .unwrap();
    based.bases[0].rows = 1;
    assert!(SignedHorizon::sign(based, &domain.publisher).is_err());
    let mut expired = statement;
    expired.peers[0].expires = NOW;
    assert!(SignedHorizon::sign(expired, &domain.publisher).is_err());
}
