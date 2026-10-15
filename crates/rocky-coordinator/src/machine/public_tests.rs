//! Tenant registration, isolation and quotas of a public service, driven
//! deterministically at the state machine.
use super::*;
use crate::coordinated::Action;
use crate::transport::nonce;
use p2panda_core::PrivateKey;
use rocky_credentials::{
    CoordinationGrant, CoordinatorControl, Publisher, SignedCoordinationGrant,
    SignedTenantAdmission, TenantAdmission,
};

const T: u64 = 1_900_000_000;

struct Tenant {
    key: PrivateKey,
    publisher: PrivateKey,
    executor: PrivateKey,
    alternate: PrivateKey,
}
impl Tenant {
    fn new() -> Self {
        Self {
            key: PrivateKey::new(),
            publisher: PrivateKey::new(),
            executor: PrivateKey::new(),
            alternate: PrivateKey::new(),
        }
    }
    fn app(&self) -> AppId {
        ApplicationManifest::app_id(*self.publisher.public_key().as_bytes(), "notes")
    }
    fn scope(&self, n: u8) -> Scope {
        Scope {
            mesh: MeshId([n; 16]),
            app: self.app(),
            domain: DomainId([n; 16]),
        }
    }
    fn grant(&self) -> SignedCoordinationGrant {
        SignedCoordinationGrant::sign(
            CoordinationGrant {
                publisher: Publisher(self.publisher.public_key()),
                application: "notes".into(),
                tenant: CoordinatorControl(self.key.public_key()),
                not_before: T - DAY,
                expires: T + 400 * DAY,
            },
            &self.publisher,
        )
        .unwrap()
    }
    fn control(&self, scope: Scope, revision: u64, trusted: &[&PrivateKey]) -> ControlCertificate {
        self.signed_control(scope, revision, trusted, &self.key)
    }
    fn signed_control(
        &self,
        scope: Scope,
        revision: u64,
        trusted: &[&PrivateKey],
        signer: &PrivateKey,
    ) -> ControlCertificate {
        ControlCertificate::sign(
            DomainControl {
                scope,
                configuration: [5; 16],
                revision,
                trusted: trusted.iter().map(|k| k.public_key()).collect(),
                not_before: T - DAY,
                expires: T + 300 * DAY,
            },
            signer,
        )
        .unwrap()
    }
}
struct Service {
    genesis: CertifiedGenesis,
    provider: PrivateKey,
}
fn limits() -> Limits {
    Limits {
        tenant_domains: 2,
        tenant_bytes: 12 * 1024,
        total_bytes: 24 * 1024,
        ..Limits::default()
    }
}
/// Budgets with room for joined logs, whose retry records are accounted.
fn log_limits() -> Limits {
    Limits {
        tenant_bytes: 128 * 1024,
        total_bytes: 256 * 1024,
        ..limits()
    }
}
impl Service {
    fn new(admission: bool) -> Self {
        Self::with(admission, limits())
    }
    fn with(admission: bool, limits: Limits) -> Self {
        let root = PrivateKey::new();
        let provider = PrivateKey::new();
        let voters: Vec<_> = (1..=3)
            .map(|id| Voter {
                id,
                address: format!("127.0.0.1:{}", 7000 + id),
                key: PrivateKey::new().public_key(),
            })
            .collect();
        let genesis = CertifiedGenesis::sign(
            Genesis {
                mesh: MeshId([0x51; 16]),
                configuration: [5; 16],
                initial_voters: [1, 2, 3].into(),
                membership_authority: PrivateKey::new().public_key(),
                directory_definition: [0; 32],
                directory: None,
                authorities: Vec::new(),
                max_control_lifetime: 366 * DAY,
                public: Some(PublicService {
                    endpoints: voters
                        .iter()
                        .map(|v| ClientEndpoint {
                            voter: v.id,
                            kind: "iroh".into(),
                            address: format!("voter-{}", v.id),
                        })
                        .collect(),
                    admission: admission.then(|| provider.public_key()),
                    limits,
                    tenants: 2,
                    registrations_per_day: 3,
                    tenant_lanes: 4,
                }),
                voters,
            },
            &root,
        )
        .unwrap();
        Self { genesis, provider }
    }
    fn open(&self) -> Metadata {
        Metadata::with_limits(self.genesis.clone(), self.genesis.limits()).unwrap()
    }
    fn admission(&self, tenant: &Tenant) -> SignedTenantAdmission {
        SignedTenantAdmission::sign(
            TenantAdmission {
                provider: self.provider.public_key(),
                tenant: CoordinatorControl(tenant.key.public_key()),
                not_before: T - DAY,
                expires: T + 400 * DAY,
            },
            &self.provider,
        )
        .unwrap()
    }
    fn apply(
        &self,
        metadata: &mut Metadata,
        key: &PrivateKey,
        operation: Operation,
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        let request =
            Request::sign_at(self.genesis.id().unwrap(), nonce(), operation, key, time).unwrap();
        metadata.apply_at(time, &request)
    }
    fn read(
        &self,
        metadata: &Metadata,
        key: &PrivateKey,
        operation: Operation,
    ) -> Result<Outcome, CoordinationError> {
        let request = Request::sign_at(
            self.genesis.id().unwrap(),
            nonce(),
            operation,
            key,
            metadata.time(),
        )
        .unwrap();
        metadata.read(&request)
    }
    fn register(
        &self,
        metadata: &mut Metadata,
        tenant: &Tenant,
        certificate: ControlCertificate,
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        let admission = self
            .genesis
            .genesis
            .public
            .as_ref()
            .unwrap()
            .admission
            .map(|_| self.admission(tenant));
        self.apply(
            metadata,
            &tenant.key,
            Operation::Register(Box::new(Registration {
                grant: tenant.grant(),
                admission,
                certificate,
            })),
            time,
        )
    }
    fn log(
        &self,
        metadata: &mut Metadata,
        key: &PrivateKey,
        scope: Scope,
        action: Action,
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        self.apply(
            metadata,
            key,
            Operation::Coordinated { scope, action },
            time,
        )
    }
    fn head(
        &self,
        metadata: &Metadata,
        key: &PrivateKey,
        scope: Scope,
    ) -> crate::coordinated::Head {
        let head = Operation::Coordinated {
            scope,
            action: Action::Head,
        };
        match self.read(metadata, key, head) {
            Ok(Outcome::CoordinatedHead(head)) => *head,
            other => panic!("head: {other:?}"),
        }
    }
    fn join(&self, metadata: &mut Metadata, key: &PrivateKey, scope: Scope, sequence: u64) {
        let head = self.head(metadata, key, scope);
        let join = Action::Join {
            position: head.commit,
            running_hash: head.running_hash,
            sequence,
        };
        self.log(metadata, key, scope, join, T).unwrap();
    }
}

