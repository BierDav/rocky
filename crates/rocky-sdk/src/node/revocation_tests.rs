//! Deterministic recovery through the tombstone transaction and intent cleanup.
use super::*;
use crate::{enrollment::fault, schema::Schema};
use rocky_credentials::ProviderError;

const SCHEMA: &str = r#"{"application":"revocation_faults","version":1,"format":6,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"parts","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"},
 {"name":"labels","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"},
 {"name":"attachments","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}]}"#;

/// Keys in memory: the file store syncs every key, which dominated this test.
/// A copy carries the template's keys to one stop.
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
        identity,
        root,
    )?;
    let vault = settings.vault(&store);
    let domain = settings.domain()?;
    let replica = EncryptedReplica::open(
        directory.join("rocky.sqlite"),
        domain.app.clone(),
        settings.config(&vault, &domain)?,
    )
    .await?;
    replica
        .install_readable_tables(domain.logical().clone(), domain.scope, domain.readers())
        .await?;
    let path = directory.join("identity.json");
    settings.save(&path)?;
    let node = Node::new(replica, domain, vault, settings, path);
    node.record_self().await?;
    Ok(node)
}
/// Opens the issuer `issuer` left in `directory`, without recording it again.
async fn reload(
    directory: &std::path::Path,
    store: Arc<dyn crate::SecretStore>,
) -> Result<Node, Failure> {
    let path = directory.join("identity.json");
    let settings = Settings::load(&path)?;
    let vault = settings.vault(&store);
    let domain = settings.domain()?;
    let replica = EncryptedReplica::open(
        directory.join("rocky.sqlite"),
        domain.app.clone(),
        settings.config(&vault, &domain)?,
    )
    .await?;
    Ok(Node::new(replica, domain, vault, settings, path))
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
    // One issuer with two partitions, copied for each stop.
    let template = tempfile::tempdir()?;
    let keys = Keys::default();
    runtime()?.block_on(async {
        let node = issuer(template.path(), Arc::new(keys.clone())).await?;
        let client = node.client(1)?;
        for list in ["L", "M"] {
            node.dispatch(&client,&json!({"type":"partition_create","group":"list","values":[{"type":"text","value":list}]})).await?;
        }
        Ok::<_, Failure>(())
    })?;
    // The stops are independent, and the injected stop is per thread.
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

/// Revokes on a copy of `template` until `stop`, then reopens it twice.
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
    let client = node.client(1)?;
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
    drop(client);
    drop(node);
    // Exercise the real database open, before any network attaches.
    let open = || crate::Open {
        identity: None,
        directory: root.path(),
        schema: SCHEMA,
        secrets: Some(store.clone()),
        network: None,
        coordination: None,
    };
    let reopened = crate::Database::open(open())?.database;
    let completed = Settings::load(&root.path().join("identity.json"))?;
    let session = reopened.session();
    for list in ["L", "M"] {
        let response: Value = serde_json::from_str(&session.call(json!({"version":1,"scope":serde_json::from_str::<Value>(&reopened.scope())?,"request":{"type":"partition_status","group":"list","values":[{"type":"text","value":list}]}}).to_string()).await?)?;
        let status = &response["value"];
        assert_eq!(response["status"], "ok");
        for table in status["tables"].as_array().ok_or("group tables")? {
            assert_eq!(table["epochs"], status["epochs"], "{stop:?}");
            assert_eq!(table["current"], status["current"], "{stop:?}");
        }
    }
    session.close().await;
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
            vec![1, *target],
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
            vec![1, *target],
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
    let again = crate::Database::open(open())?.database;
    assert_eq!(tombstones(root.path(), revoked)?, 1);
    assert_eq!(
        Settings::load(&root.path().join("identity.json"))?.keys,
        completed.keys
    );
    for (role, key) in roles.iter().zip(final_keys) {
        assert_eq!(vault.get(role)?, key);
    }
    again.close().await?;
    Ok(())
}

