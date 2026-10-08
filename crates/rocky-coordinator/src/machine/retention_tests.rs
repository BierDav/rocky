use super::*;
use p2panda_core::PrivateKey;

const DAY: u64 = crate::retention::DAY;
struct Mesh {
    genesis: CertifiedGenesis,
    authority: PrivateKey,
    executor: PrivateKey,
    phone: PrivateKey,
    owner: PrincipalId,
    scopes: Vec<Scope>,
    time: u64,
}
fn scope(n: u8) -> Scope {
    Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([n; 16]),
    }
}
impl Mesh {
    fn new(domains: u8) -> Self {
        let root = PrivateKey::new();
        let authority = PrivateKey::new();
        let executor = PrivateKey::new();
        let phone = PrivateKey::new();
        let owner = PrincipalId([9; 16]);
        let time = 1_900_000_000;
        let scopes: Vec<_> = (1..=domains).map(scope).collect();
        let mut mesh = Self {
            genesis: CertifiedGenesis::sign(
                Genesis {
                    mesh: scopes[0].mesh,
                    configuration: [5; 16],
                    voters: (1..=3)
                        .map(|id| Voter {
                            id,
                            address: format!("127.0.0.1:{}", 9000 + id),
                            key: PrivateKey::new().public_key(),
                        })
                        .collect(),
                    initial_voters: [1, 2, 3].into(),
                    membership_authority: authority.public_key(),
                    directory_definition: [1; 32],
                    directory: Some(
                        ControlCertificate::sign(
                            DomainControl {
                                scope: scopes[0],
                                configuration: [5; 16],
                                revision: 1,
                                owner,
                                executors: vec![executor.public_key()],
                                invokers: vec![
                                    (executor.public_key(), owner),
                                    (phone.public_key(), owner),
                                ],
                                schema: 1,
                                handler: 1,
                                not_before: 0,
                                expires: 366 * DAY,
                            },
                            &authority,
                        )
                        .unwrap(),
                    ),
                    authorities: scopes
                        .iter()
                        .map(|scope| Authority {
                            scope: *scope,
                            issuer: authority.public_key(),
                        })
                        .collect(),
                    max_control_lifetime: 366 * DAY,
                    public: None,
                },
                &root,
            )
            .unwrap(),
            authority,
            executor,
            phone,
            owner,
            scopes,
            time,
        };
        mesh.genesis.genesis.directory = Some(mesh.control(mesh.scopes[0], 1));
        mesh.genesis = CertifiedGenesis::sign(mesh.genesis.genesis.clone(), &root).unwrap();
        mesh
    }
    fn control(&self, scope: Scope, revision: u64) -> ControlCertificate {
        ControlCertificate::sign(
            DomainControl {
                scope,
                configuration: [5; 16],
                revision,
                owner: self.owner,
                executors: vec![self.executor.public_key()],
                invokers: vec![
                    (self.executor.public_key(), self.owner),
                    (self.phone.public_key(), self.owner),
                ],
                schema: 1,
                handler: 1,
                not_before: self.time - DAY,
                expires: self.time + 300 * DAY,
            },
            &self.authority,
        )
        .unwrap()
    }
    fn request(&self, key: &PrivateKey, operation: Operation, time: u64) -> Request {
        Request::sign_at(self.genesis.id().unwrap(), nonce(), operation, key, time).unwrap()
    }
    fn apply(
        &self,
        metadata: &mut Metadata,
        key: &PrivateKey,
        operation: Operation,
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        self.apply_signed(metadata, key, operation, time, time)
    }
    /// Applies a request signed at `signed` under leader-proposed `proposed` time.
    fn apply_signed(
        &self,
        metadata: &mut Metadata,
        key: &PrivateKey,
        operation: Operation,
        signed: u64,
        proposed: u64,
    ) -> Result<Outcome, CoordinationError> {
        metadata.apply_at(proposed, &self.request(key, operation, signed))
    }
    fn head(&self, metadata: &mut Metadata, scope: Scope, time: u64) -> StrongHead {
        match self.apply(metadata, &self.executor, Operation::Head { scope }, time) {
            Ok(Outcome::Head(head)) => head,
            other => panic!("head: {other:?}"),
        }
    }
    fn confirm(
        &self,
        metadata: &mut Metadata,
        scope: Scope,
        sequence: u64,
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        let head = self.head(metadata, scope, time);
        let mut p = Confirmation {
            caller: self.owner,
            scope,
            configuration: head.configuration,
            executor: head.executor,
            epoch: head.epoch,
            request: RequestId {
                node: node_id(self.phone.public_key()),
                sequence: AuthorSequence::new(sequence).unwrap(),
            },
            request_hash: commitment("test", &sequence.to_le_bytes()),
            expected_sequence: head.sequence,
            expected_head: head.head,
            schema: 1,
            handler: 1,
            result: [3; 32],
            new_head: [0; 32],
        };
        p.new_head = successor(p.expected_head, head.sequence + 1, p.request_hash, p.result);
        self.apply(metadata, &self.executor, Operation::Confirm(p), time)
    }
    fn lookup(
        &self,
        metadata: &mut Metadata,
        scope: Scope,
        sequence: u64,
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        let request = RequestId {
            node: node_id(self.phone.public_key()),
            sequence: AuthorSequence::new(sequence).unwrap(),
        };
        self.apply(
            metadata,
            &self.phone,
            Operation::Lookup { scope, request },
            time,
        )
    }
    fn open(&self, limits: Limits) -> Metadata {
        let mut metadata = Metadata::with_limits(self.genesis.clone(), limits).unwrap();
        self.apply(
            &mut metadata,
            &self.executor,
            Operation::Acquire {
                scope: self.scopes[0],
            },
            self.time,
        )
        .unwrap();
        metadata
    }
}
fn nonce() -> Commitment {
    *PrivateKey::new().public_key().as_bytes()
}
fn at(time: u64, n: u64) -> u64 {
    (time << ISSUE_SHIFT) + n
}