#[test]
fn tenants_register_isolated_domains_without_reaching_each_other() {
    let service = Service::with(false, log_limits());
    let mut m = service.open();
    let (a, b) = (Tenant::new(), Tenant::new());
    let (sa, sb) = (a.scope(1), b.scope(2));
    service
        .register(
            &mut m,
            &a,
            a.control(sa, 1, &[&a.executor, &a.alternate]),
            T,
        )
        .unwrap();
    service
        .register(&mut m, &b, b.control(sb, 1, &[&b.executor]), T)
        .unwrap();
    for (tenant, scope) in [(&a, sa), (&b, sb)] {
        service.join(&mut m, &tenant.executor, scope, 1);
    }

    // Nobody acts on another tenant's domain: its log, control, closure or
    // registration of its application.
    let head = service.head(&m, &a.executor, sa);
    let log_head = Operation::Coordinated {
        scope: sa,
        action: Action::Head,
    };
    for (key, operation) in [
        (
            &b.executor,
            Operation::Coordinated {
                scope: sa,
                action: Action::Join {
                    position: head.commit,
                    running_hash: head.running_hash,
                    sequence: 1,
                },
            },
        ),
        (&b.executor, log_head.clone()),
        (&b.key, log_head.clone()),
        (&b.key, Operation::Control(b.control(sa, 2, &[&b.executor]))),
        (
            &b.key,
            Operation::Control(a.signed_control(sa, 2, &[&b.executor], &b.key)),
        ),
        (&b.key, Operation::Close { scope: sa }),
    ] {
        assert_eq!(
            service.apply(&mut m, key, operation.clone(), T),
            Err(CoordinationError::Denied),
            "{operation:?}"
        );
        if operation == log_head {
            assert_eq!(
                service.read(&m, key, operation),
                Err(CoordinationError::Denied)
            );
        }
    }
    // Every accounted byte belongs to a tenant, including its account record.
    assert_eq!(
        m.total_used().unwrap(),
        m.tenant_used(a.key.public_key()).unwrap() + m.tenant_used(b.key.public_key()).unwrap()
    );
    // B's grant names B's application; A's scope belongs to A's.
    assert_eq!(
        service.register(&mut m, &b, b.control(a.scope(3), 1, &[&b.executor]), T),
        Err(CoordinationError::Denied)
    );
    // Even a grant from A's publisher cannot take over a registered scope:
    // a changed authority is a migration, not an edit.
    let squatter = Tenant {
        publisher: a.publisher.clone(),
        ..Tenant::new()
    };
    assert_eq!(
        service.register(
            &mut m,
            &squatter,
            squatter.control(sa, 1, &[&squatter.executor]),
            T
        ),
        Err(CoordinationError::Denied)
    );
    // The same registration again is the retained answer.
    assert!(matches!(
        service.register(
            &mut m,
            &a,
            a.control(sa, 1, &[&a.executor, &a.alternate]),
            T
        ),
        Ok(Outcome::Controlled(_))
    ));
    // Linearizable reads answer as the log would.
    assert_eq!(
        service.read(&m, &a.executor, log_head.clone()).unwrap(),
        service.apply(&mut m, &a.executor, log_head, T).unwrap()
    );
    assert_eq!(
        service.read(&m, &a.key, Operation::Usage).unwrap(),
        Outcome::Usage(Usage {
            open_domains: 1,
            closed_domains: 0,
            bytes: m.tenant_used(a.key.public_key()).unwrap(),
            registrations_today: 1,
        })
    );
    assert_eq!(
        service.read(&m, &a.executor, Operation::Usage),
        Err(CoordinationError::Denied)
    );
}

