use rocky_core::{
    AppId, ApplicationManifest, AuthorLogScope, DefinitionItem, DomainId, DomainRule,
    FieldSelection, MeshId, ResourceGrant, ResourceScope, Right, RowSelection, Scope,
};
use rocky_credentials::*;
use std::collections::BTreeSet;

const NOW: u64 = 1_800_000_000;
const VALIDITY: rocky_credentials::Validity = rocky_credentials::Validity {
    issued_at: NOW,
    lifetime: std::time::Duration::from_secs(30 * DAY),
    clock_skew: std::time::Duration::from_secs(300),
};

struct Table {
    scope: ResourceScope,
}
struct Domain {
    publisher: PrivateKey,
    manifest: ApplicationManifest,
    resources: Vec<Table>,
}
const READ: [Right; 2] = [Right::Read, Right::Custody];
const WRITE: [Right; 5] = [
    Right::Read,
    Right::Insert,
    Right::Update,
    Right::Delete,
    Right::Custody,
];
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
            roles: Vec::new(),
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
            resources.push(Table {
                scope: ResourceScope {
                    domain: Scope {
                        mesh: MeshId([1; 16]),
                        app: AppId(manifest.app.0),
                        domain: DomainId([2; 16]),
                    },
                    encryption: [index as u8 + 1; 32],
                    table,
                },
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
        };
        let signed = self.authority().revoke(statement).expect("revocation");
        RevocationSet::try_from(vec![signed]).expect("set")
    }
    fn grants(&self, tables: &[usize], rights: &[Right]) -> Vec<ResourceGrant> {
        tables
            .iter()
            .map(|index| ResourceGrant {
                scope: self.resources[*index].scope,
                rows: RowSelection::All,
                rights: rights.iter().copied().collect(),
                fields: FieldSelection::All,
            })
            .collect()
    }
    fn device(&self, grants: Vec<ResourceGrant>) -> (PrivateKey, PrivateKey, CredentialChain) {
        let (user, node) = (PrivateKey::new(), PrivateKey::new());
        let root = self
            .authority()
            .issue(user.public_key(), grants, VALIDITY)
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
    let (user, node, chain) = domain.device(domain.grants(&[0], &WRITE));
    let credentials = Credentials {
        publisher: domain.publisher.public_key(),
        revocations: domain.policy(),
        capabilities: chain.without_certificate(),
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
    let (user, node, chain) = domain.device(domain.grants(&[0], &WRITE));
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
fn a_self_signed_capability_grants_nothing() {
    let domain = Domain::new();
    let policy = domain.policy();
    let (user, node) = (PrivateKey::new(), PrivateKey::new());
    let grants = domain.grants(&[0, 1], &WRITE);
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
    assert_ne!(Publisher(domain.publisher.public_key()).0, principal.0);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn renewal_keeps_the_grants_refuses_the_revoked_and_seals_to_the_device() {
    let domain = Domain::new();
    let policy = domain.policy();
    let issuer = PrivateKey::new().public_key();
    let (user, node, chain) = domain.device(domain.grants(&[0], &READ));
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
    widened.grants = domain.grants(&[0, 1], &WRITE);
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
fn node_leaves_are_writers_named_by_their_own_keys() {
    let domain = Domain::new();
    let identity = PrivateKey::new();
    let nodes = [PrivateKey::new(), PrivateKey::new()];
    let grants = domain.grants(&[0, 1], &WRITE);
    let root = domain
        .authority()
        .issue(identity.public_key(), grants.clone(), VALIDITY)
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
fn a_person_signs_no_link_and_unknown_extension_versions_are_refused() {
    let domain = Domain::new();
    let policy = domain.policy();
    let grants = domain.grants(&[0], &WRITE);
    let holder = PrivateKey::new();
    let root = domain
        .authority()
        .issue(holder.public_key(), grants.clone(), VALIDITY)
        .expect("root");
    let (other, node) = (PrivateKey::new(), PrivateKey::new());
    let verify = |chain: &CredentialChain| {
        chain.verify(
            domain.publisher.public_key(),
            node.public_key(),
            &policy,
            NOW,
        )
    };
    for extended in [
        Extended::default(),
        Extended {
            issues: true,
            ..Extended::default()
        },
    ] {
        let mut shared = root.capability.clone();
        shared.subject = other.public_key();
        shared.extension = Extension::new(extended).expect("extension");
        let shared = CredentialChain(vec![
            root.clone(),
            SignedCapability::sign(shared, &holder).expect("signed"),
        ])
        .complete(&other, &node)
        .expect("chain");
        assert!(
            verify(&shared).is_err(),
            "a person shares no grant and signs itself no issuer authority"
        );
    }
    #[derive(serde::Serialize)]
    struct Later {
        version: u32,
        rotation: u64,
    }
    let bytes = rocky_credentials::encode(&Later {
        version: 18,
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
