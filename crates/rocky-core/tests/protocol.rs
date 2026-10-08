use rocky_core::*;

fn id(node: NodeId, sequence: u64) -> OperationId {
    OperationId {
        node,
        sequence: AuthorSequence::new(sequence).unwrap(),
    }
}
fn envelope() -> Envelope {
    Envelope {
        scope: fixtures::SCOPE,
        caller: fixtures::OWNER,
        versions: Versions::V1,
        id: id(fixtures::LAPTOP, 1),
        entity: id(fixtures::LAPTOP, 1),
        lamport: LamportTime::new(1).unwrap(),
        dependencies: vec![],
        payload: vec![0, 1, 0, 0, 0, b'A', 0],
    }
}
fn hex(input: &str) -> Vec<u8> {
    input
        .split_whitespace()
        .flat_map(|line| {
            line.as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        })
        .collect()
}
#[test]
fn version_one_golden_frames() {
    let create = envelope();
    let bytes = hex(include_str!("fixtures/create-v1.hex"));
    assert_eq!(create.encode().unwrap(), bytes);
    assert_eq!(Envelope::decode(&bytes).unwrap(), create);
    let mut setter = create.clone();
    setter.id = id(fixtures::PHONE, 2);
    setter.lamport = LamportTime::new(3).unwrap();
    setter.dependencies = vec![create.id, id(fixtures::PHONE, 1)];
    setter.payload = vec![1, 1, 0, 0, 0, b'B'];
    let bytes = hex(include_str!("fixtures/title-v1.hex"));
    assert_eq!(setter.encode().unwrap(), bytes);
    assert_eq!(Envelope::decode(&bytes).unwrap(), setter);
}
#[test]
fn malformed_frames_fail_explicitly() {
    let bytes = envelope().encode().unwrap();
    for end in 0..bytes.len() {
        assert!(Envelope::decode(&bytes[..end]).is_err(), "prefix {end}");
    }
    let mut frame = bytes.clone();
    frame.push(0);
    assert_eq!(Envelope::decode(&frame), Err(ProtocolError::TrailingBytes));
    for (offset, kind) in [
        (64, VersionKind::Protocol),
        (68, VersionKind::Schema),
        (72, VersionKind::Operation),
    ] {
        let mut frame = bytes.clone();
        frame[offset..offset + 4].copy_from_slice(&2_u32.to_le_bytes());
        assert_eq!(
            Envelope::decode(&frame),
            Err(ProtocolError::UnsupportedVersion { kind, value: 2 })
        );
    }
    for offset in [92, 100, 124] {
        let mut frame = bytes.clone();
        frame[offset..offset + 8].copy_from_slice(&0_u64.to_le_bytes());
        assert_eq!(Envelope::decode(&frame), Err(ProtocolError::InvalidCounter));
        frame[offset..offset + 8].copy_from_slice(&(MAX_COUNTER + 1).to_le_bytes());
        assert_eq!(Envelope::decode(&frame), Err(ProtocolError::InvalidCounter));
    }
    let mut frame = bytes.clone();
    frame[132..136].copy_from_slice(&3_u32.to_le_bytes());
    assert_eq!(
        Envelope::decode(&frame),
        Err(ProtocolError::InvalidDependencies)
    );
    let mut frame = bytes;
    frame[136..140].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(Envelope::decode(&frame), Err(ProtocolError::InvalidLength));
}
#[test]
fn dependency_shape_and_counter_limits() {
    assert!(AuthorSequence::new(0).is_err());
    assert!(LamportTime::new(0).is_err());
    assert_eq!(LocalRevision::default().get(), 0);
    assert!(LocalRevision::new(MAX_COUNTER).unwrap().next().is_err());
    assert!(AuthorSequence::new(MAX_COUNTER).unwrap().next().is_err());
    assert!(LamportTime::new(MAX_COUNTER).unwrap().next().is_err());
    let mut operation = envelope();
    operation.id = id(fixtures::PHONE, 2);
    operation.dependencies = required_dependencies(operation.id, operation.entity, false).unwrap();
    assert_eq!(
        operation.dependencies,
        vec![operation.entity, id(fixtures::PHONE, 1)]
    );
    assert!(operation.validate_dependencies(false).is_ok());
    operation.dependencies.reverse();
    assert_eq!(
        operation.validate(),
        Err(ProtocolError::InvalidDependencies)
    );
    operation.dependencies = vec![operation.id];
    assert!(operation.validate().is_err());
    operation.dependencies = vec![id(fixtures::PHONE, 3)];
    assert!(operation.validate().is_err());
    operation.dependencies = vec![operation.entity, operation.entity];
    assert!(operation.validate().is_err());
    operation.dependencies.clear();
    assert!(operation.validate().is_err());
    operation.id = id(fixtures::PHONE, 1);
    assert!(operation.validate_dependencies(false).is_err());
    assert!(operation.validate_dependencies(true).is_err());
}
#[test]
fn readiness_identity_clocks_and_contiguous_frontier() {
    let create = envelope();
    let mut successor = create.clone();
    successor.id = id(fixtures::LAPTOP, 2);
    successor.dependencies = vec![create.id];
    successor.lamport = LamportTime::new(2).unwrap();
    let mut history = AppliedOperations::new(fixtures::SCOPE);
    assert_eq!(
        history.classify(&successor).unwrap(),
        Delivery::Pending(vec![create.id])
    );
    assert_eq!(
        history.record_applied(&successor),
        Err(ProtocolError::NotReady(vec![create.id]))
    );
    history.record_applied(&create).unwrap();
    assert_eq!(history.classify(&create).unwrap(), Delivery::Duplicate);
    let mut conflict = create.clone();
    conflict.payload.push(9);
    assert_eq!(
        history.classify(&conflict),
        Err(ProtocolError::IdentityConflict(create.id))
    );
    let mut invalid = successor.clone();
    invalid.lamport = create.lamport;
    assert_eq!(
        history.classify(&invalid),
        Err(ProtocolError::InvalidLamport {
            operation: invalid.id,
            dependency: create.id
        })
    );
    history.record_applied(&successor).unwrap();
    let mut phone = envelope();
    phone.id = id(fixtures::PHONE, 1);
    phone.entity = phone.id;
    history.record_applied(&phone).unwrap();
    assert_eq!(
        history.frontier(),
        AppliedFrontier(vec![
            (fixtures::LAPTOP, AuthorSequence::new(2).unwrap()),
            (fixtures::PHONE, AuthorSequence::new(1).unwrap())
        ])
    );
    assert_eq!(
        history.canonical_bytes(create.id).unwrap(),
        create.encode().unwrap()
    );
    let mut foreign = phone;
    foreign.scope.domain = DomainId([9; 16]);
    assert_eq!(history.classify(&foreign), Err(ProtocolError::ForeignScope));
}
