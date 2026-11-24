use super::*;
use crate::{enrollment::fault, schema::Schema};
use rocky_credentials::ProviderError;

const SCHEMA: &str = r#"{"application":"revocation_faults","version":1,"format":7,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"parts","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"},
 {"name":"labels","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"},
 {"name":"attachments","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}]}"#;

fn list_partition(list: impl Into<String>) -> crate::Partition {
    crate::Partition::new("list", vec![rocky_core::SqlValue::Text(list.into())])
}

#[derive(Clone, Default)]
struct Keys(Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>>);
impl Keys {
    fn copy(&self) -> Result<Self, Failure> {
        let keys = self.0.lock().map_err(|_| "poisoned keys")?.clone();
        Ok(Self(Arc::new(std::sync::Mutex::new(keys))))
    }
}
impl crate::SecretStore for Keys {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, ProviderError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| "poisoned keys")?
            .get(name)
            .cloned())
    }
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), ProviderError> {
        self.0
            .lock()
            .map_err(|_| "poisoned keys")?
            .insert(name.into(), secret.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), ProviderError> {
        self.0.lock().map_err(|_| "poisoned keys")?.remove(name);
        Ok(())
    }
}

async fn issuer(
    directory: &std::path::Path,
    store: Arc<dyn crate::SecretStore>,
) -> Result<Node, Failure> {
    issuer_as(directory, store, None).await
}
async fn issuer_as(
    directory: &std::path::Path,
    store: Arc<dyn crate::SecretStore>,
    identity: Option<Arc<dyn rocky_credentials::IdentityProvider>>,
) -> Result<Node, Failure> {
    issuer_with_root(
        directory,
        store,
        identity,
        &rocky_credentials::RootKey::generate(),
    )
    .await
}
async fn issuer_with_root(
    directory: &std::path::Path,
    store: Arc<dyn crate::SecretStore>,
    identity: Option<Arc<dyn rocky_credentials::IdentityProvider>>,
    root: &rocky_credentials::RootKey,
) -> Result<Node, Failure> {
    let settings = Settings::create(
        &Schema::parse(SCHEMA)?,
        &store,
        crate::enrollment::DEFAULT_TTL,
        identity.unwrap_or_else(|| Arc::new(root.identity())),
        root,
    )?;
    let vault = settings.vault(&store);
    let domain = settings.domain()?;
    let replica = EncryptedReplica::open(
        directory.join("rocky.sqlite"),
        domain.app.clone(),
        settings.open_config(&vault, &domain)?,
    )
    .await?;
    replica
        .install_readable_tables(domain.logical().clone(), domain.scope, domain.readers())
        .await?;
    let path = directory.join("rocky.sqlite");
    replica
        .put_authority_state(
            crate::enrollment::AUTHORITY_IDENTITY_KEY,
            &settings.to_bytes()?,
            now()?,
        )
        .await?;
    let node = Node::new(
        replica,
        domain,
        vault,
        settings,
        path,
        BootstrapIntentFile::in_directory(directory),
    );
    node.record_self().await?;
    Ok(node)
}
async fn reload(
    directory: &std::path::Path,
    store: Arc<dyn crate::SecretStore>,
) -> Result<Node, Failure> {
    let path = directory.join("rocky.sqlite");
    let settings = Settings::load(&path)?;
    let vault = settings.vault(&store);
    let domain = settings.domain()?;
    let replica = EncryptedReplica::open(
        directory.join("rocky.sqlite"),
        domain.app.clone(),
        settings.open_config(&vault, &domain)?,
    )
    .await?;
    Ok(Node::new(
        replica,
        domain,
        vault,
        settings,
        path,
        BootstrapIntentFile::in_directory(directory),
    ))
}
fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> Result<(), Failure> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
}
fn tombstones(directory: &std::path::Path, node: PublicKey) -> Result<i64, Failure> {
    let conn = rusqlite::Connection::open_with_flags(
        directory.join("rocky.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM issuer_registry WHERE identity=? AND entry='revoked'",
        [node.to_hex()],
        |row| row.get(0),
    )?)
}

