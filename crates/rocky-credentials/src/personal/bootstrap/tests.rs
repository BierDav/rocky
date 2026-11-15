use super::*;
use crate::{DatabaseKind, IdentityKey, RootKey};
use rocky_core::{AuthorLogScope, DomainRule, Scope};

struct Fixture {
    root: RootKey,
    owner: IdentityKey,
    namespace: PersonalNamespace,
    database: DatabaseContext,
    genesis: SignedManifest,
}
fn fixture(secret: u8, purpose: &str) -> Fixture {
    let root = RootKey::from_secret([secret; 32]);
    let owner = root.identity();
    let namespace = PersonalNamespace::new("notes.example", purpose).unwrap();
    let key = *root.public_key().as_bytes();
    let (mesh, domain) = (
        namespace.mesh(owner.public_key()).unwrap(),
        namespace.domain(owner.public_key()).unwrap(),
    );
    let manifest = ApplicationManifest {
        publisher: key,
        name: "notes".into(),
        app: ApplicationManifest::app_id(key, "notes"),
        schema: 1,
        projection: 1,
        engine: rocky_core::EngineIdentity::current(),
        entities: Vec::new(),
        fields: Vec::new(),
        operations: Vec::new(),
        domains: vec![DomainRule {
            domain,
            author_log: AuthorLogScope::Domain,
            rule: "independent".into(),
        }],
    };
    let kind = DatabaseKind::Personal {
        owner: owner.public_key(),
        namespace: namespace.clone(),
        creation: [0; 16],
    };
    let genesis = SignedManifest::sign(&manifest, kind, &root).unwrap();
    let database = DatabaseContext {
        publisher: root.public_key(),
        scope: Scope {
            mesh,
            app: manifest.app,
            domain,
        },
    };
    Fixture {
        root,
        owner,
        namespace,
        database,
        genesis,
    }
}
impl Fixture {
    fn audience(&self, responder: &PrivateKey) -> PersonalAudience {
        PersonalAudience {
            database: self.database,
            owner: self.owner.public_key(),
            descriptor: self.genesis.descriptor().unwrap(),
            node: responder.public_key(),
        }
    }
    fn authorize(
        &self,
        node: &PrivateKey,
        responder: &PrivateKey,
        nonce: [u8; 32],
        act: PersonalAct,
    ) -> (PersonalAuthorization, Exchange) {
        PersonalAuthorization::sign(
            (&self.root, &self.owner),
            node,
            (self.database, responder.public_key()),
            (self.genesis.descriptor().unwrap(), nonce),
            act,
        )
        .unwrap()
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_challenge_proves_its_responder_and_only_the_derived_genesis() {
    let account = fixture(1, "account root");
    let responder = PrivateKey::new();
    let hello = PersonalHello::fresh();
    let challenge = PersonalChallenge::answer(
        &hello,
        account.database,
        &account.genesis,
        &responder,
        [7; 32],
    )
    .unwrap();
    let pinned = (account.owner.public_key(), &account.namespace);
    let (genesis, _) = challenge.verify(&hello, account.database, pinned).unwrap();
    assert_eq!(
        genesis.descriptor().unwrap(),
        account.genesis.descriptor().unwrap()
    );
    assert_eq!(challenge.responder, responder.public_key());
    // Another hello, account, namespace or location is refused.
    assert!(
        challenge
            .verify(&PersonalHello::fresh(), account.database, pinned)
            .is_err()
    );
    let other = fixture(2, "account root");
    assert!(
        challenge
            .verify(
                &hello,
                other.database,
                (other.owner.public_key(), &other.namespace)
            )
            .is_err()
    );
    let elsewhere = fixture(1, "other root");
    assert!(
        challenge
            .verify(
                &hello,
                account.database,
                (account.owner.public_key(), &elsewhere.namespace)
            )
            .is_err()
    );
    // A responder relaying another database's genesis proves nothing here.
    let relayed = PersonalChallenge::answer(
        &hello,
        account.database,
        &other.genesis,
        &responder,
        [7; 32],
    )
    .unwrap();
    assert!(relayed.verify(&hello, account.database, pinned).is_err());
    // A signature over another nonce does not verify.
    let mut forged = challenge.clone();
    forged.nonce = [8; 32];
    assert!(forged.verify(&hello, account.database, pinned).is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn an_authorization_binds_root_owner_node_audience_and_descriptor() {
    let account = fixture(1, "account root");
    let (node, responder) = (PrivateKey::new(), PrivateKey::new());
    let act = PersonalAct::Bootstrap { page: 0 };
    let (authorization, secret) = account.authorize(&node, &responder, [5; 32], act);
    let audience = account.audience(&responder);
    authorization.verify(&audience).unwrap();
    assert_eq!(authorization.node(), node.public_key());
    assert_eq!(authorization.nonce(), [5; 32]);
    // Another responder, owner or descriptor is refused.
    assert!(
        authorization
            .verify(&account.audience(&PrivateKey::new()))
            .is_err()
    );
    let other = fixture(2, "account root");
    let mut wrong = audience;
    wrong.owner = other.owner.public_key();
    assert!(authorization.verify(&wrong).is_err());
    let mut wrong = audience;
    wrong.descriptor = other.genesis.descriptor().unwrap();
    assert!(authorization.verify(&wrong).is_err());
    // An identity signature alone, without the root, proves nothing.
    let mut forged = authorization.clone();
    forged.root = account.owner.sign(Purpose::Statement, b"x").unwrap();
    assert!(forged.verify(&audience).is_err());
    let mut forged = authorization.clone();
    forged.possession = PrivateKey::new().sign(b"x");
    assert!(matches!(
        forged.verify(&audience),
        Err(Error::Personal(crate::PersonalError::Possession))
    ));
    // Another root cannot sign for this database.
    assert!(
        PersonalAuthorization::sign(
            (&other.root, &account.owner),
            &node,
            (account.database, responder.public_key()),
            (account.genesis.descriptor().unwrap(), [5; 32]),
            act,
        )
        .is_err()
    );
    // The answer opens only with the authorization's exchange secret.
    let answer = authorization.answer(b"bootstrap").unwrap();
    assert_eq!(answer.open(&authorization, &secret).unwrap(), b"bootstrap");
    let (again, other_secret) = account.authorize(&node, &responder, [5; 32], act);
    assert!(answer.open(&again, &other_secret).is_err());
    assert!(answer.open(&authorization, &other_secret).is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_publication_names_exactly_its_certificate() {
    let account = fixture(1, "account root");
    let (node, responder) = (PrivateKey::new(), PrivateKey::new());
    let grant = crate::Authority::owner(&account.root)
        .issue_trusted(account.owner.public_key(), account.database.scope, 1)
        .unwrap();
    let certificate = crate::CredentialChain(vec![grant])
        .certify_personal(
            &account.owner,
            &node,
            (
                account.owner.public_key(),
                account.genesis.descriptor().unwrap(),
            ),
        )
        .unwrap()
        .certificate()
        .unwrap()
        .clone();
    let act = |certificate| PersonalAct::Publish {
        certificate,
        publication: [9; 16],
    };
    let (authorization, _) =
        account.authorize(&node, &responder, [5; 32], act(certificate.id().unwrap()));
    let publication = PersonalPublication {
        authorization,
        certificate: certificate.clone(),
    };
    assert_eq!(publication.check().unwrap(), [9; 16]);
    let (authorization, _) = account.authorize(&node, &responder, [5; 32], act([0; 32]));
    assert!(
        PersonalPublication {
            authorization,
            certificate: certificate.clone(),
        }
        .check()
        .is_err()
    );
    // A bootstrap authorization publishes nothing.
    let (authorization, _) = account.authorize(
        &node,
        &responder,
        [5; 32],
        PersonalAct::Bootstrap { page: 0 },
    );
    assert!(
        PersonalPublication {
            authorization,
            certificate,
        }
        .check()
        .is_err()
    );
}
