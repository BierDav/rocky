use super::*;
use p2panda_core::PrivateKey;
#[test]
fn certified_replacement_is_authority_bound_and_survives_metadata_roundtrip() {
    let root = PrivateKey::new();
    let authority = PrivateKey::new();
    let executor = PrivateKey::new();
    let owner = PrincipalId([1; 16]);
    let old = Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    };
    let next = Scope {
        domain: DomainId([4; 16]),
        ..old
    };
    let control = DomainControl {
        scope: old,
        configuration: [5; 16],
        revision: 1,
        owner,
        executors: vec![executor.public_key()],
        invokers: vec![(executor.public_key(), owner)],
        schema: 1,
        handler: 1,
        not_before: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 1,
        expires: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600,
    };
    let directory = ControlCertificate::sign(control.clone(), &authority).unwrap();
    let genesis = CertifiedGenesis::sign(
        Genesis {
            mesh: old.mesh,
            configuration: [5; 16],
            voters: (1..=3)
                .map(|id| Voter {
                    id,
                    address: format!("127.0.0.1:{}", 9000 + id),
                    key: PrivateKey::new().public_key(),
                })
                .collect(),
            initial_voters: [1, 2, 3].into(),
            membership_authority: authority.public_key(),
            directory_definition: [1; 32],
            directory: Some(directory),
            authorities: vec![
                Authority {
                    scope: old,
                    issuer: authority.public_key(),
                },
                Authority {
                    scope: next,
                    issuer: authority.public_key(),
                },
            ],
            max_control_lifetime: 366 * 86400,
            public: None,
        },
        &root,
    )
    .unwrap();
    let mut metadata = Metadata::new(genesis.clone()).unwrap();
    let acquire = Request::sign(
        genesis.id().unwrap(),
        [1; 32],
        Operation::Acquire { scope: old },
        &executor,
    )
    .unwrap();
    let Outcome::Head(head) = metadata.apply(&acquire).unwrap() else {
        panic!("missing acquired head")
    };
    let record = ReplacementRecord {
        lost_head: head.clone(),
        replacement: next,
        acknowledged_loss: true,
        uncertain_effects: true,
    };
    let certificate = ControlCertificate::sign(
        DomainControl {
            scope: next,
            ..control
        },
        &authority,
    )
    .unwrap();
    let unauthorized = Request::sign(
        genesis.id().unwrap(),
        [2; 32],
        Operation::Replace {
            record: record.clone(),
            certificate: certificate.clone(),
        },
        &executor,
    )
    .unwrap();
    assert_eq!(
        metadata.apply(&unauthorized),
        Err(CoordinationError::Denied)
    );
    let replace = Request::sign(
        genesis.id().unwrap(),
        [3; 32],
        Operation::Replace {
            record: record.clone(),
            certificate,
        },
        &authority,
    )
    .unwrap();
    assert!(matches!(
        metadata.apply(&replace).unwrap(),
        Outcome::Replacement(_)
    ));
    let bytes = serde_json::to_vec(&metadata).unwrap();
    let mut restored: Metadata = serde_json::from_slice(&bytes).unwrap();
    assert!(matches!(
        restored.apply(&replace).unwrap(),
        Outcome::Replacement(_)
    ));
    let reacquire = Request::sign(
        genesis.id().unwrap(),
        [4; 32],
        Operation::Acquire { scope: old },
        &executor,
    )
    .unwrap();
    assert_eq!(restored.apply(&reacquire), Err(CoordinationError::Denied));
    let read = Request::sign(
        genesis.id().unwrap(),
        [5; 32],
        Operation::Head { scope: old },
        &executor,
    )
    .unwrap();
    let Outcome::Head(closed) = restored.apply(&read).unwrap() else {
        panic!("missing closed head")
    };
    assert_eq!(closed.sequence, head.sequence);
    assert_eq!(closed.head, head.head);
    let acquire = Request::sign(
        genesis.id().unwrap(),
        [6; 32],
        Operation::Acquire { scope: next },
        &executor,
    )
    .unwrap();
    assert!(matches!(
        restored.apply(&acquire).unwrap(),
        Outcome::Head(_)
    ));
}