#[tokio::test]
async fn a_saved_root_revocation_preserves_its_mode_and_cascade_targets() -> Result<(), Failure> {
    use crate::registry::{Admitted, IdentityRecord, Registry, TrustedRecord};
    use rocky_credentials::{Authority, RevocationMode};
    let directory = tempfile::tempdir()?;
    let root = crate::RootKey::generate();
    let store: Arc<dyn crate::SecretStore> = Arc::new(Keys::default());
    let node = issuer_with_root(directory.path(), store.clone(), None, &root).await?;
    let initial = node.settings.lock().await.clone();
    let target = PrivateKey::new().public_key();
    let enrollee = PrivateKey::new().public_key();
    let grant = Authority::owner(&root).issue_trusted(target, node.domain().scope, now()?)?;
    let mut registry = Registry::default();
    registry.trusted.insert(
        target,
        TrustedRecord {
            granted: grant.capability.not_before,
            grant,
        },
    );
    registry.records.insert(
        enrollee,
        IdentityRecord {
            admitted: Some(Admitted {
                access: crate::Access::Full,
                invited_by: target,
                origin: Some(target),
                joined: now()?,
                invitation: None,
            }),
            ..IdentityRecord::default()
        },
    );
    let membership = crate::membership::Membership::default();
    for mode in [RevocationMode::Lost, RevocationMode::Compromised] {
        let mut settings = initial.clone();
        settings.begin_revocation(
            crate::enrollment::Revoke::TrustedIdentity(target, mode),
            (&registry, &membership),
            Some(&root),
        )?;
        node.replica
            .put_authority_state(
                crate::enrollment::AUTHORITY_IDENTITY_KEY,
                &settings.to_bytes()?,
                now()?,
            )
            .await?;
        let mut saved = Settings::load(&node.path)?;
        let intent = saved
            .authority
            .as_ref()
            .and_then(|authority| authority.rotation.as_ref())
            .ok_or("missing revocation intent")?;
        assert_eq!(intent.statement.mode, mode);
        assert_eq!(
            intent
                .signed
                .as_ref()
                .ok_or("missing root signature")?
                .statement
                .mode,
            mode
        );
        assert!(intent.identities.contains(&target));
        assert_eq!(
            intent.identities.contains(&enrollee),
            mode == RevocationMode::Compromised
        );
        let id = intent.statement.id()?;
        saved.rotate(&node.vault)?;
        assert_eq!(
            saved
                .revocations
                .get(&id)
                .ok_or("missing statement")?
                .statement
                .mode,
            mode
        );
        assert_eq!(
            saved.revocations.revokes_origin(&target),
            mode == RevocationMode::Compromised
        );
        let lost_identity_escalates_to_compromised = mode == RevocationMode::Lost;
        if lost_identity_escalates_to_compromised {
            let mut committed = registry.clone();
            committed.records.entry(target).or_default().revoked = true;
            saved.complete_revocation(&node.vault, &node.domain(), (&committed, &membership))?;
            saved.begin_revocation(
                crate::enrollment::Revoke::TrustedIdentity(target, RevocationMode::Compromised),
                (&registry, &membership),
                Some(&root),
            )?;
            saved.rotate(&node.vault)?;
            assert!(saved.revocations.revokes_origin(&target));
            assert_eq!(saved.revocations.len(), 2);
        }
    }
    Ok(())
}

#[tokio::test]
async fn an_exposure_rotation_that_fails_while_absorbing_revocations_leaves_memory_as_saved()
-> Result<(), Failure> {
    use rocky_credentials::{Revocation, RevocationMode, RevocationSet, SignedRevocation, Signer};
    let directory = tempfile::tempdir()?;
    let node = issuer(directory.path(), Arc::new(Keys::default())).await?;
    let key = Settings::node_key(&node.vault)?;
    let settings = node.settings.lock().await.clone();
    let statement = Revocation {
        app: settings.domain()?.manifest().app,
        issuer: settings.identity,
        time: now()?,
        nodes: Default::default(),
        identities: [PrivateKey::new().public_key()].into(),
        capabilities: Default::default(),
        cut: Default::default(),
        mode: RevocationMode::Lost,
    };
    let signed = SignedRevocation::sign(
        statement,
        Signer::node(&key, &settings.credentials)?,
        &settings.issuer_links(),
    )?;
    node.replica
        .learn_revocations(&RevocationSet::try_from(vec![signed])?)
        .await?;
    fault::STOP.with(|at| at.set(Some((fault::Stage::Signed, 0))));
    assert!(node.absorb_revocations(None).await.is_err());
    let held = node.settings.lock().await;
    assert!(
        held.authority
            .as_ref()
            .is_some_and(|authority| authority.rotation.is_some()),
        "the rotation intent was committed"
    );
    assert!(
        held.persisted_eq(&Settings::load(&node.path)?),
        "a rotation that failed part way leaves no unsaved change in memory"
    );
    drop(held);
    node.absorb_revocations(None).await?;
    assert!(
        node.settings
            .lock()
            .await
            .authority
            .as_ref()
            .is_some_and(|authority| authority.rotation.is_none()),
        "the next absorb resumes the committed rotation and finishes it"
    );
    Ok(())
}