#[test]
fn owners_change_trusted_nodes_and_keep_the_log() {
    let service = Service::with(false, log_limits());
    let mut m = service.open();
    let a = Tenant::new();
    let scope = a.scope(1);
    service
        .register(&mut m, &a, a.control(scope, 1, &[&a.executor]), T)
        .unwrap();
    service.join(&mut m, &a.executor, scope, 1);
    let before = service.head(&m, &a.executor, scope);
    // A node of the in-sync set cannot be dropped; one can be added.
    assert_eq!(
        service.apply(
            &mut m,
            &a.key,
            Operation::Control(a.control(scope, 2, &[&a.alternate])),
            T,
        ),
        Err(CoordinationError::Backpressure)
    );
    service
        .apply(
            &mut m,
            &a.key,
            Operation::Control(a.control(scope, 2, &[&a.executor, &a.alternate])),
            T,
        )
        .unwrap();
    service.join(&mut m, &a.alternate, scope, 1);
    let after = service.head(&m, &a.alternate, scope);
    assert_eq!(
        (after.commit, after.running_hash),
        (before.commit, before.running_hash)
    );
    assert_eq!(after.members.len(), 2);
    // A revision signed by another key, even a trusted node, is refused.
    assert_eq!(
        service.apply(
            &mut m,
            &a.alternate,
            Operation::Control(a.signed_control(scope, 3, &[&a.alternate], &a.alternate)),
            T
        ),
        Err(CoordinationError::Denied)
    );
}

