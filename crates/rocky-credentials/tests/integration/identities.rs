use rocky_core::{
    AppId, ApplicationManifest, AuthorLogScope, DomainId, DomainRule, MeshId, ResourceGrant,
    ResourceScope, Right, RowSelection, Scope,
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
    owner: PrivateKey,
    manifest: ApplicationManifest,
}
impl Domain {
    fn new() -> Self {
        let owner = PrivateKey::new();
        let key = *owner.public_key().as_bytes();
        Self {
            manifest: ApplicationManifest {
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
            },
            owner,
        }
    }
    fn publisher(&self) -> PublicKey {
        self.owner.public_key()
    }
    fn grant(&self) -> ResourceGrant {
        ResourceGrant {
            scope: ResourceScope {
                domain: Scope {
                    mesh: MeshId([1; 16]),
                    app: AppId(self.manifest.app.0),
                    domain: DomainId([2; 16]),
                },
                table: [3; 32],
                encryption: [4; 32],
            },
            rows: RowSelection::All,
            rights: [Right::Read, Right::Insert].into(),
            fields: rocky_core::FieldSelection::Only(BTreeSet::new()),
        }
    }
    async fn issuer(&self) -> (FileIdentity, CredentialChain) {
        let identity = FileIdentity::new().await;
        let link = Authority::owner_in(
            &self.owner,
            DatabaseContext {
                publisher: self.publisher(),
                scope: self.grant().scope.domain,
            },
        )
        .unwrap()
        .issue_issuer(identity.key(), VALIDITY)
        .expect("issuer");
        (identity, CredentialChain(vec![link]))
    }
    fn verify(&self, chain: &CredentialChain, node: &PrivateKey, set: &RevocationSet) -> bool {
        chain
            .verify(self.publisher(), node.public_key(), set, NOW)
            .is_ok()
    }
    fn revoke(&self, statement: impl FnOnce(&mut Revocation)) -> RevocationSet {
        let mut body = Revocation {
            app: self.manifest.app,
            issuer: self.publisher(),
            time: NOW,
            nodes: BTreeSet::new(),
            identities: BTreeSet::new(),
            capabilities: BTreeSet::new(),
            cut: Default::default(),
            mode: rocky_credentials::RevocationMode::Lost,
            recovery: None,
        };
        statement(&mut body);
        let signed = Authority::owner(&self.owner)
            .revoke(body)
            .expect("statement");
        RevocationSet::try_from(vec![signed]).expect("set")
    }
}

struct FileIdentity {
    _directory: rocky_platform::fs::Scratch,
    provider: FileIdentityProvider,
}
impl FileIdentity {
    async fn new() -> Self {
        let directory = rocky_platform::fs::Scratch::new().await;
        let provider = FileIdentityProvider::open(directory.path()).expect("identity");
        Self {
            _directory: directory,
            provider,
        }
    }
    fn reopen(&self) -> FileIdentityProvider {
        FileIdentityProvider::open(self.provider.directory()).expect("identity")
    }
    fn key(&self) -> PublicKey {
        self.provider.public_key().expect("key")
    }
}

