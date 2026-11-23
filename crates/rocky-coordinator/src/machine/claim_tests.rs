use super::fixture::{Fixture, T};
use super::*;
use crate::counters::{CLAIM_WINDOW, Claim, Lookup, Name};
use p2panda_core::PrivateKey;
use rocky_credentials::{CLAIM_LIFETIME, ClaimRight, Revocation, SignedRevocation};

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}
fn claim(n: u8, issued: u64) -> Claim {
    Claim {
        id: [n; 32],
        issued,
    }
}
struct Holder {
    identity: PrivateKey,
    node: PrivateKey,
    chain: CredentialChain,
}
impl Fixture {
    fn right(&self, counters: Option<&[&str]>) -> ClaimRight {
        ClaimRight {
            scope: self.scope,
            counters: counters
                .map(|names| names.iter().map(|name| name.as_bytes().to_vec()).collect()),
            grant: [7; 32],
        }
    }
    fn holder(&self, right: ClaimRight, delegations: u8) -> Holder {
        let (identity, node) = (PrivateKey::new(), PrivateKey::new());
        let link = rocky_credentials::Authority::owner(&self.owner)
            .issue_claims(
                identity.public_key(),
                right,
                delegations,
                (T, CLAIM_LIFETIME),
            )
            .unwrap();
        let chain = CredentialChain(vec![link])
            .certify_claims(&identity, &node)
            .unwrap();
        Holder {
            identity,
            node,
            chain,
        }
    }
    fn presenting(&self, holder: &Holder, operation: Operation, time: u64) -> Request {
        Request::sign_at(
            self.state.genesis.id().unwrap(),
            crate::transport::nonce(),
            operation,
            &holder.node,
            time,
        )
        .unwrap()
        .with_claims(holder.chain.clone(), &holder.node)
        .unwrap()
    }
    fn claim_by(
        &mut self,
        holder: &Holder,
        counter: &str,
        claim: Claim,
        time: u64,
    ) -> Result<u64, CoordinationError> {
        let operation = Operation::Claim {
            scope: self.scope,
            counter: name(counter),
            claim,
        };
        let request = self.presenting(holder, operation, time);
        match self.state.apply_at(time, &request)? {
            Outcome::Claimed(value) => Ok(value),
            other => panic!("claim: {other:?}"),
        }
    }
    fn read_by(
        &self,
        holder: &Holder,
        counter: &str,
        claim: Option<Claim>,
        time: u64,
    ) -> Result<Lookup, CoordinationError> {
        let operation = Operation::Counter {
            scope: self.scope,
            counter: name(counter),
            claim,
        };
        match self.state.read(&self.presenting(holder, operation, time))? {
            Outcome::Counter(reading) => Ok(reading.claim.unwrap_or(Lookup::Unclaimed)),
            other => panic!("reading: {other:?}"),
        }
    }
    fn trusted_claim(&mut self, counter: &str, claim: Claim, time: u64) -> u64 {
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
    fn push_revocation(&mut self, identity: PublicKey, pusher: &PrivateKey, time: u64) -> Outcome {
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
fn a_holder_that_is_not_trusted_claims_gap_free_values_with_trusted_nodes() {
    let mut f = Fixture::new(Limits::default());
    let holder = f.holder(f.right(None), 1);
    assert!(
        !f.state.controls[0]
            .control
            .trusted
            .contains(&holder.identity.public_key())
    );
    assert_eq!(f.claim_by(&holder, "invoices", claim(1, T), T), Ok(1));
    assert_eq!(f.trusted_claim("invoices", claim(2, T), T), 2);
    assert_eq!(f.claim_by(&holder, "invoices", claim(3, T), T + 1), Ok(3));
    assert_eq!(
        f.claim_by(&holder, "invoices", claim(1, T), T + 2),
        Ok(1),
        "a retry answers the same value from the holder"
    );
    assert_eq!(f.trusted_claim("invoices", claim(3, T), T + 2), 3);
    assert_eq!(
        f.read_by(&holder, "invoices", Some(claim(3, T)), T + 2),
        Ok(Lookup::Claimed(3))
    );
    assert_eq!(
        f.claim_by(&holder, "invoices", claim(4, T - CLAIM_WINDOW - 1), T + 2),
        Err(CoordinationError::Expired),
        "an issue time outside the window is expired, never a new value"
    );
    assert_eq!(
        f.claim_by(&holder, "invoices", claim(1, T + 1), T + 2),
        Err(CoordinationError::RequestConflict)
    );
    assert_eq!(f.claim_by(&holder, "invoices", claim(5, T), T + 2), Ok(4));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn non_holders_and_other_counters_are_denied() {
    let mut f = Fixture::new(Limits::default());
    let invoices = f.holder(f.right(Some(&["invoices"])), 1);
    assert_eq!(f.claim_by(&invoices, "invoices", claim(1, T), T), Ok(1));
    assert_eq!(
        f.claim_by(&invoices, "credits", claim(2, T), T),
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        f.read_by(&invoices, "credits", None, T),
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        f.read_by(&invoices, "invoices", None, T),
        Ok(Lookup::Unclaimed)
    );

    let stranger = Holder {
        identity: PrivateKey::new(),
        node: PrivateKey::new(),
        chain: invoices.chain.clone(),
    };
    assert_eq!(
        f.claim_by(&stranger, "invoices", claim(3, T), T),
        Err(CoordinationError::Denied),
        "a node presenting another node's chain claims nothing"
    );
    let foreign = {
        let publisher = PrivateKey::new();
        let identity = PrivateKey::new();
        let node = PrivateKey::new();
        let link = rocky_credentials::Authority::owner(&publisher)
            .issue_claims(identity.public_key(), f.right(None), 1, (T, CLAIM_LIFETIME))
            .unwrap();
        let chain = CredentialChain(vec![link])
            .certify_claims(&identity, &node)
            .unwrap();
        Holder {
            identity,
            node,
            chain,
        }
    };
    assert_eq!(
        f.claim_by(&foreign, "invoices", claim(4, T), T),
        Err(CoordinationError::Denied)
    );
    let unchained = Request::sign_at(
        f.state.genesis.id().unwrap(),
        [1; 32],
        Operation::Claim {
            scope: f.scope,
            counter: name("invoices"),
            claim: claim(5, T),
        },
        &invoices.node,
        T,
    )
    .unwrap();
    assert_eq!(
        f.state.apply_at(T, &unchained),
        Err(CoordinationError::Denied)
    );
    let mut elsewhere = f.right(None);
    elsewhere.scope.domain = DomainId([8; 16]);
    let elsewhere = f.holder(elsewhere, 1);
    assert_eq!(
        f.claim_by(&elsewhere, "invoices", claim(6, T), T),
        Err(CoordinationError::Denied),
        "a right for another domain claims nothing here"
    );
    assert_eq!(f.trusted_claim("invoices", claim(7, T), T), 2);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_holder_never_closes_the_domain() {
    let mut f = Fixture::new(Limits::default());
    let holder = f.holder(f.right(None), 1);
    let request = f.presenting(&holder, Operation::Close { scope: f.scope }, T);
    assert_eq!(
        f.state.apply_at(T, &request),
        Err(CoordinationError::Denied),
        "a claim holder never closes the domain"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_revoked_holder_is_refused_at_the_push_or_when_its_link_expires() {
    let mut f = Fixture::new(Limits::default());
    let pushed = f.holder(f.right(None), 1);
    let lapsing = f.holder(f.right(None), 1);
    assert_eq!(f.claim_by(&pushed, "invoices", claim(1, T), T), Ok(1));
    assert_eq!(f.claim_by(&lapsing, "invoices", claim(2, T), T), Ok(2));

    let stranger = PrivateKey::new();
    let outcome = f.push_revocation(pushed.identity.public_key(), &stranger, T + 1);
    assert!(
        matches!(outcome, Outcome::Revoked(_)),
        "any node may push the owner's revocation"
    );
    assert_eq!(
        f.claim_by(&pushed, "invoices", claim(3, T), T + 1),
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        f.claim_by(&pushed, "invoices", claim(1, T), T + 1),
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        f.read_by(&pushed, "invoices", None, T + 1),
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        f.push_revocation(pushed.identity.public_key(), &stranger, T + 1),
        outcome,
        "pushing a revocation again changes nothing"
    );
    let image = crate::certified::encode(&f.state).unwrap();
    f.state = crate::certified::decode(&image).unwrap();
    assert_eq!(
        f.claim_by(&pushed, "invoices", claim(3, T), T + 2),
        Err(CoordinationError::Denied)
    );

    assert_eq!(
        f.claim_by(&lapsing, "invoices", claim(4, T), T + CLAIM_LIFETIME - 1),
        Ok(3),
        "an unpushed revocation still ends the right with its link"
    );
    assert_eq!(
        f.claim_by(&lapsing, "invoices", claim(5, T), T + CLAIM_LIFETIME),
        Err(CoordinationError::Denied)
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
        T + CLAIM_LIFETIME,
    )
    .unwrap();
    assert_eq!(
        f.state.apply_at(T + CLAIM_LIFETIME, &request),
        Err(CoordinationError::Denied),
        "a statement another publisher signed is refused"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_delegate_claims_only_within_its_delegators_right() {
    let mut f = Fixture::new(Limits::default());
    let delegator = f.holder(f.right(Some(&["invoices", "credits"])), 2);
    let (identity, node) = (PrivateKey::new(), PrivateKey::new());
    let held = delegator.chain.delegable();
    let narrowed = held
        .delegate_claims(
            &delegator.identity,
            identity.public_key(),
            f.right(Some(&["invoices"])),
            1,
            (T, CLAIM_LIFETIME),
        )
        .unwrap();
    let delegate = Holder {
        chain: narrowed.certify_claims(&identity, &node).unwrap(),
        identity,
        node,
    };
    assert_eq!(f.claim_by(&delegate, "invoices", claim(1, T), T), Ok(1));
    assert_eq!(
        f.claim_by(&delegate, "credits", claim(2, T), T),
        Err(CoordinationError::Denied)
    );
    assert!(
        held.delegate_claims(
            &delegator.identity,
            PrivateKey::new().public_key(),
            f.right(None),
            1,
            (T, CLAIM_LIFETIME),
        )
        .is_err()
    );
    f.push_revocation(delegator.identity.public_key(), &PrivateKey::new(), T + 1);
    assert_eq!(
        f.claim_by(&delegate, "invoices", claim(3, T), T + 1),
        Err(CoordinationError::Denied),
        "revoking the delegator ends its delegate's right too"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_control_without_a_publisher_admits_only_trusted_nodes() {
    let mut f = Fixture::new(Limits::default());
    let holder = f.holder(f.right(None), 1);
    let mut control = f.state.controls[0].control.clone();
    control.revision += 1;
    control.publisher = None;
    let revision = ControlCertificate::sign(control, &f.owner).unwrap();
    let request = Request::sign_at(
        f.state.genesis.id().unwrap(),
        [3; 32],
        Operation::Control(revision),
        &f.owner,
        T,
    )
    .unwrap();
    assert_eq!(
        f.state.apply_at(T, &request),
        Err(CoordinationError::Denied),
        "a revision cannot drop the domain's publisher"
    );
    let mut control = f.state.controls[0].control.clone();
    control.publisher = None;
    f.state.controls[0] = ControlCertificate::sign(control, &f.owner).unwrap();
    assert_eq!(
        f.claim_by(&holder, "invoices", claim(1, T), T),
        Err(CoordinationError::Denied),
        "a control that names no publisher admits no claim right"
    );
    assert_eq!(f.trusted_claim("invoices", claim(2, T), T), 1);
}