#[test]
fn authority_time_is_committed_monotone_and_survives_snapshot_and_failover() {
    let mesh = Mesh::new(1);
    let mut metadata = mesh.open(Limits::default());
    let scope = mesh.scopes[0];
    let t = mesh.time;
    assert!(matches!(
        mesh.confirm(&mut metadata, scope, at(t, 1), t),
        Ok(Outcome::Receipt(Some(_)))
    ));
    // A new leader whose clock is behind cannot rewind authority time,
    // so an identity whose admission closed stays closed.
    metadata
        .apply_at(
            t + DAY + 10,
            &mesh.request(&mesh.executor, Operation::Head { scope }, t + DAY + 10),
        )
        .unwrap();
    let restored: Metadata = decode(&encode(&metadata).unwrap()).unwrap();
    let mut metadata = restored;
    assert_eq!(metadata.time(), t + DAY + 10);
    let request = RequestId {
        node: node_id(mesh.phone.public_key()),
        sequence: AuthorSequence::new(at(t, 2)).unwrap(),
    };
    assert_eq!(
        mesh.apply_signed(
            &mut metadata,
            &mesh.phone,
            Operation::Lookup { scope, request },
            t + DAY + 10,
            t - 3600
        ),
        Err(CoordinationError::Expired)
    );
    assert_eq!(metadata.time(), t + DAY + 10);
    assert_eq!(
        mesh.confirm(&mut metadata, scope, at(t, 2), t + DAY + 10),
        Err(CoordinationError::Expired)
    );
    // The confirmed response is still retained within the horizon.
    assert!(matches!(
        mesh.lookup(&mut metadata, scope, at(t, 1), t + DAY + 10),
        Ok(Outcome::Receipt(Some(_)))
    ));
}