#[test]
fn reopen_completes_each_revocation_reader_secret_boundary() -> Result<(), Failure> {
    use fault::Stage;
    reopen_at_revocation_boundaries(&[(Stage::Begun, 0), (Stage::Key, 0), (Stage::Key, 1)])
}

#[test]
fn reopen_completes_each_revocation_partition_secret_boundary() -> Result<(), Failure> {
    use fault::Stage;
    reopen_at_revocation_boundaries(&[
        (Stage::Key, 2),
        (Stage::Key, 3),
        (Stage::Key, 4),
        (Stage::Key, 5),
        (Stage::Key, 6),
        (Stage::Key, 7),
    ])
}

#[test]
fn reopen_completes_each_revocation_table_boundary() -> Result<(), Failure> {
    use fault::Stage;
    reopen_at_revocation_boundaries(&[
        (Stage::Table, 0),
        (Stage::Table, 1),
        (Stage::Table, 2),
        (Stage::Table, 3),
        (Stage::Table, 4),
        (Stage::Table, 5),
    ])
}

#[test]
fn reopen_completes_each_revocation_commit_boundary() -> Result<(), Failure> {
    use fault::Stage;
    reopen_at_revocation_boundaries(&[
        (Stage::Signed, 0),
        (Stage::Renewed, 0),
        (Stage::Tombstone, 0),
        (Stage::Committed, 0),
        (Stage::Clearing, 0),
    ])
}

fn reopen_at_revocation_boundaries(stops: &[(fault::Stage, usize)]) -> Result<(), Failure> {
    let runtime = || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
    };
    let template = tempfile::tempdir()?;
    let keys = Keys::default();
    runtime()?.block_on(async {
        let node = issuer(template.path(), Arc::new(keys.clone())).await?;
        for list in ["L", "M"] {
            node.create_partition(&list_partition(list).target())
                .await?;
        }
        Ok::<_, Failure>(())
    })?;
    std::thread::scope(|scope| {
        let runs: Vec<_> = stops
            .chunks(2)
            .map(|chunk| {
                let (template, keys) = (template.path(), &keys);
                scope.spawn(move || -> Result<(), Failure> {
                    let runtime = runtime()?;
                    for &stop in chunk {
                        runtime.block_on(stop_at(template, keys, stop))?;
                    }
                    Ok(())
                })
            })
            .collect();
        runs.into_iter().try_for_each(|run| {
            run.join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        })
    })
}

