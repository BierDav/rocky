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
    let mut rows = BTreeMap::new();
    let changes = issuer.holder_changes(&vault, &device, &grants, 1, &Registry::default())?;
    let registry = Registry::merged(&mut rows, &changes)?;
    assert_eq!(registry.devices(), 1);
    assert_eq!(registry.get(&device.node).ok_or("record")?.grants.len(), 1);
    let me = PrivateKey::from_bytes(&vault.get("node")?).public_key();
    let renew = |issuer: &Settings, node: &PrivateKey| -> Result<Vec<u8>, Failure> {
        let chain = device_chain(capability.clone(), &user, node.public_key())?;
        let (request, _) = RenewRequest::sign(node, me, chain, now()?)?;
        Ok(issuer.renew(&vault, &request.to_bytes()?, &registry)?.0)
    };
    // Before revocation the same request renews, also under a new node key.
    renew(&issuer, &node)?;
    renew(&issuer, &PrivateKey::new())?;

    issuer.begin_revocation(node.public_key(), &registry)?;
    issuer.rotate(&vault)?;
    assert!(issuer.revoked_device(&node.public_key(), &registry));
    assert!(renew(&issuer, &node).is_err());
    assert!(renew(&issuer, &PrivateKey::new()).is_err());
    let renamed = DeviceKeys {
        user: user.public_key(),
        node: PrivateKey::new().public_key(),
    };
    assert!(
        issuer
            .holder_changes(&vault, &renamed, &grants, 1, &registry)
            .is_err()
    );
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

#[test]
fn a_granted_profile_reads_every_partition_as_an_enrolled_one_does() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let store: std::sync::Arc<dyn crate::SecretStore> = std::sync::Arc::new(
        crate::FileSecretStore::new(directory.path().join("secrets")),
    );
    let mut issuer = Settings::create(&Schema::parse(SCHEMA)?, &store, DEFAULT_TTL)?;
    let vault = issuer.vault(&store);
    let (user, node) = (PrivateKey::new(), PrivateKey::new());
    let device = DeviceKeys {
        user: user.public_key(),
        node: node.public_key(),
    };
    let (supplied, _) = issuer.grant(
        &vault,
        device,
        (
            Profile::Reader {
                scopes: ["lists".into()].into(),
            },
            Terms::default(),
        ),
        None,
        &Registry::default(),
    )?;
    let domain = issuer.domain()?;
    let capability = supplied
        .credentials
        .chain
        .0
        .first()
        .cloned()
        .ok_or("capability")?;
    let chain = device_chain(capability, &user, node.public_key())?;
    let (_, selectors) = chain.verify_selectors(
        issuer.publisher,
        domain.manifest().hash(),
        node.public_key(),
        &issuer.policy.statement,
        now()?,
    )?;
    let lists = domain.logical().table("lists").ok_or("lists")?;
    assert_eq!(selectors.len(), 1);
    assert_eq!(selectors[0].table, domain.logical().table_id(lists));
    assert!(
        selectors[0]
            .selector
            .0
            .values()
            .all(|selection| *selection == rocky_core::PartitionSelection::Any)
    );
    assert!(selectors[0].rights.contains(&Right::Read));
    Ok(())
}

#[test]
fn registry_grants_follow_only_the_issuer_and_custody_profiles() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let store: std::sync::Arc<dyn crate::SecretStore> = std::sync::Arc::new(
        crate::FileSecretStore::new(directory.path().join("secrets")),
    );
    let issuer = Settings::create(&Schema::parse(SCHEMA)?, &store, DEFAULT_TTL)?;
    let domain = issuer.domain()?;
    let registry = domain.registry();
    let lists: std::collections::BTreeSet<String> = ["lists".into()].into();
    for (profile, rights) in [
        (Profile::Issuer, Some(Right::Insert)),
        (
            Profile::Custody {
                scopes: lists.clone(),
            },
            Some(Right::Custody),
        ),
        (
            Profile::Writer {
                scopes: lists.clone(),
            },
            None,
        ),
        (Profile::Reader { scopes: lists }, None),
    ] {
        let grants = grants_of(&profile, &domain)?;
        let held = grants.iter().find(|grant| grant.scope == registry);
        assert_eq!(held.is_some(), rights.is_some(), "{profile:?}");
        if let (Some(held), Some(right)) = (held, rights) {
            assert!(held.rights.contains(&right));
            assert_eq!(held.rights.contains(&Right::Read), profile.issues());
        }
        // A renewal without a record recognizes the same profile.
        assert_eq!(profile_of(&grants, &domain), profile);
    }
    // No raw grant or profile names the registry itself.
    let named = Profile::Reader {
        scopes: [crate::domain::REGISTRY.into()].into(),
    };
    assert!(grants_of(&named, &domain).is_err());
    assert!(issuer.keys.contains_key(crate::domain::REGISTRY));
    Ok(())
}
