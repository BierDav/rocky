use super::*;
use crate::coordinated::{Action, Pending, Result as LogResult};
use p2panda_core::PrivateKey;

pub(super) const T: u64 = 1_900_000_000;
pub(super) struct Fixture {
    pub(super) state: Metadata,
    pub(super) owner: PrivateKey,
    pub(super) nodes: Vec<PrivateKey>,
    pub(super) voter: PrivateKey,
    pub(super) scope: Scope,
}
impl Fixture {
    pub(super) fn new(minimum: usize, limits: Limits) -> Self {
        let root = PrivateKey::new();
        let owner = PrivateKey::new();
        let nodes: Vec<_> = (0..2).map(|_| PrivateKey::new()).collect();
        let voter = PrivateKey::new();
        let scope = Scope {
            mesh: MeshId([1; 16]),
            app: AppId([2; 16]),
            domain: DomainId([3; 16]),
        };
        let certificate = ControlCertificate::sign(
            DomainControl {
                scope,
                configuration: [4; 16],
                revision: 1,
                trusted: nodes.iter().map(PrivateKey::public_key).collect(),
                not_before: T - 1,
                expires: T + 100_000,
            },
            &owner,
        )
        .unwrap();
        let genesis = CertifiedGenesis::sign(
            Genesis {
                mesh: scope.mesh,
                configuration: [4; 16],
                voters: (1..=3)
                    .map(|id| Voter {
                        id,
                        address: format!("127.0.0.1:{}", 8000 + id),
                        key: if id == 1 {
                            voter.public_key()
                        } else {
                            PrivateKey::new().public_key()
                        },
                    })
                    .collect(),
                initial_voters: [1, 2, 3].into(),
                membership_authority: owner.public_key(),
                directory_definition: [6; 32],
                directory: Some(certificate),
                authorities: vec![Authority {
                    scope,
                    issuer: owner.public_key(),
                }],
                max_control_lifetime: 200_000,
                public: None,
            },
            &root,
        )
        .unwrap();
        let state = Metadata::with_limits(genesis, limits).unwrap();
        let mut fixture = Self {
            state,
            owner,
            nodes,
            voter,
            scope,
        };
        fixture.run_owner(Action::Configure { minimum }, T).unwrap();
        for n in 0..2 {
            let head = fixture.state.log_head(tip_log(scope)).unwrap();
            fixture
                .run(
                    n,
                    Action::Join {
                        position: head.commit,
                        running_hash: head.running_hash,
                        sequence: 1,
                    },
                    T,
                )
                .unwrap();
        }
        fixture
    }
    pub(super) fn request(&self, key: &PrivateKey, action: Action, time: u64) -> Request {
        crate::machine::self_certified(
            self.state.genesis.id().unwrap(),
            crate::transport::nonce(),
            Operation::Coordinated {
                log: tip_log(self.scope),
                action,
            },
            key,
            time,
        )
        .unwrap()
    }
    pub(super) fn run(
        &mut self,
        node: usize,
        action: Action,
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        let request = self.request(&self.nodes[node], action, time);
        self.state.apply_at(time, &request)
    }
    pub(super) fn run_owner(
        &mut self,
        action: Action,
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        let request = self.request(&self.owner, action, time);
        self.state.apply_at(time, &request)
    }
    pub(super) fn append(&mut self, n: usize, sequence: u64, hash: u8) -> Pending {
        let head = self.state.log_head(tip_log(self.scope)).unwrap();
        match self
            .run(
                n,
                Action::Append {
                    expected: head.commit,
                    epoch: head.epoch,
                    hash: [hash; 32],
                    duration: 10,
                    sequence,
                },
                T,
            )
            .unwrap()
        {
            Outcome::Coordinated(LogResult::Assigned(p)) => *p,
            other => panic!("unexpected {other:?}"),
        }
    }
    pub(super) fn resolve(
        &mut self,
        pending: &Pending,
        nodes: &[usize],
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        let acknowledgments = nodes
            .iter()
            .map(|n| {
                self.request(
                    &self.nodes[*n],
                    Action::Acknowledge {
                        pending: pending.clone(),
                    },
                    T + 1,
                )
            })
            .collect();
        let request = self.request(
            &self.voter,
            Action::Resolve {
                pending: pending.clone(),
                acknowledgments,
            },
            time,
        );
        self.state.apply_at(time, &request)
    }
}
#[test]
fn competing_appends_retries_and_exact_signed_acknowledgments() {
    let mut f = Fixture::new(1, Limits::default());
    let head = f.state.log_head(tip_log(f.scope)).unwrap();
    let append = Action::Append {
        expected: 0,
        epoch: head.epoch,
        hash: [6; 32],
        duration: 10,
        sequence: 2,
    };
    let first = f.run(0, append.clone(), T).unwrap();
    assert_eq!(
        f.run(1, append.clone(), T),
        Err(CoordinationError::Configuration)
    );
    assert_eq!(f.run(0, append.clone(), T).unwrap(), first);
    let pending = f.state.log_head(tip_log(f.scope)).unwrap().pending.unwrap();
    assert_eq!(
        f.resolve(&pending, &[0], T + 2),
        Err(CoordinationError::Backpressure)
    );
    assert_eq!(f.state.log_head(tip_log(f.scope)).unwrap().commit, 0);
    let mut wrong = pending.clone();
    wrong.hash = [9; 32];
    let ack = f.request(&f.nodes[1], Action::Acknowledge { pending: wrong }, T + 1);
    let resolve = f.request(
        &f.voter,
        Action::Resolve {
            pending: pending.clone(),
            acknowledgments: vec![ack],
        },
        T + 2,
    );
    assert_eq!(
        f.state.apply_at(T + 2, &resolve),
        Err(CoordinationError::Denied)
    );
    assert!(matches!(
        f.resolve(&pending, &[0, 1], T + 2),
        Ok(Outcome::Coordinated(LogResult::Committed {
            position: 1,
            ..
        }))
    ));
    assert_eq!(f.run(0, append, T + 3).unwrap(), first);
    assert_eq!(
        f.resolve(&pending, &[0, 1], T + 3),
        Err(CoordinationError::StaleHead)
    );
    assert_eq!(f.state.log_head(tip_log(f.scope)).unwrap().commit, 1);
    assert_eq!(
        f.run(
            0,
            Action::Join {
                position: 1,
                running_hash: [0; 32],
                sequence: 2
            },
            T + 3
        ),
        Err(CoordinationError::RequestConflict)
    );
}
#[test]
fn deadlines_evict_missing_nodes_and_minimum_prevents_success() {
    for minimum in [1, 2] {
        let mut f = Fixture::new(minimum, Limits::default());
        let pending = f.append(0, 2, 7);
        let result = f.resolve(&pending, &[0], T + 10).unwrap();
        let head = f.state.log_head(tip_log(f.scope)).unwrap();
        assert_eq!(head.members.len(), 1);
        if minimum == 1 {
            assert!(matches!(
                result,
                Outcome::Coordinated(LogResult::Committed { position: 1, .. })
            ));
            assert_eq!(head.commit, 1);
            assert_eq!(
                f.run(
                    1,
                    Action::Join {
                        position: 0,
                        running_hash: rocky_core::log_genesis(tip_log(f.scope), [4; 16]),
                        sequence: 3
                    },
                    T + 11
                ),
                Err(CoordinationError::StaleHead)
            );
        } else {
            assert!(matches!(
                result,
                Outcome::Coordinated(LogResult::Aborted { position: 1, .. })
            ));
            assert_eq!(head.commit, 0);
            assert_eq!(
                f.run(
                    0,
                    Action::Append {
                        expected: 0,
                        epoch: head.epoch,
                        hash: [8; 32],
                        duration: 20,
                        sequence: 3
                    },
                    T + 11
                ),
                Err(CoordinationError::Backpressure)
            );
        }
    }
}
#[test]
fn serialized_leader_recovery_keeps_pending_but_requires_new_acknowledgments() {
    let mut f = Fixture::new(1, Limits::default());
    let pending = f.append(0, 2, 6);
    // Volatile acknowledgments are not a machine operation and cannot persist success.
    assert_eq!(
        f.run(
            0,
            Action::Acknowledge {
                pending: pending.clone()
            },
            T + 1
        ),
        Err(CoordinationError::Denied)
    );
    f.state = decode(&encode(&f.state).unwrap()).unwrap();
    assert_eq!(
        f.state.log_head(tip_log(f.scope)).unwrap().pending,
        Some(pending.clone())
    );
    assert_eq!(
        f.resolve(&pending, &[], T + 2),
        Err(CoordinationError::Backpressure)
    );
    assert!(matches!(
        f.resolve(&pending, &[0, 1], T + 2),
        Ok(Outcome::Coordinated(LogResult::Committed { .. }))
    ));
    f.state = decode(&encode(&f.state).unwrap()).unwrap();
    assert_eq!(
        f.resolve(&pending, &[0, 1], T + 3),
        Err(CoordinationError::StaleHead)
    );
    assert_eq!(f.state.log_head(tip_log(f.scope)).unwrap().hashes.len(), 1);
}
#[test]
fn tenant_rate_deadline_and_byte_budgets_fail_closed() {
    let mut f = Fixture::new(1, Limits::default());
    let h = f.state.log_head(tip_log(f.scope)).unwrap();
    assert_eq!(
        f.run(
            0,
            Action::Append {
                expected: 0,
                epoch: h.epoch,
                hash: [1; 32],
                duration: 31,
                sequence: 2
            },
            T
        ),
        Err(CoordinationError::Configuration)
    );
    let mut last = None;
    for sequence in 3..=62 {
        let pending = f.append(0, sequence, 7);
        f.resolve(&pending, &[0, 1], T + 1).unwrap();
        last = Some(pending);
    }
    let p = last.unwrap();
    assert!(matches!(
        f.run(
            0,
            Action::Append {
                expected: p.position - 1,
                epoch: p.epoch,
                hash: p.hash,
                duration: 10,
                sequence: 62
            },
            T + 2
        ),
        Ok(Outcome::Coordinated(LogResult::Assigned(_)))
    ));
    let h = f.state.log_head(tip_log(f.scope)).unwrap();
    assert_eq!(
        f.run(
            0,
            Action::Append {
                expected: h.commit,
                epoch: h.epoch,
                hash: [1; 32],
                duration: 10,
                sequence: 63
            },
            T + 2
        ),
        Err(CoordinationError::Backpressure)
    );
    f.state.state.limits.tenant_bytes = f.state.tenant_used(f.owner.public_key()).unwrap();
    let before = f.state.log_head(tip_log(f.scope)).unwrap();
    let request = f.request(
        &f.nodes[0],
        Action::Append {
            expected: before.commit,
            epoch: before.epoch,
            hash: [2; 32],
            duration: 10,
            sequence: 64,
        },
        T + 60,
    );
    let assigned = f.state.apply_at(T + 60, &request).unwrap();
    let Outcome::Coordinated(LogResult::Assigned(p)) = assigned else {
        panic!("assignment")
    };
    assert!(matches!(
        f.resolve(&p, &[0, 1], T + 61),
        Ok(Outcome::Coordinated(LogResult::Committed {
            position: 61,
            ..
        }))
    ));
    // Reserved capacity prevents a fully admitted domain from getting stuck on commit.
    assert_eq!(f.state.log_head(tip_log(f.scope)).unwrap().commit, 61);
    f.state.state.domain(f.scope).unwrap().coordinated =
        crate::coordinated::Logs::new(f.scope, [4; 16]);
    f.state.state.limits.tenant_bytes = 4096;
    let head = f.state.log_head(tip_log(f.scope)).unwrap();
    assert_eq!(
        f.run(
            0,
            Action::Join {
                position: 0,
                running_hash: head.running_hash,
                sequence: 1
            },
            T + 62
        ),
        Err(CoordinationError::Backpressure)
    );
    assert!(
        f.state
            .log_head(tip_log(f.scope))
            .unwrap()
            .members
            .is_empty()
    );
}