async fn stop_at(
    template: &std::path::Path,
    keys: &Keys,
    stop: (fault::Stage, usize),
) -> Result<(), Failure> {
    use fault::Stage;
    let root = tempfile::tempdir()?;
    copy_tree(template, root.path())?;
    let store: Arc<dyn crate::SecretStore> = Arc::new(keys.copy()?);
    let node = reload(root.path(), store.clone()).await?;
    let revoked = PrivateKey::new().public_key();
    fault::STOP.with(|at| at.set(Some(stop)));
    assert!(
        node.revoke(crate::enrollment::Revoke::Identity(revoked))
            .await
            .is_err(),
        "{stop:?}"
    );
    assert!(
        fault::STOP.with(|at| at.get()).is_none(),
        "stop not reached: {stop:?}"
    );
    let saved = Settings::load(&node.path)?;
    let intent = saved
        .authority
        .as_ref()
        .and_then(|a| a.rotation.clone())
        .ok_or("lost intent")?;
    let mut roles: Vec<String> = intent
        .keys
        .iter()
        .map(|(name, epoch)| format!("rotation.reader.{name}.{epoch}"))
        .collect();
    for (address, epoch) in &intent.groups {
        roles.push(format!("rotation.partition.{address}.{epoch}"));
    }
    let made: Vec<_> = roles
        .iter()
        .map(|role| node.vault.find(role))
        .collect::<Result<_, _>>()?;
    assert_eq!(
        tombstones(root.path(), revoked)?,
        i64::from(matches!(stop.0, Stage::Committed | Stage::Clearing)),
        "{stop:?}"
    );
    let vault = node.vault.clone();
    drop(node);
    let schema = crate::Schema::parse(SCHEMA)?;
    let configuration = crate::Configuration::new(root.path(), &schema)?.secrets(store.clone());
    let reopened = configuration.open().await?;
    let completed = Settings::load(&root.path().join("rocky.sqlite"))?;
    for list in ["L", "M"] {
        let status = reopened.partition_status(&list_partition(list)).await?;
        for table in status.tables {
            assert_eq!(table.epochs, status.epochs, "{stop:?}");
            assert_eq!(table.current, status.current, "{stop:?}");
        }
    }
    assert!(
        completed
            .authority
            .as_ref()
            .is_some_and(|a| a.rotation.is_none()),
        "{stop:?}"
    );
    assert!(
        completed.revocations.get(&intent.statement.id()?).is_some(),
        "{stop:?}"
    );
    assert_eq!(completed.revocations.len(), 1);
    for (name, target) in &intent.keys {
        assert_eq!(
            completed.keys[name]
                .iter()
                .map(|epoch| epoch.generation)
                .collect::<Vec<_>>(),
            vec![1, target.generation()],
            "{stop:?}"
        );
    }
    for (address, target) in &intent.groups {
        assert_eq!(
            completed.groups[address]
                .epochs
                .iter()
                .map(|epoch| epoch.generation)
                .collect::<Vec<_>>(),
            vec![1, target.generation()],
            "{stop:?}"
        );
    }
    let final_keys: Vec<_> = roles
        .iter()
        .map(|role| vault.get(role))
        .collect::<Result<_, _>>()?;
    for (before, after) in made.into_iter().zip(&final_keys) {
        if let Some(before) = before {
            assert_eq!(&before, after, "{stop:?}");
        }
    }
    assert_eq!(tombstones(root.path(), revoked)?, 1, "{stop:?}");
    reopened.close().await?;
    let again = configuration.open().await?;
    assert_eq!(tombstones(root.path(), revoked)?, 1);
    assert_eq!(
        Settings::load(&root.path().join("rocky.sqlite"))?.keys,
        completed.keys
    );
    for (role, key) in roles.iter().zip(final_keys) {
        assert_eq!(vault.get(role)?, key);
    }
    again.close().await?;
    Ok(())
}

#[tokio::test]
async fn status_verifies_revocations_outside_the_settings_lock() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let node = issuer(directory.path(), Arc::new(Keys::default())).await?;
    assert!(node.collects().await);
    node.refresh_trusted_watch().await?;
    node.observed_status().await?;
    node.health_report().await?;
    assert_eq!(node.verified_under_settings.load(Ordering::Relaxed), 0);
    Ok(())
}