#[test]
fn admission_quotas_and_churn_bound_every_tenant() {
    let service = Service::new(true);
    let mut m = service.open();
    let (a, b, c) = (Tenant::new(), Tenant::new(), Tenant::new());
    // Without the provider's admission, or with another tenant's, nothing registers.
    let unadmitted = Operation::Register(Box::new(Registration {
        grant: a.grant(),
        admission: None,
        certificate: a.control(a.scope(1), 1, &[&a.executor]),
    }));
    assert_eq!(
        service.apply(&mut m, &a.key, unadmitted, T),
        Err(CoordinationError::Denied)
    );
    let borrowed = Operation::Register(Box::new(Registration {
        grant: a.grant(),
        admission: Some(service.admission(&b)),
        certificate: a.control(a.scope(1), 1, &[&a.executor]),
    }));
    assert_eq!(
        service.apply(&mut m, &a.key, borrowed, T),
        Err(CoordinationError::Denied)
    );
    // Refusals hold no retry records.
    assert_eq!(m.retry_records(), 0);

    // Two open domains per tenant.
    for n in 1..=2 {
        service
            .register(&mut m, &a, a.control(a.scope(n), 1, &[&a.executor]), T)
            .unwrap();
    }
    assert_eq!(
        service.register(&mut m, &a, a.control(a.scope(3), 1, &[&a.executor]), T),
        Err(CoordinationError::Backpressure)
    );
    // Another tenant is unaffected; a third exceeds the tenant cap.
    service
        .register(&mut m, &b, b.control(b.scope(1), 1, &[&b.executor]), T)
        .unwrap();
    assert_eq!(
        service.register(&mut m, &c, c.control(c.scope(1), 1, &[&c.executor]), T),
        Err(CoordinationError::Backpressure)
    );

    // Churn: closing frees a domain slot, but registrations per day and the
    // tenant's bytes stay bounded, and closed identities never return.
    let mut time = T;
    let mut registered = 2;
    let mut outcome = Ok(());
    for n in 3..=40u8 {
        let open = a.scope(n - 2);
        service
            .apply(&mut m, &a.key, Operation::Close { scope: open }, time)
            .unwrap();
        outcome = match service.register(&mut m, &a, a.control(a.scope(n), 1, &[&a.executor]), time)
        {
            Ok(_) => {
                registered += 1;
                Ok(())
            }
            Err(CoordinationError::Backpressure) => {
                // A full day may still admit a registration tomorrow.
                time += DAY;
                match service.register(&mut m, &a, a.control(a.scope(n), 1, &[&a.executor]), time) {
                    Ok(_) => {
                        registered += 1;
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        assert!(m.tenant_used(a.key.public_key()).unwrap() <= limits().tenant_bytes);
        assert!(m.total_used().unwrap() <= limits().total_bytes);
        if outcome.is_err() {
            break;
        }
    }
    // The tenant's closed markers exhaust its own bytes, never the service's.
    assert_eq!(outcome, Err(CoordinationError::Backpressure));
    assert!(registered > 3 && registered < 40, "{registered}");
    assert!(time > T, "the daily limit applied");
    service
        .register(&mut m, &b, b.control(b.scope(2), 1, &[&b.executor]), time)
        .unwrap();
    let closed = a.scope(1);
    assert_eq!(
        service.register(&mut m, &a, a.control(closed, 1, &[&a.executor]), time),
        Err(CoordinationError::Denied),
        "a closed identity is never registered again"
    );
    // The image round-trips canonically with its tenants.
    let roundtrip: Metadata = decode(&encode(&m).unwrap()).unwrap();
    assert_eq!(encode(&roundtrip).unwrap(), encode(&m).unwrap());
}

#[test]
fn retained_outcomes_are_bounded_per_tenant() {
    let service = Service::new(false);
    let mut m = service.open();
    let a = Tenant::new();
    let scope = a.scope(1);
    service
        .register(
            &mut m,
            &a,
            a.control(scope, 1, &[&a.executor, &a.alternate]),
            T,
        )
        .unwrap();
    // Control revisions keep their outcome within the tenant's retry budget.
    let mut accepted = 0;
    for revision in 2..limits().retry_records as u64 * 2 {
        let control = a.control(scope, revision, &[&a.executor, &a.alternate]);
        match service.apply(&mut m, &a.key, Operation::Control(control), T) {
            Ok(_) => accepted += 1,
            Err(CoordinationError::Backpressure) => break,
            Err(error) => panic!("{error:?}"),
        }
    }
    // The registration itself holds one record.
    assert_eq!(accepted, limits().retry_records - 1);
    assert_eq!(m.retry_records(), limits().retry_records);
}

#[test]
fn a_public_genesis_needs_disjoint_budgets() {
    let service = Service::new(false);
    let mut genesis = service.genesis.genesis.clone();
    let public = genesis.public.as_mut().unwrap();
    public.tenants = 3;
    let root = PrivateKey::new();
    assert_eq!(
        CertifiedGenesis::sign(genesis.clone(), &root).err(),
        Some(CoordinationError::Configuration)
    );
    genesis.public = None;
    assert_eq!(
        CertifiedGenesis::sign(genesis, &root).err(),
        Some(CoordinationError::Denied),
        "a mesh genesis needs its Directory and authorities"
    );
}

#[test]
fn a_shared_device_key_uses_each_requested_tenants_own_budget() {
    let service = Service::new(false);
    let mut m = service.open();
    let (a, b) = (Tenant::new(), Tenant::new());
    let (sa, sb) = (a.scope(1), b.scope(2));
    let pending = Request::sign_at(
        service.genesis.id().unwrap(),
        nonce(),
        Operation::Register(Box::new(Registration {
            grant: a.grant(),
            admission: None,
            certificate: a.control(sa, 1, &[&b.executor]),
        })),
        &a.key,
        T,
    )
    .unwrap();
    assert_eq!(m.tenant_lane(&pending), None);
    for (tenant, scope) in [(&a, sa), (&b, sb)] {
        service
            .register(&mut m, tenant, tenant.control(scope, 1, &[&b.executor]), T)
            .unwrap();
    }
    assert_eq!(m.tenant_lane(&pending), Some(a.key.public_key()));
    // The shared trusted node's calls count against the tenant of the domain
    // it names.
    for (scope, tenant) in [(sa, &a), (sb, &b)] {
        let request = Request::sign_at(
            service.genesis.id().unwrap(),
            nonce(),
            Operation::Coordinated {
                scope,
                action: Action::Head,
            },
            &b.executor,
            T,
        )
        .unwrap();
        assert_eq!(m.retry_tenant(&request), Some(tenant.key.public_key()));
        assert_eq!(m.tenant_lane(&request), Some(tenant.key.public_key()));
    }
}
