//! Tenant registration, isolation and quotas of a public service, driven
//! deterministically at the state machine.
use super::*;
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
    phone: PrivateKey,
}
impl Tenant {
    fn new() -> Self {
        Self {
            key: PrivateKey::new(),
            publisher: PrivateKey::new(),
            executor: PrivateKey::new(),
            alternate: PrivateKey::new(),
            phone: PrivateKey::new(),
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
    fn control(
        &self,
        scope: Scope,
        revision: u64,
        executors: &[&PrivateKey],
    ) -> ControlCertificate {
        self.signed_control(scope, revision, executors, &self.key)
    }
    fn signed_control(
        &self,
        scope: Scope,
        revision: u64,
        executors: &[&PrivateKey],
        signer: &PrivateKey,
    ) -> ControlCertificate {
        ControlCertificate::sign(
            DomainControl {
                scope,
                configuration: [5; 16],
                revision,
                owner: PrincipalId([9; 16]),
                executors: executors.iter().map(|k| k.public_key()).collect(),
                invokers: vec![
                    (self.executor.public_key(), PrincipalId([9; 16])),
                    (self.phone.public_key(), PrincipalId([9; 16])),
                ],
                schema: 1,
                handler: 1,
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
impl Service {
    fn new(admission: bool) -> Self {
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
                    limits: limits(),
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
    fn head(&self, metadata: &mut Metadata, key: &PrivateKey, scope: Scope) -> StrongHead {
        match self.read(metadata, key, Operation::Head { scope }) {
            Ok(Outcome::Head(head)) => head,
            other => panic!("head: {other:?}"),
        }
    }
    fn confirm(
        &self,
        metadata: &mut Metadata,
        tenant: &Tenant,
        executor: &PrivateKey,
        scope: Scope,
        n: u64,
        time: u64,
    ) -> Result<Outcome, CoordinationError> {
        let head = self.head(metadata, executor, scope);
        let mut p = Confirmation {
            caller: PrincipalId([9; 16]),
            scope,
            configuration: head.configuration,
            executor: head.executor,
            epoch: head.epoch,
            request: RequestId {
                node: node_id(tenant.phone.public_key()),
                sequence: AuthorSequence::new((T << ISSUE_SHIFT) + n).unwrap(),
            },
            request_hash: [n as u8; 32],
            expected_sequence: head.sequence,
            expected_head: head.head,
            schema: 1,
            handler: 1,
            result: [3; 32],
            new_head: [0; 32],
        };
        p.new_head = successor(p.expected_head, head.sequence + 1, p.request_hash, p.result);
        self.apply(metadata, executor, Operation::Confirm(p), time)
    }
}

#[test]
fn tenants_register_isolated_domains_without_reaching_each_other() {
    let service = Service::new(false);
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
        service
            .apply(&mut m, &tenant.executor, Operation::Acquire { scope }, T)
            .unwrap();
        service
            .confirm(&mut m, tenant, &tenant.executor, scope, 1, T)
            .unwrap();
    }

    // Nobody acts on another tenant's domain: acquire, head, lookup, control,
    // closure, retirement or registration of its application.
    let lookup = Operation::Lookup {
        scope: sa,
        request: RequestId {
            node: node_id(a.phone.public_key()),
            sequence: AuthorSequence::new((T << ISSUE_SHIFT) + 1).unwrap(),
        },
    };
    let head = service.head(&mut m, &a.executor, sa);
    for (key, operation) in [
        (&b.executor, Operation::Acquire { scope: sa }),
        (&b.phone, Operation::Head { scope: sa }),
        (&b.phone, lookup.clone()),
        (&b.key, Operation::Control(b.control(sa, 2, &[&b.executor]))),
        (
            &b.key,
            Operation::Control(a.signed_control(sa, 2, &[&b.executor], &b.key)),
        ),
        (&b.key, Operation::Close { head: head.clone() }),
        (
            &b.executor,
            Operation::Retire {
                scope: sa,
                through: 1,
            },
        ),
    ] {
        assert_eq!(
            service.apply(&mut m, key, operation.clone(), T),
            Err(CoordinationError::Denied),
            "{operation:?}"
        );
        if matches!(operation, Operation::Head { .. } | Operation::Lookup { .. }) {
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
        service.read(&m, &a.phone, lookup.clone()).unwrap(),
        service.apply(&mut m, &a.phone, lookup, T).unwrap()
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
fn owners_change_executors_with_fencing_and_keep_the_head() {
    let service = Service::new(false);
    let mut m = service.open();
    let a = Tenant::new();
    let scope = a.scope(1);
    service
        .register(&mut m, &a, a.control(scope, 1, &[&a.executor]), T)
        .unwrap();
    service
        .apply(&mut m, &a.executor, Operation::Acquire { scope }, T)
        .unwrap();
    service
        .confirm(&mut m, &a, &a.executor, scope, 1, T)
        .unwrap();
    let before = service.head(&mut m, &a.executor, scope);
    service
        .apply(
            &mut m,
            &a.key,
            Operation::Control(a.control(scope, 2, &[&a.alternate])),
            T,
        )
        .unwrap();
    // The old executor is fenced; the new one acquires the same head.
    assert_eq!(
        service.confirm(&mut m, &a, &a.executor, scope, 2, T),
        Err(CoordinationError::Denied)
    );
    let acquired = match service
        .apply(&mut m, &a.alternate, Operation::Acquire { scope }, T)
        .unwrap()
    {
        Outcome::Head(head) => head,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        (acquired.sequence, acquired.head),
        (before.sequence, before.head)
    );
    assert_eq!(acquired.epoch, before.epoch + 1);
    service
        .confirm(&mut m, &a, &a.alternate, scope, 2, T)
        .unwrap();
    // A revision signed by another key, even an executor, is refused.
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
        let head = service.head(&mut m, &a.executor, open);
        service
            .apply(&mut m, &a.key, Operation::Close { head }, time)
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
    // Takeovers by two executor keys share their tenant's retry budget.
    let mut accepted = 0;
    for n in 0..limits().retry_records * 2 {
        let key = if n % 2 == 0 {
            &a.executor
        } else {
            &a.alternate
        };
        let epoch = service.head(&mut m, key, scope).epoch;
        let operation = if epoch == 0 {
            Operation::Acquire { scope }
        } else {
            Operation::Takeover {
                scope,
                expected_epoch: epoch,
            }
        };
        match service.apply(&mut m, key, operation, T) {
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
    // Registration holds one retry record. The shared device fills A's remainder.
    for epoch in 0..limits().retry_records - 1 {
        let operation = if epoch == 0 {
            Operation::Acquire { scope: sa }
        } else {
            Operation::Takeover {
                scope: sa,
                expected_epoch: epoch as u64,
            }
        };
        let request = Request::sign_at(
            service.genesis.id().unwrap(),
            nonce(),
            operation.clone(),
            &b.executor,
            T,
        )
        .unwrap();
        assert_eq!(m.retry_tenant(&request), Some(a.key.public_key()));
        assert_eq!(m.tenant_lane(&request), Some(a.key.public_key()));
        service.apply(&mut m, &b.executor, operation, T).unwrap();
    }
    assert_eq!(
        service.apply(&mut m, &b.executor, Operation::Acquire { scope: sa }, T),
        Err(CoordinationError::Backpressure)
    );
    let request = Request::sign_at(
        service.genesis.id().unwrap(),
        nonce(),
        Operation::Acquire { scope: sb },
        &b.executor,
        T,
    )
    .unwrap();
    assert_eq!(m.retry_tenant(&request), Some(b.key.public_key()));
    assert_eq!(m.tenant_lane(&request), Some(b.key.public_key()));
    assert!(matches!(
        service.apply(&mut m, &b.executor, Operation::Acquire { scope: sb }, T),
        Ok(Outcome::Head(_))
    ));
    assert!(matches!(
        service.apply(
            &mut m,
            &b.executor,
            Operation::Takeover {
                scope: sb,
                expected_epoch: 1
            },
            T,
        ),
        Ok(Outcome::Head(_))
    ));
}