#[tokio::test]
async fn received_sibling_resumes_after_each_table_installation() -> Result<(), Failure> {
    let template = tempfile::tempdir()?;
    let keys = Keys::default();
    let node = issuer(template.path(), Arc::new(keys.clone())).await?;
    node.create_partition(&list_partition("L").target()).await?;
    let settings = node.settings.lock().await.clone();
    let record = settings.groups.values().next().ok_or("group")?;
    let secret = [245; 32];
    let sender = settings.credentials.clone();
    let envelope = rocky_credentials::GroupSecretEnvelope::seal(
        &PrivateKey::from_bytes(&node.vault.get("node")?),
        record.address,
        1,
        secret,
        record.name,
        PrivateKey::from_bytes(&node.vault.get("node")?).public_key(),
    )?;
    let partition = crate::groups::GroupPartition::of(&node.domain(), record)?;
    drop(node);
    for passed in 0..3 {
        let root = tempfile::tempdir()?;
        copy_tree(template.path(), root.path())?;
        let store: Arc<dyn crate::SecretStore> = Arc::new(keys.copy()?);
        let node = reload(root.path(), store.clone()).await?;
        fault::STOP.with(|stop| stop.set(Some((fault::Stage::Table, passed))));
        let error = node
            .install_envelope(partition.clone(), &envelope, Some(&sender), false)
            .await
            .unwrap_err();
        assert!(
            fault::STOP.with(|stop| stop.get()).is_none(),
            "before table {passed}: {error}"
        );
        drop(node);
        let schema = crate::Schema::parse(SCHEMA)?;
        let database = crate::Configuration::new(root.path(), &schema)?
            .secrets(store)
            .open()
            .await?;
        let settings = Settings::load(&root.path().join("rocky.sqlite"))?;
        let epochs = &settings.groups.values().next().ok_or("group")?.epochs;
        assert_eq!(epochs.len(), 2);
        assert!(epochs.contains(&rocky_core::KeyEpoch::new(1, &secret)));
        let status = database.partition_status(&list_partition("L")).await?;
        for table in status.tables {
            assert_eq!(table.epochs, status.epochs);
            assert_eq!(table.current, status.current);
        }
        database.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn concurrent_membership_links_void_both_alert_and_allow_a_fresh_sequence()
-> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let identity = Arc::new(crate::IdentityKey::generate());
    let node = issuer_as(
        directory.path(),
        Arc::new(Keys::default()),
        Some(identity.clone()),
    )
    .await?;
    let settings = node.settings.lock().await.clone();
    let links = settings.issuer_links();
    let first = settings.credentials.certificate().ok_or("certificate")?;
    let predecessor = first.id()?;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let mut known = node.replica.revocations().await;
    let mut chains = Vec::new();
    for key in &keys {
        let chain = links
            .clone()
            .certify_member(&*identity, key, 1, Some(predecessor))?;
        known.link(chain.certificate().ok_or("certificate")?.clone())?;
        chains.push(chain);
    }
    node.replica.learn_revocations(&known).await?;
    let report = node.health_report().await?;
    assert!(report.alerts.iter().any(|alert| matches!(
        alert.kind,
        crate::AlertKind::MembershipFork { sequence: 1, .. }
    )));
    for (chain, key) in chains.iter().zip(&keys) {
        assert!(
            chain
                .verify_selectors(settings.publisher, key.public_key(), &known, now()?)
                .is_err()
        );
    }
    let replacement = PrivateKey::new();
    let membership = known.membership()?;
    let chain = links.certify_member(
        &*identity,
        &replacement,
        membership.next_sequence(settings.identity)?,
        membership
            .head(settings.identity)
            .map(|head| head.certificate),
    )?;
    known.link(chain.certificate().ok_or("certificate")?.clone())?;
    node.replica.learn_revocations(&known).await?;
    chain.verify_selectors(
        settings.publisher,
        replacement.public_key(),
        &node.replica.revocations().await,
        now()?,
    )?;
    Ok(())
}

#[tokio::test]
async fn unlinking_the_last_node_keeps_the_identity_and_allows_relinking() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let identity = Arc::new(crate::IdentityKey::generate());
    let node = issuer_as(
        directory.path(),
        Arc::new(Keys::default()),
        Some(identity.clone()),
    )
    .await?;
    let own = Settings::node_key(&node.vault)?.public_key();
    crate::identities::supplying(
        Some(identity.clone()),
        node.unlink_own(rocky_credentials::PeerId::of(own)),
    )
    .await?;
    let settings = node.settings.lock().await;
    assert!(node.revocation_notice.revoked(&settings).await?.is_some());
    assert!(!settings.revocations.revokes(&settings.identity));
    let ended = node
        .revocation_notice
        .revoked(&settings)
        .await?
        .ok_or("revocation")?;
    assert!(RevocationSet::try_from(vec![ended])?.ends_node(own, true));
    let membership = settings.revocations.membership()?;
    let replacement = PrivateKey::new();
    let links = settings.identity_links();
    let sequence = membership.next_sequence(settings.identity)?;
    let chain = links.certify_member(
        &*identity,
        &replacement,
        sequence,
        membership
            .head(settings.identity)
            .map(|head| head.certificate),
    )?;
    let mut known = settings.revocations.clone();
    known.link(chain.certificate().ok_or("certificate")?.clone())?;
    chain.verify_selectors(settings.publisher, replacement.public_key(), &known, now()?)?;
    Ok(())
}

