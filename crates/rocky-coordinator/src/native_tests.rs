use crate::{
    Limits,
    certified::*,
    machine::{Metadata, self_certified},
    service::{NetworkCoordinator, Operation, Outcome},
};
use p2panda_core::PrivateKey;
use rocky_core::*;
use std::{collections::BTreeSet, sync::Arc};

fn certified() -> (CertifiedGenesis, PrivateKey, Scope, PrivateKey) {
    let root = PrivateKey::new();
    let directory = PrivateKey::new();
    let executor = PrivateKey::new();
    let scope = Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    };
    let genesis = CertifiedGenesis::sign(
        Genesis {
            mesh: scope.mesh,
            configuration: [4; 16],
            voters: (1..=3)
                .map(|id| Voter {
                    id,
                    address: format!("127.0.0.1:{}", 10000 + id),
                    key: PrivateKey::new().public_key(),
                })
                .collect(),
            initial_voters: BTreeSet::from([1, 2, 3]),
            membership_authority: directory.public_key(),
            authorities: vec![Authority {
                scope,
                issuer: directory.public_key(),
            }],
            max_control_lifetime: 86400,
            public: None,
        },
        &root,
    )
    .unwrap();
    (genesis, executor, scope, directory)
}
#[test]
fn retry_records_expire_and_are_bounded_per_signer() {
    let (genesis, _, scope, directory) = certified();
    let mut metadata = Metadata::with_limits(genesis.clone(), Limits::default()).unwrap();
    let id = genesis.id().unwrap();
    let time = now().unwrap();
    let limits = Limits::default();
    let acquire = |n: u64, time| {
        let mut identity = [0; 32];
        identity[..8].copy_from_slice(&n.to_le_bytes());
        let operation = Operation::Membership {
            voters: BTreeSet::from([1, 2, 3]),
            expected: BTreeSet::from([1, 2, 3]),
            expected_revision: 0,
        };
        self_certified(id, identity, operation, &directory, time).unwrap()
    };
    for n in 0..limits.retry_records as u64 {
        let _ = metadata.apply_at(time, &acquire(n, time));
    }
    assert_eq!(
        metadata.apply_at(time, &acquire(u64::MAX, time)),
        Err(CoordinationError::Backpressure),
        "retained membership outcomes, refusals included, fill the retry budget"
    );
    assert!(
        metadata.apply_at(time, &acquire(0, time)).is_ok(),
        "a retry within the window returns the retained outcome"
    );
    let conflict = self_certified(
        id,
        acquire(0, time).id,
        Operation::Close { scope },
        &directory,
        time,
    )
    .unwrap();
    assert_eq!(
        metadata.apply_at(time, &conflict),
        Err(CoordinationError::RequestConflict)
    );
    let later = time + limits.retry_window + 1;
    assert_eq!(
        metadata.apply_at(later, &acquire(0, time)),
        Err(CoordinationError::Expired),
        "after the window a signed request is too old to apply"
    );
    assert_eq!(metadata.retry_records(), 0);
    let _ = metadata.apply_at(later, &acquire(1, later));
    assert_eq!(metadata.retry_records(), 1);
}

#[test]
fn membership_replays_cannot_reverse_a_later_configuration_after_retry_expiry() {
    let (genesis, ..) = certified();
    let authority = PrivateKey::new();
    let root = PrivateKey::new();
    let mut config = genesis.genesis;
    config.membership_authority = authority.public_key();
    let genesis = CertifiedGenesis::sign(config, &root).unwrap();
    let mut metadata = Metadata::with_limits(genesis.clone(), Limits::default()).unwrap();
    let time = now().unwrap();
    let plan = Operation::Membership {
        voters: [1, 2].into(),
        expected: [1, 2, 3].into(),
        expected_revision: 0,
    };
    let sign = |id, operation, time| {
        self_certified(genesis.id().unwrap(), id, operation, &authority, time).unwrap()
    };
    metadata
        .apply_at(time, &sign([1; 32], plan.clone(), time))
        .unwrap();
    metadata
        .apply_at(
            time,
            &sign(
                [2; 32],
                Operation::Membership {
                    voters: [1, 2, 3].into(),
                    expected: [1, 2].into(),
                    expected_revision: 1,
                },
                time,
            ),
        )
        .unwrap();
    let later = time + Limits::default().retry_window + 1;
    assert_eq!(
        metadata.apply_at(later, &sign([1; 32], plan, later)),
        Err(CoordinationError::Configuration)
    );
    assert_eq!(metadata.desired_voters, [1, 2, 3].into());
}

#[tokio::test]
async fn client_follows_authenticated_leader_hints_and_receives_clock_skew() {
    use crate::{
        service::Reply,
        transport::{self, Envelope, Message},
    };
    let (original, executor, _, _) = certified();
    let mut genesis = original.genesis;
    let mut listeners = Vec::new();
    let mut keys = Vec::new();
    for voter in &mut genesis.voters {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let key = PrivateKey::new();
        voter.address = listener.local_addr().unwrap().to_string();
        voter.key = key.public_key();
        listeners.push(listener);
        keys.push(key);
    }
    let root = PrivateKey::new();
    let genesis = CertifiedGenesis::sign(genesis, &root).unwrap();
    let identity = genesis.id().unwrap();
    let visited = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut servers = Vec::new();
    for (index, (listener, key)) in listeners.into_iter().zip(keys).enumerate() {
        let visited = visited.clone();
        servers.push(tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let envelope = transport::read(&mut stream).await.unwrap();
                envelope.authenticate(identity).unwrap();
                let Message::Client(request) = decode(&envelope.body).unwrap() else {
                    panic!("client");
                };
                request.authenticate(identity).unwrap();
                visited.lock().unwrap().push(index + 1);
                let result = if index == 0 {
                    Err(CoordinationError::Unavailable)
                } else {
                    request.fresh().map(|()| Outcome::Closed)
                };
                let response = Envelope::from_body(
                    identity,
                    envelope.nonce,
                    encode(&Reply {
                        leader: Some(3),
                        result,
                    })
                    .unwrap(),
                    &key,
                )
                .unwrap();
                transport::write(&mut stream, &response).await.unwrap();
            }
        }));
    }
    let client = NetworkCoordinator::open(
        genesis.clone(),
        root.public_key(),
        genesis.genesis.mesh,
        executor.clone(),
    )
    .unwrap();
    for _ in 0..2 {
        assert_eq!(
            client.call([1; 32], Operation::Usage).await.unwrap(),
            Outcome::Closed
        );
    }
    assert_eq!(*visited.lock().unwrap(), vec![1, 3, 3]);
    let time = now().unwrap();
    let request =
        self_certified(identity, [2; 32], Operation::Usage, &executor, time + 60).unwrap();
    let reply: Reply = transport::exchange(
        &genesis.genesis.voters[2],
        identity,
        &executor,
        Message::Client(Box::new(request)),
    )
    .await
    .unwrap();
    assert!(
        matches!(reply.result, Err(CoordinationError::ClockSkew { coordinator_time }) if coordinator_time.abs_diff(time) <= 1)
    );
    assert_eq!(
        client.call([2; 32], Operation::Usage).await.unwrap(),
        Outcome::Closed
    );
    assert_eq!(*visited.lock().unwrap(), vec![1, 3, 3, 3, 3]);
    for server in servers {
        server.abort();
    }
}