#[test]
fn hash_window_is_bounded_and_encoded_domain_footprint_is_measured() {
    let f = Fixture::new(1, Limits::default());
    let domain = f.state.state.find(f.scope).unwrap();
    let initial = encode(&(domain, &f.state.controls[0])).unwrap().len();
    let mut log = domain
        .coordinated
        .get(tip_log(f.scope).tip)
        .unwrap()
        .clone();
    for position in 1..=1025_u64 {
        let time = T + position * 60;
        let action = Action::Append {
            expected: position - 1,
            epoch: log.head.epoch,
            hash: commitment("test-entry", &position.to_be_bytes()),
            duration: 10,
            sequence: position + 1,
        };
        let LogResult::Assigned(pending) = log
            .apply(f.nodes[0].public_key(), &action, time, true)
            .unwrap()
        else {
            panic!("assignment")
        };
        log.resolve(
            &pending,
            &[f.nodes[0].public_key(), f.nodes[1].public_key()],
            time + 1,
        )
        .unwrap();
    }
    assert_eq!(log.head.hashes.len(), 1024);
    assert_eq!(
        log.head.hashes.front(),
        Some(&commitment("test-entry", &2_u64.to_be_bytes()))
    );
    let encoded = encode(&log).unwrap();
    let restored: crate::coordinated::Log =
        p2panda_core::cbor::decode_cbor(encoded.as_slice()).unwrap();
    assert_eq!(encode(&restored).unwrap(), encoded);
    assert_eq!(restored.head, log.head);
    assert!(encoded.len() < 40 * 1024);
    println!(
        "two-node domain and control: {initial} bytes; full log/window: {} bytes; reserved accounting: {} bytes",
        encoded.len(),
        log.bytes()
    );
}

