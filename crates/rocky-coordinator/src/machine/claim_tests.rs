//! Claim rights: nodes that are not trusted claim and read the counters
//! their publisher-rooted claim chain covers, and do nothing else.
use super::coordinated_tests::{Fixture, T};
use super::*;
use crate::coordinated::Action;
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
/// An identity holding a claim right, and one node it certified.
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
    /// The owner, the domain's publisher, grants `right` at `T`.
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
            .certify_claims(&identity, node.public_key(), 1)
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
        let request = crate::machine::self_certified(
            self.state.genesis.id().unwrap(),
            crate::transport::nonce(),
            Operation::Claim {
                scope: self.scope,
                counter: name(counter),
                claim,
            },
            &self.nodes[0],
            time,
        )
        .unwrap();
        match self.state.apply_at(time, &request).unwrap() {
            Outcome::Claimed(value) => value,
            other => panic!("claim: {other:?}"),
        }
    }
    /// The owner revokes `identity`; `pusher` pushes the statement.
    fn push_revocation(&mut self, identity: PublicKey, pusher: &PrivateKey, time: u64) -> Outcome {
        let statement = SignedRevocation::sign(
            Revocation {
                app: self.scope.app,
                issuer: self.owner.public_key(),
                time,
                nodes: Default::default(),
                identities: [identity].into(),
                capabilities: Default::default(),
                cut: Default::default(),
            },
            &self.owner,
            &CredentialChain::default(),
        )
        .unwrap();
        let request = Request::sign_at(
            self.state.genesis.id().unwrap(),
            crate::transport::nonce(),
            Operation::Revoke {
                scope: self.scope,
                revocations: RevocationSet::try_from(vec![statement]).unwrap(),
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
    let mut f = Fixture::new(1, Limits::default());
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
    // A retry answers the same value, from the holder or a trusted node.
    assert_eq!(f.claim_by(&holder, "invoices", claim(1, T), T + 2), Ok(1));
    assert_eq!(f.trusted_claim("invoices", claim(3, T), T + 2), 3);
    assert_eq!(
        f.read_by(&holder, "invoices", Some(claim(3, T)), T + 2),
        Ok(Lookup::Claimed(3))
    );
    // An issue time outside the window is `Expired`, never a new value.
    assert_eq!(
        f.claim_by(&holder, "invoices", claim(4, T - CLAIM_WINDOW - 1), T + 2),
        Err(CoordinationError::Expired)
    );
    assert_eq!(
        f.claim_by(&holder, "invoices", claim(1, T + 1), T + 2),
        Err(CoordinationError::RequestConflict)
    );
    assert_eq!(f.claim_by(&holder, "invoices", claim(5, T), T + 2), Ok(4));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn non_holders_and_other_counters_are_denied() {
    let mut f = Fixture::new(1, Limits::default());
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

    // A node presenting no chain, another node's chain, or a chain
    // another publisher roots.
    let stranger = Holder {
        identity: PrivateKey::new(),
        node: PrivateKey::new(),
        chain: invoices.chain.clone(),
    };
    assert_eq!(
        f.claim_by(&stranger, "invoices", claim(3, T), T),
        Err(CoordinationError::Denied)
    );
    let foreign = {
        let publisher = PrivateKey::new();
        let identity = PrivateKey::new();
        let node = PrivateKey::new();
        let link = rocky_credentials::Authority::owner(&publisher)
            .issue_claims(identity.public_key(), f.right(None), 1, (T, CLAIM_LIFETIME))
            .unwrap();
        let chain = CredentialChain(vec![link])
            .certify_claims(&identity, node.public_key(), 1)
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
    // A right for another domain claims nothing here.
    let mut elsewhere = f.right(None);
    elsewhere.scope.domain = DomainId([8; 16]);
    let elsewhere = f.holder(elsewhere, 1);
    assert_eq!(
        f.claim_by(&elsewhere, "invoices", claim(6, T), T),
        Err(CoordinationError::Denied)
    );
    assert_eq!(f.trusted_claim("invoices", claim(7, T), T), 2);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_holder_neither_joins_logs_nor_acknowledges_nor_controls() {
    let mut f = Fixture::new(1, Limits::default());
    let holder = f.holder(f.right(None), 1);
    let log = tip_log(f.scope);
    let head = f.state.log_head(log).unwrap();
    let as_holder = |f: &Fixture, action: Action| {
        f.presenting(&holder, Operation::Coordinated { log, action }, T)
    };
    for action in [
        Action::Join {
            position: head.commit,
            running_hash: head.running_hash,
            sequence: 1,
        },
        Action::Head,
        Action::Configure { minimum: 1 },
    ] {
        let request = as_holder(&f, action);
        assert_eq!(
            f.state.apply_at(T, &request),
            Err(CoordinationError::Denied)
        );
    }
    // Its acknowledgment never counts toward a resolution.
    let pending = f.append(0, 2, 6);
    let acknowledgment = as_holder(
        &f,
        Action::Acknowledge {
            pending: pending.clone(),
        },
    );
    let resolve = f.request(
        &f.voter,
        Action::Resolve {
            pending,
            acknowledgments: vec![acknowledgment],
        },
        T + 2,
    );
    assert_eq!(
        f.state.apply_at(T + 2, &resolve),
        Err(CoordinationError::Denied)
    );
    // Nor does it close or reconfigure the domain.
    for operation in [
        Operation::Close { scope: f.scope },
        Operation::Configure {
            scope: f.scope,
            minimum: 1,
        },
    ] {
        let request = f.presenting(&holder, operation, T);
        assert_eq!(
            f.state.apply_at(T, &request),
            Err(CoordinationError::Denied)
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_revoked_holder_is_refused_at_the_push_or_when_its_link_expires() {
    let mut f = Fixture::new(1, Limits::default());
    let pushed = f.holder(f.right(None), 1);
    let lapsing = f.holder(f.right(None), 1);
    assert_eq!(f.claim_by(&pushed, "invoices", claim(1, T), T), Ok(1));
    assert_eq!(f.claim_by(&lapsing, "invoices", claim(2, T), T), Ok(2));

    // Any node pushes the owner's statement; it holds from that commit on,
    // for retries too.
    let stranger = PrivateKey::new();
    let outcome = f.push_revocation(pushed.identity.public_key(), &stranger, T + 1);
    assert!(matches!(outcome, Outcome::Revoked(_)));
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
    // Pushing it again changes nothing; a snapshot keeps it.
    assert_eq!(
        f.push_revocation(pushed.identity.public_key(), &stranger, T + 1),
        outcome
    );
    let image = crate::certified::encode(&f.state).unwrap();
    f.state = crate::certified::decode(&image).unwrap();
    assert_eq!(
        f.claim_by(&pushed, "invoices", claim(3, T), T + 2),
        Err(CoordinationError::Denied)
    );

    // Unpushed, a revocation still ends the right with its link: no issuer
    // renews a revoked identity.
    assert_eq!(
        f.claim_by(&lapsing, "invoices", claim(4, T), T + CLAIM_LIFETIME - 1),
        Ok(3)
    );
    assert_eq!(
        f.claim_by(&lapsing, "invoices", claim(5, T), T + CLAIM_LIFETIME),
        Err(CoordinationError::Denied)
    );

    // A statement another publisher signed is refused.
    let forged = SignedRevocation::sign(
        Revocation {
            app: f.scope.app,
            issuer: stranger.public_key(),
            time: T,
            nodes: Default::default(),
            identities: [lapsing.identity.public_key()].into(),
            capabilities: Default::default(),
            cut: Default::default(),
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
            revocations: RevocationSet::try_from(vec![forged]).unwrap(),
        },
        &stranger,
        T + CLAIM_LIFETIME,
    )
    .unwrap();
    assert_eq!(
        f.state.apply_at(T + CLAIM_LIFETIME, &request),
        Err(CoordinationError::Denied)
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_delegate_claims_only_within_its_delegators_right() {
    let mut f = Fixture::new(1, Limits::default());
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
        chain: narrowed
            .certify_claims(&identity, node.public_key(), 2)
            .unwrap(),
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
    // Revoking the delegator ends its delegate's right too.
    f.push_revocation(delegator.identity.public_key(), &PrivateKey::new(), T + 1);
    assert_eq!(
        f.claim_by(&delegate, "invoices", claim(3, T), T + 1),
        Err(CoordinationError::Denied)
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_control_without_a_publisher_admits_only_trusted_nodes() {
    let mut f = Fixture::new(1, Limits::default());
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
    // The publisher is fixed for the domain: a revision cannot drop it.
    assert_eq!(
        f.state.apply_at(T, &request),
        Err(CoordinationError::Denied)
    );
    // A control that names none admits no claim right.
    let mut control = f.state.controls[0].control.clone();
    control.publisher = None;
    f.state.controls[0] = ControlCertificate::sign(control, &f.owner).unwrap();
    assert_eq!(
        f.claim_by(&holder, "invoices", claim(1, T), T),
        Err(CoordinationError::Denied)
    );
    assert_eq!(f.trusted_claim("invoices", claim(2, T), T), 1);
}