#[tokio::test]
async fn foreign_membership_and_unlinks_are_refused_before_runtime_mutation() -> Result<(), Failure>
{
    let directory = tempfile::tempdir()?;
    let roots = [crate::RootKey::generate(), crate::RootKey::generate()];
    let identity = Arc::new(crate::IdentityKey::generate());
    let mut nodes = Vec::new();
    for index in 0..3 {
        let path = directory.path().join(index.to_string());
        std::fs::create_dir_all(&path)?;
        nodes.push(
            issuer_with_root(
                &path,
                Arc::new(Keys::default()),
                Some(identity.clone()),
                &roots[usize::from(index == 1)],
            )
            .await?,
        );
    }
    let local = &nodes[0];
    let context = local.settings.lock().await.database_context()?;
    for foreign in &nodes[1..] {
        let settings = foreign.settings.lock().await.clone();
        let foreign_context = settings.database_context()?;
        assert_ne!(context, foreign_context);
        let certificate = settings
            .credentials
            .certificate()
            .ok_or("certificate")?
            .clone();
        let mut evidence = foreign.replica.revocations().await;
        let unlink = settings.credentials.unlink(
            &*identity,
            foreign_context.scope.app,
            now()?,
            certificate.capability.subject,
        )?;
        evidence.merge(
            &RevocationSet::try_from(vec![unlink])?,
            settings.publisher,
            foreign_context.scope.app,
            now()?,
        )?;
        let evidence = RevocationSet::from_bytes(&evidence.to_bytes()?)?;
        let before = local.replica.revocations().await;
        let revision = *local.replica.observe_revision().borrow();
        let saved = std::fs::read(&local.path)?;
        assert!(local.replica.learn_revocations(&evidence).await.is_err());
        assert!(local.accept_link(certificate.clone()).await.is_err());
        assert!(local.accept_unlink(evidence.clone()).await.is_err());
        assert_eq!(local.replica.revocations().await, before);
        assert_eq!(*local.replica.observe_revision().borrow(), revision);
        assert_eq!(std::fs::read(&local.path)?, saved);
        let key = PrivateKey::new();
        let local_settings = local.settings.lock().await.clone();
        let audience = Settings::node_key(&local.vault)?.public_key();
        let (request, _) = rocky_credentials::IdentityRequest::sign(
            &*identity,
            &key,
            audience,
            now()?,
            foreign_context,
        )?;
        assert!(
            local_settings
                .answer_identity(&local.vault, &request.to_bytes()?)
                .is_err()
        );
        assert_eq!(std::fs::read(&local.path)?, saved);
    }
    for node in &nodes {
        let known = node.replica.revocations().await;
        let membership = known.membership()?;
        assert_eq!(membership.next_sequence(identity.public_key())?, 1);
        assert!(!membership.is_forked(identity.public_key()));
    }
    Ok(())
}

#[tokio::test]
async fn link_challenges_are_bounded_retryable_and_expire_before_bundle_release()
-> Result<(), Failure> {
    use crate::self_enrollment::LinkRequest;
    use rocky_credentials::{IdentityKey, IdentityRequest};
    let directory = tempfile::tempdir()?;
    let key = Arc::new(IdentityKey::generate());
    let node = issuer_as(
        directory.path(),
        Arc::new(Keys::default()),
        Some(key.clone()),
    )
    .await?;
    let settings = node.settings.lock().await.clone();
    let audience = Settings::node_key(&node.vault)?.public_key();
    let time = now()?;
    let mut requests = Vec::new();
    for index in 0..33 {
        let (request, _) = IdentityRequest::sign(
            &*key,
            &PrivateKey::new(),
            audience,
            time,
            settings.database_context()?,
        )?;
        let wire = LinkRequest {
            request: request.clone(),
            code: None,
        };
        assert!(matches!(
            node.answer_link(&settings, wire)?,
            rocky_credentials::IdentityReply::ConfirmationRequired
        ));
        let make_first_arrival_oldest = index == 0;
        if make_first_arrival_oldest {
            node.link_requests
                .lock()
                .map_err(|_| "poisoned requests")?
                .get_mut(&request.node)
                .ok_or("missing request")?
                .received = time - 1;
        }
        requests.push(request);
    }
    {
        let held = node.link_requests.lock().map_err(|_| "poisoned requests")?;
        assert_eq!(held.len(), 32);
        assert!(!held.contains_key(&requests[0].node));
    }
    let request = &requests[32];
    let code = request.confirmation_code()?;
    node.approve_link(request.node, &crate::LinkCode(code.clone()))?;
    let wire = LinkRequest {
        request: request.clone(),
        code: None,
    };
    node.answer_link(&settings, wire)?;
    let confirmed = LinkRequest {
        request: request.clone(),
        code: Some(code.clone()),
    };
    assert!(
        matches!(node.answer_link(&settings, LinkRequest { request: confirmed.request.clone(), code: confirmed.code.clone() })?, rocky_credentials::IdentityReply::Enrolled(bytes) if rocky_credentials::IdentityAnswer::from_bytes(&bytes).is_ok())
    );
    node.link_requests
        .lock()
        .map_err(|_| "poisoned requests")?
        .get_mut(&request.node)
        .ok_or("missing request")?
        .received = time - 300;
    assert!(
        node.approve_link(request.node, &crate::LinkCode(code.clone()))
            .is_err()
    );
    assert!(node.answer_link(&settings, confirmed).is_err());
    assert_eq!(
        node.replica
            .revocations()
            .await
            .membership()?
            .next_sequence(settings.identity)?,
        1
    );
    Ok(())
}