#[test]
fn deadline_resolution_uses_authority_at_the_signed_storage_acknowledgment() {
    let mut f = Fixture::new(1, Limits::default());
    let mut control = f.state.controls[0].control.clone();
    control.expires = T + 5;
    f.state.controls[0] = ControlCertificate::sign(control, &f.owner).unwrap();
    let pending = f.append(0, 2, 7);
    assert!(matches!(
        f.resolve(&pending, &[0], T + 10),
        Ok(Outcome::Coordinated(LogResult::Committed {
            position: 1,
            ..
        }))
    ));
    assert_eq!(f.state.log_head(tip_log(f.scope)).unwrap().members.len(), 1);
    let request = f.request(&f.owner, Action::Head, T + 11);
    assert_eq!(f.state.read(&request), Err(CoordinationError::Denied));
}

#[test]
fn log_roles_and_signed_proofs_cannot_bypass_domain_control() {
    let mut f = Fixture::new(1, Limits::default());
    let head = f.request(&f.owner, Action::Head, T);
    assert_eq!(f.state.read(&head), Err(CoordinationError::Denied));
    // A trusted node relays a tip's opening, never after the first join.
    assert_eq!(
        f.run(0, Action::Configure { minimum: 1 }, T),
        Err(CoordinationError::Configuration)
    );
    let mut control = f.state.controls[0].control.clone();
    control.revision = 2;
    control
        .trusted
        .extend((0..31).map(|_| PrivateKey::new().public_key()));
    let request = crate::machine::self_certified(
        f.state.genesis.id().unwrap(),
        [9; 32],
        Operation::Control(ControlCertificate::sign(control, &f.owner).unwrap()),
        &f.owner,
        T,
    )
    .unwrap();
    assert_eq!(
        f.state.apply_at(T, &request),
        Err(CoordinationError::Backpressure)
    );
    let pending = f.append(0, 2, 7);
    assert_eq!(
        f.run(
            0,
            Action::Resolve {
                pending: pending.clone(),
                acknowledgments: vec![]
            },
            T + 10
        ),
        Err(CoordinationError::Denied)
    );
    let ack = f.request(
        &f.nodes[0],
        Action::Acknowledge {
            pending: pending.clone(),
        },
        T + 1,
    );
    let duplicate = f.request(
        &f.voter,
        Action::Resolve {
            pending: pending.clone(),
            acknowledgments: vec![ack.clone(), ack],
        },
        T + 2,
    );
    assert_eq!(
        f.state.apply_at(T + 2, &duplicate),
        Err(CoordinationError::Denied)
    );
    let mut forged = f.request(
        &f.nodes[0],
        Action::Acknowledge {
            pending: pending.clone(),
        },
        T + 1,
    );
    forged.key = f.nodes[1].public_key();
    let request = f.request(
        &f.voter,
        Action::Resolve {
            pending: pending.clone(),
            acknowledgments: vec![forged],
        },
        T + 2,
    );
    assert_eq!(
        f.state.apply_at(T + 2, &request),
        Err(CoordinationError::Denied)
    );
    // Closure cannot strand a pending log entry.
    let close = crate::machine::self_certified(
        f.state.genesis.id().unwrap(),
        [8; 32],
        Operation::Close { scope: f.scope },
        &f.owner,
        T + 2,
    )
    .unwrap();
    assert_eq!(
        f.state.apply_at(T + 2, &close),
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        f.state.log_head(tip_log(f.scope)).unwrap().pending,
        Some(pending)
    );
}