#[test]
fn only_the_issuing_node_retires_its_identities() {
    let mesh = Mesh::new(1);
    let mut metadata = mesh.open(Limits::default());
    let scope = mesh.scopes[0];
    let t = mesh.time;
    for n in 1..=3 {
        mesh.confirm(&mut metadata, scope, at(t, n), t).unwrap();
    }
    // The executor's key retires the executor's own (empty) session only.
    assert_eq!(
        mesh.apply(
            &mut metadata,
            &mesh.executor,
            Operation::Retire {
                scope,
                through: at(t, 2)
            },
            t
        ),
        Ok(Outcome::Floor(at(t, 2)))
    );
    assert!(matches!(
        mesh.lookup(&mut metadata, scope, at(t, 1), t),
        Ok(Outcome::Receipt(Some(_)))
    ));
    assert_eq!(
        mesh.apply(
            &mut metadata,
            &mesh.phone,
            Operation::Retire {
                scope,
                through: at(t, 2)
            },
            t
        ),
        Ok(Outcome::Floor(at(t, 2)))
    );
    assert_eq!(
        mesh.lookup(&mut metadata, scope, at(t, 1), t),
        Err(CoordinationError::Retired)
    );
    assert_eq!(
        mesh.confirm(&mut metadata, scope, at(t, 2), t),
        Err(CoordinationError::Retired)
    );
    assert!(matches!(
        mesh.lookup(&mut metadata, scope, at(t, 3), t),
        Ok(Outcome::Receipt(Some(_)))
    ));
    // A floor ahead of authority time would block future identities.
    assert_eq!(
        mesh.apply(
            &mut metadata,
            &mesh.phone,
            Operation::Retire {
                scope,
                through: at(t + DAY, 0)
            },
            t
        ),
        Err(CoordinationError::Denied)
    );
}

#[test]
fn closed_domains_never_execute_or_reopen_and_keep_a_marker() {
    let mesh = Mesh::new(2);
    let mut metadata = mesh.open(Limits::default());
    let scope = mesh.scopes[0];
    let t = mesh.time;
    mesh.confirm(&mut metadata, scope, at(t, 1), t).unwrap();
    let head = mesh.head(&mut metadata, scope, t);
    assert_eq!(
        mesh.apply(
            &mut metadata,
            &mesh.executor,
            Operation::Close { head: head.clone() },
            t
        ),
        Err(CoordinationError::Denied)
    );
    let mut stale = head.clone();
    stale.sequence = 0;
    assert_eq!(
        mesh.apply(
            &mut metadata,
            &mesh.authority,
            Operation::Close { head: stale },
            t
        ),
        Err(CoordinationError::StaleHead)
    );
    let Ok(Outcome::Head(closed)) = mesh.apply(
        &mut metadata,
        &mesh.authority,
        Operation::Close { head: head.clone() },
        t,
    ) else {
        panic!("closure")
    };
    assert_eq!((closed.sequence, closed.head), (head.sequence, head.head));
    assert!(closed.epoch > head.epoch);
    assert_eq!(
        mesh.confirm(&mut metadata, scope, at(t, 2), t),
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        mesh.apply(
            &mut metadata,
            &mesh.executor,
            Operation::Acquire { scope },
            t
        ),
        Err(CoordinationError::Denied)
    );
    for revision in [1, 2] {
        assert!(
            mesh.apply(
                &mut metadata,
                &mesh.authority,
                Operation::Control(mesh.control(scope, revision)),
                t
            )
            .is_err()
        );
    }
    // Long after closure only the fixed-size marker remains.
    let later = t + 8 * DAY;
    let head = mesh.head(&mut metadata, scope, later);
    assert_eq!(head, closed);
    assert_eq!(metadata.retention().sessions, 0);
}

