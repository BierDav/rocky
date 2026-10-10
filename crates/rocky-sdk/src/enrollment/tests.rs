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
        &issuer.revocations,
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

const TWO: &str = r#"{"format":4,"application":"two","version":1,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]},{"name":"notes","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

/// A new issuer of `schema` with its identity saved at `path`.
fn issuer_at(
    schema: &str,
    directory: &Path,
) -> Result<(Settings, Vault, std::path::PathBuf), Failure> {
    let store: std::sync::Arc<dyn crate::SecretStore> =
        std::sync::Arc::new(crate::FileSecretStore::new(directory.join("secrets")));
    let issuer = Settings::create(&Schema::parse(schema)?, &store, DEFAULT_TTL)?;
    let vault = issuer.vault(&store);
    let path = directory.join("identity.json");
    issuer.save(&path)?;
    Ok((issuer, vault, path))
}

#[test]
fn a_revocation_stopped_at_any_step_completes_with_the_same_statement_and_keys()
-> Result<(), Failure> {
    use fault::Stage;
    // A device without a record may have read every collection, the
    // registry included: three keys.
    let stops = [
        (Stage::Begun, 0),
        (Stage::Key, 0),
        (Stage::Key, 1),
        (Stage::Key, 2),
        (Stage::Signed, 0),
        (Stage::Renewed, 0),
    ];
    for stop in stops {
        let directory = tempfile::tempdir()?;
        let (mut issuer, vault, path) = issuer_at(TWO, directory.path())?;
        let node = PrivateKey::new().public_key();
        issuer.begin_revocation(node, &Registry::default())?;
        issuer.save(&path)?;
        let intent = issuer
            .authority
            .as_ref()
            .and_then(|authority| authority.rotation.clone())
            .ok_or("intent")?;
        assert_eq!(
            intent.keys,
            [
                (crate::domain::REGISTRY.into(), 2),
                ("items".into(), 2),
                ("notes".into(), 2)
            ]
            .into()
        );
        fault::STOP.with(|at| at.set(Some(stop)));
        assert!(issuer.rotate(&vault).is_err(), "{stop:?}");
        // The process stops: what remains is the saved intent and the keys
        // already made.
        let made: Vec<Option<[u8; 32]>> = intent
            .keys
            .iter()
            .map(|(name, epoch)| vault.find(&key_role(name, *epoch)))
            .collect::<Result<_, _>>()?;
        let mut reopened = Settings::load(&path)?;
        let keys = reopened.rotate(&vault)?.ok_or("the intent was lost")?;
        // No completion before the registry transaction has committed.
        assert!(reopened.complete_revocation(&Registry::default()).is_err());
        let mut rows = BTreeMap::new();
        let mut changes = registry::Changes::default();
        changes.revoked(&node)?;
        let completed = Registry::merged(&mut rows, &changes)?;
        reopened.complete_revocation(&completed)?;
        reopened.save(&path)?;
        assert_eq!(reopened.revocations.len(), 1, "{stop:?}");
        assert!(
            reopened.revocations.get(&intent.statement.id()?).is_some(),
            "{stop:?}"
        );
        assert_eq!(keys.len(), 3);
        for ((name, epoch, key), before) in keys.iter().zip(made) {
            assert_eq!(intent.keys.get(name), Some(epoch));
            if let Some(before) = before {
                assert_eq!(before, *key, "{stop:?}: {name}");
            }
            assert_eq!(vault.find(&key_role(name, *epoch))?, Some(*key));
        }
        assert_eq!(reopened.keys["items"], [1, 2]);
        // Completed once: nothing is left to do at the next open.
        assert!(Settings::load(&path)?.rotate(&vault)?.is_none());
    }
    Ok(())
}

