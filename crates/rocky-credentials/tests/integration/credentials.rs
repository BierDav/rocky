use rocky_core::{
    AppId, ApplicationManifest, AuthorLogScope, DefinitionItem, DomainId, DomainRule,
    FieldSelection, MeshId, PrincipalKind, ResourceGrant, ResourceScope, Right, RowSelection,
    Scope,
};
use rocky_credentials::*;
use std::collections::BTreeSet;

const NOW: u64 = 1_800_000_000;
const VALIDITY: rocky_credentials::Validity = rocky_credentials::Validity {
    issued_at: NOW,
    lifetime: std::time::Duration::from_secs(30 * DAY),
    clock_skew: std::time::Duration::from_secs(300),
};

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
            engine: rocky_core::EngineIdentity::current(),
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
    fn revoking(&self, subjects: &[PublicKey]) -> RevocationSet {
        let statement = Revocation {
            app: self.manifest.app,
            issuer: self.publisher.public_key(),
            time: NOW,
            nodes: BTreeSet::new(),
            identities: subjects.iter().copied().collect(),
            capabilities: BTreeSet::new(),
            cut: Default::default(),
            mode: rocky_credentials::RevocationMode::Lost,
            recovery: None,
        };
        let signed = self.authority().revoke(statement).expect("revocation");
        RevocationSet::try_from(vec![signed]).expect("set")
    }
    fn names(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }
    fn device(&self, profile: &Profile) -> (PrivateKey, PrivateKey, CredentialChain) {
        let (user, node) = (PrivateKey::new(), PrivateKey::new());
        let root = self
            .authority()
            .issue(
                user.public_key(),
                profile.grants(&self.resources).expect("grants"),
                VALIDITY,
            )
            .expect("capability");
        let chain = device_chain(root, &user, &node).expect("chain");
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_supplied_set_verifies_with_keys_and_bytes_alone() {
    let domain = Domain::new();
    let writer = Profile::Writer {
        scopes: Domain::names(&["notes"]),
    };
    let (user, node, chain) = domain.device(&writer);
    let credentials = Credentials {
        publisher: domain.publisher.public_key(),
        revocations: domain.policy(),
        capabilities: chain.delegable(),
        certificate: chain.certificate().cloned(),
    };
    let received = Credentials::from_bytes(&credentials.to_bytes().expect("bytes")).expect("set");
    let verified = received
        .verify(node.public_key(), domain.manifest.app, NOW)
        .expect("verified");
    assert_eq!(
        verified.principal,
        user.public_key(),
        "any channel may carry the bytes; they verify on arrival"
    );
    assert_eq!(verified.device, node.public_key());
    assert_eq!(
        verified.revocations,
        RevocationSet::new().digest(NOW).expect("digest")
    );
    assert_eq!(verified.expires, NOW + 30 * DAY);
    assert_eq!(verified.grants.len(), 1);
    assert_eq!(verified.grants[0].scope, domain.resources[0].scope);
    assert!(
        received
            .verify(PrivateKey::new().public_key(), domain.manifest.app, NOW)
            .is_err(),
        "another device is refused"
    );
    assert!(
        received
            .verify(node.public_key(), domain.manifest.app, NOW + 31 * DAY)
            .is_err()
    );
    assert!(
        received
            .verify(node.public_key(), AppId([9; 16]), NOW)
            .is_err(),
        "another application's credentials are refused"
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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
    assert!(
        chain
            .verify(
                PrivateKey::new().public_key(),
                node.public_key(),
                &policy,
                NOW
            )
            .is_err(),
        "another publisher ends the chain"
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
    let mut long = chain.0[0].capability.clone();
    long.expires = long.not_before + MAX_CHAIN_LIFETIME + LIFETIME_SKEW + 1;
    let long = device_chain(
        SignedCapability::sign(long, &domain.publisher).expect("signed"),
        &user,
        &node,
    )
    .expect("chain");
    assert!(
        domain.verify(&long, &node, &policy, NOW).is_err(),
        "a link valid for longer than a chain may live is refused"
    );
    assert!(
        domain
            .authority()
            .issue(
                user.public_key(),
                chain.0[0].capability.grants.clone(),
                rocky_credentials::Validity {
                    issued_at: NOW,
                    lifetime: std::time::Duration::from_secs(MAX_CHAIN_LIFETIME + 1),
                    clock_skew: std::time::Duration::from_secs(300)
                }
            )
            .is_err()
    );
    let mut leaf = chain.0[1].capability.clone();
    leaf.grants = chain.0[0].capability.grants.clone();
    assert!(
        SignedCapability::sign(leaf, &user).is_err(),
        "a certificate naming grants of its own is refused"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_login_principal_alone_grants_nothing() {
    let domain = Domain::new();
    let policy = domain.policy();
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
    let (user, node) = (PrivateKey::new(), PrivateKey::new());
    let grants = Profile::Issuer.grants(&domain.resources).expect("grants");
    let claimed = Authority::owner(&user)
        .issue(user.public_key(), grants, VALIDITY)
        .expect("self-signed");
    let chain = device_chain(claimed, &user, &node).expect("chain");
    assert!(
        domain.verify(&chain, &node, &policy, NOW).is_err(),
        "a capability the user signs itself grants nothing"
    );
    assert!(
        chain
            .historic_provenance(domain.publisher.public_key(), node.public_key())
            .is_err()
    );
    let principal = Identity(user.public_key());
    let device = SigningDevice(node.public_key());
    assert_ne!(
        principal.0, device.0,
        "distinct identities are distinct types over distinct keys"
    );
    assert_ne!(Publisher(domain.publisher.public_key()).0, principal.0);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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
    for names in [&[][..], &["missing"][..], &["notes", "missing"][..]] {
        let scopes = Domain::names(names);
        assert!(
            Profile::Reader {
                scopes: scopes.clone()
            }
            .grants(&domain.resources)
            .is_err(),
            "a reader profile has no default and no unknown scope"
        );
        assert!(
            Profile::Writer { scopes }
                .grants(&domain.resources)
                .is_err()
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn an_invitation_binds_its_profile_counts_wrong_tokens_and_keeps_its_retry_identity() {
    let token = [5; 32];
    let profile = Profile::Reader {
        scopes: Domain::names(&["notes"]),
    };
    let mut invitation = Invitation::open(&token, Some(profile.clone()), NOW + 600);
    assert_eq!(invitation.profile, Some(profile.clone()));
    assert!(invitation.is_open(&token, NOW));
    assert!(invitation.is_open(&token, NOW + 600));
    assert!(!invitation.is_open(&token, NOW + 601));
    assert!(!invitation.is_open(&[6; 32], NOW));
    let request = |token: &[u8; 32], nonce: u8| {
        PairRequest::new(
            token,
            format!("{nonce:02x}").repeat(32),
            (
                PrivateKey::new().public_key(),
                PrivateKey::new().public_key(),
            ),
            "laptop".into(),
            [3; 32],
        )
    };
    let first = request(&token, 1);
    assert_eq!(
        invitation.admit(&token, &first, NOW).expect("first"),
        Admission::First
    );
    assert_eq!(
        invitation.admit(&token, &first, NOW).expect("again"),
        Admission::First,
        "until claimed, a refused context consumes nothing"
    );
    invitation.claim(&first);
    assert!(!invitation.is_open(&token, NOW));
    assert_eq!(
        invitation.admit(&token, &first, NOW).expect("retry"),
        Admission::Retry
    );
    assert!(invitation.admit(&token, &request(&token, 2), NOW).is_err());
    assert!(invitation.admit(&token, &first, NOW + 601).is_err());
    let sealed = seal_enrollment(&token, &first, b"enrollment").expect("sealed");
    let round = PairRequest::from_bytes(&first.to_bytes().expect("bytes")).expect("request");
    assert_eq!(
        open_enrollment(&token, &round, &sealed).expect("opened"),
        b"enrollment",
        "the sealed enrollment opens for that request and token"
    );
    assert!(open_enrollment(&[6; 32], &first, &sealed).is_err());
    assert!(open_enrollment(&token, &request(&token, 1), &sealed).is_err());
    let mut locked = Invitation::open(&token, Some(profile), NOW + 600);
    for attempt in 0..10 {
        assert!(
            locked
                .admit(&token, &request(&[attempt + 100; 32], 1), NOW)
                .is_err(),
            "ten wrong tokens lock the invitation, also for the right one"
        );
    }
    assert!(locked.admit(&token, &first, NOW).is_err());
    assert!(!locked.is_open(&token, NOW));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_joining_devices_routes_and_name_are_bound_by_its_proof() {
    let token = [5; 32];
    let mut invitation = Invitation::open(&token, None, NOW + 600);
    let routes = vec![("tcp".to_owned(), "127.0.0.1:4000".to_owned())];
    let request = PairRequest::new(
        &token,
        "01".repeat(32),
        (
            PrivateKey::new().public_key(),
            PrivateKey::new().public_key(),
        ),
        "laptop".into(),
        [3; 32],
    )
    .with_routes(&token, routes.clone());
    let round = PairRequest::from_bytes(&request.to_bytes().expect("bytes")).expect("request");
    assert_eq!(round.routes, routes);
    let mut moved = round.clone();
    moved.routes[0].1 = "127.0.0.1:4001".into();
    assert!(invitation.admit(&token, &moved, NOW).is_err());
    let mut renamed = round.clone();
    renamed.name = "phone".into();
    assert!(invitation.admit(&token, &renamed, NOW).is_err());
    assert_eq!(
        invitation.admit(&token, &round, NOW).expect("admitted"),
        Admission::First
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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
    assert_eq!(
        renewal.capability.capability.grants, chain.0[0].capability.grants,
        "a renewed reader stays a reader"
    );
    let renewed = device_chain(renewal.capability.clone(), &user, &node).expect("chain");
    assert_eq!(
        rights(
            &domain
                .verify(&renewed, &node, &policy, NOW)
                .expect("grants")
        ),
        [Right::Read, Right::Custody].into()
    );

    let other = PrivateKey::new().public_key();
    let anchor = (domain.publisher.public_key(),);
    assert!(
        request.verify(other, anchor.0, (NOW, 300)).is_err(),
        "another issuer is refused"
    );
    assert!(request.verify(issuer, anchor.0, (NOW + 301, 300)).is_err());
    let (_, stranger) = RenewRequest::sign(&node, issuer, chain.clone(), NOW).expect("request");
    assert!(renewal.open(anchor.0, &request, &stranger).is_err());
    let (again, again_secret) =
        RenewRequest::sign(&node, issuer, chain.clone(), NOW).expect("request");
    assert!(
        renewal.open(anchor.0, &again, &again_secret).is_err(),
        "an answer for another request is refused"
    );
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

    let mut widened = chain.0[0].capability.clone();
    widened.grants = Profile::Issuer.grants(&domain.resources).expect("grants");
    let claimed = CredentialChain(vec![
        SignedCapability::sign(widened, &user).expect("self-signed"),
        chain.0[1].clone(),
    ]);
    let (escalating, _) = RenewRequest::sign(&node, issuer, claimed, NOW).expect("request");
    assert!(
        escalating.verify(issuer, anchor.0, (NOW, 300)).is_err(),
        "a chain the device widened itself has no provenance"
    );

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
        rocky_credentials::Validity {
            issued_at: later,
            lifetime: std::time::Duration::from_secs(30 * DAY),
            clock_skew: std::time::Duration::from_secs(300),
        },
        b"",
    )
    .expect("renewal");
    renewal.open(anchor.0, &late, &late_secret).expect("opened");
    assert_eq!(
        renewal.revocations, next,
        "an expired chain still proves who asks; another device's revocation does not stop it"
    );

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
            .is_err(),
            "revoking the user or the device ends renewal"
        );
        let reissued = domain
            .authority()
            .issue(
                user.public_key(),
                chain.0[0].capability.grants.clone(),
                VALIDITY,
            )
            .expect("capability");
        let chain = device_chain(reissued, &user, &node).expect("chain");
        assert!(domain.verify(&chain, &node, &revoked, NOW).is_err());
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn raw_grants_name_rights_and_columns_and_profiles_are_built_on_them() {
    let mut resources = Domain::new().resources;
    resources[0].columns.insert("title".into(), [6; 32]);
    let parse = |value: serde_json::Value| serde_json::from_value::<Vec<Grant>>(value);
    let raw = parse(serde_json::json!([
        {"scope": "notes", "rights": ["read", "update", "custody"], "columns": ["title"]},
        {"scope": "payroll", "rights": ["delete", "custody"]},
    ]))
    .expect("grants");
    let grants = Grant::resolve_all(&raw, &resources).expect("resolved");
    assert_eq!(
        grants[0].fields,
        FieldSelection::Only([[6; 32]].into()),
        "columns narrow fields"
    );
    assert_eq!(rights(&grants[1..]), [Right::Delete, Right::Custody].into());
    assert_eq!(
        serde_json::to_value(&raw[0]).expect("json")["rights"],
        serde_json::json!(["read", "update", "custody"])
    );
    let described: Vec<Grant> = grants
        .iter()
        .filter_map(|grant| Grant::describe(grant, &resources))
        .collect();
    assert_eq!(
        described, raw,
        "each signed grant describes back to the raw grant that made it"
    );
    assert_eq!(
        Profile::of(&grants, &resources),
        Profile::Grants { grants: raw }
    );
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
            "unknown names, empty sets and the schema right are refused: {refused}"
        );
    }
    for name in ["fly", "invoke", "execute"] {
        assert!(
            parse(serde_json::json!([{"scope": "notes", "rights": [name]}])).is_err(),
            "names that are no right, the removed command rights included, are refused"
        );
    }
    assert!(parse(serde_json::json!([{"scope": "notes", "rights": [], "rows": "all"}])).is_err());
    let writer = Profile::Writer {
        scopes: Domain::names(&["payroll"]),
    };
    let raw = writer.raw(&resources).expect("raw");
    assert_eq!(
        raw.len(),
        1,
        "the starter profiles are functions over raw grants"
    );
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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
        .issue_delegable(holder.public_key(), grants.clone(), 2, VALIDITY)
        .expect("root");
    let own = device_chain(root.clone(), &holder, &holder_node).expect("own");
    assert_eq!(own.delegable().0.len(), 1);
    let (delegate, node) = (PrivateKey::new(), PrivateKey::new());
    let mut narrow = grants[0].clone();
    narrow.rights = [Right::Read, Right::Custody].into();
    let link = own
        .delegable()
        .delegate(
            &holder,
            rocky_credentials::Delegation {
                subject: delegate.public_key(),
                kind: PrincipalKind::User,
                grants: vec![narrow.clone()],
                partitions: Vec::new(),
                terms: rocky_credentials::DelegationTerms { delegations: 1 },
                validity: rocky_credentials::Validity {
                    issued_at: NOW,
                    lifetime: std::time::Duration::from_secs(90 * DAY),
                    clock_skew: std::time::Duration::from_secs(300),
                },
            },
        )
        .expect("delegated");
    assert_eq!(
        link.expires(),
        NOW + 30 * DAY,
        "the delegation ends with its parent, however long it was asked for"
    );
    let chain = link.clone().complete(&delegate, &node).expect("chain");
    assert_eq!(chain.0.len(), 3);
    assert_eq!(
        domain
            .verify(&chain, &node, &policy, NOW)
            .expect("verified"),
        [narrow.clone()]
    );
    assert_eq!(chain.grants(), [narrow.clone()]);
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
            rocky_credentials::Delegation {
                subject: delegate.public_key(),
                kind: PrincipalKind::User,
                grants,
                partitions: Vec::new(),
                terms: rocky_credentials::DelegationTerms { delegations: depth },
                validity: VALIDITY,
            },
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
        assert!(
            delegate_with(holder, grants, depth).is_err(),
            "wider grants, other scopes, more depth and holding no key are refused"
        );
    }
    let mut widened = wider.clone();
    widened.rights.insert(Right::PublishSchema);
    assert!(delegate_with(&holder, vec![wider, widened], 1).is_err());
    assert!(
        link.delegate(
            &delegate,
            rocky_credentials::Delegation {
                subject: PrivateKey::new().public_key(),
                kind: PrincipalKind::User,
                grants: vec![narrow],
                partitions: Vec::new(),
                terms: rocky_credentials::DelegationTerms { delegations: 1 },
                validity: VALIDITY
            }
        )
        .is_err(),
        "the delegate reaches only its own node"
    );
    let issuer = PrivateKey::new().public_key();
    let (request, _) = RenewRequest::sign(&node, issuer, chain, NOW).expect("request");
    let anchor = (domain.publisher.public_key(),);
    assert!(
        request.verify(issuer, anchor.0, (NOW, 300)).is_err(),
        "an issuer renews the delegator, never the delegate"
    );
    let renewed = domain
        .authority()
        .renew(&root.capability, &policy, VALIDITY)
        .expect("renewed");
    assert_eq!(
        renewed.capability.delegations, 2,
        "a renewed delegator keeps its depth"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn node_leaves_are_writers_named_by_their_own_keys() {
    let domain = Domain::new();
    let identity = PrivateKey::new();
    let nodes = [PrivateKey::new(), PrivateKey::new()];
    let grants = Profile::Issuer.grants(&domain.resources).unwrap();
    let root = domain
        .authority()
        .issue_terms(
            identity.public_key(),
            grants.clone(),
            rocky_credentials::DelegationTerms { delegations: 1 },
            VALIDITY,
        )
        .unwrap();
    let prefix = CredentialChain(vec![root]);
    let first = prefix.clone().certify(&identity, &nodes[0]).unwrap();
    let second = prefix
        .clone()
        .certify_member(
            &identity,
            &nodes[1],
            1,
            Some(first.certificate().unwrap().id().unwrap()),
        )
        .unwrap();
    let chains = [first, second];
    for (index, chain) in chains.iter().enumerate() {
        for resource in &domain.resources {
            let verified = chain
                .verify_author(
                    domain.publisher.public_key(),
                    nodes[index].public_key(),
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
        assert!(
            decoded
                .verify_author(
                    domain.publisher.public_key(),
                    nodes[1 - index].public_key(),
                    domain.resources[0].scope.table,
                    &domain.policy(),
                    NOW
                )
                .is_err(),
            "another node's key never writes under this certificate"
        );
    }
    let writer =
        |node: &PrivateKey| rocky_core::WriterId::from_bytes(*node.public_key().as_bytes());
    assert_ne!(
        rocky_core::RowIdentity::next(writer(&nodes[0]), 0).unwrap(),
        rocky_core::RowIdentity::next(writer(&nodes[1]), 0).unwrap(),
        "independent nodes name distinct identities from their own keys"
    );
    let mut forged = chains[0].clone();
    let mut leaf = forged.0.pop().unwrap().capability;
    leaf.extension = Extension::new(Extended {
        database: leaf.extended().unwrap().database,
        node: Some(rocky_credentials::PeerId::of(nodes[1].public_key())),
        sequence: Some(0),
        ..Default::default()
    })
    .unwrap();
    assert!(
        SignedCapability::sign(leaf, &identity).is_err(),
        "a leaf naming another node than its subject is refused"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn delegations_narrow_and_unknown_extension_versions_are_refused() {
    let domain = Domain::new();
    let policy = domain.policy();
    let grants = Profile::Writer {
        scopes: Domain::names(&["notes"]),
    }
    .grants(&domain.resources)
    .expect("grants");
    let holder = PrivateKey::new();
    let root = domain
        .authority()
        .issue_terms(
            holder.public_key(),
            grants.clone(),
            rocky_credentials::DelegationTerms { delegations: 2 },
            VALIDITY,
        )
        .expect("root");
    let prefix = CredentialChain(vec![root.clone()]);
    let delegate = PrivateKey::new();
    let delegate_with = |delegations| {
        prefix.delegate(
            &holder,
            rocky_credentials::Delegation {
                subject: delegate.public_key(),
                kind: PrincipalKind::User,
                grants: grants.clone(),
                partitions: Vec::new(),
                terms: rocky_credentials::DelegationTerms { delegations },
                validity: VALIDITY,
            },
        )
    };
    let narrowed = delegate_with(1).expect("narrowed");
    assert!(delegate_with(2).is_err());
    let node = PrivateKey::new();
    let chain = narrowed.clone().complete(&delegate, &node).expect("chain");
    let verify = |chain: &CredentialChain| {
        chain.verify(
            domain.publisher.public_key(),
            node.public_key(),
            &policy,
            NOW,
        )
    };
    assert!(verify(&chain).is_ok());
    let mut wide = narrowed.0[1].capability.clone();
    wide.extension = Extension::new(Extended {
        issues: true,
        ..Extended::default()
    })
    .expect("extension");
    let widened = CredentialChain(vec![
        root.clone(),
        SignedCapability::sign(wide, &holder).expect("signed"),
    ])
    .complete(&delegate, &node)
    .expect("chain");
    assert!(
        verify(&widened).is_err(),
        "a delegate that signs itself issuer authority is refused on verification"
    );
    let mut leaf = chain.0[2].capability.clone();
    leaf.extension = narrowed.0[1].capability.extension.clone();
    assert!(
        SignedCapability::sign(leaf, &delegate).is_err(),
        "a node link cannot carry a user extension"
    );
    #[derive(serde::Serialize)]
    struct Later {
        version: u32,
        rotation: u64,
    }
    let bytes = rocky_credentials::encode(&Later {
        version: 15,
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
        "a later extension version is refused with a clear error: {message}"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_profile_refuses_fields_its_kind_does_not_have() {
    let parse = |value: serde_json::Value| serde_json::from_value::<Profile>(value);
    assert_eq!(
        parse(serde_json::json!({ "kind": "issuer" })).expect("issuer"),
        Profile::Issuer
    );
    assert_eq!(
        parse(serde_json::json!({ "kind": "reader", "scopes": ["notes"] })).expect("reader"),
        Profile::Reader {
            scopes: Domain::names(&["notes"])
        }
    );
    for refused in [
        serde_json::json!({ "kind": "issuer", "scopes": ["notes"] }),
        serde_json::json!({ "kind": "reader", "scopes": ["notes"], "issues": true }),
        serde_json::json!({ "kind": "custody" }),
    ] {
        assert!(parse(refused.clone()).is_err(), "{refused} is refused");
    }
}
