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
