use super::*;
use crate::{
    certified::SkewedClock,
    counters::{CLAIM_WINDOW, Claim, Lookup, Name},
    machine::{Metadata, self_certificate_in},
    retention::DAY,
};
use rocky_core::net::{BoxFuture, Failure, Stream};
use rocky_credentials::{CoordinationGrant, CoordinatorControl, Publisher};
use std::sync::atomic::{AtomicUsize, Ordering};

const KIND: &str = "memory";

struct Memory {
    genesis: Commitment,
    key: PrivateKey,
    state: Arc<Mutex<Metadata>>,
    streams: AtomicUsize,
    clock_ahead_seconds: std::sync::atomic::AtomicU64,
}
impl Memory {
    fn answer(&self, request: &Request) -> Result<Outcome, CoordinationError> {
        request.authenticate(self.genesis)?;
        let clock_ahead_seconds = self.clock_ahead_seconds.load(Ordering::Relaxed);
        if clock_ahead_seconds > 0 {
            return Err(CoordinationError::ClockSkew {
                coordinator_time: now()? + clock_ahead_seconds,
            });
        }
        request.fresh()?;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.preflight(request)?;
        if request.read_only() {
            state.read(request)
        } else {
            state.apply_at(now()?, request)
        }
    }
    async fn serve(self: Arc<Self>, mut stream: tokio::io::DuplexStream) {
        let served = async {
            let envelope = read(&mut stream).await?;
            envelope.authenticate(self.genesis)?;
            let Message::Client(request) = decode(&envelope.body)? else {
                return Err(CoordinationError::Denied);
            };
            let reply = Reply {
                leader: None,
                result: self.answer(&request),
            };
            let response = crate::transport::Envelope::from_body(
                self.genesis,
                envelope.nonce,
                encode(&reply)?,
                &self.key,
            )?;
            write(&mut stream, &response).await
        };
        let _ = served.await;
    }
}
struct Lane(Arc<Memory>, &'static str);
impl Transport for Lane {
    fn request<'a>(
        &'a self,
        _: &'a [String],
        _: Scope,
        _: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>> {
        Box::pin(async { Err("the test voter serves coordination only".into()) })
    }
    fn session<'a>(&'a self, _: &'a [String], _: Scope) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async { Err("the test voter serves coordination only".into()) })
    }
    fn kind(&self) -> &str {
        self.1
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async { Err("a client lane does not accept".into()) })
    }
    fn connect<'a>(
        &'a self,
        _: &'a str,
        protocol: Protocol,
        _scope: Option<rocky_core::Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move {
            if protocol != Protocol::Coordinate {
                return Err("the test voter serves coordination only".into());
            }
            self.0.streams.fetch_add(1, Ordering::Relaxed);
            let (client, voter) = tokio::io::duplex(2 * MAX_FRAME);
            drop(rocky_platform::task::spawn(self.0.clone().serve(voter)));
            Ok(Stream::new(client))
        })
    }
    fn accept(&self) -> BoxFuture<'_, Option<rocky_core::net::Incoming>> {
        Box::pin(async { None })
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

fn genesis(key: &PrivateKey) -> CertifiedGenesis {
    CertifiedGenesis::sign(
        Genesis {
            mesh: MeshId([0x71; 16]),
            configuration: [7; 16],
            voters: (1..=3)
                .map(|id| Voter {
                    id,
                    address: format!("127.0.0.1:{id}"),
                    key: if id == 1 {
                        key.public_key()
                    } else {
                        PrivateKey::new().public_key()
                    },
                })
                .collect(),
            initial_voters: [1, 2, 3].into(),
            membership_authority: PrivateKey::new().public_key(),
            directory_definition: NO_DIRECTORY_DEFINITION,
            directory: None,
            authorities: Vec::new(),
            max_control_lifetime: DAY,
            public: Some(PublicService {
                endpoints: vec![ClientEndpoint {
                    voter: 1,
                    kind: KIND.into(),
                    address: "voter-1".into(),
                }],
                admission: None,
                limits: crate::Limits::default(),
                tenants: 2,
                registrations_per_day: 4,
                tenant_lanes: 4,
            }),
        },
        &PrivateKey::new(),
    )
    .unwrap()
}

