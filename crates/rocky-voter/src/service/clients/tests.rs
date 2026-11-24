use super::*;
use rocky_coordinator::protocol::{converse, nonce};
use rocky_core::net::{BoxFuture, Failure, Incoming, Stream};

struct CallerOwned {
    incoming: tokio::sync::mpsc::UnboundedSender<Option<Incoming>>,
    accepted: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Option<Incoming>>>,
    listening_refused: AtomicBool,
    closes: AtomicU64,
}
impl CallerOwned {
    fn new() -> Arc<Self> {
        let (incoming, accepted) = tokio::sync::mpsc::unbounded_channel();
        Arc::new(Self {
            incoming,
            accepted: tokio::sync::Mutex::new(accepted),
            listening_refused: AtomicBool::new(false),
            closes: AtomicU64::new(0),
        })
    }
    fn refuse_listening(&self, refused: bool) {
        self.listening_refused.store(refused, Ordering::SeqCst);
    }
    fn closes(&self) -> u64 {
        self.closes.load(Ordering::SeqCst)
    }
    fn deliver(&self, protocol: Protocol) -> tokio::io::DuplexStream {
        let (caller, voter) = tokio::io::duplex(2 * MAX_FRAME);
        self.incoming
            .send(Some(Incoming::Stream(protocol, Stream::new(voter))))
            .expect("the caller's transport stays open to new streams");
        caller
    }
    fn pause_accepting(&self) {
        self.incoming
            .send(None)
            .expect("the caller's transport stays open to new streams");
    }
}
impl Transport for CallerOwned {
    fn kind(&self) -> &str {
        "memory"
    }
    fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async { Ok("voter-1".into()) })
    }
    fn connect<'a>(
        &'a self,
        _: &'a str,
        protocol: Protocol,
        _: Option<Scope>,
    ) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async move {
            if self.closes() > 0 {
                return Err("the caller's transport was closed".into());
            }
            Ok(Stream::new(self.deliver(protocol)))
        })
    }
    fn accept(&self) -> BoxFuture<'_, Option<Incoming>> {
        Box::pin(async move { self.accepted.lock().await.recv().await.flatten() })
    }
    fn listen(&self, _: Option<Scope>, _: bool) -> Result<(), Failure> {
        if self.listening_refused.load(Ordering::SeqCst) {
            return Err("listening refused".into());
        }
        Ok(())
    }
    fn request<'a>(
        &'a self,
        _: &'a [String],
        _: Scope,
        _: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<u8>, Failure>> {
        Box::pin(async { Err("the caller's transport carries coordination only".into()) })
    }
    fn session<'a>(&'a self, _: &'a [String], _: Scope) -> BoxFuture<'a, Result<Stream, Failure>> {
        Box::pin(async { Err("the caller's transport carries coordination only".into()) })
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }
}

async fn reply_to(mut caller: tokio::io::DuplexStream) -> Vec<u8> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    caller.shutdown().await.unwrap();
    let mut reply = Vec::new();
    caller.read_to_end(&mut reply).await.unwrap();
    reply
}

struct PublicVoter {
    _directory: tempfile::TempDir,
    genesis: CertifiedGenesis,
    admin: PrivateKey,
    voter_port: TcpListener,
    voter: Arc<VoterService>,
}

