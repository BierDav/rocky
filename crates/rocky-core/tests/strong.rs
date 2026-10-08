use rocky_core::*;
fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}
#[test]
fn version_one_commitment_vectors_and_strict_frames() {
    let head = genesis(fixtures::SCOPE, [7; 16]);
    let digest = state_commitment(&[0; 4]);
    let request = StrongRequest {
        scope: fixtures::SCOPE,
        caller: fixtures::OWNER,
        id: RequestId {
            node: fixtures::PHONE,
            sequence: AuthorSequence::new(1).unwrap(),
        },
        method: "PublishListSnapshot".into(),
        version: 1,
        schema: 1,
        handler: 1,
        client_request: [0; 32],
        arguments: Vec::new(),
        policy: [0; 32],
        frontier: AppliedFrontier::default(),
        digest,
    };
    let p = StrongPackage {
        request: request.clone(),
        base: StrongHead {
            scope: fixtures::SCOPE,
            configuration: [7; 16],
            executor: fixtures::LAPTOP,
            epoch: 1,
            schema: 1,
            handler: 1,
            sequence: 0,
            head,
        },
        sequence: 1,
        rows: vec![0; 4],
        required: Vec::new(),
    };
    let c = p.confirmation().unwrap();
    assert_eq!(
        hex(&head),
        "48ead3a3d0babf04aee931f6f43e86c2f38453111304988dd63867dcdf87469a"
    );
    assert_eq!(
        hex(&digest),
        "6300e5b2723812489cc0ab00decd4030a15bee4543acde071b091ee2b9c9ee70"
    );
    assert_eq!(
        hex(&request.hash().unwrap()),
        "064b3440f48ab38f1c0e6fbb7423f496cb0e2ea332ed4fda5698e407f5ac11b8"
    );
    assert_eq!(
        hex(&c.result),
        "29c1567673ac5baf7f7a80a62b6b08dddba29ad44cc87aeed63028756537c4ea"
    );
    assert_eq!(
        hex(&c.new_head),
        "af37782eb3a0cb627934a1678f13649762a60b80983694227cbce08212176a6b"
    );
    assert_eq!(
        StrongRequest::decode(&request.encode().unwrap()).unwrap(),
        request
    );
    assert_eq!(request.encode().unwrap().len(), 159);
    let bytes = p.encode().unwrap();
    assert_eq!(bytes.len(), 275);
    assert_eq!(StrongPackage::decode(&bytes).unwrap(), p);
    for n in 0..bytes.len() {
        assert!(StrongPackage::decode(&bytes[..n]).is_err());
    }
    let mut extra = bytes;
    extra.push(0);
    assert_eq!(
        StrongPackage::decode(&extra),
        Err(ProtocolError::TrailingBytes)
    );
    let receipt = StrongReceipt {
        confirmation: c,
        sequence: 1,
    };
    assert_eq!(StrongReceipt::decode(&receipt.encode()).unwrap(), receipt);
}

#[test]
fn extended_requests_bind_caller_identity_arguments_and_policy_strictly() {
    let base = StrongRequest {
        scope: fixtures::SCOPE,
        caller: fixtures::OWNER,
        id: RequestId {
            node: fixtures::LAPTOP,
            sequence: AuthorSequence::new(1).unwrap(),
        },
        method: "ReserveStock".into(),
        version: 1,
        schema: 1,
        handler: 1,
        client_request: [0; 32],
        arguments: Vec::new(),
        policy: [0; 32],
        frontier: AppliedFrontier::default(),
        digest: [1; 32],
    };
    let prefix = base.encode().unwrap().len();
    let mut request = base.clone();
    request.arguments = 7u64.to_le_bytes().to_vec();
    request.policy = [9; 32];
    request.client_request = [8; 32];
    let bytes = request.encode().unwrap();
    assert_eq!(StrongRequest::decode(&bytes).unwrap(), request);
    for length in prefix + 1..bytes.len() {
        assert!(StrongRequest::decode(&bytes[..length]).is_err());
    }
    let mut changed = request.clone();
    changed.arguments[0] += 1;
    assert_ne!(changed.hash().unwrap(), request.hash().unwrap());
    changed = request.clone();
    changed.policy[0] += 1;
    assert_ne!(changed.hash().unwrap(), request.hash().unwrap());
    changed = request;
    changed.client_request[0] += 1;
    assert_ne!(
        changed.hash().unwrap(),
        StrongRequest::decode(&bytes).unwrap().hash().unwrap()
    );
    let mut unknown = bytes;
    unknown[prefix..prefix + 4].copy_from_slice(&3u32.to_le_bytes());
    assert!(StrongRequest::decode(&unknown).is_err());
    let mut redundant = base.encode().unwrap();
    redundant.extend(2u32.to_le_bytes());
    redundant.extend(0u32.to_le_bytes());
    redundant.extend([0; 64]);
    assert!(StrongRequest::decode(&redundant).is_err());
}
#[test]
fn request_sequences_carry_issue_time_and_strictly_increase() {
    let now = 1_800_000_000;
    let first = next_request_sequence(7, now).unwrap().get();
    assert_eq!(issued_at(first), now);
    let second = next_request_sequence(first, now).unwrap().get();
    assert_eq!((second, issued_at(second)), (first + 1, now));
    // A clock that moved backwards keeps the sequence increasing.
    let third = next_request_sequence(second, now - 60).unwrap().get();
    assert_eq!((third, issued_at(third)), (second + 1, now));
    assert!(next_request_sequence(0, u64::MAX >> 8).is_err());
}
