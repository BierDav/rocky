use super::fixture::{Fixture, T};
use super::*;
use crate::counters::{CLAIM_WINDOW, Claim, Lookup, Name};
use p2panda_core::PrivateKey;
use rocky_core::{FieldSelection, ResourceGrant, ResourceScope, RowSelection};
use rocky_credentials::{Revocation, SignedRevocation, Validity};
use std::time::Duration;

const LIFETIME: u64 = 3_600;

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}
fn claim(n: u8, issued: u64) -> Claim {
    Claim {
        id: [n; 32],
        issued,
    }
}
struct Person {
    identity: PrivateKey,
    node: PrivateKey,
    chain: CredentialChain,
}
impl Fixture {
    fn grant(&self, scope: Scope, rights: &[Right]) -> ResourceGrant {
        ResourceGrant {
            scope: ResourceScope {
                domain: scope,
                encryption: [5; 32],
                table: [6; 32],
            },
            rows: RowSelection::All,
            rights: rights.iter().copied().collect(),
            fields: FieldSelection::All,
        }
    }
    fn person_of(&self, publisher: &PrivateKey, scope: Scope, rights: &[Right]) -> Person {
        let (identity, node) = (PrivateKey::new(), PrivateKey::new());
        let link = rocky_credentials::Authority::owner_in(
            publisher,
            rocky_credentials::DatabaseContext {
                publisher: publisher.public_key(),
                scope,
            },
        )
        .unwrap()
        .issue(
            identity.public_key(),
            vec![self.grant(scope, rights)],
            Validity {
                issued_at: T,
                lifetime: Duration::from_secs(LIFETIME),
                clock_skew: Duration::ZERO,
            },
        )
        .unwrap();
        let chain = CredentialChain(vec![link])
            .certify(&identity, &node)
            .unwrap();
        Person {
            identity,
            node,
            chain,
        }
    }
    fn person(&self, rights: &[Right]) -> Person {
        self.person_of(&self.owner.clone(), self.scope, rights)
    }
    fn presenting(&self, person: &Person, operation: Operation, time: u64) -> Request {
        Request::unsigned(
            self.state.genesis.id().unwrap(),
            crate::transport::nonce(),
            operation,
            (person.node.public_key(), None, Some(person.chain.clone())),
            time,
        )
        .signed(&person.node)
        .unwrap()
    }
    fn claim_by(
        &mut self,
        person: &Person,
        counter: &str,
        claim: Claim,
        time: u64,
    ) -> Result<u64, CoordinationError> {
        let operation = Operation::Claim {
            scope: self.scope,
            counter: name(counter),
            claim,
        };
        let request = self.presenting(person, operation, time);
        match self.state.apply_at(time, &request)? {
            Outcome::Claimed(value) => Ok(value),
            other => panic!("claim: {other:?}"),
        }
    }
    fn read_by(
        &self,
        person: &Person,
        counter: &str,
        claim: Option<Claim>,
        time: u64,
    ) -> Result<Lookup, CoordinationError> {
        let operation = Operation::Counter {
            scope: self.scope,
            counter: name(counter),
            claim,
        };
        match self.state.read(&self.presenting(person, operation, time))? {
            Outcome::Counter(reading) => Ok(reading.claim.unwrap_or(Lookup::Unclaimed)),
            other => panic!("reading: {other:?}"),
        }
    }
    fn admin_claim(&mut self, counter: &str, claim: Claim, time: u64) -> u64 {
        let request = self.certified(
            Operation::Claim {
                scope: self.scope,
                counter: name(counter),
                claim,
            },
            &self.nodes[0],
            time,
        );
        match self.state.apply_at(time, &request).unwrap() {
            Outcome::Claimed(value) => value,
            other => panic!("claim: {other:?}"),
        }
    }
    fn push_removal(&mut self, identity: PublicKey, pusher: &PrivateKey, time: u64) -> Outcome {
        let statement = SignedRevocation::sign_in(
            Revocation {
                app: self.scope.app,
                issuer: self.owner.public_key(),
                time,
                nodes: Default::default(),
                identities: [identity].into(),
                capabilities: Default::default(),
                cut: Default::default(),
                mode: rocky_credentials::RevocationMode::Lost,
            },
            &self.owner,
            &CredentialChain::default(),
            rocky_credentials::DatabaseContext {
                publisher: self.owner.public_key(),
                scope: self.scope,
            },
        )
        .unwrap();
        let request = Request::sign_at(
            self.state.genesis.id().unwrap(),
            crate::transport::nonce(),
            Operation::Revoke {
                scope: self.scope,
                revocations: Box::new(RevocationSet::try_from(vec![statement]).unwrap()),
            },
            pusher,
            time,
        )
        .unwrap();
        self.state.apply_at(time, &request).unwrap()
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_writer_that_is_not_an_admin_claims_gap_free_values_with_admin_nodes() {
    let mut f = Fixture::new(Limits::default());
    let writer = f.person(&[Right::Insert]);
    assert!(
        !f.state.controls[0]
            .control
            .admins
            .contains(&writer.node.public_key())
    );
    assert_eq!(f.claim_by(&writer, "invoices", claim(1, T), T), Ok(1));
    assert_eq!(f.admin_claim("invoices", claim(2, T), T), 2);
    assert_eq!(f.claim_by(&writer, "invoices", claim(3, T), T + 1), Ok(3));
    assert_eq!(
        f.claim_by(&writer, "invoices", claim(1, T), T + 2),
        Ok(1),
        "a retry answers the same value"
    );
    assert_eq!(
        f.read_by(&writer, "invoices", Some(claim(3, T)), T + 2),
        Ok(Lookup::Claimed(3))
    );
    assert_eq!(
        f.claim_by(&writer, "invoices", claim(4, T - CLAIM_WINDOW - 1), T + 2),
        Err(CoordinationError::Expired),
        "an issue time outside the window is expired, never a new value"
    );
    assert_eq!(
        f.claim_by(&writer, "invoices", claim(1, T + 1), T + 2),
        Err(CoordinationError::RequestConflict)
    );
    assert_eq!(f.claim_by(&writer, "invoices", claim(5, T), T + 2), Ok(4));
    for right in [Right::Update, Right::Delete] {
        let writer = f.person(&[right]);
        assert!(f.claim_by(&writer, "credits", claim(6, T), T + 2).is_ok());
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn readers_strangers_and_foreign_chains_never_claim() {
    let mut f = Fixture::new(Limits::default());
    let writer = f.person(&[Right::Insert]);
    let reader = f.person(&[Right::Read]);
    assert_eq!(f.claim_by(&writer, "invoices", claim(1, T), T), Ok(1));
    assert_eq!(
        f.claim_by(&reader, "invoices", claim(2, T), T),
        Err(CoordinationError::Denied(Refusal::ReadOnly)),
        "a person without write access claims nothing"
    );
    assert_eq!(
        f.read_by(&reader, "invoices", Some(claim(1, T)), T),
        Ok(Lookup::Claimed(1)),
        "a person without write access still reads"
    );
    let stranger = Person {
        identity: PrivateKey::new(),
        node: PrivateKey::new(),
        chain: writer.chain.clone(),
    };
    assert_eq!(
        f.claim_by(&stranger, "invoices", claim(3, T), T),
        Err(CoordinationError::Denied(Refusal::NotMember)),
        "a node presenting another node's chain claims nothing"
    );
    let foreign = f.person_of(&PrivateKey::new(), f.scope, &[Right::Insert]);
    assert_eq!(
        f.claim_by(&foreign, "invoices", claim(4, T), T),
        Err(CoordinationError::Denied(Refusal::NotMember)),
        "a person of another publisher claims nothing"
    );
    let unchained = Request::sign_at(
        f.state.genesis.id().unwrap(),
        [1; 32],
        Operation::Claim {
            scope: f.scope,
            counter: name("invoices"),
            claim: claim(5, T),
        },
        &writer.node,
        T,
    )
    .unwrap();
    assert_eq!(
        f.state.apply_at(T, &unchained),
        Err(CoordinationError::Denied(Refusal::NotMember))
    );
    let mut elsewhere = f.scope;
    elsewhere.domain = DomainId([8; 16]);
    let elsewhere = f.person_of(&f.owner.clone(), elsewhere, &[Right::Insert]);
    assert_eq!(
        f.claim_by(&elsewhere, "invoices", claim(6, T), T),
        Err(CoordinationError::Denied(Refusal::NotMember)),
        "a person of another domain claims nothing here"
    );
    assert_eq!(f.admin_claim("invoices", claim(7, T), T), 2);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_request_signed_outside_the_extension_label_is_refused() {
    let mut f = Fixture::new(Limits::default());
    let writer = f.person(&[Right::Insert]);
    f.admin_claim("invoices", claim(1, T), T);
    let request = f.presenting(
        &writer,
        Operation::Counter {
            scope: f.scope,
            counter: name("invoices"),
            claim: None,
        },
        T,
    );
    let body = request.extension_body().unwrap();
    let forged = request.clone().with_signature(writer.node.sign(&body));
    assert_eq!(
        f.state.read(&forged),
        Err(CoordinationError::Denied(Refusal::Signature))
    );
    let signed = request.with_signature(
        writer
            .node
            .sign(&rocky_credentials::extension_message(REQUEST_LABEL, &body).unwrap()),
    );
    assert_eq!(f.state.read(&signed).map(drop), Ok(()));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_member_never_closes_the_domain() {
    let mut f = Fixture::new(Limits::default());
    let writer = f.person(&[Right::Insert]);
    let request = f.presenting(&writer, Operation::Close { scope: f.scope }, T);
    assert_eq!(
        f.state.apply_at(T, &request),
        Err(CoordinationError::Denied(Refusal::NotDomainAuthority))
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_removed_person_is_refused_once_pushed_or_when_their_link_expires() {
    let mut f = Fixture::new(Limits::default());
    let removed = f.person(&[Right::Insert]);
    let lapsing = f.person(&[Right::Insert]);
    assert_eq!(f.claim_by(&removed, "invoices", claim(1, T), T), Ok(1));
    assert_eq!(f.claim_by(&lapsing, "invoices", claim(2, T), T), Ok(2));

    let stranger = PrivateKey::new();
    let outcome = f.push_removal(removed.identity.public_key(), &stranger, T + 1);
    assert!(
        matches!(outcome, Outcome::Revoked(_)),
        "any node may push the publisher's removal"
    );
    assert_eq!(
        f.claim_by(&removed, "invoices", claim(3, T), T + 1),
        Err(CoordinationError::Denied(Refusal::Revoked))
    );
    assert_eq!(
        f.claim_by(&removed, "invoices", claim(1, T), T + 1),
        Err(CoordinationError::Denied(Refusal::Revoked)),
        "a removed person cannot even replay an earlier claim"
    );
    assert_eq!(
        f.read_by(&removed, "invoices", None, T + 1),
        Err(CoordinationError::Denied(Refusal::Revoked))
    );
    assert_eq!(
        f.push_removal(removed.identity.public_key(), &stranger, T + 1),
        outcome,
        "pushing a removal again changes nothing"
    );
    let image = crate::certified::encode(&f.state).unwrap();
    f.state = crate::certified::decode(&image).unwrap();
    assert_eq!(
        f.claim_by(&removed, "invoices", claim(3, T), T + 2),
        Err(CoordinationError::Denied(Refusal::Revoked))
    );

    assert_eq!(
        f.claim_by(&lapsing, "invoices", claim(4, T), T + LIFETIME - 1),
        Ok(3)
    );
    assert_eq!(
        f.claim_by(&lapsing, "invoices", claim(5, T), T + LIFETIME + 1),
        Err(CoordinationError::Denied(Refusal::NotMember)),
        "an expired person link claims nothing"
    );

    let forged = SignedRevocation::sign(
        Revocation {
            app: f.scope.app,
            issuer: stranger.public_key(),
            time: T,
            nodes: Default::default(),
            identities: [lapsing.identity.public_key()].into(),
            capabilities: Default::default(),
            cut: Default::default(),
            mode: rocky_credentials::RevocationMode::Lost,
        },
        &stranger,
        &CredentialChain::default(),
    )
    .unwrap();
    let request = Request::sign_at(
        f.state.genesis.id().unwrap(),
        [2; 32],
        Operation::Revoke {
            scope: f.scope,
            revocations: Box::new(RevocationSet::try_from(vec![forged]).unwrap()),
        },
        &stranger,
        T + 2,
    )
    .unwrap();
    assert_eq!(
        f.state.apply_at(T + 2, &request),
        Err(CoordinationError::Denied(Refusal::InvalidRevocations)),
        "a statement another publisher signed is refused"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_control_without_a_publisher_admits_only_admin_nodes() {
    let mut f = Fixture::new(Limits::default());
    let writer = f.person(&[Right::Insert]);
    let mut control = f.state.controls[0].control.clone();
    control.revision += 1;
    control.publisher = None;
    let revision = ControlCertificate::sign(control, &f.owner).unwrap();
    let request = Request::sign_at(
        f.state.genesis.id().unwrap(),
        [3; 32],
        Operation::Control(Box::new(revision)),
        &f.owner,
        T,
    )
    .unwrap();
    assert_eq!(
        f.state.apply_at(T, &request),
        Err(CoordinationError::Denied(Refusal::NotDomainAuthority)),
        "a revision cannot drop the domain's publisher"
    );
    let mut control = f.state.controls[0].control.clone();
    control.publisher = None;
    f.state.controls[0] = ControlCertificate::sign(control, &f.owner).unwrap();
    assert_eq!(
        f.claim_by(&writer, "invoices", claim(1, T), T),
        Err(CoordinationError::Denied(Refusal::NotMember)),
        "a control that names no publisher admits no person"
    );
    assert_eq!(f.admin_claim("invoices", claim(2, T), T), 1);
}
