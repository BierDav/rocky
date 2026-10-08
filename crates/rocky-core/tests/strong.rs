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
        cut_base: AppliedFrontier::default(),
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
        "de8e2010001f02e68e1a598eb9a34bd5a82c047a67ba3f6917716cb557d2859c"
    );
    assert_eq!(
        hex(&c.new_head),
        "14043bf2f8fda50c1c9639ec6ba7635a83f1f0dc80a7bc69369b068b4329081f"
    );
    assert_eq!(
        StrongRequest::decode(&request.encode().unwrap()).unwrap(),
        request
    );
    assert_eq!(request.encode().unwrap().len(), 159);
    let bytes = p.encode().unwrap();
    assert_eq!(bytes.len(), 291);
    assert_eq!(StrongPackage::decode(&bytes).unwrap(), p);
    for n in 0..bytes.len() {
        assert!(StrongPackage::decode(&bytes[..n]).is_err());
    }
    let mut extra = bytes.clone();
    extra.push(0);
    assert_eq!(
        StrongPackage::decode(&extra),
        Err(ProtocolError::TrailingBytes)
    );
    // Any other format is refused, including the earlier one without magic.
    assert!(StrongPackage::decode(&bytes[16..]).is_err());
    let mut other = bytes;
    other[15] = b'1';
    assert!(StrongPackage::decode(&other).is_err());
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

fn frontier(entries: &[(NodeId, u64)]) -> AppliedFrontier {
    AppliedFrontier(
        entries
            .iter()
            .map(|(node, sequence)| (*node, AuthorSequence::new(*sequence).unwrap()))
            .collect(),
    )
}

#[test]
fn a_cut_covers_only_the_operations_after_its_base() {
    let (laptop, phone) = (fixtures::LAPTOP, fixtures::PHONE);
    let cut = frontier(&[(laptop, 100_000), (phone, 3)]);
    let base = frontier(&[(laptop, 99_998)]);
    assert_eq!(cut_size(&base, &cut), Ok(5));
    let ids = cut_operations(&base, &cut).unwrap();
    let named: Vec<_> = ids.iter().map(|id| (id.node, id.sequence.get())).collect();
    assert_eq!(
        named,
        [
            (laptop, 99_999),
            (laptop, 100_000),
            (phone, 1),
            (phone, 2),
            (phone, 3)
        ]
    );
    // Without a base the lifetime counts, and is refused past the bound.
    assert_eq!(cut_size(&AppliedFrontier::default(), &cut), Ok(100_003));
    assert_eq!(
        cut_operations(&AppliedFrontier::default(), &cut),
        Err(ProtocolError::InvalidLength)
    );
    let edge = frontier(&[(laptop, 99_998 + MAX_CUT_OPERATIONS as u64)]);
    assert_eq!(
        cut_operations(&base, &edge).unwrap().len(),
        MAX_CUT_OPERATIONS
    );
    // A base ahead of the cut, or naming an author outside it, is refused.
    for base in [
        frontier(&[(laptop, 100_001)]),
        frontier(&[(NodeId([0x77; 16]), 1)]),
    ] {
        assert_eq!(
            cut_size(&base, &cut),
            Err(ProtocolError::InvalidDependencies)
        );
    }
}

#[test]
fn a_package_names_its_base_and_refuses_one_outside_the_bound() {
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
        frontier: frontier(&[(fixtures::LAPTOP, 200_000)]),
        digest: [1; 32],
    };
    let mut package = StrongPackage {
        request,
        base: StrongHead {
            scope: fixtures::SCOPE,
            configuration: [7; 16],
            executor: fixtures::LAPTOP,
            epoch: 1,
            schema: 1,
            handler: 1,
            sequence: 0,
            head: genesis(fixtures::SCOPE, [7; 16]),
        },
        sequence: 1,
        rows: vec![0; 4],
        cut_base: frontier(&[(fixtures::LAPTOP, 199_990)]),
    };
    let bytes = package.encode().unwrap();
    let decoded = StrongPackage::decode(&bytes).unwrap();
    assert_eq!(decoded, package);
    assert_eq!(decoded.required().unwrap().len(), 10);
    assert_eq!(
        StrongPackage::request_bytes(&bytes),
        Some(package.request.encode().unwrap().as_slice())
    );
    // The base is bound: another one is another result.
    let mut moved = package.clone();
    moved.cut_base = frontier(&[(fixtures::LAPTOP, 199_991)]);
    assert_ne!(
        moved.confirmation().unwrap().result,
        package.confirmation().unwrap().result
    );
    package.cut_base = frontier(&[(fixtures::LAPTOP, 1)]);
    assert_eq!(package.encode(), Err(ProtocolError::InvalidLength));
    package.cut_base = frontier(&[(fixtures::LAPTOP, 200_001)]);
    assert_eq!(package.encode(), Err(ProtocolError::InvalidDependencies));
}
