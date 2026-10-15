//! Identities hold permissions; nodes sign under certificates their
//! identity signed; the publisher key stays with the owner's identity and
//! issuers sign through their chains. Keys and bytes only.
use rocky_core::{
    AppId, ApplicationManifest, AuthorLogScope, DomainId, DomainRule, MeshId, ResourceGrant,
    ResourceScope, Right, RowSelection, Scope,
};
use rocky_credentials::*;
use std::collections::BTreeSet;

const NOW: u64 = 1_800_000_000;
const VALIDITY: (u64, u64, u64) = (NOW, 30 * DAY, 300);

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
    /// An issuer identity the owner appointed, with its issuer chain.
    fn issuer(&self, authors: AuthorRange) -> (FileIdentity, CredentialChain) {
        let identity = FileIdentity::new();
        let link = Authority::owner(&self.owner)
            .issue_issuer(identity.key(), Some(authors), VALIDITY)
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
        };
        statement(&mut body);
        let signed = Authority::owner(&self.owner)
            .revoke(body)
            .expect("statement");
        RevocationSet::try_from(vec![signed]).expect("set")
    }
}

/// An identity in its own file-backed provider.
struct FileIdentity {
    _directory: tempfile::TempDir,
    provider: FileIdentityProvider,
}
impl FileIdentity {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("directory");
        let provider = FileIdentityProvider::open(directory.path()).expect("identity");
        Self {
            _directory: directory,
            provider,
        }
    }
    /// The same identity opened again, as another node's process would.
    fn reopen(&self) -> FileIdentityProvider {
        FileIdentityProvider::open(self.provider.directory()).expect("identity")
    }
    fn key(&self) -> PublicKey {
        self.provider.public_key().expect("key")
    }
}

/// A node of `identity` certified under `chain`, with an author number its
/// provider reserves from the identity's range.
fn node(identity: &dyn IdentityProvider, chain: &CredentialChain) -> (PrivateKey, CredentialChain) {
    let node = PrivateKey::new();
    let range = chain
        .authors()
        .expect("range")
        .expect("the identity's range");
    let author = identity.reserve_author(range).expect("author");
    let certified = chain
        .clone()
        .certify(identity, node.public_key(), author)
        .expect("certificate");
    (node, certified)
}

#[test]
fn a_file_identity_reopens_and_reserves_each_author_once() {
    let identity = FileIdentity::new();
    let again = identity.reopen();
    assert_eq!(
        identity.provider.public_key().expect("key"),
        again.public_key().expect("key")
    );
    let range = AuthorRange::new(7, 8).expect("range");
    assert_eq!(identity.provider.reserve_author(range).expect("7"), 7);
    assert_eq!(again.reserve_author(range).expect("8"), 8);
    assert!(identity.provider.reserve_author(range).is_err());
    let signature = again.sign(Purpose::Statement, b"x").expect("signature");
    assert!(identity.key().verify(b"x", &signature));
    // A key held in memory signs, but keeps no author numbers.
    assert!(PrivateKey::new().reserve_author(AuthorRange::ALL).is_err());
}

