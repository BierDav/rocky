use super::*;
use rocky_credentials::PartitionGrant;
use serde_json::{Value, json};

/// `links`, which end at `user`, completed with a certificate for `node`.
fn member_chain(
    links: &CredentialChain,
    user: &PrivateKey,
    node: PublicKey,
) -> Result<CredentialChain, Failure> {
    let first = links.authors()?.ok_or("principal range")?.first;
    Ok(links.clone().complete(user, node, first)?)
}

const SCHEMA: &str = r#"{"format":6,"application":"holders","version":1,"tables":[{"name":"lists","class":"replayed","columns":[{"name":"name","type":"text","not_null":true}],"primary_key":["name"],"partition_by":["name"],"partition_group":"list"}]}"#;

/// The registry and an empty membership view.
fn known(registry: &Registry) -> (&Registry, &'static Membership) {
    static EMPTY: std::sync::LazyLock<Membership> = std::sync::LazyLock::new(Membership::default);
    (registry, &EMPTY)
}

#[test]
fn revoking_an_identity_known_only_by_its_grant_ends_it_under_any_node_key() -> Result<(), Failure>
{
    let directory = tempfile::tempdir()?;
    let store: std::sync::Arc<dyn crate::SecretStore> = std::sync::Arc::new(
        crate::FileSecretStore::new(directory.path().join("secrets")),
    );
    let mut issuer = Settings::create(
        &Schema::parse(SCHEMA)?,
        &store,
        DEFAULT_TTL,
        None,
        &rocky_credentials::RootKey::generate(),
    )?;
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
        fields: rocky_core::FieldSelection::Only(BTreeSet::new()),
    }];
    let (user, node) = (PrivateKey::new(), PrivateKey::new());
    let device = NodeKeys {
        identity: user.public_key(),
        node: node.public_key(),
    };
    let issuer_links = issuer.issuer_links();
    let issuer_node = PrivateKey::from_bytes(&vault.get("node")?);
    let capability = Authority::issuer(&issuer_node, &issuer.credentials)?.issue_partition_terms(
        user.public_key(),
        vec![],
        grants.clone(),
        (1, Some(rocky_credentials::AuthorRange::ALL)),
        issuer.validity()?,
    )?;
    let mut rows = BTreeMap::new();
    let changes = issuer.holder_changes(&device, &grants, 1, &Registry::default())?;
    let registry = Registry::merged(&mut rows, &changes)?;
    assert_eq!(registry.identities(), 1);
    assert_eq!(
        registry.get(&device.identity).ok_or("record")?.grants.len(),
        1
    );
    let me = PrivateKey::from_bytes(&vault.get("node")?).public_key();
    let mut links = issuer_links.clone();
    links.0.push(capability.clone());
    let renew = |issuer: &Settings, node: &PrivateKey| -> Result<Vec<u8>, Failure> {
        let chain = member_chain(&links, &user, node.public_key())?;
        let (request, _) = RenewRequest::sign(node, me, chain, now()?)?;
        Ok(issuer
            .renew(&vault, &request.to_bytes()?, &registry, None)?
            .0)
    };
    // Before revocation the same request renews, also under a new node key.
    renew(&issuer, &node)?;
    renew(&issuer, &PrivateKey::new())?;
    // Revoking the identity ends every node of it, a fresh one too.
    issuer.begin_revocation(Revoke::Identity(user.public_key()), known(&registry), None)?;
    issuer.rotate(&vault)?;
    assert!(issuer.revoked_node(&device, &registry));
    assert!(renew(&issuer, &node).is_err());
    assert!(renew(&issuer, &PrivateKey::new()).is_err());
    let renamed = NodeKeys {
        identity: user.public_key(),
        node: PrivateKey::new().public_key(),
    };
    assert!(
        issuer
            .holder_changes(&renamed, &grants, 1, &registry)
            .is_err()
    );
    Ok(())
}

