use super::*;
use openraft::{CommittedLeaderId, LogId};

fn evidence(scope: Scope, sequence: u64, key: &PrivateKey) -> ReceiptEvidence {
    let mut confirmation = Confirmation {
        scope,
        caller: fixtures::OWNER,
        configuration: [7; 16],
        executor: fixtures::LAPTOP,
        epoch: 1,
        request: RequestId {
            node: fixtures::PHONE,
            sequence: AuthorSequence::new(sequence).unwrap(),
        },
        request_hash: [1; 32],
        expected_sequence: sequence - 1,
        expected_head: [2; 32],
        schema: 1,
        handler: 1,
        result: [3; 32],
        new_head: [0; 32],
    };
    confirmation.new_head = successor([2; 32], sequence, [1; 32], [3; 32]);
    ReceiptEvidence::sign(
        [4; 32],
        key,
        LogId::new(CommittedLeaderId::new(1, 1), sequence),
        StrongReceipt {
            confirmation,
            sequence,
        },
    )
    .unwrap()
}

#[test]
fn evidence_cache_plateaus_without_losing_scope_watermarks() {
    let key = PrivateKey::new();
    let other = Scope {
        domain: DomainId([9; 16]),
        ..fixtures::SCOPE
    };
    let watermark = evidence(other, 1, &key);
    assert!(encode(&watermark).unwrap().len() <= 4096);
    let current = ClientCache {
        format: CACHE_FORMAT,
        settled: Vec::new(),
        genesis: [4; 32],
        receipts: vec![watermark.clone()],
        heads: Vec::new(),
    };
    let mut cache = ClientCache::read(&encode(&current).unwrap()).unwrap();
    assert_eq!(cache.format, CACHE_FORMAT);
    for sequence in 1..=MAX_EVIDENCE as u64 + 32 {
        cache
            .insert(evidence(fixtures::SCOPE, sequence, &key), fixtures::LAPTOP)
            .unwrap();
        assert!(cache.receipts.len() <= MAX_EVIDENCE);
    }
    assert!(cache.receipts.contains(&watermark));
    let newest = cache
        .receipts
        .iter()
        .map(|e| e.receipt.sequence)
        .max()
        .unwrap();
    assert_eq!(newest, MAX_EVIDENCE as u64 + 32);
    assert_eq!(cache.receipts.len(), MAX_EVIDENCE);
    let roundtrip = ClientCache::read(&encode(&cache).unwrap()).unwrap();
    assert_eq!(roundtrip.receipts, cache.receipts);
    cache.format += 1;
    assert!(matches!(
        ClientCache::read(&encode(&cache).unwrap()),
        Err(CoordinationError::UnsupportedFormat)
    ));
}

#[test]
fn unacknowledged_own_evidence_is_never_evicted() {
    let key = PrivateKey::new();
    // The requests are the phone's own: nothing it did not acknowledge goes.
    let mut cache = ClientCache {
        format: CACHE_FORMAT,
        genesis: [4; 32],
        receipts: Vec::new(),
        heads: Vec::new(),
        settled: Vec::new(),
    };
    for sequence in 1..=MAX_EVIDENCE as u64 {
        cache
            .insert(evidence(fixtures::SCOPE, sequence, &key), fixtures::PHONE)
            .unwrap();
    }
    let next = evidence(fixtures::SCOPE, MAX_EVIDENCE as u64 + 1, &key);
    assert!(matches!(
        cache.insert(next.clone(), fixtures::PHONE),
        Err(CoordinationError::Backpressure)
    ));
    assert_eq!(cache.receipts.len(), MAX_EVIDENCE);
    // Acknowledging through 10 releases exactly those.
    cache.settle(fixtures::SCOPE, 10);
    cache.insert(next, fixtures::PHONE).unwrap();
    assert!(!cache.receipts.iter().any(|e| e.receipt.sequence == 1));
    assert!(cache.receipts.iter().any(|e| e.receipt.sequence == 11));
    let roundtrip = ClientCache::read(&encode(&cache).unwrap()).unwrap();
    assert_eq!(roundtrip.settled, vec![(fixtures::SCOPE, 10)]);
}

#[test]
fn evidence_cache_refuses_every_other_version_and_missing_floors() {
    let mut cache = ClientCache {
        format: CACHE_FORMAT,
        genesis: [4; 32],
        receipts: Vec::new(),
        heads: Vec::new(),
        settled: Vec::new(),
    };
    for format in [0, 1, 2, 4, u32::MAX] {
        cache.format = format;
        assert!(matches!(
            ClientCache::read(&encode(&cache).unwrap()),
            Err(CoordinationError::UnsupportedFormat)
        ));
    }
    #[derive(Serialize)]
    struct MissingFloors {
        format: u32,
        genesis: Commitment,
        receipts: Vec<ReceiptEvidence>,
        heads: Vec<StrongHead>,
    }
    let incomplete = MissingFloors {
        format: CACHE_FORMAT,
        genesis: cache.genesis,
        receipts: cache.receipts,
        heads: cache.heads,
    };
    assert!(ClientCache::read(&encode(&incomplete).unwrap()).is_err());
}