fn node(
    identity: &dyn IdentityProvider,
    chain: &CredentialChain,
    previous: Option<&CredentialChain>,
) -> (PrivateKey, CredentialChain) {
    let node = PrivateKey::new();
    let previous = previous.map(|chain| chain.certificate().unwrap());
    let sequence = previous.map_or(0, |certificate| {
        certificate.membership().unwrap().unwrap().0 + 1
    });
    let certified = chain
        .clone()
        .certify_member(
            identity,
            node.public_key(),
            sequence,
            previous.map(|certificate| certificate.id().unwrap()),
        )
        .expect("certificate");
    (node, certified)
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_file_identity_reopens_with_its_one_key() {
    let identity = FileIdentity::new().await;
    let again = identity.reopen();
    assert_eq!(
        identity.provider.public_key().expect("key"),
        again.public_key().expect("key")
    );
    let signature = again.sign(Purpose::Statement, b"x").expect("signature");
    assert!(identity.key().verify(b"x", &signature));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn issuers_grant_identities_whose_nodes_certify_themselves_without_an_issuer() {
    let domain = Domain::new();
    let (issuer, issuing) = domain.issuer().await;
    let authority = rocky_credentials::testing::authority(&issuer.provider, &issuing);
    let member = FileIdentity::new().await;
    let link = authority
        .issue_terms(
            member.key(),
            vec![domain.grant()],
            rocky_credentials::DelegationTerms { delegations: 1 },
            VALIDITY,
        )
        .expect("granted");
    let identity_chain = authority.chain_of(link);
    assert_eq!(
        identity_chain.0[0].issuer,
        domain.publisher(),
        "the issuer grants a member identity through its own chain, never with the publisher key"
    );
    assert_eq!(identity_chain.0[1].issuer, issuer.key());
    let set = RevocationSet::new();
    let first = node(&member.provider, &identity_chain, None);
    let second = node(&member.reopen(), &identity_chain, Some(&first.1));
    let third = node(&member.reopen(), &identity_chain, Some(&second.1));
    let mut writers = BTreeSet::new();
    for (key, chain) in [&first, &second, &third] {
        assert!(
            domain.verify(chain, key, &set),
            "three nodes of the identity each sign as their own writer"
        );
        assert_eq!(
            chain
                .verify(domain.publisher(), key.public_key(), &set, NOW,)
                .expect("grants"),
            vec![domain.grant()]
        );
        assert_eq!(chain.principal(), Some(member.key()));
        writers.insert(key.public_key());
    }
    assert_eq!(writers.len(), 3);
    let certificate = first.1.certificate().expect("certificate");
    assert!(
        certificate.capability.grants.is_empty(),
        "a certificate holds no rights of its own"
    );
    assert_eq!(certificate.capability.expires, rocky_credentials::NEVER);
    let stranger = PrivateKey::new();
    assert!(
        identity_chain
            .clone()
            .certify_member(&member.provider, stranger.public_key(), 1, None)
            .is_err()
    );
    let other = PrivateKey::new();
    assert!(
        identity_chain
            .clone()
            .certify(&other, stranger.public_key())
            .is_err(),
        "another identity cannot certify a node of this one"
    );
    let member_authority = rocky_credentials::testing::authority(&member.provider, &identity_chain);
    assert!(
        member_authority
            .issue(
                PrivateKey::new().public_key(),
                vec![domain.grant()],
                VALIDITY
            )
            .is_err(),
        "a member holds no issuer authority"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn revoking_an_identity_stops_all_its_nodes_and_a_certificate_only_its_node() {
    let domain = Domain::new();
    let member = FileIdentity::new().await;
    let link = Authority::owner(&domain.owner)
        .issue_terms(
            member.key(),
            vec![domain.grant()],
            rocky_credentials::DelegationTerms { delegations: 1 },
            VALIDITY,
        )
        .expect("granted");
    let chain = CredentialChain(vec![link]);
    let first = node(&member.provider, &chain, None);
    let second = node(&member.provider, &chain, Some(&first.1));
    let third = node(&member.provider, &chain, Some(&second.1));
    let nodes = [first, second, third];
    let none = RevocationSet::new();
    assert!(
        nodes
            .iter()
            .all(|(key, chain)| domain.verify(chain, key, &none))
    );

    let one = domain.revoke(|statement| {
        statement.nodes.insert(PeerId::of(nodes[1].0.public_key()));
    });
    let alive: Vec<bool> = nodes
        .iter()
        .map(|(key, chain)| domain.verify(chain, key, &one))
        .collect();
    assert_eq!(alive, [true, false, true]);
    let replacement = node(&member.provider, &chain, Some(&nodes[2].1));
    assert!(
        domain.verify(&replacement.1, &replacement.0, &one),
        "the identity certifies a fresh node after one was revoked"
    );

    let identity = domain.revoke(|statement| {
        statement.identities.insert(member.key());
    });
    assert!(
        nodes
            .iter()
            .chain([&replacement])
            .all(|(key, chain)| !domain.verify(chain, key, &identity))
    );
    let owner = Revocation {
        app: domain.manifest.app,
        issuer: domain.publisher(),
        time: NOW,
        nodes: BTreeSet::new(),
        identities: [domain.publisher()].into(),
        capabilities: BTreeSet::new(),
        cut: Default::default(),
        mode: rocky_credentials::RevocationMode::Lost,
        recovery: None,
    };
    let signed = Authority::owner(&domain.owner)
        .revoke(owner)
        .expect("signed");
    assert!(
        signed
            .verify_historic(domain.publisher(), domain.manifest.app)
            .is_err(),
        "the owner's identity is never revoked"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_revoked_issuer_signs_nothing_that_verifies_and_never_held_the_owners_key() {
    let domain = Domain::new();
    let (issuer, issuing) = domain.issuer().await;
    let authority = rocky_credentials::testing::authority(&issuer.provider, &issuing);
    let none = RevocationSet::new();
    let mut next = domain.manifest.clone();
    next.schema = 2;
    let earlier = SignedManifest::endorse(
        &next,
        rocky_credentials::DatabaseKind::Shared,
        authority.signer.clone(),
        &issuing,
    )
    .expect("before revocation the issuer endorses a manifest through its chain");
    assert_ne!(issuer.key(), domain.publisher());
    assert_eq!(
        earlier.endorsement().claimed_signer(domain.publisher()),
        issuer.key()
    );
    earlier
        .verify_current(domain.publisher(), &none, NOW)
        .expect("current");
    let member = PrivateKey::new();
    let link = authority
        .issue(member.public_key(), vec![domain.grant()], VALIDITY)
        .expect("the member's chain runs through the issuer");
    let member_node = PrivateKey::new();
    let member_chain = authority
        .chain_of(link)
        .complete(&member, member_node.public_key())
        .expect("a key held in memory certifies a node that writes under its own key");
    assert!(domain.verify(&member_chain, &member_node, &none));

    let revoked = domain.revoke(|statement| {
        statement.identities.insert(issuer.key());
    });
    earlier
        .verify_historic(domain.publisher())
        .expect("authentic");
    assert!(
        earlier
            .verify_current(domain.publisher(), &revoked, NOW)
            .is_err(),
        "nothing a revoked issuer signs verifies where its revocation is known"
    );
    next.schema = 3;
    let later = SignedManifest::endorse(
        &next,
        rocky_credentials::DatabaseKind::Shared,
        authority.signer.clone(),
        &issuing,
    )
    .expect("signed");
    assert!(
        later
            .verify_current(domain.publisher(), &revoked, NOW)
            .is_err()
    );
    let transition_chain = later.endorsement().chain.clone();
    assert!(
        transition_chain
            .issuer(domain.publisher(), Some((&revoked, NOW)))
            .is_err()
    );
    assert!(
        !domain.verify(&member_chain, &member_node, &revoked),
        "every chain through a revoked issuer ends"
    );
    let (other, other_chain) = domain.issuer().await;
    let renewer = rocky_credentials::testing::authority(&other.provider, &other_chain);
    let (request, secret) =
        RenewRequest::sign(&member_node, other.key(), member_chain.clone(), NOW).expect("request");
    let prior = request
        .verify(other.key(), domain.publisher(), (NOW, 300))
        .expect("provenance")
        .clone();
    let renewal = Renewal::seal(
        &renewer,
        &request,
        &prior,
        revoked.clone(),
        VALIDITY,
        b"keys",
    )
    .expect("renewed");
    assert_eq!(
        renewal
            .open(domain.publisher(), &request, &secret)
            .expect("opened"),
        b"keys"
    );
    assert_eq!(
        renewal.signer(domain.publisher()).expect("signer"),
        other.key()
    );
    let renewed = renewal
        .chain()
        .complete(&member, member_node.public_key())
        .expect("certified");
    assert!(domain.verify(&renewed, &member_node, &revoked));
    let stale = Renewal::seal(&authority, &request, &prior, none, VALIDITY, b"keys")
        .expect("signed anyway");
    let stale = stale
        .chain()
        .complete(&member, member_node.public_key())
        .expect("certified");
    assert!(
        !domain.verify(&stale, &member_node, &revoked),
        "a revoked issuer's renewal does not verify where its revocation is known"
    );
    assert!(
        renewer
            .renew(&issuing.0[0].capability, &RevocationSet::new(), VALIDITY)
            .is_err(),
        "issuers renew identities below them, never one at their own level"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_new_node_proves_its_identity_to_a_node_of_the_same_identity() {
    let identity = FileIdentity::new().await;
    let (node, answering) = (PrivateKey::new(), PrivateKey::new());
    let domain = Domain::new();
    let database = DatabaseContext {
        publisher: domain.publisher(),
        scope: domain.grant().scope.domain,
    };
    let (request, secret) = IdentityRequest::sign(
        &identity.provider,
        &node,
        answering.public_key(),
        NOW,
        database,
    )
    .expect("request");
    let request =
        IdentityRequest::from_bytes(&request.to_bytes().expect("bytes")).expect("request");
    request
        .verify(answering.public_key(), identity.key(), (NOW, 300), database)
        .expect("proven");
    let foreign = DatabaseContext {
        scope: Scope {
            mesh: MeshId([9; 16]),
            ..database.scope
        },
        ..database
    };
    assert!(
        request
            .verify(answering.public_key(), identity.key(), (NOW, 300), foreign)
            .is_err()
    );
    assert!(
        request
            .verify(
                answering.public_key(),
                node.public_key(),
                (NOW, 300),
                database
            )
            .is_err(),
        "another identity, audience or time is refused"
    );
    assert!(
        request
            .verify(node.public_key(), identity.key(), (NOW, 300), database)
            .is_err()
    );
    assert!(
        request
            .verify(
                answering.public_key(),
                identity.key(),
                (NOW + 301, 300),
                database
            )
            .is_err()
    );
    let answer = IdentityAnswer::from_bytes(
        &request
            .answer(b"bundle")
            .expect("answer")
            .to_bytes()
            .expect("bytes"),
    )
    .expect("answer");
    assert_eq!(
        answer.open(&request, &secret).expect("opened"),
        b"bundle",
        "the answer opens only with the request's one-time key"
    );
    let (_, other) = IdentityRequest::sign(
        &identity.provider,
        &node,
        answering.public_key(),
        NOW,
        database,
    )
    .expect("request");
    assert!(answer.open(&request, &other).is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_confirmation_code_is_six_digits_both_nodes_derive_alike() {
    let identity = FileIdentity::new().await;
    let (node, answering) = (PrivateKey::new(), PrivateKey::new());
    let domain = Domain::new();
    let database = DatabaseContext {
        publisher: domain.publisher(),
        scope: domain.grant().scope.domain,
    };
    let (request, _) = IdentityRequest::sign(
        &identity.provider,
        &node,
        answering.public_key(),
        NOW,
        database,
    )
    .expect("request");
    let code = request.confirmation_code().expect("code");
    assert_eq!(code.len(), 6);
    assert!(code.bytes().all(|byte| byte.is_ascii_digit()), "{code}");
    let received =
        IdentityRequest::from_bytes(&request.to_bytes().expect("bytes")).expect("request");
    assert_eq!(received.confirmation_code().expect("code"), code);
}