#[test]
fn revoking_the_identity_of_a_configured_contact_keeps_the_settings_reopenable()
-> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let store: std::sync::Arc<dyn crate::SecretStore> = std::sync::Arc::new(
        crate::FileSecretStore::new(directory.path().join("secrets")),
    );
    let mut issuer = Settings::create(
        &Schema::parse(SCHEMA)?,
        &store,
        DEFAULT_TTL,
        None,
        &rocky_credentials::RootKey::generate(),
    )?;
    let vault = issuer.vault(&store);
    let node = PrivateKey::new().public_key();
    let record = ContactRecord {
        v: CONTACT_VERSION,
        publisher: issuer.publisher,
        database: issuer.database_context()?,
        node,
        routes: vec![Route {
            transport: "tcp".into(),
            address: "127.0.0.1:9000".into(),
        }],
        issuer: false,
        enrollment_only: false,
    };
    let uri = format!(
        "rocky://peer/{}",
        BASE64URL_NOPAD.encode(&serde_json::to_vec(&record)?)
    );
    issuer.add_peer(&vault, &uri)?;
    assert!(issuer.contact_meta.contains_key(&node));
    let identity = PrivateKey::new().public_key();
    let mut seen = BTreeMap::new();
    crate::membership::see(&mut seen, identity, node, now()?);
    let membership = Membership::of([], &seen);
    issuer.begin_revocation(
        Revoke::Identity(identity),
        (&Registry::default(), &membership),
        None,
    )?;
    issuer.rotate(&vault)?;
    let path = directory.path().join("identity.json");
    issuer.save(&path)?;
    let reopened = Settings::load(&path)?;
    assert!(!reopened.peers.contains_key(&node));
    assert!(!reopened.contact_meta.contains_key(&node));
    assert!(reopened.revocations.revokes(&identity));
    Ok(())
}

