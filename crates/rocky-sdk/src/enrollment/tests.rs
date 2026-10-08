use super::*;
use rocky_credentials::PartitionGrant;

const SCHEMA: &str = r#"{"format":4,"application":"holders","version":1,"tables":[{"name":"lists","class":"mergeable","columns":[{"name":"name","type":"text","not_null":true}],"primary_key":["name"],"partition_by":["name"],"partition_group":"list"}]}"#;

#[test]
fn revoking_a_device_known_only_by_its_grant_ends_its_user_under_any_node_key()
-> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let store: std::sync::Arc<dyn crate::SecretStore> = std::sync::Arc::new(
        crate::FileSecretStore::new(directory.path().join("secrets")),
    );
    let mut issuer = Settings::create(&Schema::parse(SCHEMA)?, &store, DEFAULT_TTL)?;
    let vault = issuer.vault(&store);
    let domain = issuer.domain()?;
    let logical = domain.logical();
    let lists = logical.table("lists").ok_or("lists")?;
    let column = lists.column("name").ok_or("name")?;
    let grants = vec![PartitionGrant {
        domain: domain.scope,
        table: logical.table_id(lists),
        selector: rocky_core::PartitionSelector(
            [(
                logical.field_id(lists, column),
                rocky_core::PartitionSelection::Any,
            )]
            .into(),
        ),
        rows: RowSelection::All,
        rights: [Right::Read].into(),
        fields: BTreeSet::new(),
    }];
    let (user, node) = (PrivateKey::new(), PrivateKey::new());
    let device = DeviceKeys {
        user: user.public_key(),
        node: node.public_key(),
    };
    let capability = Authority {
        publisher: &PrivateKey::from_bytes(&vault.get("publisher")?),
        manifest: domain.manifest().hash(),
    }
    .issue_partition_terms(
        user.public_key(),
        vec![],
        grants.clone(),
        (1, None),
        issuer.policy.statement.epoch,
        issuer.validity()?,
    )?;
    issuer.record_holder(&device, &grants)?;
    let me = PrivateKey::from_bytes(&vault.get("node")?).public_key();
    let renew =
        |issuer: &mut Settings, node: &PrivateKey, path: &str| -> Result<Vec<u8>, Failure> {
            let chain = device_chain(capability.clone(), &user, node.public_key())?;
            let (request, _) = RenewRequest::sign(node, me, chain, now()?)?;
            issuer.renew(&directory.path().join(path), &vault, &request.to_bytes()?)
        };
    // Before revocation the same request renews, also under a new node key.
    renew(&mut issuer.clone(), &node, "control.json")?;
    renew(&mut issuer.clone(), &PrivateKey::new(), "control.json")?;

    issuer.begin_revocation(node.public_key())?;
    issuer.rotate(&vault)?;
    assert!(issuer.revoked_device(&node.public_key()));
    assert!(renew(&mut issuer, &node, "identity.json").is_err());
    assert!(renew(&mut issuer, &PrivateKey::new(), "identity.json").is_err());
    let renamed = DeviceKeys {
        user: user.public_key(),
        node: PrivateKey::new().public_key(),
    };
    assert!(issuer.record_holder(&renamed, &grants).is_err());
    Ok(())
}

#[test]
fn contact_versions_are_independent_of_routing_flags() {
    let key = PrivateKey::new().public_key();
    let mut record = ContactRecord {
        v: CONTACT_VERSION,
        publisher: key,
        node: key,
        routes: Vec::new(),
        issuer: true,
        enrollment_only: false,
    };
    let uri = |record: &ContactRecord| {
        format!(
            "rocky://peer/{}",
            BASE64URL_NOPAD.encode(&serde_json::to_vec(record).unwrap())
        )
    };
    for enrollment_only in [false, true] {
        record.enrollment_only = enrollment_only;
        assert_eq!(
            ContactRecord::parse(&uri(&record)).unwrap().enrollment_only,
            enrollment_only
        );
    }
    for version in [0, 1, 3, u32::MAX] {
        record.v = version;
        assert_eq!(
            ContactRecord::parse(&uri(&record))
                .err()
                .unwrap()
                .to_string(),
            "unsupported contact version"
        );
    }
}