#[tokio::test]
async fn received_sibling_resumes_after_each_table_installation() -> Result<(), Failure> {
    let template = tempfile::tempdir()?;
    let keys = Keys::default();
    let node = issuer(template.path(), Arc::new(keys.clone())).await?;
    let client = node.client(1)?;
    let request =
        json!({"type":"partition_create","group":"list","values":[{"type":"text","value":"L"}]});
    node.dispatch(&client, &request).await?;
    let settings = node.settings.lock().await.clone();
    let record = settings.groups.values().next().ok_or("group")?;
    let secret = [245; 32];
    // The node signs under its chain, as every sender does.
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
    drop(client);
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
        let database = crate::Database::open(crate::Open {
            identity: None,
            directory: root.path(),
            schema: SCHEMA,
            secrets: Some(store),
            network: None,
            coordination: None,
        })?
        .database;
        let settings = Settings::load(&root.path().join("identity.json"))?;
        let epochs = &settings.groups.values().next().ok_or("group")?.epochs;
        assert_eq!(epochs.len(), 2);
        assert!(epochs.contains(&rocky_core::KeyEpoch::new(1, &secret)));
        let session = database.session();
        let reply: Value = serde_json::from_str(&session.call(json!({"version":1,"scope":serde_json::from_str::<Value>(&database.scope())?,"request":{"type":"partition_status","group":"list","values":[{"type":"text","value":"L"}]}}).to_string()).await?)?;
        assert_eq!(reply["status"], "ok");
        let status = &reply["value"];
        for table in status["tables"].as_array().ok_or("tables")? {
            assert_eq!(table["epochs"], status["epochs"]);
            assert_eq!(table["current"], status["current"]);
        }
        session.close().await;
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
    let author = links.authors()?.ok_or("authors")?.first;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let mut known = node.replica.revocations().await;
    let mut chains = Vec::new();
    for key in &keys {
        let chain = links.clone().certify_member(
            &*identity,
            key.public_key(),
            author + 1,
            1,
            Some(predecessor),
        )?;
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
        replacement.public_key(),
        author + 2,
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
    assert_eq!(chain.author_number()?, author + 2);
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
    crate::identities::supplying(Some(identity.clone()), node.unlink(own)).await?;
    let settings = node.settings.lock().await;
    assert!(settings.revocation(&node.path)?.is_some());
    assert!(!settings.revocations.revokes(&settings.identity));
    assert!(settings.revocations.ends_node(own, true));
    let membership = settings.revocations.membership()?;
    let replacement = PrivateKey::new();
    let links = settings.identity_links();
    let first = links.authors()?.ok_or("authors")?.first;
    let sequence = membership.next_sequence(settings.identity)?;
    let chain = links.certify_member(
        &*identity,
        replacement.public_key(),
        first + u32::try_from(sequence)?,
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
        // These bytes are what authority-set pages and the unlink transport import.
        let evidence = RevocationSet::from_bytes(&evidence.to_bytes()?)?;
        let before = local.replica.revocations().await;
        let revision = *local.replica.observe_revision().borrow();
        let saved = std::fs::read(&local.path)?;
        assert!(local.replica.learn_revocations(&evidence).await.is_err());
        assert!(
            local
                .accept_link(&serde_json::to_vec(&certificate)?)
                .await
                .is_err()
        );
        assert!(local.accept_unlink(&evidence.to_bytes()?).await.is_err());
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
        let wire = serde_json::to_vec(&LinkRequest {
            request: request.to_bytes()?,
            code: None,
        })?;
        assert_eq!(
            node.answer_link(&settings, &wire)?,
            b"confirmation required"
        );
        // Give the first arrival an unambiguous oldest timestamp without sleeping.
        if index == 0 {
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
    node.confirm_link(&json!({"node": request.node.to_hex(), "code":code}))
        .await?;
    // Retrying the same challenge preserves the answering device's confirmation.
    let wire = serde_json::to_vec(&LinkRequest {
        request: request.to_bytes()?,
        code: None,
    })?;
    node.answer_link(&settings, &wire)?;
    let confirmed = serde_json::to_vec(&LinkRequest {
        request: request.to_bytes()?,
        code: Some(code.clone()),
    })?;
    assert!(
        rocky_credentials::IdentityAnswer::from_bytes(&node.answer_link(&settings, &confirmed)?)
            .is_ok()
    );
    node.link_requests
        .lock()
        .map_err(|_| "poisoned requests")?
        .get_mut(&request.node)
        .ok_or("missing request")?
        .received = time - 300;
    assert!(
        node.confirm_link(&json!({"node":request.node.to_hex(), "code":code}))
            .await
            .is_err()
    );
    assert!(node.answer_link(&settings, &confirmed).is_err());
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