async fn public_voter() -> PublicVoter {
    let directory = tempfile::tempdir().unwrap();
    let root = PrivateKey::new();
    let admin = PrivateKey::new();
    let key = PrivateKey::new();
    let voter_port = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let voter_address = voter_port.local_addr().unwrap().to_string();
    let genesis = CertifiedGenesis::sign(
        Genesis {
            mesh: MeshId([0x61; 16]),
            configuration: [6; 16],
            voters: (1..=3)
                .map(|id| Voter {
                    id,
                    address: if id == 1 {
                        voter_address.clone()
                    } else {
                        format!("127.0.0.1:{id}")
                    },
                    key: if id == 1 {
                        key.public_key()
                    } else {
                        PrivateKey::new().public_key()
                    },
                })
                .collect(),
            initial_voters: BTreeSet::from([1, 2, 3]),
            membership_authority: admin.public_key(),
            authorities: Vec::new(),
            max_control_lifetime: 86_400,
            public: Some(PublicService {
                endpoints: vec![ClientEndpoint {
                    voter: 1,
                    kind: "memory".into(),
                    address: "voter-1".into(),
                }],
                admission: None,
                limits: rocky_coordinator::Limits::default(),
                tenants: 4,
                registrations_per_day: 16,
                tenant_lanes: 4,
            }),
        },
        &root,
    )
    .unwrap();
    let voter = VoterService::open(
        directory.path(),
        genesis.clone(),
        root.public_key(),
        genesis.genesis.mesh,
        1,
        key,
        true,
    )
    .await
    .unwrap();
    PublicVoter {
        _directory: directory,
        genesis,
        admin,
        voter_port,
        voter,
    }
}

async fn lead_alone(voter: &VoterService, address: String) {
    voter
        .raft
        .initialize(std::collections::BTreeMap::from([(
            1,
            BasicNode::new(address),
        )]))
        .await
        .unwrap();
    voter
        .raft
        .wait(Some(std::time::Duration::from_secs(20)))
        .current_leader(1, "a lone initialized voter leads")
        .await
        .unwrap();
}

async fn registered_tenant(
    transport: Arc<CallerOwned>,
    genesis: &CertifiedGenesis,
) -> NetworkCoordinator {
    use rocky_coordinator::tenancy::{
        CoordinationGrant, CoordinatorControl, SignedCoordinationGrant,
    };
    use rocky_credentials::Publisher;
    let tenant = PrivateKey::new();
    let publisher = PrivateKey::new();
    let time = now().unwrap();
    let scope = Scope {
        mesh: MeshId([1; 16]),
        app: ApplicationManifest::app_id(*publisher.public_key().as_bytes(), "notes"),
        domain: rocky_core::DomainId([1; 16]),
    };
    let grant = SignedCoordinationGrant::sign(
        CoordinationGrant {
            publisher: Publisher(publisher.public_key()),
            application: "notes".into(),
            tenant: CoordinatorControl(tenant.public_key()),
            not_before: time - 60,
            expires: time + 86_400,
        },
        &publisher,
    )
    .unwrap();
    let control = ControlCertificate::sign(
        DomainControl {
            scope,
            configuration: genesis.genesis.configuration,
            revision: 1,
            trusted: vec![tenant.public_key()],
            publisher: None,
            not_before: time - 60,
            expires: time + 3_600,
        },
        &tenant,
    )
    .unwrap();
    let client =
        NetworkCoordinator::connect(&TrustBundle::new(genesis).unwrap(), tenant, transport)
            .unwrap();
    client
        .register(grant, None, control, [1; 32])
        .await
        .expect("a tenant registers on an idle voter");
    client
}

async fn ask_for_usage(
    transport: &CallerOwned,
    genesis: &CertifiedGenesis,
) -> Result<Reply, CoordinationError> {
    let key = PrivateKey::new();
    let id = genesis.id()?;
    let request = Request::certified(id, nonce(), Operation::Usage, (&key, None), now()?)?;
    let mut caller = transport.deliver(Protocol::Coordinate);
    converse(
        &mut caller,
        genesis.genesis.voters[0].key,
        id,
        &key,
        Message::Client(Box::new(request)),
    )
    .await
}

fn silent_clients(transport: &CallerOwned, count: usize) -> Vec<tokio::io::DuplexStream> {
    (0..count)
        .map(|_| transport.deliver(Protocol::Coordinate))
        .collect()
}

fn unadmitted_client_budget() -> usize {
    CLIENT_LANES as usize * UNADMITTED_CLIENT_STREAMS_PER_CLIENT_LANE
}

fn stale_membership() -> Operation {
    Operation::Membership {
        voters: BTreeSet::from([1, 2]),
        expected: BTreeSet::new(),
        expected_revision: 1,
    }
}