#[test]
fn issuers_grant_identities_whose_nodes_certify_themselves_without_an_issuer() {
    let domain = Domain::new();
    let (issuer, issuing) = domain.issuer(AuthorRange::new(100, 199).expect("range"));
    let authority = Authority::issuer(&issuer.provider, &issuing);
    // The issuer grants a member identity a range and its grants; it signs
    // through its own chain, never with the publisher key.
    let member = FileIdentity::new();
    let link = authority
        .issue_terms(
            member.key(),
            vec![domain.grant()],
            (1, Some(AuthorRange::new(120, 129).expect("range"))),
            VALIDITY,
        )
        .expect("granted");
    let identity_chain = authority.chain_of(link);
    assert_eq!(identity_chain.0[0].issuer, domain.publisher());
    assert_eq!(identity_chain.0[1].issuer, issuer.key());
    // Three nodes of the identity: each signs under its own certificate
    // and author number; the later ones need no issuer and no invitation.
    let set = RevocationSet::new();
    let first = node(&member.provider, &identity_chain);
    let second = node(&member.reopen(), &identity_chain);
    let third = node(&member.reopen(), &identity_chain);
    let mut authors = BTreeSet::new();
    for (key, chain) in [&first, &second, &third] {
        assert!(domain.verify(chain, key, &set));
        assert_eq!(
            chain
                .verify(domain.publisher(), key.public_key(), &set, NOW,)
                .expect("grants"),
            vec![domain.grant()]
        );
        assert_eq!(chain.principal(), Some(member.key()));
        authors.insert(chain.author_number().expect("number"));
    }
    assert_eq!(authors, (120..=122).collect());
    // A certificate adds no rights and keeps to the identity's range.
    let mut wider = domain.grant();
    wider.rights.insert(Right::Delete);
    let stranger = PrivateKey::new();
    assert!(
        identity_chain
            .clone()
            .certify_with(
                &member.provider,
                (stranger.public_key(), 129),
                (vec![wider], Vec::new()),
            )
            .is_ok_and(|chain| !domain.verify(&chain, &stranger, &set))
    );
    assert!(
        identity_chain
            .clone()
            .certify(&member.provider, stranger.public_key(), 130)
            .is_err()
    );
    // Another identity cannot certify a node of this one.
    let other = PrivateKey::new();
    assert!(
        identity_chain
            .clone()
            .certify(&other, stranger.public_key(), 129)
            .is_err()
    );
    // A member holds no issuer authority: it neither appoints issuers nor
    // grants through an authority.
    let member_authority = Authority::issuer(&member.provider, &identity_chain);
    assert!(
        member_authority
            .issue(
                PrivateKey::new().public_key(),
                vec![domain.grant()],
                AuthorRange::new(120, 129).expect("range"),
                VALIDITY
            )
            .is_err()
    );
}

#[test]
fn revoking_an_identity_stops_all_its_nodes_and_a_certificate_only_its_node() {
    let domain = Domain::new();
    let member = FileIdentity::new();
    let link = Authority::owner(&domain.owner)
        .issue_terms(
            member.key(),
            vec![domain.grant()],
            (1, Some(AuthorRange::new(1, 9).expect("range"))),
            VALIDITY,
        )
        .expect("granted");
    let chain = CredentialChain(vec![link]);
    let nodes = [
        node(&member.provider, &chain),
        node(&member.provider, &chain),
        node(&member.provider, &chain),
    ];
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
    // The identity certifies a fresh node after one was revoked.
    let replacement = node(&member.provider, &chain);
    assert!(domain.verify(&replacement.1, &replacement.0, &one));

    let identity = domain.revoke(|statement| {
        statement.identities.insert(member.key());
    });
    assert!(
        nodes
            .iter()
            .chain([&replacement])
            .all(|(key, chain)| !domain.verify(chain, key, &identity))
    );
    // The owner's identity is never revoked.
    let owner = Revocation {
        app: domain.manifest.app,
        issuer: domain.publisher(),
        time: NOW,
        nodes: BTreeSet::new(),
        identities: [domain.publisher()].into(),
        capabilities: BTreeSet::new(),
    };
    let signed = Authority::owner(&domain.owner)
        .revoke(owner)
        .expect("signed");
    assert!(
        signed
            .verify(domain.publisher(), domain.manifest.app)
            .is_err()
    );
}

