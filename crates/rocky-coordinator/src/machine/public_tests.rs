use super::*;
use crate::counters::{CLAIM_WINDOW, Claim, Claimed, Name};
use crate::tenancy::{
    CoordinationGrant, CoordinatorControl, SignedCoordinationGrant, SignedTenantAdmission,
    TenantAdmission,
};
use crate::transport::nonce;
use p2panda_core::PrivateKey;
use rocky_credentials::Publisher;

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
    fn control(&self, scope: Scope, revision: u64, admins: &[&PrivateKey]) -> ControlCertificate {
        self.signed_control(scope, revision, admins, &self.key)
    }
    fn signed_control(
        &self,
        scope: Scope,
        revision: u64,
        admins: &[&PrivateKey],
        signer: &PrivateKey,
    ) -> ControlCertificate {
        ControlCertificate::sign(
            DomainControl {
                scope,
                configuration: [5; 16],
                revision,
                admins: admins.iter().map(|k| k.public_key()).collect(),
                publisher: None,
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
        let request = crate::request::self_certified(
            self.genesis.id().unwrap(),
            nonce(),
            operation,
            key,
            time,
        )
        .unwrap();
        metadata.apply_at(time, &request)
    }
    fn read(
        &self,
        metadata: &Metadata,
        key: &PrivateKey,
        operation: Operation,
    ) -> Result<Outcome, CoordinationError> {
        let request = crate::request::self_certified(
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
    fn claim(&self, metadata: &mut Metadata, key: &PrivateKey, scope: Scope) -> u64 {
        match self.apply(metadata, key, claim(scope), T) {
            Ok(Outcome::Claimed(Claimed::First(value))) => value,
            other => panic!("claim: {other:?}"),
        }
    }
    fn reading(&self, metadata: &Metadata, key: &PrivateKey, scope: Scope) -> u64 {
        match self.read(metadata, key, read(scope)) {
            Ok(Outcome::Counter(value)) => value,
            other => panic!("reading: {other:?}"),
        }
    }
}
fn read(scope: Scope) -> Operation {
    Operation::Counter {
        scope,
        counter: Name::new("invoices").unwrap(),
    }
}
fn claim(scope: Scope) -> Operation {
    Operation::Claim {
        scope,
        counter: Name::new("invoices").unwrap(),
        claim: Claim::unkeyed_until(T + CLAIM_WINDOW),
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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
        service.claim(&mut m, &tenant.executor, scope);
    }

    let log_head = read(sa);
    for (key, operation, refusal) in [
        (&b.executor, claim(sa), Refusal::NotMember),
        (&b.executor, log_head.clone(), Refusal::NotMember),
        (&b.key, log_head.clone(), Refusal::NotMember),
        (
            &b.key,
            Operation::Control(Box::new(b.control(sa, 2, &[&b.executor]))),
            Refusal::NotDomainAuthority,
        ),
        (
            &b.key,
            Operation::Control(Box::new(a.signed_control(sa, 2, &[&b.executor], &b.key))),
            Refusal::NotDomainAuthority,
        ),
        (
            &b.key,
            Operation::Close { scope: sa },
            Refusal::NotDomainAuthority,
        ),
    ] {
        assert_eq!(
            service.apply(&mut m, key, operation.clone(), T),
            Err(CoordinationError::Denied(refusal)),
            "nobody acts on another tenant's domain: {operation:?}"
        );
        if operation == log_head {
            assert_eq!(
                service.read(&m, key, operation),
                Err(CoordinationError::Denied(refusal))
            );
        }
    }
    assert_eq!(
        m.total_used().unwrap(),
        m.tenant_used(a.key.public_key()).unwrap() + m.tenant_used(b.key.public_key()).unwrap(),
        "every accounted byte belongs to a tenant, including its account record"
    );
    assert_eq!(
        service.register(&mut m, &b, b.control(a.scope(3), 1, &[&b.executor]), T),
        Err(CoordinationError::Denied(Refusal::InvalidGrant)),
        "B's grant names B's application, and A's scope belongs to A's"
    );
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
        Err(CoordinationError::Denied(Refusal::AlreadyRegistered)),
        "a changed authority is a migration, not an edit, even under A's publisher"
    );
    assert!(
        matches!(
            service.register(
                &mut m,
                &a,
                a.control(sa, 1, &[&a.executor, &a.alternate]),
                T
            ),
            Ok(Outcome::Controlled(_))
        ),
        "the same registration again is the retained answer"
    );
    assert_eq!(
        service.read(&m, &a.executor, log_head.clone()).unwrap(),
        service.apply(&mut m, &a.executor, log_head, T).unwrap(),
        "linearizable reads answer as the counter would"
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
        Err(CoordinationError::Denied(Refusal::UnknownTenant))
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn owners_change_admin_nodes_and_keep_the_counters() {
    let service = Service::with(false, log_limits());
    let mut m = service.open();
    let a = Tenant::new();
    let scope = a.scope(1);
    service
        .register(&mut m, &a, a.control(scope, 1, &[&a.executor]), T)
        .unwrap();
    let taken = service.claim(&mut m, &a.executor, scope);
    service
        .apply(
            &mut m,
            &a.key,
            Operation::Control(Box::new(a.control(scope, 2, &[&a.alternate]))),
            T,
        )
        .unwrap();
    assert_eq!(service.reading(&m, &a.alternate, scope), taken);
    assert_eq!(
        service.apply(&mut m, &a.executor, claim(scope), T),
        Err(CoordinationError::Denied(Refusal::NotMember)),
        "a node the owner dropped claims nothing"
    );
    assert_eq!(service.claim(&mut m, &a.alternate, scope), taken + 1);
    assert_eq!(
        service.apply(
            &mut m,
            &a.alternate,
            Operation::Control(Box::new(a.signed_control(
                scope,
                3,
                &[&a.alternate],
                &a.alternate
            ))),
            T
        ),
        Err(CoordinationError::Denied(Refusal::NotDomainAuthority)),
        "a revision signed by another key, even an admin node, is refused"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn admission_quotas_and_churn_bound_every_tenant() {
    let service = Service::new(true);
    let mut m = service.open();
    let (a, b, c) = (Tenant::new(), Tenant::new(), Tenant::new());
    let unadmitted = Operation::Register(Box::new(Registration {
        grant: a.grant(),
        admission: None,
        certificate: a.control(a.scope(1), 1, &[&a.executor]),
    }));
    assert_eq!(
        service.apply(&mut m, &a.key, unadmitted, T),
        Err(CoordinationError::Denied(Refusal::NotAdmitted)),
        "without the provider's admission nothing registers"
    );
    let borrowed = Operation::Register(Box::new(Registration {
        grant: a.grant(),
        admission: Some(service.admission(&b)),
        certificate: a.control(a.scope(1), 1, &[&a.executor]),
    }));
    assert_eq!(
        service.apply(&mut m, &a.key, borrowed, T),
        Err(CoordinationError::Denied(Refusal::NotAdmitted))
    );
    assert_eq!(m.retry_records(), 0, "refusals hold no retry records");

    for n in 1..=2 {
        service
            .register(&mut m, &a, a.control(a.scope(n), 1, &[&a.executor]), T)
            .unwrap();
    }
    assert_eq!(
        service.register(&mut m, &a, a.control(a.scope(3), 1, &[&a.executor]), T),
        Err(CoordinationError::Backpressure),
        "a tenant holds at most two open domains"
    );
    service
        .register(&mut m, &b, b.control(b.scope(1), 1, &[&b.executor]), T)
        .expect("another tenant is unaffected by A's domain cap");
    assert_eq!(
        service.register(&mut m, &c, c.control(c.scope(1), 1, &[&c.executor]), T),
        Err(CoordinationError::Backpressure),
        "a third tenant exceeds the tenant cap"
    );

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
    assert_eq!(
        outcome,
        Err(CoordinationError::Backpressure),
        "churn ends when the tenant's closed markers exhaust its own bytes, never the service's"
    );
    assert!(registered > 3 && registered < 40, "{registered}");
    assert!(time > T, "the daily limit applied");
    service
        .register(&mut m, &b, b.control(b.scope(2), 1, &[&b.executor]), time)
        .unwrap();
    let closed = a.scope(1);
    assert_eq!(
        service.register(&mut m, &a, a.control(closed, 1, &[&a.executor]), time),
        Err(CoordinationError::Denied(Refusal::AlreadyRegistered)),
        "a closed identity is never registered again"
    );
    let roundtrip: Metadata = decode(&encode(&m).unwrap()).unwrap();
    assert_eq!(
        encode(&roundtrip).unwrap(),
        encode(&m).unwrap(),
        "the image round-trips canonically with its tenants"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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
    let mut accepted = 0;
    for revision in 2..limits().retry_records as u64 * 2 {
        let control = a.control(scope, revision, &[&a.executor, &a.alternate]);
        match service.apply(&mut m, &a.key, Operation::Control(Box::new(control)), T) {
            Ok(_) => accepted += 1,
            Err(CoordinationError::Backpressure) => break,
            Err(error) => panic!("{error:?}"),
        }
    }
    assert_eq!(
        accepted,
        limits().retry_records - 1,
        "the registration itself holds one retry record"
    );
    assert_eq!(m.retry_records(), limits().retry_records);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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
        Some(CoordinationError::Denied(Refusal::Signature)),
        "a genesis needs authorities or a public service"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_shared_device_key_uses_each_requested_tenants_own_budget() {
    let service = Service::new(false);
    let mut m = service.open();
    let (a, b) = (Tenant::new(), Tenant::new());
    let (sa, sb) = (a.scope(1), b.scope(2));
    let pending = crate::request::self_certified(
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
    for (scope, tenant) in [(sa, &a), (sb, &b)] {
        let request = crate::request::self_certified(
            service.genesis.id().unwrap(),
            nonce(),
            read(scope),
            &b.executor,
            T,
        )
        .unwrap();
        assert_eq!(
            m.retry_tenant(&request),
            Some(tenant.key.public_key()),
            "the shared admin node's calls count against the tenant of the domain it names"
        );
        assert_eq!(m.tenant_lane(&request), Some(tenant.key.public_key()));
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_registered_control_names_the_publisher_of_its_coordination_grant() {
    let service = Service::new(false);
    let mut metadata = service.open();
    let tenant = Tenant::new();
    let scope = tenant.scope(1);
    let named = |publisher: PublicKey| {
        let mut control = tenant.control(scope, 1, &[&tenant.executor]).control;
        control.publisher = Some(Box::new(publisher));
        ControlCertificate::sign(control, &tenant.key).unwrap()
    };
    assert_eq!(
        service.register(
            &mut metadata,
            &tenant,
            named(PrivateKey::new().public_key()),
            T
        ),
        Err(CoordinationError::Denied(Refusal::InvalidGrant))
    );
    assert!(matches!(
        service.register(
            &mut metadata,
            &tenant,
            named(tenant.publisher.public_key()),
            T
        ),
        Ok(Outcome::Controlled(_))
    ));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn limits_at_1_gib_bounds_validate_correctly() {
    let max_total = crate::certified::MAX_STORE / 4 * 3;
    assert_eq!(max_total, 768 * 1024 * 1024);

    let valid_limits = Limits {
        tenant_domains: 16,
        tenant_bytes: 1024 * 1024,
        total_bytes: max_total,
        retry_window: 3600,
        retry_records: 64,
    };
    assert!(valid_limits.validate().is_ok());

    let mut invalid_limits = valid_limits;
    invalid_limits.total_bytes = max_total + 1;
    assert_eq!(
        invalid_limits.validate().err(),
        Some(CoordinationError::Configuration)
    );

    let service = Service::new(false);
    let voters = &service.genesis.genesis.voters;
    let endpoints = service
        .genesis
        .genesis
        .public
        .as_ref()
        .unwrap()
        .endpoints
        .clone();

    let public = PublicService {
        endpoints: endpoints.clone(),
        admission: None,
        limits: valid_limits,
        tenants: 768,
        registrations_per_day: 256,
        tenant_lanes: 4,
    };
    assert!(
        public.validate(voters).is_ok(),
        "768 tenants of 1 MiB and 64 retry records each fit the service"
    );

    let starved = PublicService {
        tenants: 769,
        ..public.clone()
    };
    assert_eq!(
        starved.validate(voters).err(),
        Some(CoordinationError::Configuration),
        "769 tenants of 1 MiB exceed the 768 MiB total"
    );

    let too_many_retries = PublicService {
        limits: Limits {
            retry_records: 128,
            ..valid_limits
        },
        ..public
    };
    assert_eq!(
        too_many_retries.validate(voters).err(),
        Some(CoordinationError::Configuration),
        "768 tenants of 128 retry records each exceed the record budget"
    );
}