async fn until(condition: impl Fn() -> bool) {
    while !condition() {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

fn administration_an_hour_old(
    genesis: &CertifiedGenesis,
    admin: &PrivateKey,
) -> Result<Request, CoordinationError> {
    Request::certified(
        genesis.id()?,
        nonce(),
        stale_membership(),
        (admin, None),
        now()? - 3_600,
    )
}

fn refused_before_raft_for_its_age(result: &Result<Outcome, CoordinationError>) -> bool {
    matches!(result, Err(CoordinationError::ClockSkew { .. }))
}

#[tokio::test(start_paused = true)]
async fn streams_awaiting_their_request_take_no_client_lane_so_a_registered_tenant_is_answered() {
    let PublicVoter {
        _directory,
        genesis,
        voter_port,
        voter,
        ..
    } = public_voter().await;
    lead_alone(&voter, voter_port.local_addr().unwrap().to_string()).await;
    let transport = CallerOwned::new();
    let serving = tokio::spawn(voter.clone().serve_clients(transport.clone()));
    let tenant = registered_tenant(transport.clone(), &genesis).await;

    let silent = silent_clients(&transport, unadmitted_client_budget() - 1);
    tokio::time::timeout(
        CLIENT_ADMISSION_TIMEOUT,
        until(|| {
            voter
                .admission
                .unadmitted_client_streams
                .available_permits()
                == 1
        }),
    )
    .await
    .expect("every silent stream holds an unadmitted slot before its admission deadline");
    assert_eq!(
        voter.admission.client_lanes.available_permits(),
        CLIENT_LANES as usize,
        "streams awaiting their request hold no client lane"
    );
    assert_eq!(
        tenant.usage().await.map(|usage| usage.open_domains),
        Ok(1),
        "a registered tenant is answered beside more silent streams than client lanes"
    );
    assert!(
        voter
            .admission
            .unadmitted_client_streams
            .available_permits()
            <= 1,
        "the silent streams still await their request when the tenant is answered"
    );
    assert_eq!(voter.counters.snapshot().busy, 0);

    transport.pause_accepting();
    serving.await.unwrap();
    drop(silent);
}

#[tokio::test(start_paused = true)]
async fn a_full_unadmitted_budget_drops_further_streams_until_the_read_timeout_frees_it() {
    use tokio::io::AsyncReadExt;
    let PublicVoter {
        _directory,
        genesis,
        voter,
        ..
    } = public_voter().await;
    let transport = CallerOwned::new();
    let serving = tokio::spawn(voter.clone().serve_clients(transport.clone()));

    let started = tokio::time::Instant::now();
    let silent = silent_clients(&transport, unadmitted_client_budget());
    assert_eq!(
        ask_for_usage(&transport, &genesis)
            .await
            .map(|reply| reply.result),
        Err(CoordinationError::Unavailable),
        "a stream beyond a full unadmitted budget is dropped before it is read"
    );
    assert_eq!(voter.counters.snapshot().busy, 1);

    for mut stream in silent {
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).await.unwrap();
        assert_eq!(reply, Vec::<u8>::new());
    }
    let waited = started.elapsed();
    assert!(
        (CLIENT_ADMISSION_TIMEOUT..CLIENT_REQUEST_TIMEOUT).contains(&waited),
        "silent streams are dropped at the read timeout, not the request timeout: {waited:?}"
    );
    assert_eq!(
        voter
            .admission
            .unadmitted_client_streams
            .available_permits(),
        unadmitted_client_budget(),
        "dropped silent streams return their unadmitted slots"
    );
    assert_eq!(
        ask_for_usage(&transport, &genesis)
            .await
            .map(|reply| reply.result),
        Ok(Err(CoordinationError::Denied(Refusal::UnknownTenant))),
        "the freed budget admits the next client"
    );

    transport.pause_accepting();
    serving.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn unadmitted_client_streams_beyond_their_budget_are_dropped_until_one_ends() {
    let PublicVoter {
        _directory,
        genesis,
        voter,
        ..
    } = public_voter().await;
    let transport = CallerOwned::new();
    let serving = tokio::spawn(voter.clone().serve_clients(transport.clone()));

    let mut unadmitted = silent_clients(&transport, unadmitted_client_budget());
    assert_eq!(
        ask_for_usage(&transport, &genesis)
            .await
            .map(|reply| reply.result),
        Err(CoordinationError::Unavailable),
        "a stream beyond the unadmitted budget is dropped before it is read"
    );
    assert_eq!(voter.counters.snapshot().busy, 1);

    let failed = unadmitted.pop().expect("the unadmitted budget is in use");
    assert_eq!(
        reply_to(failed).await,
        Vec::<u8>::new(),
        "a client that hangs up before its request is dropped unanswered"
    );
    assert_eq!(
        ask_for_usage(&transport, &genesis)
            .await
            .map(|reply| reply.result),
        Ok(Err(CoordinationError::Denied(Refusal::UnknownTenant))),
        "the unadmitted slot a failed read held admits the next client"
    );
    assert_eq!(voter.counters.snapshot().busy, 1);

    transport.pause_accepting();
    serving.await.unwrap();
    drop(unadmitted);
}

#[tokio::test(start_paused = true)]
async fn a_frame_beyond_the_client_frame_limit_is_dropped_before_its_body_arrives() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let PublicVoter {
        _directory, voter, ..
    } = public_voter().await;
    let transport = CallerOwned::new();
    let serving = tokio::spawn(voter.clone().serve_clients(transport.clone()));

    let started = tokio::time::Instant::now();
    let mut oversized = transport.deliver(Protocol::Coordinate);
    oversized
        .write_u32(u32::try_from(MAX_CLIENT_FRAME + 1).unwrap())
        .await
        .unwrap();
    let mut reply = Vec::new();
    oversized.read_to_end(&mut reply).await.unwrap();
    assert_eq!(reply, Vec::<u8>::new());
    assert!(
        started.elapsed() < CLIENT_ADMISSION_TIMEOUT,
        "the announced length alone refuses the frame: {:?}",
        started.elapsed()
    );
    assert_eq!(
        voter
            .admission
            .unadmitted_client_streams
            .available_permits(),
        unadmitted_client_budget()
    );

    transport.pause_accepting();
    serving.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn tenant_resolution_beyond_the_admission_deadline_is_refused_with_a_signed_backpressure() {
    let PublicVoter {
        _directory,
        genesis,
        voter,
        ..
    } = public_voter().await;
    let transport = CallerOwned::new();
    let serving = tokio::spawn(voter.clone().serve_clients(transport.clone()));

    let held = voter.store.hold_machine().await;
    let started = tokio::time::Instant::now();
    assert_eq!(
        ask_for_usage(&transport, &genesis)
            .await
            .map(|reply| reply.result),
        Ok(Err(CoordinationError::Backpressure)),
        "a tenant that cannot be resolved before the admission deadline is answered busy"
    );
    assert_eq!(
        started.elapsed(),
        CLIENT_ADMISSION_TIMEOUT,
        "tenant resolution is bounded by the admission deadline, not the request timeout"
    );
    assert_eq!(voter.counters.snapshot().busy, 1);
    assert_eq!(
        voter
            .admission
            .unadmitted_client_streams
            .available_permits(),
        unadmitted_client_budget(),
        "a refused admission returns its unadmitted slot"
    );
    drop(held);
    assert_eq!(
        ask_for_usage(&transport, &genesis)
            .await
            .map(|reply| reply.result),
        Ok(Err(CoordinationError::Denied(Refusal::UnknownTenant))),
        "a resolvable tenant is admitted again"
    );

    transport.pause_accepting();
    serving.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_admitted_request_returns_its_unadmitted_slot_while_it_is_answered() {
    let PublicVoter {
        _directory,
        genesis,
        voter_port,
        voter,
        ..
    } = public_voter().await;
    lead_alone(&voter, voter_port.local_addr().unwrap().to_string()).await;
    let transport = CallerOwned::new();
    let serving = tokio::spawn(voter.clone().serve_clients(transport.clone()));
    let tenant = registered_tenant(transport.clone(), &genesis).await;

    voter
        .raft
        .add_learner(
            2,
            BasicNode::new(genesis.genesis.voters[1].address.clone()),
            false,
        )
        .await
        .unwrap();
    let quorum_with_an_unreachable_voter = tokio::spawn({
        let voter = voter.clone();
        async move {
            voter
                .raft
                .change_membership(BTreeSet::from([1, 2]), false)
                .await
                .map(drop)
        }
    });
    tokio::time::timeout(
        CLIENT_ADMISSION_TIMEOUT,
        until(|| {
            voter
                .raft
                .metrics()
                .borrow()
                .membership_config
                .membership()
                .get_joint_config()
                .len()
                == 2
        }),
    )
    .await
    .expect("the leader enters a joint configuration it cannot commit");

    let usage = tokio::spawn(async move { tenant.usage().await.map(|usage| usage.open_domains) });
    tokio::time::timeout(
        CLIENT_ADMISSION_TIMEOUT,
        until(|| voter.admission.client_lanes.available_permits() < CLIENT_LANES as usize),
    )
    .await
    .expect("the tenant's linearizable read is admitted and waits for a quorum");
    assert_eq!(
        voter
            .admission
            .unadmitted_client_streams
            .available_permits(),
        unadmitted_client_budget(),
        "an admitted request holds a client lane, not an unadmitted slot"
    );

    usage.abort();
    quorum_with_an_unreachable_voter.abort();
    transport.pause_accepting();
    serving.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_request_beyond_the_client_lanes_is_refused_with_a_signed_backpressure() {
    let PublicVoter {
        _directory,
        genesis,
        voter,
        ..
    } = public_voter().await;
    let transport = CallerOwned::new();
    let serving = tokio::spawn(voter.clone().serve_clients(transport.clone()));

    let lanes = voter
        .admission
        .client_lanes
        .try_acquire_many(CLIENT_LANES)
        .unwrap();
    assert_eq!(
        ask_for_usage(&transport, &genesis)
            .await
            .map(|reply| reply.result),
        Ok(Err(CoordinationError::Backpressure)),
        "a read request beyond the client lanes is answered busy, not dropped"
    );
    assert_eq!(voter.counters.snapshot().busy, 1);
    assert_eq!(
        voter
            .admission
            .unadmitted_client_streams
            .available_permits(),
        unadmitted_client_budget(),
        "a refused request returns its unadmitted slot"
    );
    drop(lanes);
    assert_eq!(
        ask_for_usage(&transport, &genesis)
            .await
            .map(|reply| reply.result),
        Ok(Err(CoordinationError::Denied(Refusal::UnknownTenant)))
    );

    transport.pause_accepting();
    serving.await.unwrap();
}

#[tokio::test]
async fn administration_on_the_voter_port_bypasses_full_client_lanes() {
    let PublicVoter {
        _directory,
        genesis,
        admin,
        voter_port,
        voter,
    } = public_voter().await;
    let voters = tokio::spawn(voter.clone().serve_voters(voter_port));
    let lanes = voter
        .admission
        .client_lanes
        .try_acquire_many(CLIENT_LANES)
        .unwrap();

    let id = genesis.id().unwrap();
    let answered = rocky_coordinator::protocol::exchange::<Reply>(
        &genesis.genesis.voters[0],
        id,
        &admin,
        Message::Client(Box::new(
            administration_an_hour_old(&genesis, &admin).unwrap(),
        )),
    )
    .await
    .map(|reply| reply.result);
    assert!(
        answered.as_ref().is_ok_and(refused_before_raft_for_its_age),
        "administration on the voter port takes no client lane and is answered by the voter: {answered:?}"
    );
    assert_eq!(voter.counters.snapshot().busy, 0);

    drop(lanes);
    voters.abort();
}

#[tokio::test(start_paused = true)]
async fn administration_takes_one_of_two_lanes_and_a_fenced_voter_answers_unavailable_first() {
    let PublicVoter {
        _directory,
        genesis,
        admin,
        voter,
        ..
    } = public_voter().await;
    let administer = || {
        let request = administration_an_hour_old(&genesis, &admin).unwrap();
        let voter = voter.clone();
        async move { voter.administer(request).await.result }
    };
    let held = voter
        .admission
        .administrative_lanes
        .try_acquire_many(u32::try_from(ADMINISTRATIVE_LANES - 1).unwrap())
        .unwrap();
    assert!(
        refused_before_raft_for_its_age(&administer().await),
        "a free administrative lane admits an administrative request"
    );
    let last = voter.admission.administrative_lanes.try_acquire().unwrap();
    assert_eq!(
        administer().await,
        Err(CoordinationError::Backpressure),
        "administration beyond its lanes is answered busy"
    );
    assert_eq!(voter.counters.snapshot().busy, 1);

    voter.fenced.store(true, Ordering::SeqCst);
    assert_eq!(
        administer().await,
        Err(CoordinationError::Unavailable),
        "a fenced voter answers unavailable before any lane is considered"
    );
    assert_eq!(voter.counters.snapshot().busy, 1);
    voter.fenced.store(false, Ordering::SeqCst);

    drop((held, last));
    assert!(
        refused_before_raft_for_its_age(&administer().await),
        "released administrative lanes admit the next administrative request"
    );
}

#[tokio::test]
async fn a_cancelled_client_admission_frees_its_unadmitted_slot() {
    let PublicVoter {
        _directory, voter, ..
    } = public_voter().await;
    let unadmitted = voter
        .counters
        .connection_slot(&voter.admission.unadmitted_client_streams)
        .expect("an idle voter has a free unadmitted slot");
    let (_caller, stream) = tokio::io::duplex(2 * MAX_FRAME);
    let mut cancelled = Box::pin(
        voter
            .clone()
            .serve_client(rocky_core::net::Stream::new(stream), unadmitted),
    );
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(cancelled.as_mut().poll(cx)))
            .await
            .is_pending(),
        "the admission waits for its request"
    );
    assert_eq!(
        voter
            .admission
            .unadmitted_client_streams
            .available_permits(),
        unadmitted_client_budget() - 1
    );

    drop(cancelled);
    assert_eq!(
        voter
            .admission
            .unadmitted_client_streams
            .available_permits(),
        unadmitted_client_budget(),
        "a cancelled admission returns its unadmitted slot"
    );
}

#[tokio::test(start_paused = true)]
async fn serving_clients_never_closes_the_callers_transport() {
    let PublicVoter {
        _directory,
        genesis,
        voter,
        ..
    } = public_voter().await;
    let transport = CallerOwned::new();
    let client = NetworkCoordinator::connect(
        &TrustBundle::new(&genesis).unwrap(),
        PrivateKey::new(),
        transport.clone(),
    )
    .unwrap();

    transport.refuse_listening(true);
    voter.clone().serve_clients(transport.clone()).await;
    transport.refuse_listening(false);

    let serving = tokio::spawn(voter.clone().serve_clients(transport.clone()));
    assert_eq!(
        reply_to(transport.deliver(Protocol::Link)).await,
        Vec::<u8>::new(),
        "a stream of another protocol is dropped unanswered"
    );
    assert_eq!(
        reply_to(transport.deliver(Protocol::Coordinate)).await,
        Vec::<u8>::new(),
        "a connection that sends no request is dropped unanswered"
    );
    assert_eq!(
        client.usage().await.map(drop),
        Err(CoordinationError::Denied(Refusal::UnknownTenant)),
        "an unregistered client is answered with a refusal"
    );
    transport.pause_accepting();
    serving.await.unwrap();

    let aborted = tokio::spawn(voter.clone().serve_clients(transport.clone()));
    assert_eq!(
        client.usage().await.map(drop),
        Err(CoordinationError::Denied(Refusal::UnknownTenant)),
        "the caller's transport serves another request"
    );
    aborted.abort();
    assert!(aborted.await.unwrap_err().is_cancelled());

    assert_eq!(
        transport.closes(),
        0,
        "serving clients never closes the transport its caller owns"
    );
}
