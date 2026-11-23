use super::*;
use crate::tenancy::{CoordinationGrant, CoordinatorControl};
use crate::{
    counters::{CLAIM_WINDOW, Claim, Lookup, Name},
    machine::{Metadata, self_certificate_in},
    retention::DAY,
};
use rocky_core::net::{BoxFuture, Failure, Stream};
use rocky_credentials::Publisher;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(crate) const KIND: &str = "memory";

pub(crate) struct Memory {
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
pub(crate) struct Lane(pub(crate) Arc<Memory>, pub(crate) &'static str);
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

pub(crate) struct Service {
    pub(crate) bundle: TrustBundle,
    pub(crate) voter: Arc<Memory>,
    pub(crate) scope: Scope,
    publisher: PublicKey,
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
        let publisher = PrivateKey::new();
        let scope = Scope {
            mesh: MeshId([0x72; 16]),
            app: ApplicationManifest::app_id(*publisher.public_key().as_bytes(), "invoices"),
            domain: DomainId([0x73; 16]),
        };
        Self::serving(impostor, &publisher, scope, "invoices").await
    }
    pub(crate) async fn serving(
        impostor: Option<PrivateKey>,
        publisher: &dyn rocky_credentials::IdentityProvider,
        scope: Scope,
        application: &str,
    ) -> Self {
        let publisher_key = publisher.public_key().unwrap();
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
        let tenant = PrivateKey::new();
        let node = PrivateKey::new();
        let bundle = TrustBundle::new(&genesis).unwrap();
        let time = now().unwrap();
        let grant = SignedCoordinationGrant::sign(
            CoordinationGrant {
                publisher: Publisher(publisher_key),
                application: application.into(),
                tenant: CoordinatorControl(tenant.public_key()),
                not_before: time - 60,
                expires: time + 3600,
            },
            publisher,
        )
        .unwrap();
        let control = DomainControl {
            scope,
            configuration: genesis.genesis.configuration,
            revision: 1,
            trusted: vec![node.public_key()],
            publisher: Some(Box::new(publisher_key)),
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
            publisher: publisher_key,
            node,
            owner,
            tenant,
            control: Mutex::new(control),
        }
    }
    fn database(&self) -> rocky_credentials::DatabaseContext {
        rocky_credentials::DatabaseContext {
            publisher: self.publisher,
            scope: self.scope,
        }
    }
    fn client(&self, key: &PrivateKey) -> NetworkCoordinator {
        self.client_on(key, Arc::new(Lane(self.voter.clone(), KIND)))
    }
    pub(crate) fn client_on(
        &self,
        key: &PrivateKey,
        transport: Arc<dyn Transport>,
    ) -> NetworkCoordinator {
        let client = NetworkCoordinator::connect(&self.bundle, key.clone(), transport).unwrap();
        client.certify(self_certificate_in(key, self.database()));
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

fn control_beyond_the_client_request_limit(service: &Service) -> ControlCertificate {
    let mut control = service
        .control
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    control.revision += 1;
    control
        .trusted
        .extend((0..Request::MAX_CLIENT_BYTES / 32).map(|_| PrivateKey::new().public_key()));
    ControlCertificate::sign(control, &service.tenant).unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_request_beyond_the_client_request_limit_is_denied_without_contacting_a_voter() {
    let service = Service::start().await;
    let contacted = service.voter.streams.load(Ordering::Relaxed);
    let oversized = control_beyond_the_client_request_limit(&service);
    assert_eq!(
        service.owner.control(oversized, nonce()).await,
        Err(CoordinationError::Denied),
        "a request no voter answers is refused for good, not reported as an outage"
    );
    assert_eq!(
        service.voter.streams.load(Ordering::Relaxed),
        contacted,
        "the client refuses an oversized request before it contacts any voter"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_observer_never_hears_of_a_request_refused_before_any_voter_was_contacted() {
    let service = Service::start().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    service.owner.observe(Arc::new(move |result| {
        record
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(result.err().cloned());
    }));
    let oversized = control_beyond_the_client_request_limit(&service);
    assert_eq!(
        service.owner.control(oversized, nonce()).await,
        Err(CoordinationError::Denied)
    );
    assert_eq!(
        *seen.lock().unwrap_or_else(PoisonError::into_inner),
        Vec::new(),
        "a local refusal says nothing about whether any voter answers"
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
async fn an_observer_sees_every_voter_answer_and_refusal() {
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
pub(crate) struct CallerOwned {
    lane: Lane,
    refusing: AtomicBool,
    closes: AtomicUsize,
}
impl CallerOwned {
    pub(crate) fn new(lane: Lane) -> Arc<Self> {
        Arc::new(Self {
            lane,
            refusing: AtomicBool::new(false),
            closes: AtomicUsize::new(0),
        })
    }
    pub(crate) fn refuse_connections(&self, refusing: bool) {
        self.refusing.store(refusing, Ordering::SeqCst);
    }
    fn closes(&self) -> usize {
        self.closes.load(Ordering::SeqCst)
    }
}
impl Transport for CallerOwned {
    fn kind(&self) -> &str {
        self.lane.kind()
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        self.lane.address()
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
        scope: Option<Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        if self.closes() > 0 {
            return Box::pin(async { Err("the caller's transport was closed".into()) });
        }
        if self.refusing.load(Ordering::SeqCst) {
            return Box::pin(async { Err("connection refused".into()) });
        }
        self.lane.connect(address, protocol, scope)
    }
    fn accept(&self) -> BoxFuture<'_, Option<rocky_core::net::Incoming>> {
        self.lane.accept()
    }
    fn request<'a>(
        &'a self,
        addresses: &'a [String],
        scope: Scope,
        request: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>> {
        self.lane.request(addresses, scope, request)
    }
    fn session<'a>(
        &'a self,
        addresses: &'a [String],
        scope: Scope,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        self.lane.session(addresses, scope)
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn connecting_never_closes_the_callers_transport() {
    let service = Service::start().await;
    let transport = CallerOwned::new(Lane(service.voter.clone(), KIND));
    let invoices = Name::new("invoices").unwrap();
    {
        let client = service.client_on(&service.node, transport.clone());
        assert_eq!(
            client
                .claim(service.scope, invoices.clone(), Claim::fresh().unwrap())
                .await,
            Ok(1)
        );
        let stranger = service.client_on(&PrivateKey::new(), transport.clone());
        assert_eq!(
            stranger
                .claim(service.scope, invoices.clone(), Claim::fresh().unwrap())
                .await,
            Err(CoordinationError::Denied),
            "the voter refuses the request"
        );
        transport.refuse_connections(true);
        assert_eq!(
            client
                .claim(service.scope, invoices.clone(), Claim::fresh().unwrap())
                .await,
            Err(CoordinationError::Unavailable),
            "every connection fails"
        );
        transport.refuse_connections(false);
    }
    let unserved = CallerOwned::new(Lane(service.voter.clone(), "other"));
    assert!(matches!(
        NetworkCoordinator::connect(&service.bundle, PrivateKey::new(), unserved.clone()),
        Err(CoordinationError::Configuration)
    ));
    let reopened = service.client_on(&service.node, transport.clone());
    assert_eq!(
        reopened
            .claim(service.scope, invoices, Claim::fresh().unwrap())
            .await,
        Ok(2),
        "the caller's transport still carries a new client"
    );
    assert_eq!(
        (transport.closes(), unserved.closes()),
        (0, 0),
        "a client never closes the transport its caller owns"
    );
}