struct Service {
    bundle: TrustBundle,
    voter: Arc<Memory>,
    scope: Scope,
    publisher: PrivateKey,
    node: PrivateKey,
    owner: NetworkCoordinator,
    tenant: PrivateKey,
    control: Mutex<DomainControl>,
}
impl Service {
    async fn start() -> Self {
        Self::answered_by(None).await
    }
    async fn answered_by(impostor: Option<PrivateKey>) -> Self {
        let voter = PrivateKey::new();
        let genesis = genesis(&voter);
        let memory = Arc::new(Memory {
            genesis: genesis.id().unwrap(),
            key: voter,
            state: Arc::new(Mutex::new(
                Metadata::with_limits(genesis.clone(), genesis.limits()).unwrap(),
            )),
            streams: AtomicUsize::new(0),
            clock_ahead_seconds: Default::default(),
        });
        let publisher = PrivateKey::new();
        let tenant = PrivateKey::new();
        let node = PrivateKey::new();
        let scope = Scope {
            mesh: MeshId([0x72; 16]),
            app: ApplicationManifest::app_id(*publisher.public_key().as_bytes(), "invoices"),
            domain: DomainId([0x73; 16]),
        };
        let bundle = TrustBundle::new(&genesis).unwrap();
        let time = now().unwrap();
        let grant = SignedCoordinationGrant::sign(
            CoordinationGrant {
                publisher: Publisher(publisher.public_key()),
                application: "invoices".into(),
                tenant: CoordinatorControl(tenant.public_key()),
                not_before: time - 60,
                expires: time + 3600,
            },
            &publisher,
        )
        .unwrap();
        let control = DomainControl {
            scope,
            configuration: genesis.genesis.configuration,
            revision: 1,
            trusted: vec![node.public_key()],
            publisher: Some(Box::new(publisher.public_key())),
            not_before: time - 60,
            expires: time + 3600,
        };
        let owner = NetworkCoordinator::connect(
            &bundle,
            tenant.clone(),
            Arc::new(Lane(memory.clone(), KIND)),
        )
        .unwrap();
        owner
            .register(
                grant,
                None,
                ControlCertificate::sign(control.clone(), &tenant).unwrap(),
                [1; 32],
            )
            .await
            .unwrap();
        let memory = match impostor {
            None => memory,
            Some(key) => Arc::new(Memory {
                genesis: memory.genesis,
                key,
                state: memory.state.clone(),
                streams: AtomicUsize::new(0),
                clock_ahead_seconds: Default::default(),
            }),
        };
        Self {
            bundle,
            voter: memory,
            scope,
            publisher,
            node,
            owner,
            tenant,
            control: Mutex::new(control),
        }
    }
    async fn pass(&self, clock: &SkewedClock, seconds: u64) {
        clock.pass(seconds);
        let time = now().unwrap();
        let renewed = {
            let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
            control.revision += 1;
            control.not_before = time - 60;
            control.expires = time + 3600;
            ControlCertificate::sign(control.clone(), &self.tenant).unwrap()
        };
        self.owner.control(renewed, nonce()).await.unwrap();
    }
    fn database(&self) -> rocky_credentials::DatabaseContext {
        rocky_credentials::DatabaseContext {
            publisher: self.publisher.public_key(),
            scope: self.scope,
        }
    }
    fn client(&self, key: &PrivateKey) -> NetworkCoordinator {
        let client = NetworkCoordinator::connect(
            &self.bundle,
            key.clone(),
            Arc::new(Lane(self.voter.clone(), KIND)),
        )
        .unwrap();
        client.certify(self_certificate_in(
            key,
            rocky_credentials::DatabaseContext {
                publisher: self.publisher.public_key(),
                scope: self.scope,
            },
        ));
        client
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_trusted_node_claims_and_a_retry_returns_the_same_value() {
    let service = Service::start().await;
    let client = service.client(&service.node);
    let invoices = Name::new("company-1/invoices").unwrap();
    let first = Claim::fresh().unwrap();
    assert_eq!(
        client.claim(service.scope, invoices.clone(), first).await,
        Ok(1)
    );
    assert_eq!(
        client.claim(service.scope, invoices.clone(), first).await,
        Ok(1)
    );
    let second = Claim::fresh().unwrap();
    assert_eq!(
        client.claim(service.scope, invoices.clone(), second).await,
        Ok(2)
    );
    let reopened = service.client(&service.node);
    let reading = reopened
        .counter(service.scope, invoices.clone(), Some(first))
        .await
        .unwrap();
    assert_eq!(
        (reading.value, reading.claim),
        (2, Some(Lookup::Claimed(1))),
        "a client reopened on the same node sees the same counter"
    );
    assert_eq!(reopened.claim(service.scope, invoices, second).await, Ok(2));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn the_network_coordinator_meets_the_claim_contract() {
    let clock = SkewedClock::new();
    let service = Service::start().await;
    rocky_credentials::testing::claim_contract(
        &service.client(&service.node),
        service.scope,
        async |seconds| service.pass(&clock, seconds).await,
    )
    .await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn the_network_coordinator_meets_the_coordinator_contract() {
    let service = Service::start().await;
    rocky_credentials::testing::coordinator_contract(
        &service.client(&service.node),
        &service.node,
        service.database(),
        rocky_credentials::testing::CoordinatorBehaviour {
            observes_calls: true,
            denies_untrusted_claims: true,
        },
    )
    .await;
}

const MEMORY_LOG_SCOPE: Scope = Scope {
    mesh: MeshId([1; 16]),
    app: AppId([2; 16]),
    domain: DomainId([3; 16]),
};

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn the_memory_log_meets_the_claim_contract() {
    let log = crate::coordinated::MemoryLog::new();
    rocky_credentials::testing::claim_contract(
        &log.client(PrivateKey::new().public_key()),
        MEMORY_LOG_SCOPE,
        async |seconds| log.advance(seconds),
    )
    .await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn the_memory_log_meets_the_coordinator_contract() {
    let trusted = PrivateKey::new();
    rocky_credentials::testing::coordinator_contract(
        &crate::coordinated::MemoryLog::new().client(trusted.public_key()),
        &trusted,
        rocky_credentials::DatabaseContext {
            publisher: PrivateKey::new().public_key(),
            scope: MEMORY_LOG_SCOPE,
        },
        rocky_credentials::testing::CoordinatorBehaviour {
            observes_calls: false,
            denies_untrusted_claims: false,
        },
    )
    .await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_claim_beyond_its_window_is_expired_and_takes_no_value() {
    let service = Service::start().await;
    let client = service.client(&service.node);
    let invoices = Name::new("invoices").unwrap();
    let stale = Claim {
        id: [5; 32],
        issued: now().unwrap() - CLAIM_WINDOW - 60,
    };
    assert_eq!(
        client.claim(service.scope, invoices.clone(), stale).await,
        Err(CoordinationError::Expired)
    );
    let reading = client.counter(service.scope, invoices, Some(stale)).await;
    assert_eq!(
        reading.map(|reading| (reading.value, reading.claim)),
        Ok((0, Some(Lookup::Expired)))
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn only_trusted_nodes_claim() {
    let service = Service::start().await;
    let invoices = Name::new("invoices").unwrap();
    let stranger = service.client(&PrivateKey::new());
    assert_eq!(
        stranger
            .claim(service.scope, invoices.clone(), Claim::fresh().unwrap())
            .await,
        Err(CoordinationError::Denied)
    );
    let uncertified = NetworkCoordinator::connect(
        &service.bundle,
        service.node.clone(),
        Arc::new(Lane(service.voter.clone(), KIND)),
    )
    .unwrap();
    assert_eq!(
        uncertified
            .claim(service.scope, invoices.clone(), Claim::fresh().unwrap())
            .await,
        Err(CoordinationError::Denied),
        "a trusted node's key without its certificate is no identity"
    );
    let trusted = service.client(&service.node);
    let reading = trusted
        .counter(service.scope, invoices, None)
        .await
        .unwrap();
    assert_eq!(reading.value, 0);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_claim_right_holder_claims_until_its_revocation_is_pushed() {
    use rocky_credentials::{
        Authority, CLAIM_LIFETIME, ClaimRight, CredentialChain, Revocation, RevocationSet,
        SignedRevocation,
    };
    let service = Service::start().await;
    let (identity, node) = (PrivateKey::new(), PrivateKey::new());
    let time = now().unwrap();
    let link = Authority::owner(&service.publisher)
        .issue_claims(
            identity.public_key(),
            ClaimRight {
                scope: service.scope,
                counters: Some([b"invoices".to_vec()].into()),
                grant: [7; 32],
            },
            1,
            (time, CLAIM_LIFETIME),
        )
        .unwrap();
    let chain = CredentialChain(vec![link])
        .certify_claims(&identity, node.public_key())
        .unwrap();
    let holder = service.client(&node);
    let invoices = Name::new("invoices").unwrap();
    let first = Claim::fresh().unwrap();
    assert_eq!(
        holder.claim(service.scope, invoices.clone(), first).await,
        Err(CoordinationError::Denied)
    );
    holder.present_claims(Some(chain));
    assert_eq!(
        holder.claim(service.scope, invoices.clone(), first).await,
        Ok(1)
    );
    assert_eq!(
        holder.claim(service.scope, invoices.clone(), first).await,
        Ok(1)
    );
    assert_eq!(
        holder
            .claim(
                service.scope,
                Name::new("credits").unwrap(),
                Claim::fresh().unwrap()
            )
            .await,
        Err(CoordinationError::Denied)
    );
    let trusted = service.client(&service.node);
    assert_eq!(
        trusted
            .claim(service.scope, invoices.clone(), Claim::fresh().unwrap())
            .await,
        Ok(2)
    );

    let statement = SignedRevocation::sign_in(
        Revocation {
            app: service.scope.app,
            issuer: service.publisher.public_key(),
            time,
            nodes: Default::default(),
            identities: [identity.public_key()].into(),
            capabilities: Default::default(),
            cut: Default::default(),
            mode: rocky_credentials::RevocationMode::Lost,
            recovery: None,
        },
        &service.publisher,
        &CredentialChain::default(),
        rocky_credentials::DatabaseContext {
            publisher: service.publisher.public_key(),
            scope: service.scope,
        },
    )
    .unwrap();
    holder
        .revoke(
            service.scope,
            RevocationSet::try_from(vec![statement]).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        holder.claim(service.scope, invoices.clone(), first).await,
        Err(CoordinationError::Denied),
        "a pushed revocation denies its holder from then on"
    );
    assert_eq!(
        holder.counter(service.scope, invoices, None).await,
        Err(CoordinationError::Denied)
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_answer_not_signed_by_the_certified_voter_is_refused() {
    let service = Service::answered_by(Some(PrivateKey::new())).await;
    let client = service.client(&service.node);
    let invoices = Name::new("invoices").unwrap();
    assert_eq!(
        client
            .claim(service.scope, invoices, Claim::fresh().unwrap())
            .await,
        Err(CoordinationError::Unavailable),
        "every attempt is refused, so the outcome stays uncertain"
    );
    assert!(service.voter.streams.load(Ordering::Relaxed) > 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_observer_sees_every_answer_and_refusal() {
    let service = Service::start().await;
    let client = service.client(&service.node);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    client.observe(Arc::new(move |result| {
        record
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(result.err().cloned());
    }));
    let invoices = Name::new("invoices").unwrap();
    assert_eq!(
        client
            .claim(service.scope, invoices.clone(), Claim::fresh().unwrap())
            .await,
        Ok(1)
    );
    service
        .voter
        .clock_ahead_seconds
        .store(3600, Ordering::Relaxed);
    assert!(matches!(
        client
            .claim(service.scope, invoices, Claim::fresh().unwrap())
            .await,
        Err(CoordinationError::ClockSkew { .. })
    ));
    let seen = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], None);
    assert!(
        matches!(seen[1], Some(CoordinationError::ClockSkew { coordinator_time }) if coordinator_time >= now().unwrap() + 3599)
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_client_needs_an_endpoint_of_its_transport_kind() {
    let genesis = genesis(&PrivateKey::new());
    let memory = Arc::new(Memory {
        genesis: genesis.id().unwrap(),
        key: PrivateKey::new(),
        state: Arc::new(Mutex::new(Metadata::new(genesis.clone()).unwrap())),
        streams: AtomicUsize::new(0),
        clock_ahead_seconds: Default::default(),
    });
    let bundle = TrustBundle::new(&genesis).unwrap();
    assert!(matches!(
        NetworkCoordinator::connect(&bundle, PrivateKey::new(), Arc::new(Lane(memory, "other"))),
        Err(CoordinationError::Configuration)
    ));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_memory_log_binds_only_the_key_its_certificate_names() {
    let client = crate::coordinated::MemoryLog::new().client(PrivateKey::new().public_key());
    let key = PrivateKey::new();
    let certificate = self_certificate_in(
        &key,
        rocky_credentials::DatabaseContext {
            publisher: PrivateKey::new().public_key(),
            scope: Scope {
                mesh: MeshId([1; 16]),
                app: AppId([2; 16]),
                domain: DomainId([3; 16]),
            },
        },
    );
    assert!(matches!(
        rocky_credentials::Coordinator::bind(&client, PrivateKey::new(), certificate.clone()),
        Err(CoordinationError::Denied)
    ));
    assert_eq!(
        rocky_credentials::Coordinator::bind(&client, key.clone(), certificate)
            .unwrap()
            .node(),
        *key.public_key().as_bytes()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn application_client_bindings_keep_node_authority_and_observers_independent() {
    let service = Service::start().await;
    let client =
        crate::Client::connect(&service.bundle, Arc::new(Lane(service.voter.clone(), KIND)))
            .unwrap();
    let bind = |key: &PrivateKey| {
        client
            .bind(
                key.clone(),
                self_certificate_in(
                    key,
                    rocky_credentials::DatabaseContext {
                        publisher: service.publisher.public_key(),
                        scope: service.scope,
                    },
                ),
            )
            .unwrap()
    };
    let trusted = bind(&service.node);
    let stranger = bind(&PrivateKey::new());
    assert_eq!(trusted.node(), *service.node.public_key().as_bytes());
    assert_ne!(trusted.node(), stranger.node());
    let trusted_calls = Arc::new(AtomicUsize::new(0));
    let stranger_calls = Arc::new(AtomicUsize::new(0));
    for (client, calls) in [
        (&trusted, trusted_calls.clone()),
        (&stranger, stranger_calls.clone()),
    ] {
        client.observe(Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
        }));
    }
    let counter = Name::new("invoices").unwrap();
    let claim = Claim::fresh().unwrap();
    assert_eq!(
        trusted.claim(service.scope, counter.clone(), claim).await,
        Ok(1)
    );
    assert_eq!(
        stranger.claim(service.scope, counter.clone(), claim).await,
        Err(CoordinationError::Denied)
    );
    assert_eq!(trusted.claim(service.scope, counter, claim).await, Ok(1));
    assert_eq!(trusted_calls.load(Ordering::SeqCst), 2);
    assert_eq!(stranger_calls.load(Ordering::SeqCst), 1);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_connector_reaches_only_the_coordinator_its_bundle_certifies() {
    use rocky_credentials::{Coordinator, CoordinatorConnector};
    let certified = genesis(&PrivateKey::new());
    let memory = Arc::new(Memory {
        genesis: certified.id().unwrap(),
        key: PrivateKey::new(),
        state: Arc::new(Mutex::new(Metadata::new(certified.clone()).unwrap())),
        streams: AtomicUsize::new(0),
        clock_ahead_seconds: Default::default(),
    });
    let lane = Arc::new(Lane(memory, KIND));
    let named = NetworkCoordinator::connect(
        &TrustBundle::new(&certified).unwrap(),
        PrivateKey::new(),
        lane.clone(),
    )
    .unwrap()
    .bundle()
    .unwrap();
    let connector = crate::Connector::new(lane);
    let reached = connector.connect(&named).unwrap();
    assert_eq!(reached.identity(), named.identity);

    let mut forged = named.clone();
    forged.certified = genesis(&PrivateKey::new()).to_bytes().unwrap();
    assert!(
        connector.connect(&forged).is_err(),
        "another service's genesis does not verify against the named identity"
    );
    let mut renamed = named;
    renamed.identity.genesis = [9; 32];
    assert!(
        connector.connect(&renamed).is_err(),
        "a genesis other than the named one is refused"
    );
}
