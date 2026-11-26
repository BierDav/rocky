use super::*;
use crate::counters::{CLAIM_WINDOW, Claim, Name};
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
        crate::request::self_certified(self.genesis.id().unwrap(), nonce(), operation, key, time)
            .unwrap()
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
    fn reading(&self, metadata: &mut Metadata, scope: Scope, time: u64) -> i64 {
        let read = Operation::Counter {
            scope,
            counter: Name::new("invoices").unwrap(),
        };
        match self.apply(metadata, &self.executor, read, time) {
            Ok(Outcome::Counter(value)) => value,
            other => panic!("reading: {other:?}"),
        }
    }
    fn claim(&self, scope: Scope, issued: u64) -> Operation {
        Operation::claim(
            scope,
            Name::new("invoices").unwrap(),
            Claim::unique_until(issued + CLAIM_WINDOW),
        )
    }
    fn open(&self, limits: Limits) -> Metadata {
        let mut metadata = Metadata::with_limits(self.genesis.clone(), limits).unwrap();
        metadata.control(self.control(self.scopes[0], 1)).unwrap();
        metadata
    }
}
fn control(
    authority: &PrivateKey,
    admin: &PrivateKey,
    scope: Scope,
    revision: u64,
    time: u64,
) -> ControlCertificate {
    ControlCertificate::sign(
        DomainControl {
            scope,
            configuration: [5; 16],
            revision,
            admins: vec![admin.public_key()],
            publisher: None,
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn authority_time_is_committed_monotone_and_survives_snapshot_and_failover() {
    let mesh = Mesh::new(1);
    let mut metadata = mesh.open(Limits::default());
    let scope = mesh.scopes[0];
    let t = mesh.time;
    mesh.apply(&mut metadata, &mesh.executor, mesh.claim(scope, t), t)
        .unwrap();
    mesh.reading(&mut metadata, scope, t + DAY + 10);
    let restored: Metadata = decode(&encode(&metadata).unwrap()).unwrap();
    let mut metadata = restored;
    assert_eq!(metadata.time(), t + DAY + 10);
    assert_eq!(
        mesh.apply_signed(
            &mut metadata,
            &mesh.executor,
            mesh.claim(scope, t),
            t,
            t - 3600
        ),
        Err(CoordinationError::Expired),
        "a new leader whose clock is behind cannot rewind authority time"
    );
    assert_eq!(metadata.time(), t + DAY + 10);
    assert_eq!(mesh.reading(&mut metadata, scope, t + DAY + 10), 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn closed_domains_never_take_entries_or_reopen_and_keep_a_marker() {
    let mesh = Mesh::new(2);
    let mut metadata = mesh.open(Limits::default());
    let scope = mesh.scopes[0];
    let t = mesh.time;
    mesh.apply(&mut metadata, &mesh.executor, mesh.claim(scope, t), t)
        .unwrap();
    let reading = mesh.reading(&mut metadata, scope, t);
    assert_eq!(
        mesh.apply(&mut metadata, &mesh.executor, Operation::Close { scope }, t),
        Err(CoordinationError::Denied(Refusal::NotDomainAuthority))
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
    assert_eq!(
        mesh.apply(&mut metadata, &mesh.executor, mesh.claim(scope, t), t),
        Err(CoordinationError::Denied(Refusal::ClosedDomain))
    );
    for revision in [1, 2] {
        assert!(
            mesh.apply(
                &mut metadata,
                &mesh.authority,
                Operation::Control(Box::new(mesh.control(scope, revision))),
                t
            )
            .is_err()
        );
    }
    assert_eq!(
        mesh.reading(&mut metadata, scope, t + 8 * DAY),
        reading,
        "long after closure the marker and its counters remain"
    );
    assert_eq!(metadata.retention().domains, 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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
        Operation::Control(Box::new(mesh.control(mesh.scopes[1], 1))),
        t,
    )
    .unwrap();
    assert_eq!(
        mesh.apply(
            &mut metadata,
            &mesh.authority,
            Operation::Control(Box::new(mesh.control(mesh.scopes[2], 1))),
            t
        ),
        Err(CoordinationError::Backpressure)
    );
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
        Operation::Control(Box::new(mesh.control(mesh.scopes[2], 1))),
        t,
    )
    .expect("closing a domain frees its slot");

    let used = {
        let probe = mesh.open(Limits::default());
        Limits::default().tenant_bytes - probe.tenant_available(mesh.scopes[0]).unwrap()
    };
    let limits = Limits {
        tenant_bytes: used + 8,
        ..Limits::default()
    };
    let mut metadata = mesh.open(limits);
    let scope = mesh.scopes[0];
    assert_eq!(
        mesh.apply(&mut metadata, &mesh.executor, mesh.claim(scope, t), t),
        Err(CoordinationError::Backpressure),
        "a counter that would grow past the tenant's bytes is refused"
    );
    assert_eq!(mesh.reading(&mut metadata, scope, t), 0);
}