/// `identity`'s certificate for the node `node`.
fn certificate(identity: &PrivateKey, node: &PrivateKey) -> rocky_credentials::SignedCapability {
    use rocky_credentials::{Capability, Extended, Extension, PeerId};
    rocky_credentials::SignedCapability::sign(
        Capability {
            subject: node.public_key(),
            kind: rocky_core::PrincipalKind::Node,
            grants: Vec::new(),
            not_before: T - 1,
            expires: T + 1000,
            delegations: 0,
            extension: Extension::new(Extended {
                node: Some(PeerId::of(node.public_key())),
                ..Extended::default()
            })
            .unwrap(),
        },
        identity,
    )
    .unwrap()
}

#[test]
fn controls_trust_identities_whose_certified_nodes_join_as_nodes() {
    let mut f = Fixture::new(1, Limits::default());
    let (genesis, owner, scope) = (f.state.genesis.id().unwrap(), f.owner.clone(), f.scope);
    let identity = PrivateKey::new();
    let mut control = f.state.controls[0].control.clone();
    control.revision = 2;
    control.trusted.push(identity.public_key());
    let revise = |control: DomainControl, id: u8| {
        crate::machine::self_certified(
            genesis,
            [id; 32],
            Operation::Control(ControlCertificate::sign(control, &owner).unwrap()),
            &owner,
            T,
        )
        .unwrap()
    };
    f.state.apply_at(T, &revise(control.clone(), 9)).unwrap();
    let join = |head: crate::coordinated::Head,
                node: &PrivateKey,
                certificate: Option<rocky_credentials::SignedCapability>| {
        Request::certified(
            genesis,
            crate::transport::nonce(),
            Operation::Coordinated {
                log: tip_log(scope),
                action: Action::Join {
                    position: head.commit,
                    running_hash: head.running_hash,
                    sequence: 1,
                },
            },
            (node, certificate),
            T,
        )
        .unwrap()
    };
    // Two nodes of the trusted identity join; the set counts them apart.
    let (first, second) = (PrivateKey::new(), PrivateKey::new());
    for node in [&first, &second] {
        let head = f.state.log_head(tip_log(scope)).unwrap();
        let request = join(head, node, Some(certificate(&identity, node)));
        f.state.apply_at(T, &request).unwrap();
    }
    let members = f.state.log_head(tip_log(scope)).unwrap().members;
    assert_eq!(members.len(), 4);
    for node in [&first, &second] {
        let member = members.iter().find(|m| m.key == node.public_key()).unwrap();
        assert_eq!(member.identity, identity.public_key());
    }
    // No certificate, another identity's, or one for another node.
    let stranger = PrivateKey::new();
    let other = PrivateKey::new();
    let head = f.state.log_head(tip_log(scope)).unwrap();
    for request in [
        join(head.clone(), &stranger, None),
        join(
            head.clone(),
            &stranger,
            Some(certificate(&other, &stranger)),
        ),
        join(head, &stranger, Some(certificate(&identity, &first))),
    ] {
        assert_eq!(
            f.state.apply_at(T, &request),
            Err(CoordinationError::Denied)
        );
    }
    // A control that drops the identity waits while its nodes are in the set.
    control.revision = 3;
    control.trusted.retain(|key| *key != identity.public_key());
    assert_eq!(
        f.state.apply_at(T, &revise(control, 10)),
        Err(CoordinationError::Backpressure)
    );
}

