//! The client against the authority state machine behind an in-memory
//! transport, on every target: one test voter answers each stream as a
//! voter's client lane does, signed with its key.
use super::*;
use crate::{
    counters::{CLAIM_WINDOW, Claim, Lookup, Name},
    machine::{Metadata, self_certificate},
    retention::DAY,
};
use rocky_core::net::{BoxFuture, Failure, Stream};
use rocky_credentials::{CoordinationGrant, CoordinatorControl, Publisher};
use std::sync::atomic::{AtomicUsize, Ordering};

const KIND: &str = "memory";

/// One voter of a public service, reached through `Transport::connect`.
struct Memory {
    genesis: Commitment,
    /// The key the voter answers with: the certified one, or an impostor's.
    key: PrivateKey,
    state: Arc<Mutex<Metadata>>,
    streams: AtomicUsize,
    /// Seconds the voter's clock runs ahead of every request's: above zero
    /// it refuses each one as skewed.
    ahead: std::sync::atomic::AtomicU64,
}
impl Memory {
    fn answer(&self, request: &Request) -> Result<Outcome, CoordinationError> {
        request.authenticate(self.genesis)?;
        let ahead = self.ahead.load(Ordering::Relaxed);
        if ahead > 0 {
            return Err(CoordinationError::ClockSkew {
                coordinator_time: now()? + ahead,
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
/// The client's transport of kind `.1`: each stream reaches the test voter.
struct Lane(Arc<Memory>, &'static str);
impl Transport for Lane {
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
    fn accept(&self) -> BoxFuture<'_, Option<(Protocol, Stream)>> {
        Box::pin(async { None })
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// A public service whose voter 1, of `key`, the client lane reaches; the
/// other two have no client endpoint.
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
            directory_definition: [0; 32],
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

/// A public service, the tenant that registered a domain on
/// it, the domain's publisher and its trusted node.
struct Service {
    bundle: TrustBundle,
    voter: Arc<Memory>,
    scope: Scope,
    publisher: PrivateKey,
    node: PrivateKey,
}
impl Service {
    async fn start() -> Self {
        Self::answered_by(None).await
    }
    /// A service whose voter answers with `impostor` instead of its key.
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
            ahead: Default::default(),
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
        // Controls name trusted identities; this node is its own.
        let control = ControlCertificate::sign(
            DomainControl {
                scope,
                configuration: genesis.genesis.configuration,
                revision: 1,
                trusted: vec![node.public_key()],
                publisher: Some(Box::new(publisher.public_key())),
                not_before: time - 60,
                expires: time + 3600,
            },
            &tenant,
        )
        .unwrap();
        let owner =
            NetworkCoordinator::connect(&bundle, tenant, Arc::new(Lane(memory.clone(), KIND)))
                .unwrap();
        owner.register(grant, None, control, [1; 32]).await.unwrap();
        let memory = match impostor {
            None => memory,
            Some(key) => Arc::new(Memory {
                genesis: memory.genesis,
                key,
                state: memory.state.clone(),
                streams: AtomicUsize::new(0),
                ahead: Default::default(),
            }),
        };
        Self {
            bundle,
            voter: memory,
            scope,
            publisher,
            node,
        }
    }
    /// A client of `key` presenting its own node certificate.
    fn client(&self, key: &PrivateKey) -> NetworkCoordinator {
        let client = NetworkCoordinator::connect(
            &self.bundle,
            key.clone(),
            Arc::new(Lane(self.voter.clone(), KIND)),
        )
        .unwrap();
        client.certify(self_certificate(key));
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
    // Another client of the same node, as after a restart, sees the same counter.
    let reopened = service.client(&service.node);
    let reading = reopened
        .counter(service.scope, invoices.clone(), Some(first))
        .await
        .unwrap();
    assert_eq!(
        (reading.value, reading.claim),
        (2, Some(Lookup::Claimed(1)))
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
    // Without its certificate, the trusted node's key alone is no identity.
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
        Err(CoordinationError::Denied)
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
        .certify_claims(&identity, node.public_key(), 1)
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

    let statement = SignedRevocation::sign(
        Revocation {
            app: service.scope.app,
            issuer: service.publisher.public_key(),
            time,
            nodes: Default::default(),
            identities: [identity.public_key()].into(),
            capabilities: Default::default(),
            cut: Default::default(),
        },
        &service.publisher,
        &CredentialChain::default(),
    )
    .unwrap();
    // The revoked holder itself may push it; it holds from then on.
    holder
        .revoke(
            service.scope,
            RevocationSet::try_from(vec![statement]).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        holder.claim(service.scope, invoices.clone(), first).await,
        Err(CoordinationError::Denied)
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
    // Every attempt is refused, so the outcome stays uncertain.
    assert_eq!(
        client
            .claim(service.scope, invoices, Claim::fresh().unwrap())
            .await,
        Err(CoordinationError::Unavailable)
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
    service.voter.ahead.store(3600, Ordering::Relaxed);
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
        ahead: Default::default(),
    });
    let bundle = TrustBundle::new(&genesis).unwrap();
    assert!(matches!(
        NetworkCoordinator::connect(&bundle, PrivateKey::new(), Arc::new(Lane(memory, "other"))),
        Err(CoordinationError::Configuration)
    ));
}