#[test]
fn contact_versions_are_independent_of_routing_flags() {
    let key = PrivateKey::new().public_key();
    let mut record = ContactRecord {
        v: CONTACT_VERSION,
        publisher: key,
        database: rocky_credentials::DatabaseContext {
            publisher: key,
            scope: rocky_core::Scope {
                mesh: MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: DomainId([3; 16]),
            },
        },
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
    for version in [0, 1, 2, 4, u32::MAX] {
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
    let mut issuer = Settings::create(
        &Schema::parse(SCHEMA)?,
        &store,
        DEFAULT_TTL,
        None,
        &rocky_credentials::RootKey::generate(),
    )?;
    let vault = issuer.vault(&store);
    let (user, node) = (PrivateKey::new(), PrivateKey::new());
    let device = NodeKeys {
        identity: user.public_key(),
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
    let chain = member_chain(&supplied.credentials.chain(), &user, node.public_key())?;
    let (_, selectors) = chain.verify_selectors(
        issuer.publisher,
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
    let issuer = Settings::create(
        &Schema::parse(SCHEMA)?,
        &store,
        DEFAULT_TTL,
        None,
        &rocky_credentials::RootKey::generate(),
    )?;
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

const TWO: &str = r#"{"format":6,"application":"two","version":1,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]},{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

/// A new issuer of `schema` with its identity saved at `path`.
fn issuer_at(
    schema: &str,
    directory: &Path,
) -> Result<(Settings, Vault, std::path::PathBuf), Failure> {
    let store: std::sync::Arc<dyn crate::SecretStore> =
        std::sync::Arc::new(crate::FileSecretStore::new(directory.join("secrets")));
    let issuer = Settings::create(
        &Schema::parse(schema)?,
        &store,
        DEFAULT_TTL,
        None,
        &rocky_credentials::RootKey::generate(),
    )?;
    let vault = issuer.vault(&store);
    let path = directory.join("identity.json");
    issuer.save(&path)?;
    Ok((issuer, vault, path))
}

#[test]
fn a_revocation_stopped_at_any_step_completes_with_the_same_statement_and_keys()
-> Result<(), Failure> {
    use fault::Stage;
    // An identity without a record may have read every collection, the
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
        let identity = PrivateKey::new().public_key();
        issuer.begin_revocation(
            Revoke::Identity(identity),
            known(&Registry::default()),
            None,
        )?;
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
            .map(|(name, epoch)| vault.find(&format!("rotation.reader.{name}.{epoch}")))
            .collect::<Result<_, _>>()?;
        let mut reopened = Settings::load(&path)?;
        let keys = reopened.rotate(&vault)?.ok_or("the intent was lost")?;
        let domain = reopened.domain()?;
        // No completion before the registry transaction has committed.
        assert!(
            reopened
                .complete_revocation(&vault, &domain, known(&Registry::default()))
                .is_err()
        );
        let mut rows = BTreeMap::new();
        let mut changes = registry::Changes::default();
        changes.revoked(&identity)?;
        let completed = Registry::merged(&mut rows, &changes)?;
        reopened.complete_revocation(&vault, &domain, known(&completed))?;
        reopened.save(&path)?;
        assert_eq!(reopened.revocations.len(), 1, "{stop:?}");
        assert!(
            reopened.revocations.get(&intent.statement.id()?).is_some(),
            "{stop:?}"
        );
        assert_eq!(keys.len(), 3);
        for ((name, epoch, key), before) in keys.iter().zip(made) {
            assert_eq!(intent.keys.get(name), Some(&epoch.generation));
            if let Some(before) = before {
                assert_eq!(before, *key, "{stop:?}: {name}");
            }
            assert_eq!(vault.find(&key_role(name, *epoch))?, Some(*key));
        }
        assert_eq!(
            reopened.keys["items"]
                .iter()
                .map(|epoch| epoch.generation)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        // Completed once: nothing is left to do at the next open.
        assert!(Settings::load(&path)?.rotate(&vault)?.is_none());
    }
    Ok(())
}

#[test]
fn the_1025th_live_statement_is_refused_by_name_and_rotates_nothing() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let (mut issuer, vault, _) = issuer_at(TWO, directory.path())?;
    let publisher = issuer.identity;
    let links = issuer.issuer_links();
    let credentials = issuer.credentials.clone();
    let node = PrivateKey::from_bytes(&vault.get("node")?);
    let app = issuer.domain()?.manifest().app;
    let time = now()?;
    let full = RevocationSet::try_from(
        (0..MAX_LIVE_REVOCATIONS)
            .map(|_| {
                rocky_credentials::SignedRevocation::sign(
                    Revocation {
                        app,
                        issuer: publisher,
                        time,
                        nodes: BTreeSet::new(),
                        identities: [PrivateKey::new().public_key()].into(),
                        capabilities: BTreeSet::new(),
                        cut: Default::default(),
                        recovery: None,
                    },
                    rocky_credentials::Signer::node(&node, &credentials)?,
                    &links,
                )
            })
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    issuer.learn_revocations(&full)?;
    assert_eq!(issuer.revocations.live(time), MAX_LIVE_REVOCATIONS);
    let (keys, groups) = (issuer.keys.clone(), issuer.groups.len());
    let error = issuer
        .begin_revocation(
            Revoke::Identity(PrivateKey::new().public_key()),
            known(&Registry::default()),
            None,
        )
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
            NodeKeys {
                identity: user.public_key(),
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
        Ok((user, node.public_key(), supplied.credentials.chain()))
    };
    let (revoked_user, _, revoked_root) = enroll(&mut a, &mut registry)?;
    let (kept_user, _, kept_root) = enroll(&mut a, &mut registry)?;
    // B is an issuer too but never saw either device or its record.
    let mut b = a.clone();
    b.authority = Some(Issuing::default());
    b.revocations = RevocationSet::new();
    a.begin_revocation(
        Revoke::Identity(revoked_user.public_key()),
        known(&registry),
        None,
    )?;
    a.rotate(&vault)?;
    // B learns the statement as a renewal answer brings it.
    b.learn_revocations(&a.revocations)?;
    let mut changes = registry::Changes::default();
    changes.revoked(&revoked_user.public_key())?;
    registry = Registry::merged(&mut rows, &changes)?;
    // The statement leaves both sets, as it does a year after it was made.
    let later = now()? + 2 * rocky_credentials::MAX_CHAIN_LIFETIME;
    for issuer in [&mut a, &mut b] {
        issuer.revocations = issuer.revocations.pruned(later);
        assert_eq!(issuer.revocations.live(later), 0);
    }
    let renew = |issuer: &Settings,
                 registry: &Registry,
                 user: &PrivateKey,
                 root: &CredentialChain|
     -> Result<Vec<u8>, Failure> {
        let fresh = PrivateKey::new();
        let chain = member_chain(root, user, fresh.public_key())?;
        let (request, _) = RenewRequest::sign(&fresh, me, chain, now()?)?;
        Ok(issuer
            .renew(&vault, &request.to_bytes()?, registry, None)?
            .0)
    };
    for (issuer, registry) in [(&a, &registry), (&b, &Registry::default())] {
        assert!(renew(issuer, registry, &revoked_user, &revoked_root).is_err());
        renew(issuer, registry, &kept_user, &kept_root)?;
    }
    Ok(())
}

/// Independent issuer vaults with the same domain, without filesystem key I/O.
#[derive(Default)]
struct EpochKeys(std::sync::Mutex<BTreeMap<String, Vec<u8>>>);
impl crate::SecretStore for EpochKeys {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, Failure> {
        Ok(self
            .0
            .lock()
            .map_err(|_| "keys poisoned")?
            .get(name)
            .cloned())
    }
    fn store(&self, name: &str, bytes: &[u8]) -> Result<(), Failure> {
        self.0
            .lock()
            .map_err(|_| "keys poisoned")?
            .insert(name.into(), bytes.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), Failure> {
        self.0.lock().map_err(|_| "keys poisoned")?.remove(name);
        Ok(())
    }
}

#[test]
fn concurrent_reader_revocations_supersede_both_exposures_and_equal_statements_stop_rotating()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let store: std::sync::Arc<dyn crate::SecretStore> = std::sync::Arc::new(EpochKeys::default());
    let owner = rocky_credentials::RootKey::generate();
    let mut a = Settings::create(&Schema::parse(SCHEMA)?, &store, DEFAULT_TTL, None, &owner)?;
    let av = a.vault(&store);
    let domain = a.domain()?;
    let mut partition = GroupPartition::resolve(
        &domain,
        Some("list"),
        None,
        vec![rocky_core::SqlValue::Text("L".into())],
        |_| Ok([21; 32]),
    )?;
    let initial_secret = [19; 32];
    let initial = rocky_core::KeyEpoch::new(1, &initial_secret);
    partition.record.hold(initial)?;
    let address = crate::groups::record_key(&partition.record.address);
    av.put(
        &secret_role(&partition.record.address, initial),
        &initial_secret,
    )?;
    a.groups.insert(address.clone(), partition.record.clone());
    let mut b = a.clone();
    b.installation = "second-issuer".into();
    let bv = b.vault(&store);
    for role in a.roles() {
        // A second node of the owner's identity, with its own node key.
        if role != "node"
            && let Some(key) = av.load_bytes(&role)?
        {
            bv.store_bytes(&role, &key)?;
        }
    }
    bv.create("node")?;
    b.revocations
        .link(a.credentials.certificate().ok_or("certificate")?.clone())?;
    b.credentials =
        crate::identities::with_identity(std::sync::Arc::new(owner.identity()), || {
            b.renew_locally(&bv)
        })?;
    let mut registry = Registry::default();
    let readers = [
        PrivateKey::new().public_key(),
        PrivateKey::new().public_key(),
    ];
    let issuer = a.identity;
    let admitted = |profile| -> Result<registry::IdentityRecord, Failure> {
        Ok(registry::IdentityRecord {
            admitted: Some(registry::Admitted {
                origin: None,
                kind: registry::Kind::Paired,
                profile: Some(profile),
                issuer,
                enrolled: now()?,
                invitation: None,
                single_use: false,
            }),
            ..Default::default()
        })
    };
    for identity in readers {
        registry.records.insert(
            identity,
            admitted(Profile::Reader {
                scopes: ["lists".into()].into(),
            })?,
        );
    }
    a.begin_revocation(Revoke::Identity(readers[0]), known(&registry), None)?;
    a.rotate(&av)?;
    a.authority.as_mut().ok_or("authority")?.rotation = None;
    b.begin_revocation(Revoke::Identity(readers[1]), known(&registry), None)?;
    b.rotate(&bv)?;
    b.authority.as_mut().ok_or("authority")?.rotation = None;
    let sibling_a = a.groups[&address].current().ok_or("epoch")?;
    let sibling_b = b.groups[&address].current().ok_or("epoch")?;
    assert_eq!(sibling_a.generation, 2);
    assert_eq!(sibling_b.generation, 2);
    assert_ne!(sibling_a, sibling_b);
    let statements = b.revocations.clone();
    a.learn_revocations(&statements)?;
    b.learn_revocations(&a.revocations)?;
    // Each issuer learns the other's keys and statements while partitioned from
    // the resulting superseding secret, so both start generation 3.
    let (a_before, b_before) = (a.clone(), b.clone());
    for (from, source, to, target) in [(&a_before, &av, &mut b, &bv), (&b_before, &bv, &mut a, &av)]
    {
        for (name, epochs) in &from.keys {
            for epoch in epochs {
                to.hold(target, name, *epoch, &source.get(&key_role(name, *epoch))?)?;
            }
        }
        to.epoch_statements.extend(from.epoch_statements.clone());
        for epoch in &from.groups[&address].epochs {
            target.put(
                &secret_role(&partition.record.address, *epoch),
                &source.get(&secret_role(&partition.record.address, *epoch))?,
            )?;
            to.groups.get_mut(&address).ok_or("group")?.hold(*epoch)?;
            let role = secret_role(&partition.record.address, *epoch);
            if let Some(ids) = from.epoch_statements.get(&role) {
                to.epoch_statements.insert(role, ids.clone());
            }
        }
    }
    assert!(a.begin_exposure_rotation(&registry)?);
    assert!(b.begin_exposure_rotation(&registry)?);
    a.rotate(&av)?;
    b.rotate(&bv)?;
    for settings in [&mut a, &mut b] {
        settings.authority.as_mut().ok_or("authority")?.rotation = None;
    }
    let superseding = [
        a.groups[&address].current().ok_or("epoch")?,
        b.groups[&address].current().ok_or("epoch")?,
    ];
    assert_ne!(superseding[0], superseding[1]);
    for epoch in superseding {
        assert_eq!(epoch.generation, 3);
    }
    // Every delivery is a real sealed renewal payload, including both current
    // siblings. Ten exchange rounds must not create generation 4.
    for round in 0..10 {
        let (a_before, b_before) = (a.clone(), b.clone());
        for (source, source_vault, target, target_vault, path) in [
            (&a_before, &av, &mut b, &bv, root.path().join("b.json")),
            (&b_before, &bv, &mut a, &av, root.path().join("a.json")),
        ] {
            let target_node = PrivateKey::from_bytes(&target_vault.get("node")?);
            let issuer = PrivateKey::from_bytes(&source_vault.get("node")?).public_key();
            let (request, secret) =
                RenewRequest::sign(&target_node, issuer, target.credentials.clone(), now()?)?;
            let reply = Renewal::from_bytes(&source.answer_epochs(
                source_vault,
                &request.to_bytes()?,
                &registry,
            )?)?;
            let sealed = Sealed::parse(&reply.open(source.publisher, &request, &secret)?)?;
            let keys = sealed
                .keys
                .iter()
                .flat_map(|(name, epochs)| {
                    epochs
                        .iter()
                        .map(move |(epoch, key)| Ok((name.clone(), *epoch, hex32(key)?)))
                })
                .collect::<Result<Vec<_>, Failure>>()?;
            target.adopt(
                &path,
                target_vault,
                &Renewed {
                    contacted: None,
                    revocations: reply.revocations,
                    chain: target.credentials.clone(),
                    keys,
                    groups: sealed.groups,
                    epoch_statements: sealed.epoch_statements,
                    profile: None,
                    issuers: Vec::new(),
                    claims: None,
                },
            )?;
            assert!(!target.begin_exposure_rotation(&registry)?, "round {round}");
            assert_eq!(
                target.groups[&address].current(),
                superseding.into_iter().max()
            );
        }
    }
    // A recorded custody identity read neither the collection nor the group.
    let nonreader = PrivateKey::new().public_key();
    registry.records.insert(
        nonreader,
        admitted(Profile::Custody {
            scopes: ["lists".into()].into(),
        })?,
    );
    let before = a.groups[&address].epochs.clone();
    let before_keys = a.keys.clone();
    assert_eq!(b.keys, before_keys);
    a.begin_revocation(Revoke::Identity(nonreader), known(&registry), None)?;
    a.rotate(&av)?;
    a.authority.as_mut().ok_or("authority")?.rotation = None;
    b.learn_revocations(&a.revocations)?;
    assert!(!a.begin_exposure_rotation(&registry)?);
    assert!(!b.begin_exposure_rotation(&registry)?);
    assert_eq!(a.groups[&address].epochs, before);
    assert_eq!(b.groups[&address].epochs, before);
    assert_eq!(a.keys, before_keys);
    assert_eq!(b.keys, before_keys);
    Ok(())
}

#[test]
fn saved_contacts_keep_routes_and_flags_without_foreign_node_keys() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let store: std::sync::Arc<dyn crate::SecretStore> = std::sync::Arc::new(
        crate::FileSecretStore::new(directory.path().join("secrets")),
    );
    let mut settings = Settings::create(
        &Schema::parse(SCHEMA)?,
        &store,
        DEFAULT_TTL,
        None,
        &rocky_credentials::RootKey::generate(),
    )?;
    let vault = settings.vault(&store);
    let node = PrivateKey::new().public_key();
    let record = ContactRecord {
        v: CONTACT_VERSION,
        publisher: settings.publisher,
        database: settings.database_context()?,
        node,
        routes: vec![Route {
            transport: "tcp".into(),
            address: "127.0.0.1:9000".into(),
        }],
        issuer: true,
        enrollment_only: false,
    };
    let uri = format!(
        "rocky://peer/{}",
        BASE64URL_NOPAD.encode(&serde_json::to_vec(&record)?)
    );
    settings.add_peer(&vault, &uri)?;
    let path = directory.path().join("identity.json");
    settings.save(&path)?;
    let bytes = std::fs::read(&path)?;
    assert!(
        !bytes
            .windows(node.to_hex().len())
            .any(|part| part == node.to_hex().as_bytes())
    );
    let mut reopened = Settings::load(&path)?;
    assert!(reopened.peers.is_empty());
    assert_eq!(reopened.unresolved_contacts().len(), 1);
    reopened.save(&path)?;
    reopened = Settings::load(&path)?;
    reopened.bind_saved_contact(PrivateKey::new().public_key());
    assert!(reopened.peers.is_empty());
    reopened.bind_saved_contact(node);
    assert_eq!(reopened.peers[&node].routes[0].address, "127.0.0.1:9000");
    assert_eq!(reopened.issuers, [node]);
    reopened.remove_peer(node)?;
    reopened.save(&path)?;
    assert!(Settings::load(&path)?.unresolved_contacts().is_empty());
    Ok(())
}

#[test]
fn wildcard_member_renewal_does_not_deliver_unshared_group_keys() -> Result<(), Failure> {
    let store: std::sync::Arc<dyn crate::SecretStore> = std::sync::Arc::new(EpochKeys::default());
    let mut issuer = Settings::create(
        &Schema::parse(SCHEMA)?,
        &store,
        DEFAULT_TTL,
        None,
        &rocky_credentials::RootKey::generate(),
    )?;
    let vault = issuer.vault(&store);
    let domain = issuer.domain()?;
    let mut partition = GroupPartition::resolve(
        &domain,
        Some("list"),
        None,
        vec![rocky_core::SqlValue::Text("L".into())],
        |_| Ok([21; 32]),
    )?;
    let secret = [19; 32];
    let epoch = rocky_core::KeyEpoch::new(1, &secret);
    partition.record.hold(epoch)?;
    vault.put(&secret_role(&partition.record.address, epoch), &secret)?;
    issuer.groups.insert(
        crate::groups::record_key(&partition.record.address),
        partition.record,
    );
    let node = PrivateKey::new();
    let user = PrivateKey::new();
    let profile = Profile::Reader {
        scopes: ["lists".into()].into(),
    };
    let issuer_node = PrivateKey::from_bytes(&vault.get("node")?);
    let capability = issuer.issue(
        &issuer.authority(&issuer_node)?,
        &domain,
        user.public_key(),
        grants_of(&profile, &domain)?,
        (false, Some(AuthorRange::new(2, 1025)?)),
    )?;
    let chain = member_chain(
        &issuer.authority(&issuer_node)?.chain_of(capability),
        &user,
        node.public_key(),
    )?;
    let issuer_node = PrivateKey::from_bytes(&vault.get("node")?).public_key();
    let (request, exchange) = RenewRequest::sign(&node, issuer_node, chain, now()?)?;
    for bytes in [
        issuer.answer_epochs(&vault, &request.to_bytes()?, &Registry::default())?,
        issuer
            .renew(&vault, &request.to_bytes()?, &Registry::default(), None)?
            .0,
    ] {
        let renewal = Renewal::from_bytes(&bytes)?;
        let payload = Sealed::parse(&renewal.open(issuer.publisher, &request, &exchange)?)?;
        assert!(payload.groups.is_empty());
    }
    assert_eq!(issuer.groups.len(), 1);
    Ok(())
}

#[test]
fn a_tip_on_another_engine_leaves_the_same_schema_ahead() -> Result<(), Failure> {
    let store: std::sync::Arc<dyn crate::SecretStore> = std::sync::Arc::new(EpochKeys::default());
    let mut installed = Settings::create(
        &Schema::parse(SCHEMA)?,
        &store,
        DEFAULT_TTL,
        None,
        &rocky_credentials::RootKey::generate(),
    )?;
    let schema = installed.schema.clone();
    assert_eq!(
        installed.application(&schema)?,
        crate::DeclaredSchema::Current
    );
    // The installed tip names an older engine: an owner on this build opens
    // a tip with the same tables on its own.
    installed.engine = rocky_core::EngineIdentity::of("3.0.0", rocky_core::REPLAY_SEMANTICS);
    assert_ne!(
        installed.domain()?.manifest().hash(),
        crate::domain::Domain::new(
            &schema,
            *installed.publisher.as_bytes(),
            installed.mesh,
            installed.domain
        )?
        .manifest()
        .hash()
    );
    assert_eq!(
        installed.application(&schema)?,
        crate::DeclaredSchema::Ahead
    );
    let mut older = schema;
    older.version = 0;
    assert_eq!(
        installed.application(&older)?,
        crate::DeclaredSchema::Behind
    );
    Ok(())
}

#[test]
fn forged_tip_schema_records_are_refused_without_changing_the_installation() -> Result<(), Failure>
{
    use crate::tip_schemas::Record;
    let store: std::sync::Arc<dyn crate::SecretStore> = std::sync::Arc::new(EpochKeys::default());
    let installed = Settings::create(
        &Schema::parse(SCHEMA)?,
        &store,
        DEFAULT_TTL,
        None,
        &rocky_credentials::RootKey::generate(),
    )?;
    let mut schema = installed.schema.clone();
    schema.version = 2;
    let record = Record::new(&schema, 2, rocky_core::EngineIdentity::current());
    let domain = Domain::at(
        &schema,
        *installed.publisher.as_bytes(),
        installed.mesh,
        installed.domain,
        2,
        rocky_core::EngineIdentity::current(),
    )?;
    let manifest = domain.manifest().hash();
    let bytes = record.bytes()?;
    Record::parse(&bytes)?.domain(&installed, manifest)?;
    let original = serde_json::to_vec(&installed)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    for changed in [
        {
            let mut value = value.clone();
            value["format"] = json!(0);
            value
        },
        {
            let mut value = value.clone();
            value["generation"] = json!(3);
            value
        },
        {
            // A record names the engine the tip's manifest commits.
            let mut value = value.clone();
            value["engine"] = json!([7u8; 32].to_vec());
            value
        },
        {
            let mut value = value.clone();
            value["schema"]["migrations"] = json!([]);
            value
        },
        {
            let mut value = value.clone();
            value["schema"]["application"] = json!("foreign");
            value
        },
        {
            let mut value = value.clone();
            value["schema"]["tables"] = json!(null);
            value
        },
    ] {
        let checked = Record::parse(&serde_json::to_vec(&changed)?)
            .and_then(|record| record.domain(&installed, manifest));
        assert!(checked.is_err());
    }
    let mut other = installed.clone();
    other.domain = DomainId([99; 16]);
    assert!(record.domain(&other, manifest).is_err());
    assert!(record.domain(&installed, [99; 32]).is_err());
    assert!(Record::parse(&vec![b' '; rocky_core::MAX_DEFINITION_BYTES + 1]).is_err());
    assert!(Record::parse(b"{malformed").is_err());
    assert_eq!(serde_json::to_vec(&installed)?, original);
    Ok(())
}

#[test]
fn an_issuer_without_the_key_of_a_known_partition_may_not_migrate() -> Result<(), Failure> {
    let store: std::sync::Arc<dyn crate::SecretStore> = std::sync::Arc::new(EpochKeys::default());
    let mut issuer = Settings::create(
        &Schema::parse(SCHEMA)?,
        &store,
        DEFAULT_TTL,
        None,
        &rocky_credentials::RootKey::generate(),
    )?;
    let domain = issuer.domain()?;
    assert!(issuer.may_migrate(&domain)?);
    // A group whose secret this issuer never received: it cannot read it.
    let mut partition = GroupPartition::resolve(
        &domain,
        Some("list"),
        None,
        vec![rocky_core::SqlValue::Text("L".into())],
        |_| Ok([21; 32]),
    )?;
    let address = crate::groups::record_key(&partition.record.address);
    issuer
        .groups
        .insert(address.clone(), partition.record.clone());
    assert!(!issuer.may_migrate(&domain)?);
    // Holding it, under the wildcard grants of its own credentials, it may.
    partition
        .record
        .hold(rocky_core::KeyEpoch::new(1, &[19; 32]))?;
    issuer.groups.insert(address, partition.record);
    assert!(issuer.may_migrate(&domain)?);
    Ok(())
}

#[test]
fn any_issuer_renews_a_recorded_claim_right_and_none_a_revoked_one() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let (mut a, vault, _) = issuer_at(TWO, directory.path())?;
    let me = PrivateKey::from_bytes(&vault.get("node")?).public_key();
    let owner = a.identity;
    let owner_links = a.issuer_links();
    let issuer_node = PrivateKey::from_bytes(&vault.get("node")?);
    let credentials = a.credentials.clone();
    let mut rows = BTreeMap::new();
    let mut registry = Registry::default();
    let mut holders = Vec::new();
    for _ in 0..3 {
        let (user, node) = (PrivateKey::new(), PrivateKey::new());
        let (supplied, changes) = a.grant(
            &vault,
            NodeKeys {
                identity: user.public_key(),
                node: node.public_key(),
            },
            (
                Profile::Reader {
                    scopes: ["items".into()].into(),
                },
                Terms::default(),
            ),
            None,
            &registry,
        )?;
        Registry::merged(&mut rows, &changes)?;
        let (grant, changes) = a.grant_claims(
            &vault,
            user.public_key(),
            Some([b"invoices".to_vec()].into()),
            1,
        )?;
        registry = Registry::merged(&mut rows, &changes)?;
        holders.push((user, supplied.credentials.chain(), grant));
    }
    // B is another issuer that granted nothing: it renews from the registry
    // alone, after A's links lapsed.
    let mut b = a.clone();
    b.authority = Some(Issuing::default());
    let request = |user: &PrivateKey, root: &CredentialChain, time: u64| {
        let fresh = PrivateKey::new();
        let chain = member_chain(root, user, fresh.public_key())?;
        Ok::<_, Failure>(RenewRequest::sign(&fresh, me, chain, time)?.0.to_bytes()?)
    };
    let links = |grant: &crate::ClaimGrant| -> Result<CredentialChain, Failure> {
        Ok(rocky_credentials::decode_bounded(
            &HEXLOWER.decode(grant.grant.as_bytes())?,
            rocky_credentials::MAX_CREDENTIAL_BYTES,
        )?)
    };
    let (user, root, grant) = &holders[0];
    let lapsed = grant.expires + 600;
    let none = RevocationSet::new();
    assert!(
        links(grant)?
            .claim_right(a.publisher, user.public_key(), &none, lapsed)
            .is_err()
    );
    let renewed = b
        .renew_claims(&vault, &request(user, root, lapsed)?, &registry, lapsed)?
        .ok_or("B renews the right A granted")?;
    let right = renewed.claim_right(a.publisher, user.public_key(), &none, lapsed)?;
    assert_eq!(right.counters, Some([b"invoices".to_vec()].into()));
    assert!(renewed.expires() > grant.expires);

    // A revoked identity, a revoked grant and a removed row renew nowhere.
    let time = now()?;
    let revoke = |identities: BTreeSet<PublicKey>, capabilities: BTreeSet<Commitment>| {
        let statement = rocky_credentials::SignedRevocation::sign(
            rocky_credentials::Revocation {
                app: a.domain()?.manifest().app,
                issuer: owner,
                time,
                nodes: BTreeSet::new(),
                identities,
                capabilities,
                cut: Default::default(),
                recovery: None,
            },
            rocky_credentials::Signer::node(&issuer_node, &credentials)?,
            &owner_links,
        )?;
        Ok::<_, Failure>(RevocationSet::try_from(vec![statement])?)
    };
    let (revoked_user, revoked_root, _) = &holders[1];
    let identity = revoke([revoked_user.public_key()].into(), BTreeSet::new())?;
    let (ended_user, ended_root, ended_grant) = &holders[2];
    let ended = links(ended_grant)?
        .0
        .iter()
        .find(|link| link.capability.extended().is_ok_and(|e| e.claims.is_some()))
        .ok_or("claim link")?
        .capability
        .extended()?;
    let grant = revoke(
        BTreeSet::new(),
        [ended.claims.ok_or("claim link")?.grant].into(),
    )?;
    for issuer in [&mut a, &mut b] {
        issuer.learn_revocations(&identity)?;
        issuer.learn_revocations(&grant)?;
    }
    for issuer in [&a, &b] {
        for (user, root) in [(revoked_user, revoked_root), (ended_user, ended_root)] {
            let renewal = issuer.renew_claims(&vault, &request(user, root, time)?, &registry, time);
            assert!(!matches!(renewal, Ok(Some(_))));
        }
        let (_, changes) = issuer.revoke_claims(user.public_key(), &registry)?;
        let mut rows = rows.clone();
        let unclaimed = Registry::merged(&mut rows, &changes)?;
        assert!(
            issuer
                .renew_claims(&vault, &request(user, root, time)?, &unclaimed, time)?
                .is_none()
        );
    }
    Ok(())
}