#[test]
fn duration_uses_authority_time_and_retry_never_extends_assignment() {
    let mut f = Fixture::new(2, Limits::default());
    let head = f.state.log_head(tip_log(f.scope)).unwrap();
    let action = Action::Append {
        expected: 0,
        epoch: head.epoch,
        hash: [31; 32],
        duration: 20,
        sequence: 2,
    };
    let request = f.request(&f.nodes[0], action, T - 25);
    let assigned = f.state.apply_at(T, &request).unwrap();
    let Outcome::Coordinated(LogResult::Assigned(pending)) = &assigned else {
        panic!("assignment");
    };
    assert_eq!(pending.deadline, T + 20);
    assert_eq!(f.state.apply_at(T + 15, &request).unwrap(), assigned);
}

#[test]
fn compact_status_omits_the_entire_hash_window() {
    let mut f = Fixture::new(2, Limits::default());
    let pending = f.append(0, 2, 17);
    let log = tip_log(f.scope);
    let mut head = f.state.log_head(log).unwrap();
    head.hashes = (0..crate::coordinated::HASH_WINDOW)
        .map(|_| [1; 32])
        .collect();
    let status = head.status(pending.position, pending.hash);
    assert_eq!(status.decision, LogDecision::Pending);
    let full_bytes = encode(&Outcome::CoordinatedHead(Box::new(head)))
        .unwrap()
        .len();
    let status_bytes = encode(&Outcome::CoordinatedStatus(status)).unwrap().len();
    println!(
        "full head={full_bytes} bytes; pending status={status_bytes} bytes; saved={} bytes/poll",
        full_bytes - status_bytes
    );
    assert!(full_bytes - status_bytes >= crate::coordinated::HASH_WINDOW * 32);
}

#[test]
fn sixty_second_clock_skew_is_explicit_and_correction_reuses_the_append() {
    let mut f = Fixture::new(2, Limits::default());
    let now = now().unwrap();
    let head = f.state.log_head(tip_log(f.scope)).unwrap();
    let action = Action::Append {
        expected: 0,
        epoch: head.epoch,
        hash: [19; 32],
        duration: 20,
        sequence: 2,
    };
    let skewed = f.request(&f.nodes[0], action.clone(), now + 60);
    assert!(
        matches!(skewed.fresh(), Err(CoordinationError::ClockSkew { coordinator_time }) if coordinator_time.abs_diff(now) <= 1)
    );
    assert!(
        f.state
            .log_head(tip_log(f.scope))
            .unwrap()
            .pending
            .is_none()
    );
    let corrected = f.request(&f.nodes[0], action.clone(), now);
    corrected.fresh().unwrap();
    // The fixture's durable authority time is fixed independently of the device.
    assert!(matches!(
        f.run(0, action, T),
        Ok(Outcome::Coordinated(LogResult::Assigned(_)))
    ));
}
