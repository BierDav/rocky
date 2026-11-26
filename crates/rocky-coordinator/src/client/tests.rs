use super::*;
use crate::tenancy::{CoordinationGrant, CoordinatorControl, SignedCoordinationGrant};
use crate::{
    counters::{Claim, Claimed::*, Name},
    machine::Metadata,
    request::self_certificate_in,
    retention::DAY,
};
use p2panda_core::PublicKey;
use rocky_core::net::{BoxFuture, Failure, Stream};
use rocky_credentials::Publisher;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(crate) const SCHEME: &str = "memory";

fn unique() -> Claim {
    let id: Commitment = rocky_platform::entropy::random();
    Claim::new(id, id)
}

pub(crate) struct Memory {
    genesis: Commitment,
    key: PrivateKey,
    state: Arc<Mutex<Metadata>>,
    streams: AtomicUsize,
    clock_ahead_seconds: std::sync::atomic::AtomicU64,
    loses_replies: AtomicBool,
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
            let envelope = read_bounded(&mut stream, MAX_CLIENT_FRAME).await?;
            let reply = Reply {
                leader: None,
                result: envelope
                    .client_request(self.genesis)
                    .and_then(|request| self.answer(&request)),
            };
            if self.loses_replies.load(Ordering::Relaxed) {
                return Ok(());
            }
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
    fn schemes(&self) -> &[&str] {
        std::slice::from_ref(&self.1)
    }
    fn dial_only(&self) -> bool {
        true
    }
    fn address(&self) -> BoxFuture<'_, Result<Option<String>, Failure>> {
        Box::pin(async { Ok(None) })
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
                    route: format!("{SCHEME}:voter-1"),
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
            loses_replies: AtomicBool::new(false),
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
            admins: vec![node.public_key()],
            publisher: Some(Box::new(publisher_key)),
            not_before: time - 60,
            expires: time + 3600,
        };
        let owner = NetworkCoordinator::connect(
            &bundle,
            tenant.clone(),
            Arc::new(Lane(memory.clone(), SCHEME)),
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
                loses_replies: AtomicBool::new(false),
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
        self.client_on(key, Arc::new(Lane(self.voter.clone(), SCHEME)))
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
async fn an_admin_node_claims_and_a_retry_returns_the_same_value() {
    let service = Service::start().await;
    let client = service.client(&service.node);
    let invoices = Name::new("company-1/invoices").unwrap();
    let first = unique();
    assert_eq!(
        client.claim(service.scope, invoices.clone(), first).await,
        Ok(First(1))
    );
    assert_eq!(
        client.claim(service.scope, invoices.clone(), first).await,
        Ok(First(1))
    );
    let second = unique();
    assert_eq!(
        client.claim(service.scope, invoices.clone(), second).await,
        Ok(First(2))
    );
    let reopened = service.client(&service.node);
    assert_eq!(
        reopened.counter(service.scope, invoices.clone()).await,
        Ok(2),
        "a client reopened on the same node sees the same counter"
    );
    assert_eq!(
        reopened.claim(service.scope, invoices, second).await,
        Ok(First(2))
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn two_doors_racing_one_ticket_admit_it_exactly_once() {
    let service = Service::start().await;
    let doors = [service.client(&service.node), service.client(&service.node)];
    let door = Name::new("door").unwrap();
    let scans = [
        Claim::new(b"ticket-42", b"east-scan-1"),
        Claim::new(b"ticket-42", b"west-scan-1"),
    ];
    let (east, west) = tokio::join!(
        doors[0].claim(service.scope, door.clone(), scans[0]),
        doors[1].claim(service.scope, door.clone(), scans[1]),
    );
    let answers = [east.unwrap(), west.unwrap()];
    let mut sorted = answers;
    sorted.sort_by_key(|answer| matches!(answer, Taken(_)));
    assert_eq!(
        sorted,
        [First(1), Taken(1)],
        "exactly one door admits the ticket, and both see its one number"
    );
    let rescans = [
        Claim::new(b"ticket-42", b"east-scan-1"),
        Claim::new(b"ticket-42", b"west-scan-1"),
    ];
    for ((door_client, scan), answer) in doors.iter().zip(rescans).zip(answers) {
        assert_eq!(
            door_client.claim(service.scope, door.clone(), scan).await,
            Ok(answer),
            "a retried scan repeats its door's answer"
        );
    }
    assert_eq!(
        doors[0].counter(service.scope, door).await,
        Ok(1),
        "the ticket took one number"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_lost_reply_is_outcome_unknown_and_the_same_claim_learns_its_number() {
    let service = Service::start().await;
    let client = service.client(&service.node);
    let invoices = Name::new("invoices").unwrap();
    let order = Claim::new(b"order-17", b"checkout-3");
    service.voter.loses_replies.store(true, Ordering::Relaxed);
    assert_eq!(
        client.claim(service.scope, invoices.clone(), order).await,
        Err(CoordinationError::OutcomeUnknown),
        "a claim that reached the voter without an answer is not reported as taking no number"
    );
    assert!(CoordinationError::OutcomeUnknown.retryable());
    assert_eq!(
        client.counter(service.scope, invoices.clone()).await,
        Err(CoordinationError::Unavailable),
        "a lost answer to a read changed nothing"
    );
    service.voter.loses_replies.store(false, Ordering::Relaxed);
    assert_eq!(
        client.counter(service.scope, invoices.clone()).await,
        Ok(1),
        "the unanswered claim took its number"
    );
    let rebuilt_from_the_order_row = Claim::new(b"order-17", b"checkout-3");
    assert_eq!(
        client
            .claim(service.scope, invoices.clone(), rebuilt_from_the_order_row)
            .await,
        Ok(First(1)),
        "the claim rebuilt from the same key and attempt learns the number it took"
    );
    assert_eq!(
        client
            .claim(service.scope, invoices.clone(), unique())
            .await,
        Ok(First(2)),
        "no number was lost or taken twice"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_claim_beyond_its_window_is_expired_and_takes_no_value() {
    let service = Service::start().await;
    let client = service.client(&service.node);
    let invoices = Name::new("invoices").unwrap();
    let stale = Claim::new(b"order-5", b"order-5").expires(
        rocky_platform::time::SystemTime::now() - rocky_platform::time::Duration::from_secs(60),
    );
    assert_eq!(
        client.claim(service.scope, invoices.clone(), stale).await,
        Err(CoordinationError::Expired)
    );
    assert_eq!(client.counter(service.scope, invoices).await, Ok(0));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn only_admin_nodes_claim() {
    let service = Service::start().await;
    let invoices = Name::new("invoices").unwrap();
    let stranger = service.client(&PrivateKey::new());
    assert_eq!(
        stranger
            .claim(service.scope, invoices.clone(), unique())
            .await,
        Err(CoordinationError::Denied(Refusal::NotMember))
    );
    let uncertified = NetworkCoordinator::connect(
        &service.bundle,
        service.node.clone(),
        Arc::new(Lane(service.voter.clone(), SCHEME)),
    )
    .unwrap();
    assert_eq!(
        uncertified
            .claim(service.scope, invoices.clone(), unique())
            .await,
        Err(CoordinationError::Denied(Refusal::NotMember)),
        "an admin node's key without its certificate is no identity"
    );
    let admin = service.client(&service.node);
    assert_eq!(admin.counter(service.scope, invoices).await, Ok(0));
}

fn control_beyond_the_client_request_limit(service: &Service) -> ControlCertificate {
    let mut control = service
        .control
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    control.revision += 1;
    control
        .admins
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
        Err(CoordinationError::Denied(Refusal::Oversized)),
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
        Err(CoordinationError::Denied(Refusal::Oversized))
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
        client.claim(service.scope, invoices, unique()).await,
        Err(CoordinationError::OutcomeUnknown),
        "every answer is refused after the request was sent, so its outcome is unknown"
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
            .claim(service.scope, invoices.clone(), unique())
            .await,
        Ok(First(1))
    );
    service
        .voter
        .clock_ahead_seconds
        .store(3600, Ordering::Relaxed);
    assert!(matches!(
        client.claim(service.scope, invoices, unique()).await,
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
fn a_client_needs_an_endpoint_its_transport_claims() {
    let genesis = genesis(&PrivateKey::new());
    let memory = Arc::new(Memory {
        genesis: genesis.id().unwrap(),
        key: PrivateKey::new(),
        state: Arc::new(Mutex::new(Metadata::new(genesis.clone()).unwrap())),
        streams: AtomicUsize::new(0),
        clock_ahead_seconds: Default::default(),
        loses_replies: AtomicBool::new(false),
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
    fn schemes(&self) -> &[&str] {
        self.lane.schemes()
    }
    fn dial_only(&self) -> bool {
        self.lane.dial_only()
    }
    fn address(&self) -> BoxFuture<'_, Result<Option<String>, Failure>> {
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
    let transport = CallerOwned::new(Lane(service.voter.clone(), SCHEME));
    let invoices = Name::new("invoices").unwrap();
    {
        let client = service.client_on(&service.node, transport.clone());
        assert_eq!(
            client
                .claim(service.scope, invoices.clone(), unique())
                .await,
            Ok(First(1))
        );
        let stranger = service.client_on(&PrivateKey::new(), transport.clone());
        assert_eq!(
            stranger
                .claim(service.scope, invoices.clone(), unique())
                .await,
            Err(CoordinationError::Denied(Refusal::NotMember)),
            "the voter refuses the request"
        );
        transport.refuse_connections(true);
        assert_eq!(
            client
                .claim(service.scope, invoices.clone(), unique())
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
        reopened.claim(service.scope, invoices, unique()).await,
        Ok(First(2)),
        "the caller's transport still carries a new client"
    );
    assert_eq!(
        (transport.closes(), unserved.closes()),
        (0, 0),
        "a client never closes the transport its caller owns"
    );
}