#[test]
fn the_1025th_live_statement_is_refused_by_name_and_rotates_nothing() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let (mut issuer, vault, _) = issuer_at(TWO, directory.path())?;
    let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
    let app = issuer.domain()?.manifest().app;
    let other = PrivateKey::new();
    let time = now()?;
    let full = RevocationSet::try_from(
        (0..MAX_LIVE_REVOCATIONS)
            .map(|_| {
                rocky_credentials::SignedRevocation::sign(
                    Revocation {
                        app,
                        issuer: other.public_key(),
                        time,
                        nodes: [PrivateKey::new().public_key()].into(),
                        users: BTreeSet::new(),
                        capabilities: BTreeSet::new(),
                        pseudonyms: BTreeSet::new(),
                    },
                    &publisher,
                    &other,
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    issuer.learn_revocations(&full)?;
    assert_eq!(issuer.revocations.live(time), MAX_LIVE_REVOCATIONS);
    let (keys, groups) = (issuer.keys.clone(), issuer.groups.len());
    let error = issuer
        .begin_revocation(PrivateKey::new().public_key(), &Registry::default())
        .unwrap_err();
    assert!(error.is::<RevocationSetFull>());
    assert_eq!(error.to_string(), REVOCATION_SET_FULL);
    assert_eq!(crate::node::error(&error)["code"], "revocation_set_full");
    assert!(
        issuer
            .authority
            .as_ref()
            .is_some_and(|authority| authority.rotation.is_none())
    );
    assert!(issuer.rotate(&vault)?.is_none());
    assert_eq!((issuer.keys, issuer.groups.len()), (keys, groups));
    Ok(())
}

#[test]
fn a_tombstoned_user_renews_at_no_issuer_also_after_its_statement_left_the_set()
-> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let (mut a, vault, _) = issuer_at(TWO, directory.path())?;
    let me = PrivateKey::from_bytes(&vault.get("node")?).public_key();
    let mut rows = BTreeMap::new();
    let mut registry = Registry::default();
    let mut enroll = |issuer: &mut Settings,
                      registry: &mut Registry|
     -> Result<(PrivateKey, PublicKey, CredentialChain), Failure> {
        let (user, node) = (PrivateKey::new(), PrivateKey::new());
        let (supplied, changes) = issuer.grant(
            &vault,
            DeviceKeys {
                user: user.public_key(),
                node: node.public_key(),
            },
            (
                Profile::Reader {
                    scopes: ["items".into()].into(),
                },
                Terms::default(),
            ),
            None,
            registry,
        )?;
        *registry = Registry::merged(&mut rows, &changes)?;
        Ok((
            user,
            node.public_key(),
            CredentialChain(supplied.credentials.chain.0),
        ))
    };
    let (revoked_user, revoked_node, revoked_root) = enroll(&mut a, &mut registry)?;
    let (kept_user, _, kept_root) = enroll(&mut a, &mut registry)?;
    // B holds the publisher key but never saw either device or its record.
    let mut b = a.clone();
    b.authority = Some(Issuing::default());
    b.revocations = RevocationSet::new();
    a.begin_revocation(revoked_node, &registry)?;
    a.rotate(&vault)?;
    // B learns the statement as a renewal answer brings it.
    b.learn_revocations(&a.revocations)?;
    let mut changes = registry::Changes::default();
    changes.revoked(&revoked_node)?;
    registry = Registry::merged(&mut rows, &changes)?;
    // The statement leaves both sets, as it does a year after it was made.
    let later = now()? + 2 * rocky_credentials::MAX_CHAIN_LIFETIME;
    for issuer in [&mut a, &mut b] {
        issuer.revocations = issuer.revocations.pruned(later);
        assert!(issuer.revocations.is_empty());
    }
    let renew = |issuer: &Settings,
                 registry: &Registry,
                 user: &PrivateKey,
                 root: &CredentialChain|
     -> Result<Vec<u8>, Failure> {
        let fresh = PrivateKey::new();
        let chain = device_chain(root.0[0].clone(), user, fresh.public_key())?;
        let (request, _) = RenewRequest::sign(&fresh, me, chain, now()?)?;
        Ok(issuer.renew(&vault, &request.to_bytes()?, registry)?.0)
    };
    for (issuer, registry) in [(&a, &registry), (&b, &Registry::default())] {
        assert!(renew(issuer, registry, &revoked_user, &revoked_root).is_err());
        renew(issuer, registry, &kept_user, &kept_root)?;
    }
    Ok(())
}
