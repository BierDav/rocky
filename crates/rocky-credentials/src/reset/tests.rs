use crate::{
    Authority, CredentialChain, FenceLog, FenceScope, PeerId, PrivateKey, PublicKey, Reset,
    ResetStanding, Revocation, RevocationMode, RevocationSet, RootDecision, SignedRevocation,
};
use rocky_core::{AppId, ResourceScope};
use std::collections::BTreeSet;

const APP: AppId = AppId([7; 16]);
const NOW: u64 = 2_000_000_000;
const DAY: u64 = crate::DAY;

fn scope(table: u8) -> ResourceScope {
    ResourceScope {
        domain: rocky_core::Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: APP,
            domain: rocky_core::DomainId([3; 16]),
        },
        encryption: [0; 32],
        table: [table; 32],
    }
}

/// One owner database: its root and a trusted issuer identity.
struct Owner {
    root: PrivateKey,
    identity: PrivateKey,
    chain: CredentialChain,
}
impl Owner {
    fn new() -> Self {
        let root = PrivateKey::new();
        let identity = PrivateKey::new();
        let database = crate::DatabaseContext {
            publisher: root.public_key(),
            scope: scope(0).domain,
        };
        let link = Authority::owner_in(&root, database)
            .unwrap()
            .issue_issuer(
                identity.public_key(),
                crate::Validity {
                    issued_at: NOW - 5 * DAY,
                    lifetime: std::time::Duration::from_secs(300 * DAY),
                    clock_skew: std::time::Duration::from_secs(DAY),
                },
            )
            .unwrap();
        Self {
            root,
            identity,
            chain: CredentialChain(vec![link]),
        }
    }
    /// A node of the owner identity: its key and full chain.
    fn node(&self) -> (PublicKey, CredentialChain) {
        let (key, chain) = self.device();
        (key.public_key(), chain)
    }
    /// A node of the owner identity that signs itself.
    fn device(&self) -> (PrivateKey, CredentialChain) {
        crate::testing::node(&self.identity, &self.chain)
    }
    /// `by` admits `node` after the resets `follows`.
    fn admit(
        &self,
        by: &PrivateKey,
        node: PublicKey,
        follows: &[&SignedRevocation],
    ) -> crate::SignedResetAdmission {
        crate::SignedResetAdmission::sign(
            by,
            crate::DatabaseContext {
                publisher: self.root.public_key(),
                scope: scope(0).domain,
            },
            PeerId::of(node),
            follows.iter().map(|reset| reset.id().unwrap()).collect(),
        )
        .unwrap()
    }
    fn reset(
        &self,
        follows: &[&SignedRevocation],
        keep: &[PublicKey],
        fence: Vec<FenceScope>,
    ) -> SignedRevocation {
        self.sign(
            Reset {
                follows: follows.iter().map(|reset| reset.id().unwrap()).collect(),
                keep: keep.iter().map(|key| PeerId::of(*key)).collect(),
                fence,
            },
            RevocationMode::Lost,
        )
        .unwrap()
    }
    fn sign(&self, reset: Reset, mode: RevocationMode) -> Result<SignedRevocation, crate::Error> {
        SignedRevocation::sign(
            statement(&self.root, reset, mode),
            &self.root,
            &CredentialChain::default(),
        )
    }
    fn revoke(
        &self,
        mode: RevocationMode,
        nodes: &[PublicKey],
        identities: &[PublicKey],
    ) -> SignedRevocation {
        SignedRevocation::sign(
            Revocation {
                app: APP,
                issuer: self.root.public_key(),
                time: NOW,
                mode,
                nodes: nodes.iter().map(|key| PeerId::of(*key)).collect(),
                identities: identities.iter().copied().collect(),
                capabilities: BTreeSet::new(),
                cut: Vec::new(),
                recovery: None,
            },
            &self.root,
            &CredentialChain::default(),
        )
        .unwrap()
    }
}
fn statement(root: &PrivateKey, reset: Reset, mode: RevocationMode) -> Revocation {
    Revocation {
        app: APP,
        issuer: root.public_key(),
        time: NOW,
        mode,
        nodes: BTreeSet::new(),
        identities: BTreeSet::new(),
        capabilities: BTreeSet::new(),
        cut: Vec::new(),
        recovery: Some(RootDecision::Reset(Box::new(reset))),
    }
}
fn fence(entries: &[(u8, PublicKey, u64)]) -> Vec<FenceScope> {
    let mut scopes: Vec<FenceScope> = Vec::new();
    let mut entries = entries.to_vec();
    entries.sort_by_key(|(table, writer, _)| (scope(*table), *writer));
    for (table, writer, next) in entries {
        match scopes.last_mut() {
            Some(last) if last.scope == scope(table) => last.logs.push(FenceLog { writer, next }),
            _ => scopes.push(FenceScope {
                scope: scope(table),
                logs: vec![FenceLog { writer, next }],
            }),
        }
    }
    scopes
}
fn set(statements: &[&SignedRevocation]) -> RevocationSet {
    let mut set = RevocationSet::new();
    for statement in statements {
        let single = RevocationSet::try_from(vec![(*statement).clone()]).unwrap();
        set.merge(&single, single_publisher(statement), APP, NOW)
            .unwrap();
    }
    set
}
fn single_publisher(statement: &SignedRevocation) -> PublicKey {
    statement.statement.issuer
}
fn ended(set: &RevocationSet, chain: &CredentialChain) -> bool {
    set.ends_chain(chain).unwrap() && set.revoking(chain).unwrap().is_some()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_reset_ends_every_node_it_does_not_keep_even_one_never_seen() {
    let owner = Owner::new();
    let (this, this_chain) = owner.node();
    let (custodian, custodian_chain) = owner.node();
    let (laptop, laptop_chain) = owner.node();
    let reset = owner.reset(&[], &[this, custodian], fence(&[(1, laptop, 3)]));
    reset.verify(owner.root.public_key(), APP).unwrap();
    let held = set(&[&reset]);
    assert!(!ended(&held, &this_chain));
    assert!(!ended(&held, &custodian_chain));
    assert!(ended(&held, &laptop_chain));
    // A node certified before the reset that its signer never saw.
    let (hidden, hidden_chain) = owner.node();
    assert!(ended(&held, &hidden_chain));
    assert!(held.ends_node(hidden, true) && held.revokes(&hidden));
    assert_eq!(
        held.ending_resets(&PeerId::of(hidden)),
        vec![reset.id().unwrap()]
    );
    // The identity's own trusted link stays: only node links end.
    assert!(!held.ends_chain(&owner.chain).unwrap());
    // Later operations of a fenced writer stay refused; an unseen writer keeps nothing.
    assert_eq!(
        held.reset_standing(&scope(1), &laptop, 2),
        ResetStanding::Covered
    );
    assert_eq!(
        held.reset_standing(&scope(1), &laptop, 3),
        ResetStanding::Fenced
    );
    assert_eq!(
        held.reset_standing(&scope(2), &laptop, 0),
        ResetStanding::Fenced
    );
    assert_eq!(
        held.reset_standing(&scope(1), &hidden, 0),
        ResetStanding::Fenced
    );
    assert_eq!(
        held.reset_standing(&scope(1), &this, 99),
        ResetStanding::Live
    );
    assert_eq!(
        held.reset_standing(&scope(1), &custodian, 99),
        ResetStanding::Live
    );
    // Covered operations are judged as if no reset ended their writer.
    assert!(!held.unfenced().ends_chain(&laptop_chain).unwrap());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn concurrent_resets_end_each_other_and_a_following_reset_admits_a_new_node() {
    let owner = Owner::new();
    let (a, a_chain) = owner.node();
    let (b, b_chain) = owner.node();
    let (custodian, custodian_chain) = owner.node();
    let first = owner.reset(&[], &[a, custodian], fence(&[(1, b, 4)]));
    let second = owner.reset(&[], &[b, custodian], fence(&[(1, a, 2)]));
    for order in [[&first, &second], [&second, &first]] {
        let held = set(&order);
        assert!(ended(&held, &a_chain) && ended(&held, &b_chain));
        assert!(!ended(&held, &custodian_chain));
        assert_eq!(
            held.reset_heads(),
            [first.id().unwrap(), second.id().unwrap()].into()
        );
        assert_eq!(held.reset_kept(), [PeerId::of(custodian)].into());
        // Each writer keeps what the other's fence covers.
        assert_eq!(
            held.reset_standing(&scope(1), &a, 1),
            ResetStanding::Covered
        );
        assert_eq!(held.reset_standing(&scope(1), &a, 2), ResetStanding::Fenced);
        assert_eq!(
            held.reset_standing(&scope(1), &b, 3),
            ResetStanding::Covered
        );
        assert_eq!(held.reset_standing(&scope(1), &b, 4), ResetStanding::Fenced);
    }
    // The owner signs in again as a new node, following both resets.
    let (fresh, fresh_chain) = owner.node();
    let third = owner.reset(
        &[&first, &second],
        &[fresh, custodian],
        fence(&[(1, a, 2), (1, b, 4)]),
    );
    for order in [
        [&first, &second, &third],
        [&third, &second, &first],
        [&second, &third, &first],
    ] {
        let held = set(&order);
        assert!(!ended(&held, &fresh_chain) && !ended(&held, &custodian_chain));
        assert!(ended(&held, &a_chain) && ended(&held, &b_chain));
        assert_eq!(held.reset_heads(), [third.id().unwrap()].into());
        assert_eq!(
            held.reset_standing(&scope(1), &fresh, 0),
            ResetStanding::Live
        );
        // Fences only shrink: every reset ending a writer must cover.
        assert_eq!(
            held.reset_standing(&scope(1), &a, 1),
            ResetStanding::Covered
        );
        assert_eq!(held.reset_standing(&scope(1), &a, 2), ResetStanding::Fenced);
    }
    // Before the following reset arrives, the new node is ended too.
    assert!(ended(&set(&[&first, &second]), &fresh_chain));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_node_a_kept_node_admits_after_a_reset_stays_and_one_a_signed_out_node_admits_ends() {
    let owner = Owner::new();
    let (this, _) = owner.device();
    let (laptop, _) = owner.device();
    let reset = owner.reset(&[], &[this.public_key()], Vec::new());
    let (phone, phone_chain) = owner.device();
    let (tablet, tablet_chain) = owner.node();
    let (stolen, stolen_chain) = owner.node();
    let admissions = [
        owner.admit(&this, phone.public_key(), &[&reset]),
        // An admitted node admits in turn.
        owner.admit(&phone, tablet, &[&reset]),
        // The signed-out laptop admits nobody, nor does an admission vouch
        // for its own signer.
        owner.admit(&laptop, stolen, &[&reset]),
        owner.admit(&laptop, laptop.public_key(), &[&reset]),
    ];
    for order in [[0, 1, 2, 3], [3, 2, 1, 0]] {
        let mut held = set(&[&reset]);
        for index in order {
            assert!(held.admit(admissions[index].clone()).unwrap());
        }
        assert!(!ended(&held, &phone_chain) && !ended(&held, &tablet_chain));
        assert!(ended(&held, &stolen_chain));
        assert!(held.reset_ends(&PeerId::of(laptop.public_key())));
        assert_eq!(
            held.reset_standing(&scope(1), &tablet, 0),
            ResetStanding::Live
        );
        // The admissions travel with the set, in its one current format.
        let decoded = RevocationSet::from_bytes(&held.to_bytes().unwrap()).unwrap();
        assert_eq!(decoded, held);
        assert!(!decoded.reset_ends(&PeerId::of(tablet)));
        // A following reset that keeps neither ends both again, and an
        // admission naming only the earlier reset does not get past it.
        let again = owner.reset(&[&reset], &[this.public_key()], Vec::new());
        let mut later = held.clone();
        later
            .merge(&set(&[&again]), owner.root.public_key(), APP, NOW)
            .unwrap();
        assert!(ended(&later, &phone_chain) && ended(&later, &tablet_chain));
        assert_eq!(
            later.ending_resets(&PeerId::of(tablet)),
            vec![again.id().unwrap()]
        );
    }
    // Naming a reset the set does not hold admits nothing yet; a forged
    // signature is refused outright.
    let mut early = RevocationSet::new();
    early.admit(admissions[0].clone()).unwrap();
    assert!(!early.reset_ends(&PeerId::of(phone.public_key())));
    let mut forged = admissions[2].clone();
    forged.admission.signer = this.public_key();
    assert!(set(&[&reset]).admit(forged).is_err());
    // A statement ending the admitter by name voids its admissions.
    let lost = owner.revoke(RevocationMode::Lost, &[this.public_key()], &[]);
    let mut held = set(&[&reset, &lost]);
    held.admit(admissions[0].clone()).unwrap();
    assert!(held.reset_ends(&PeerId::of(phone.public_key())));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn lost_and_compromised_dominate_a_fence_in_every_merge_order() {
    let owner = Owner::new();
    let (this, _) = owner.node();
    let (laptop, laptop_chain) = owner.node();
    let (phone, phone_chain) = owner.node();
    let reset = owner.reset(&[], &[this], fence(&[(1, laptop, 5), (1, phone, 5)]));
    let lost = owner.revoke(RevocationMode::Lost, &[laptop], &[]);
    let other = PrivateKey::new().public_key();
    let compromised = owner.revoke(RevocationMode::Compromised, &[], &[other]);
    let statements = [&reset, &lost, &compromised];
    let mut digests = BTreeSet::new();
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let held = set(&order.map(|index| statements[index]));
        digests.insert(held.digest(NOW).unwrap());
        // The fence keeps covered operations of a node ended only by the reset.
        assert_eq!(
            held.reset_standing(&scope(1), &phone, 4),
            ResetStanding::Covered
        );
        assert!(!held.unfenced().ends_chain(&phone_chain).unwrap());
        // A Lost unlink of the same node refuses them anyway.
        assert_eq!(
            held.reset_standing(&scope(1), &laptop, 4),
            ResetStanding::Covered
        );
        assert!(held.unfenced().ends_chain(&laptop_chain).unwrap());
        assert!(held.revokes_trusted(&other) && held.revokes_origin(&other));
    }
    assert_eq!(digests.len(), 1);
    // A reset keeping a node never revives what Lost ended.
    let keeping = owner.reset(&[&reset], &[this, laptop], Vec::new());
    let held = set(&[&reset, &lost, &keeping]);
    assert!(!held.reset_ends(&PeerId::of(laptop)));
    assert!(ended(&held, &laptop_chain));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn forged_malformed_and_old_resets_are_refused() {
    let owner = Owner::new();
    let (this, _) = owner.node();
    let (laptop, _) = owner.node();
    let valid = Reset {
        follows: BTreeSet::new(),
        keep: [PeerId::of(this)].into(),
        fence: fence(&[(1, laptop, 1)]),
    };
    owner.sign(valid.clone(), RevocationMode::Lost).unwrap();
    // Only the root resets, never cascading or naming other targets.
    assert!(
        owner
            .sign(valid.clone(), RevocationMode::Compromised)
            .is_err()
    );
    assert!(
        SignedRevocation::sign(
            Revocation {
                issuer: owner.identity.public_key(),
                ..statement(&owner.root, valid.clone(), RevocationMode::Lost)
            },
            crate::testing::signer(&owner.identity, &owner.chain),
            &owner.chain,
        )
        .is_err()
    );
    assert!(
        SignedRevocation::sign(
            Revocation {
                nodes: [PeerId::of(laptop)].into(),
                ..statement(&owner.root, valid.clone(), RevocationMode::Lost)
            },
            &owner.root,
            &CredentialChain::default(),
        )
        .is_err()
    );
    let malformed = [
        Reset {
            keep: BTreeSet::new(),
            ..valid.clone()
        },
        Reset {
            fence: fence(&[(1, this, 1)]),
            ..valid.clone()
        },
        Reset {
            fence: fence(&[(1, laptop, 0)]),
            ..valid.clone()
        },
        Reset {
            fence: [fence(&[(2, laptop, 1)]), fence(&[(1, laptop, 1)])].concat(),
            ..valid.clone()
        },
        Reset {
            fence: vec![FenceScope {
                scope: scope(1),
                logs: Vec::new(),
            }],
            ..valid.clone()
        },
    ];
    for reset in malformed {
        assert!(owner.sign(reset, RevocationMode::Lost).is_err());
    }
    // Another root's statement, and a tampered keep-list, fail verification.
    let signed = owner.sign(valid.clone(), RevocationMode::Lost).unwrap();
    assert!(signed.verify(PrivateKey::new().public_key(), APP).is_err());
    let mut tampered = signed.clone();
    if let Some(RootDecision::Reset(reset)) = &mut tampered.statement.recovery {
        reset.keep.insert(PeerId::of(laptop));
    }
    assert!(tampered.verify(owner.root.public_key(), APP).is_err());
    let forged = RevocationSet::try_from(vec![tampered]).unwrap();
    assert!(
        RevocationSet::new()
            .merge(&forged, owner.root.public_key(), APP, NOW)
            .is_err()
    );
    // A set in an earlier replicated format is refused.
    #[derive(serde::Serialize)]
    struct Earlier {
        format: u32,
        database: Option<crate::DatabaseContext>,
        statements: Vec<SignedRevocation>,
        certificates: Vec<crate::SignedCapability>,
    }
    let earlier = crate::encode(&Earlier {
        format: 4,
        database: None,
        statements: vec![signed.clone()],
        certificates: Vec::new(),
    })
    .unwrap();
    assert!(RevocationSet::from_bytes(&earlier).is_err());
    let current = RevocationSet::try_from(vec![signed]).unwrap();
    assert_eq!(
        RevocationSet::from_bytes(&current.to_bytes().unwrap()).unwrap(),
        current
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_reset_keeps_the_links_an_ended_node_signed() {
    let owner = Owner::new();
    let publisher = owner.root.public_key();
    // The owner's phone, which the reset ends, enrolled the laptop's identity.
    let (phone, phone_chain) = crate::testing::node(&owner.identity, &owner.chain);
    let laptop_identity = PrivateKey::new();
    let link = Authority::issuer(&phone, &phone_chain)
        .unwrap()
        .issue_issuer(
            laptop_identity.public_key(),
            crate::Validity {
                issued_at: NOW - DAY,
                lifetime: std::time::Duration::from_secs(30 * DAY),
                clock_skew: std::time::Duration::from_secs(DAY),
            },
        )
        .unwrap();
    let issuer = CredentialChain([owner.chain.0.clone(), vec![link]].concat());
    let (laptop, laptop_chain) = crate::testing::node(&laptop_identity, &issuer);
    let reset = owner.reset(&[], &[laptop.public_key()], Vec::new());
    let held = set(&[&reset]);
    assert!(ended(&held, &phone_chain));
    assert!(!ended(&held, &laptop_chain));
    laptop_chain
        .verify(publisher, laptop.public_key(), &held, NOW)
        .unwrap();
    // Naming the phone in a Lost revocation still ends what it signed.
    let lost = owner.revoke(RevocationMode::Lost, &[phone.public_key()], &[]);
    let held = set(&[&reset, &lost]);
    assert!(
        laptop_chain
            .verify(publisher, laptop.public_key(), &held, NOW)
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn resets_never_crowd_out_other_revocations() {
    let owner = Owner::new();
    let (this, this_chain) = owner.node();
    let (laptop, laptop_chain) = owner.node();
    let publisher = owner.root.public_key();
    let mut held = RevocationSet::new();
    let mut resets = 0;
    loop {
        let writers: Vec<_> = (0..1500)
            .map(|_| (1, PrivateKey::new().public_key(), 1))
            .collect();
        let reset = owner.reset(&[], &[this], fence(&writers));
        let single = RevocationSet::try_from(vec![reset]).unwrap();
        let before = held.clone();
        if held.merge(&single, publisher, APP, NOW).is_err() {
            assert_eq!(held, before);
            break;
        }
        resets += 1;
        assert!(resets * 64 * 1024 <= crate::MAX_RESET_SET_BYTES);
    }
    assert!(resets >= 1);
    // Lost and Compromised revocations still merge once resets are full.
    let lost = owner.revoke(RevocationMode::Lost, &[laptop], &[]);
    let compromised = owner.revoke(
        RevocationMode::Compromised,
        &[],
        &[owner.identity.public_key()],
    );
    for statement in [lost, compromised] {
        held.merge(
            &RevocationSet::try_from(vec![statement]).unwrap(),
            publisher,
            APP,
            NOW,
        )
        .unwrap();
    }
    assert!(ended(&held, &laptop_chain) && ended(&held, &this_chain));
}