#[test]
fn revocation_keys_and_their_copy_each_meet_the_key_provider_contract() -> Result<(), Failure> {
    let keys = Keys::default();
    rocky_credentials::testing::key_provider_contract(&keys);
    rocky_credentials::testing::key_provider_contract(&keys.copy()?);
    Ok(())
}

#[tokio::test]
async fn an_invited_node_that_lost_its_answer_redeems_again_and_no_other_node_can()
-> Result<(), Failure> {
    use crate::invitation::{AUTHORITY_INVITATIONS_KEY, Book};
    use rocky_credentials::{IdentityKey, IdentityRequest, InvitedEnrollmentRequest};
    let directory = tempfile::tempdir()?;
    let node = issuer(directory.path(), Arc::new(Keys::default())).await?;
    let person = IdentityKey::generate();
    let settings = node.settings.lock().await.clone();
    let registry = node.registry(&settings).await?;
    let profile = crate::Access::Full;
    let routes = || {
        vec![crate::enrollment::Route {
            transport: "memory".into(),
            address: "issuer".into(),
        }]
    };
    let mut book = Book::read(&node.replica).await?;
    let time = now()?;
    let invite = |profile: crate::Access| {
        crate::Invite::new(profile).for_identity(crate::PersonId::new(person.public_key()))
    };
    let open = |book: &mut Book, profile: crate::Access| {
        settings.invite(
            (&node.vault, routes()),
            book,
            &invite(profile),
            &registry,
            time,
        )
    };
    let wider = crate::Access::Read;
    let issued = open(&mut book, profile.clone())?;
    let spare = open(&mut book, profile.clone())?;
    let conflicting = open(&mut book, wider.clone())?;
    node.replica
        .put_authority_state(AUTHORITY_INVITATIONS_KEY, &book.to_bytes()?, time)
        .await?;
    let audience = Settings::node_key(&node.vault)?.public_key();
    let database = settings.database_context()?;
    let ask_for = |key: &PrivateKey, invitation: crate::InvitationId| {
        let (request, _) = IdentityRequest::sign(&person, key, audience, now()?, database)?;
        Ok::<_, Failure>(InvitedEnrollmentRequest::sign(
            request,
            invitation.0,
            None,
            key,
        )?)
    };
    let ask = |key: &PrivateKey| ask_for(key, issued.id);
    let joiner = PrivateKey::new();
    let mut substituted = ask(&joiner)?;
    substituted.invitation = spare.id.0;
    assert!(
        node.answer_invitation(substituted).await.is_err(),
        "a request bound to one invitation redeems no other"
    );
    for attempt in ["first answer", "the answer the joiner lost, again"] {
        assert!(
            matches!(
                node.answer_invitation(ask(&joiner)?).await?,
                IdentityReply::Enrolled(_)
            ),
            "{attempt}"
        );
    }
    let other = node
        .answer_invitation(ask(&PrivateKey::new())?)
        .await
        .err()
        .ok_or("a second node redeemed the invitation")?;
    assert_eq!(
        other.downcast_ref::<crate::JoinError>(),
        Some(&crate::JoinError::Used)
    );
    assert!(
        node.answer_invitation(ask_for(&PrivateKey::new(), conflicting.id)?)
            .await
            .is_err(),
        "an invitation issued before the identity was admitted keeps the admitted profile"
    );
    let held = Book::read(&node.replica).await?.listed(now()?);
    let redeemer = |id: crate::InvitationId| {
        held.iter()
            .find(|open| open.id == id)
            .map(|open| open.redeemed_by)
    };
    assert_eq!(
        redeemer(issued.id),
        Some(Some(crate::PersonId::new(person.public_key())))
    );
    assert_eq!(redeemer(spare.id), Some(None));
    assert_eq!(redeemer(conflicting.id), Some(None));
    let snapshot = node.settings_snapshot().await;
    let recorded = node.registry(&snapshot).await?;
    let admitted = recorded
        .get(&person.public_key())
        .and_then(|record| record.admitted.as_ref())
        .ok_or("the invited identity is not admitted")?;
    assert_eq!(admitted.invitation, Some(issued.id));
    assert_eq!(admitted.access, profile);
    assert!(
        settings
            .invite(
                (&node.vault, routes()),
                &mut Book::read(&node.replica).await?,
                &invite(wider),
                &recorded,
                now()?,
            )
            .is_err(),
        "an admitted identity is not invited into another profile"
    );
    Ok(())
}

