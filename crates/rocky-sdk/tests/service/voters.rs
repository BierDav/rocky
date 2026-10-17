use super::*;

pub(super) fn starter() -> TransportOptions {
    TransportOptions::Iroh {
        relay: Some("disabled".into()),
        lookup: Some("disabled".into()),
        topic: false,
        local: false,
        bind: Some("127.0.0.1:0".into()),
    }
}

/// Three voters of a public service with open admission, and its bundle.
pub(super) async fn service(
    dir: &std::path::Path,
) -> Result<(TrustBundle, NetworkCoordinator), Failure> {
    let root = PrivateKey::new();
    let admin = PrivateKey::new();
    let keys: Vec<_> = (0..3).map(|_| PrivateKey::new()).collect();
    let secrets: Vec<[u8; 32]> = (0..3).map(|_| *PrivateKey::new().as_bytes()).collect();
    let mut voters = Vec::new();
    let mut endpoints = Vec::new();
    let mut binds = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let id = index as u64 + 1;
        let tcp = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
        let udp = std::net::UdpSocket::bind("127.0.0.1:0")?.local_addr()?;
        voters.push(Voter {
            id,
            address: tcp.to_string(),
            key: key.public_key(),
        });
        let address = rocky_sdk::iroh::iroh::EndpointAddr::new(
            rocky_sdk::iroh::iroh::SecretKey::from_bytes(&secrets[index]).public(),
        )
        .with_ip_addr(udp);
        endpoints.push(ClientEndpoint {
            voter: id,
            kind: "iroh".into(),
            address: serde_json::to_string(&address)?,
        });
        binds.push(udp.to_string());
    }
    let genesis = CertifiedGenesis::sign(
        Genesis {
            mesh: MeshId([0x61; 16]),
            configuration: [6; 16],
            initial_voters: BTreeSet::from([1, 2, 3]),
            membership_authority: admin.public_key(),
            directory_definition: [0; 32],
            directory: None,
            authorities: Vec::new(),
            max_control_lifetime: 86_400,
            public: Some(PublicService {
                endpoints,
                admission: None,
                limits: rocky_coordinator::Limits::default(),
                tenants: 4,
                registrations_per_day: 16,
                tenant_lanes: 4,
            }),
            voters,
        },
        &root,
    )?;
    for (index, key) in keys.into_iter().enumerate() {
        let id = index as u64 + 1;
        let voter = VoterService::open(
            &dir.join(format!("voter-{id}")),
            genesis.clone(),
            root.public_key(),
            genesis.genesis.mesh,
            id,
            key,
            true,
        )
        .await?;
        let lane = rocky_sdk::iroh::Options {
            relay: Some("disabled".into()),
            lookup: Some("disabled".into()),
            topic: false,
            local: false,
            bind: Some(binds[index].clone()),
        }
        .start_serving(secrets[index], vec![Protocol::Coordinate])
        .await?;
        tokio::spawn(voter.clone().serve_clients(lane.transport()));
        tokio::spawn(voter.serve());
    }
    let admin = NetworkCoordinator::open(
        genesis.clone(),
        root.public_key(),
        genesis.genesis.mesh,
        admin,
    )?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while admin.initialize(1).await.is_err() {
        assert!(tokio::time::Instant::now() < deadline, "voters offline");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    loop {
        if admin.status(1).await.is_ok_and(|s| s.leader.is_some()) {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no leader");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok((TrustBundle::new(&genesis)?, admin))
}