#[test]
fn tenant_domain_and_byte_quotas_backpressure_without_eviction() {
    let mesh = Mesh::new(3);
    let limits = Limits {
        tenant_domains: 2,
        ..Limits::default()
    };
    let mut metadata = mesh.open(limits);
    let t = mesh.time;
    mesh.apply(
        &mut metadata,
        &mesh.authority,
        Operation::Control(mesh.control(mesh.scopes[1], 1)),
        t,
    )
    .unwrap();
    assert_eq!(
        mesh.apply(
            &mut metadata,
            &mesh.authority,
            Operation::Control(mesh.control(mesh.scopes[2], 1)),
            t
        ),
        Err(CoordinationError::Backpressure)
    );
    // Closing a domain frees its slot; its identity is not reusable.
    let head = mesh.head(&mut metadata, mesh.scopes[1], t);
    mesh.apply(&mut metadata, &mesh.authority, Operation::Close { head }, t)
        .unwrap();
    mesh.apply(
        &mut metadata,
        &mesh.authority,
        Operation::Control(mesh.control(mesh.scopes[2], 1)),
        t,
    )
    .unwrap();

    let used = {
        let mut probe = mesh.open(Limits::default());
        mesh.confirm(&mut probe, mesh.scopes[0], at(t, 1), t)
            .unwrap();
        probe.tenant_available(mesh.scopes[0]).unwrap()
    };
    let limits = Limits {
        tenant_bytes: Limits::default().tenant_bytes - used + 1,
        ..Limits::default()
    };
    let mut metadata = mesh.open(limits);
    mesh.confirm(&mut metadata, mesh.scopes[0], at(t, 1), t)
        .unwrap();
    assert_eq!(
        mesh.confirm(&mut metadata, mesh.scopes[0], at(t, 2), t),
        Err(CoordinationError::Backpressure)
    );
    // The first response is retained, not evicted for the second.
    assert!(matches!(
        mesh.lookup(&mut metadata, mesh.scopes[0], at(t, 1), t),
        Ok(Outcome::Receipt(Some(_)))
    ));
}

#[test]
fn a_generation_activates_once_at_an_exact_head_by_the_scope_authority() {
    let mesh = Mesh::new(2);
    let mut metadata = mesh.open(Limits::default());
    let scope = mesh.scopes[0];
    let t = mesh.time;
    mesh.confirm(&mut metadata, scope, at(t, 1), t).unwrap();
    let head = mesh.head(&mut metadata, scope, t);
    let activation = Activation {
        head: head.clone(),
        schema: 2,
        handler: 1,
        transition: [7; 32],
        state: [8; 32],
    };
    let activate = |metadata: &mut Metadata, key: &PrivateKey, activation: &Activation| {
        mesh.apply(metadata, key, Operation::Activate(activation.clone()), t)
    };
    assert_eq!(
        activate(&mut metadata, &mesh.executor, &activation),
        Err(CoordinationError::Denied)
    );
    for invalid in [
        Activation {
            state: [0; 32],
            ..activation.clone()
        },
        Activation {
            transition: [0; 32],
            ..activation.clone()
        },
        Activation {
            schema: 3,
            ..activation.clone()
        },
        Activation {
            schema: 1,
            ..activation.clone()
        },
    ] {
        assert_eq!(
            activate(&mut metadata, &mesh.authority, &invalid),
            Err(CoordinationError::Denied)
        );
    }
    let mut stale = activation.clone();
    stale.head.sequence = 0;
    assert_eq!(
        activate(&mut metadata, &mesh.authority, &stale),
        Err(CoordinationError::StaleHead)
    );
    let Ok(Outcome::Head(active)) = activate(&mut metadata, &mesh.authority, &activation) else {
        panic!("activation")
    };
    assert_eq!((active.schema, active.handler), (2, 1));
    assert_eq!(
        (active.sequence, active.head, active.epoch, active.executor),
        (head.sequence, head.head, head.epoch, head.executor)
    );
    // Repeating it is idempotent; a confirmation of the old generation is not.
    assert_eq!(
        activate(&mut metadata, &mesh.authority, &activation),
        Ok(Outcome::Head(active.clone()))
    );
    assert_eq!(
        mesh.confirm(&mut metadata, scope, at(t, 2), t),
        Err(CoordinationError::Generations)
    );
    assert_eq!(mesh.head(&mut metadata, scope, t), active);
}