#[test]
fn a_revoked_issuer_signs_nothing_that_verifies_and_never_held_the_owners_key() {
    let domain = Domain::new();
    let (issuer, issuing) = domain.issuer(AuthorRange::new(100, 199).expect("range"));
    let authority = Authority::issuer(&issuer.provider, &issuing);
    let none = RevocationSet::new();
    // Before revocation the issuer endorses a manifest through its chain.
    let mut next = domain.manifest.clone();
    next.schema = 2;
    let earlier = SignedManifest::endorse(&next, &issuer.provider, &issuing).expect("endorsed");
    assert_ne!(issuer.key(), domain.publisher());
    assert_eq!(
        earlier.endorsement().signer(domain.publisher()),
        issuer.key()
    );
    earlier
        .verify_current(domain.publisher(), &none, NOW)
        .expect("current");
    // Its member's chain runs through it.
    let member = PrivateKey::new();
    let link = authority
        .issue(
            member.public_key(),
            vec![domain.grant()],
            AuthorRange::new(150, 159).expect("range"),
            VALIDITY,
        )
        .expect("granted");
    // A key held in memory keeps no author numbers: the node takes 150.
    let member_node = PrivateKey::new();
    let member_chain = authority
        .chain_of(link)
        .complete(&member, member_node.public_key(), 150)
        .expect("certified");
    assert!(domain.verify(&member_chain, &member_node, &none));

    let revoked = domain.revoke(|statement| {
        statement.identities.insert(issuer.key());
    });
    // What it signed before stays authentic; nothing it signs now verifies
    // where its revocation is known.
    earlier.verify(domain.publisher()).expect("authentic");
    assert!(
        earlier
            .verify_current(domain.publisher(), &revoked, NOW)
            .is_err()
    );
    next.schema = 3;
    let later = SignedManifest::endorse(&next, &issuer.provider, &issuing).expect("signed");
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
    // Every chain through it ends; another issuer renews its members.
    assert!(!domain.verify(&member_chain, &member_node, &revoked));
    let (other, other_chain) = domain.issuer(AuthorRange::new(200, 299).expect("range"));
    let renewer = Authority::issuer(&other.provider, &other_chain);
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
        .complete(&member, member_node.public_key(), 150)
        .expect("certified");
    assert!(domain.verify(&renewed, &member_node, &revoked));
    // A revoked issuer's renewal does not verify where its revocation is known.
    let stale = Renewal::seal(&authority, &request, &prior, none, VALIDITY, b"keys")
        .expect("signed anyway");
    let stale = stale
        .chain()
        .complete(&member, member_node.public_key(), 150)
        .expect("certified");
    assert!(!domain.verify(&stale, &member_node, &revoked));
    // Issuers renew identities below them, never one at their own level.
    assert!(
        renewer
            .renew(&issuing.0[0].capability, &RevocationSet::new(), VALIDITY)
            .is_err()
    );
}

#[test]
fn a_new_node_proves_its_identity_to_a_node_of_the_same_identity() {
    let identity = FileIdentity::new();
    let (node, answering) = (PrivateKey::new(), PrivateKey::new());
    let (request, secret) =
        IdentityRequest::sign(&identity.provider, &node, answering.public_key(), NOW)
            .expect("request");
    let request =
        IdentityRequest::from_bytes(&request.to_bytes().expect("bytes")).expect("request");
    request
        .verify(answering.public_key(), identity.key(), (NOW, 300))
        .expect("proven");
    // Another identity, audience or time is refused.
    assert!(
        request
            .verify(answering.public_key(), node.public_key(), (NOW, 300))
            .is_err()
    );
    assert!(
        request
            .verify(node.public_key(), identity.key(), (NOW, 300))
            .is_err()
    );
    assert!(
        request
            .verify(answering.public_key(), identity.key(), (NOW + 301, 300))
            .is_err()
    );
    // The answer opens only with the request's one-time key.
    let answer = IdentityAnswer::from_bytes(
        &request
            .answer(b"bundle")
            .expect("answer")
            .to_bytes()
            .expect("bytes"),
    )
    .expect("answer");
    assert_eq!(answer.open(&request, &secret).expect("opened"), b"bundle");
    let (_, other) = IdentityRequest::sign(&identity.provider, &node, answering.public_key(), NOW)
        .expect("request");
    assert!(answer.open(&request, &other).is_err());
}