#[test]
fn oversized_cache_backpressures_without_replacing_durable_evidence() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("evidence");
    let mut cache = ClientCache {
        format: CACHE_FORMAT,
        genesis: [4; 32],
        receipts: Vec::new(),
        heads: Vec::new(),
        settled: Vec::new(),
    };
    persist_cache(&path, &cache).unwrap();
    let before = std::fs::read(&path).unwrap();
    cache.settled = (0..MAX_STORE / 64)
        .map(|index| {
            (
                Scope {
                    domain: DomainId((index as u128).to_le_bytes()),
                    ..fixtures::SCOPE
                },
                1,
            )
        })
        .collect();
    assert!(matches!(
        persist_cache(&path, &cache),
        Err(CoordinationError::Backpressure)
    ));
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn eviction_preserves_equal_newest_watermarks_and_pinned_requests() {
    let key = PrivateKey::new();
    let other = Scope {
        domain: DomainId([9; 16]),
        ..fixtures::SCOPE
    };
    let mut newest = evidence(other, 7, &key);
    let mut tied = newest.clone();
    tied.receipt.confirmation.request_hash = [9; 32];
    let mut pinned = evidence(fixtures::SCOPE, 1, &key);
    pinned.receipt.confirmation.request.node = fixtures::LAPTOP;
    let evictable = evidence(fixtures::SCOPE, 2, &key);
    let mut cache = ClientCache {
        format: CACHE_FORMAT,
        genesis: [4; 32],
        heads: Vec::new(),
        settled: Vec::new(),
        receipts: vec![
            newest.clone(),
            tied.clone(),
            pinned.clone(),
            evictable.clone(),
        ],
    };
    newest = evidence(fixtures::SCOPE, 100, &key);
    cache.receipts.resize(MAX_EVIDENCE, newest.clone());
    cache
        .insert(evidence(fixtures::SCOPE, 101, &key), fixtures::LAPTOP)
        .unwrap();
    assert!(cache.receipts.contains(&pinned));
    assert!(cache.receipts.contains(&tied));
    assert!(cache.receipts.contains(&newest));
    assert!(!cache.receipts.contains(&evictable));
}

fn public_genesis() -> CertifiedGenesis {
    let root = PrivateKey::new();
    CertifiedGenesis::sign(
        Genesis {
            mesh: fixtures::SCOPE.mesh,
            configuration: [7; 16],
            voters: (1..=3)
                .map(|id| Voter {
                    id,
                    address: format!("127.0.0.1:{}", 10000 + id),
                    key: PrivateKey::new().public_key(),
                })
                .collect(),
            initial_voters: std::collections::BTreeSet::from([1, 2, 3]),
            membership_authority: root.public_key(),
            directory_definition: [0; 32],
            directory: None,
            authorities: Vec::new(),
            max_control_lifetime: 86400,
            public: Some(PublicService {
                endpoints: vec![ClientEndpoint {
                    voter: 1,
                    kind: "test".into(),
                    address: "test".into(),
                }],
                admission: None,
                limits: crate::Limits::default(),
                tenants: 1,
                registrations_per_day: 4096,
                tenant_lanes: 1,
            }),
        },
        &root,
    )
    .unwrap()
}

#[tokio::test]
async fn observed_heads_reopen_at_65_and_the_public_cap_without_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("heads.evidence");
    let genesis = public_genesis();
    let key = PrivateKey::new();
    let open = || {
        NetworkCoordinator::open(
            genesis.clone(),
            genesis.root,
            genesis.genesis.mesh,
            key.clone(),
            path.clone(),
        )
        .unwrap()
    };
    let mut client = open();
    let head = |index: usize| StrongHead {
        scope: Scope {
            domain: DomainId((index as u128).to_le_bytes()),
            ..fixtures::SCOPE
        },
        configuration: [7; 16],
        executor: fixtures::LAPTOP,
        epoch: 2,
        sequence: 1,
        head: [9; 32],
        schema: 1,
        handler: 1,
    };
    {
        let mut cache = client.cache.lock().await;
        let mut updated = cache.clone();
        updated.heads.extend((0..64).map(head));
        client.persist(&updated).unwrap();
        *cache = updated;
    }
    client.observe_head(&head(64)).await.unwrap();
    drop(client);
    client = open();
    assert_eq!(client.cache.lock().await.heads.len(), 65);
    // Seed the observed-head snapshot, then exercise insertion at the cap.
    // Avoid thousands of whole-file fsyncs in the regression test.
    {
        let mut cache = client.cache.lock().await;
        let mut updated = cache.clone();
        updated.heads.extend((65..MAX_PUBLIC_DOMAINS - 1).map(head));
        client.persist(&updated).unwrap();
        *cache = updated;
    }
    client
        .observe_head(&head(MAX_PUBLIC_DOMAINS - 1))
        .await
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    eprintln!(
        "4096 cached heads: {} bytes (store cap {})",
        bytes.len(),
        MAX_STORE
    );
    assert!(bytes.len() <= MAX_STORE);
    assert_eq!(
        client.observe_head(&head(MAX_PUBLIC_DOMAINS)).await,
        Err(CoordinationError::Configuration)
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    drop(client);
    let client = open();
    assert_eq!(client.cache.lock().await.heads.len(), MAX_PUBLIC_DOMAINS);
    let mut lower = head(0);
    lower.epoch -= 1;
    assert_eq!(
        client.observe_head(&lower).await,
        Err(CoordinationError::CoordinationStateLost)
    );
    lower = head(0);
    lower.sequence -= 1;
    assert_eq!(
        client.observe_head(&lower).await,
        Err(CoordinationError::CoordinationStateLost)
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let mut oversized = client.cache.lock().await.clone();
    oversized.heads.push(head(MAX_PUBLIC_DOMAINS));
    persist_cache(&path, &oversized).unwrap();
    let oversized_bytes = std::fs::read(&path).unwrap();
    drop(client);
    assert!(matches!(
        NetworkCoordinator::open(
            genesis.clone(),
            genesis.root,
            genesis.genesis.mesh,
            key,
            path.clone()
        ),
        Err(CoordinationError::Configuration)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), oversized_bytes);
}
