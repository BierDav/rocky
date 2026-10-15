//! Authority time, closure and tenant budgets of root-bound domains.
use super::*;
use crate::coordinated::Action;
use p2panda_core::PrivateKey;

const DAY: u64 = crate::retention::DAY;
struct Mesh {
    genesis: CertifiedGenesis,
    authority: PrivateKey,
    executor: PrivateKey,
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
        let time = 1_900_000_000;
        let scopes: Vec<_> = (1..=domains).map(scope).collect();
        let directory = control(&authority, &executor, scopes[0], 1, time);
        Self {
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
                    directory: Some(directory),
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
            scopes,
            time,
        }
    }
    fn control(&self, scope: Scope, revision: u64) -> ControlCertificate {
        control(&self.authority, &self.executor, scope, revision, self.time)
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
    fn head(&self, metadata: &mut Metadata, scope: Scope, time: u64) -> crate::coordinated::Head {
        let head = Operation::Coordinated {
            scope,
            action: Action::Head,
        };
        match self.apply(metadata, &self.executor, head, time) {
            Ok(Outcome::CoordinatedHead(head)) => *head,
            other => panic!("head: {other:?}"),
        }
    }
    fn join(&self, scope: Scope, sequence: u64) -> Operation {
        Operation::Coordinated {
            scope,
            action: Action::Join {
                position: 0,
                running_hash: genesis(scope, [5; 16]),
                sequence,
            },
        }
    }
    fn open(&self, limits: Limits) -> Metadata {
        Metadata::with_limits(self.genesis.clone(), limits).unwrap()
    }
}
fn control(
    authority: &PrivateKey,
    trusted: &PrivateKey,
    scope: Scope,
    revision: u64,
    time: u64,
) -> ControlCertificate {
    ControlCertificate::sign(
        DomainControl {
            scope,
            configuration: [5; 16],
            revision,
            trusted: vec![trusted.public_key()],
            not_before: time - DAY,
            expires: time + 300 * DAY,
        },
        authority,
    )
    .unwrap()
}
fn nonce() -> Commitment {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    commitment(
        "test.retention-nonce",
        &NEXT.fetch_add(1, Ordering::Relaxed).to_le_bytes(),
    )
}

#[test]
fn authority_time_is_committed_monotone_and_survives_snapshot_and_failover() {
    let mesh = Mesh::new(1);
    let mut metadata = mesh.open(Limits::default());
    let scope = mesh.scopes[0];
    let t = mesh.time;
    mesh.apply(&mut metadata, &mesh.executor, mesh.join(scope, 1), t)
        .unwrap();
    mesh.head(&mut metadata, scope, t + DAY + 10);
    let restored: Metadata = decode(&encode(&metadata).unwrap()).unwrap();
    let mut metadata = restored;
    assert_eq!(metadata.time(), t + DAY + 10);
    // A new leader whose clock is behind cannot rewind authority time, so
    // a request signed before the retry window never applies.
    assert_eq!(
        mesh.apply_signed(
            &mut metadata,
            &mesh.executor,
            mesh.join(scope, 2),
            t,
            t - 3600
        ),
        Err(CoordinationError::Expired)
    );
    assert_eq!(metadata.time(), t + DAY + 10);
    assert_eq!(
        mesh.head(&mut metadata, scope, t + DAY + 10).members.len(),
        1
    );
}

#[test]
fn closed_domains_never_take_entries_or_reopen_and_keep_a_marker() {
    let mesh = Mesh::new(2);
    let mut metadata = mesh.open(Limits::default());
    let scope = mesh.scopes[0];
    let t = mesh.time;
    mesh.apply(&mut metadata, &mesh.executor, mesh.join(scope, 1), t)
        .unwrap();
    let head = mesh.head(&mut metadata, scope, t);
    assert_eq!(
        mesh.apply(&mut metadata, &mesh.executor, Operation::Close { scope }, t),
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        mesh.apply(
            &mut metadata,
            &mesh.authority,
            Operation::Close { scope },
            t
        ),
        Ok(Outcome::Closed)
    );
    let append = Operation::Coordinated {
        scope,
        action: Action::Append {
            expected: 0,
            epoch: head.epoch,
            hash: [7; 32],
            deadline: t + 20,
            sequence: 2,
        },
    };
    assert_eq!(
        mesh.apply(&mut metadata, &mesh.executor, append, t),
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
    // Long after closure the marker and its log head remain.
    assert_eq!(mesh.head(&mut metadata, scope, t + 8 * DAY), head);
    assert_eq!(metadata.retention().domains, 1);
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
    mesh.apply(
        &mut metadata,
        &mesh.authority,
        Operation::Close {
            scope: mesh.scopes[1],
        },
        t,
    )
    .unwrap();
    mesh.apply(
        &mut metadata,
        &mesh.authority,
        Operation::Control(mesh.control(mesh.scopes[2], 1)),
        t,
    )
    .unwrap();

    // A log that would grow past the tenant's bytes is refused, unchanged.
    let used = {
        let probe = mesh.open(Limits::default());
        Limits::default().tenant_bytes - probe.tenant_available(mesh.scopes[0]).unwrap()
    };
    let limits = Limits {
        tenant_bytes: used + 1024,
        ..Limits::default()
    };
    let mut metadata = mesh.open(limits);
    let scope = mesh.scopes[0];
    assert_eq!(
        mesh.apply(&mut metadata, &mesh.executor, mesh.join(scope, 1), t),
        Err(CoordinationError::Backpressure)
    );
    assert!(mesh.head(&mut metadata, scope, t).members.is_empty());
}