#[tokio::test]
async fn invited_redemptions_run_one_at_a_time_and_neither_a_retry_nor_a_failed_record_repeats_one()
-> Result<(), Failure> {
    use crate::invitation::{AUTHORITY_INVITATIONS_KEY, Book};
    use rocky_credentials::{IdentityKey, IdentityRequest, InvitedEnrollmentRequest};
    let directory = tempfile::tempdir()?;
    let node = issuer(directory.path(), Arc::new(Keys::default())).await?;
    let person = IdentityKey::generate();
    let identity = person.public_key();
    let settings = node.settings.lock().await.clone();
    let registry = node.registry(&settings).await?;
    let narrow = crate::Access::Read;
    let wide = crate::Access::Full;
    let time = now()?;
    let mut book = Book::read(&node.replica).await?;
    let mut open = |profile: crate::Access, label: &str| {
        settings.invite(
            (
                &node.vault,
                vec![crate::enrollment::Route {
                    transport: "memory".into(),
                    address: "issuer".into(),
                }],
            ),
            &mut book,
            &crate::Invite::new(profile)
                .for_identity(crate::PersonId::new(identity))
                .label(label),
            &registry,
            time,
        )
    };
    let invited = open(narrow.clone(), "Alice")?;
    let rival = open(wide, "Mallory")?;
    node.replica
        .put_authority_state(AUTHORITY_INVITATIONS_KEY, &book.to_bytes()?, time)
        .await?;
    let audience = Settings::node_key(&node.vault)?.public_key();
    let database = settings.database_context()?;
    let ask = |key: &PrivateKey, invitation: crate::InvitationId| {
        let (request, _) = IdentityRequest::sign(&person, key, audience, now()?, database)?;
        Ok::<_, Failure>(InvitedEnrollmentRequest::sign(
            request,
            invitation.0,
            None,
            key,
        )?)
    };
    let redeemers = || async {
        Ok::<_, Failure>(
            Book::read(&node.replica)
                .await?
                .listed(now()?)
                .into_iter()
                .map(|open| (open.id, open.redeemed_by))
                .collect::<BTreeMap<_, _>>(),
        )
    };
    let redemptions = || {
        node.activity
            .after(0)
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event.detail,
                    crate::activity::ActivityKind::InvitationRedeemed { .. }
                )
            })
            .count()
    };
    let (joiner, rival_node) = (PrivateKey::new(), PrivateKey::new());

    *node
        .redemption_hook
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(super::invitations::RedemptionHook::Fail);
    assert!(
        node.answer_invitation(ask(&joiner, invited.id)?)
            .await
            .is_err()
    );
    assert!(
        redeemers().await?.values().all(Option::is_none),
        "a redemption its registry write failed for leaves the invitation open"
    );
    assert_eq!(redemptions(), 0);

    let gate = Arc::new(super::invitations::RedemptionGate {
        reached: tokio::sync::Notify::new(),
        open: tokio::sync::Semaphore::new(0),
    });
    *node
        .redemption_hook
        .lock()
        .unwrap_or_else(PoisonError::into_inner) =
        Some(super::invitations::RedemptionHook::Pause(gate.clone()));
    let (admitted, ()) = tokio::join!(node.answer_invitation(ask(&joiner, invited.id)?), async {
        gate.reached.notified().await;
        assert!(
            node.redeeming.try_lock().is_err(),
            "a redemption holds every other one off until its registry write ends"
        );
        gate.open.add_permits(1);
    });
    assert!(matches!(admitted?, IdentityReply::Enrolled(_)));
    assert!(
        node.answer_invitation(ask(&rival_node, rival.id)?)
            .await
            .is_err(),
        "of two invitations into different profiles only one admits the identity"
    );
    assert_eq!(
        redeemers().await?,
        BTreeMap::from([
            (invited.id, Some(crate::PersonId::new(identity))),
            (rival.id, None)
        ])
    );
    assert_eq!(redemptions(), 1);

    let mut relabeled = crate::registry::Changes::default();
    relabeled.label(&identity, b"Alice Smith")?;
    node.commit(relabeled).await?;
    assert!(matches!(
        node.answer_invitation(ask(&joiner, invited.id)?).await?,
        IdentityReply::Enrolled(_)
    ));
    let record = node.registry(&*node.settings_snapshot().await).await?;
    let record = record
        .get(&identity)
        .ok_or("the identity is not recorded")?;
    assert_eq!(
        record.label.as_deref(),
        Some(b"Alice Smith".as_slice()),
        "a retried redemption keeps a label edited after the first"
    );
    assert_eq!(
        record.access(),
        Some(&narrow),
        "the admitted access is the one that won"
    );
    assert_eq!(redemptions(), 1, "a retry records no second redemption");
    Ok(())
}
