mod common;
use common::{Device, Failure};
use rocky_sdk::{Network, transport::memory::Hub};
use serde_json::json;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn retention_defaults_preserve_sole_copies_and_configuration_survives_reopen()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let device = Device::create(root.path(), None)?;
    let default = device
        .ok(json!({"type": "retention", "collection": "items"}))
        .await;
    assert_eq!(
        default["policy"],
        json!({"required": [], "behind": [], "copies": 1, "window": 256})
    );
    device.add(1, "sole copy").await;
    let policy = json!({"required": [], "behind": [], "copies": 1, "window": 0});
    let status = device
        .ok(json!({"type": "retention", "collection": "items", "policy": policy, "maintain": true}))
        .await;
    assert_eq!(status["covered"], 1);
    assert_eq!(status["horizon"], json!([]));
    assert!(
        device
            .call(json!({"type": "retention", "collection": "missing", "policy": policy}))
            .await
            .is_err()
    );
    assert!(device.call(json!({"type": "retention_limits", "limits": {"checkpoint_operations": 0, "soft_bytes": 1, "hard_bytes": 1, "handles": 1}})).await.is_err());
    device.close().await;
    let device = Device::open(root.path(), None)?;
    assert_eq!(
        device
            .ok(json!({"type": "retention", "collection": "items"}))
            .await["policy"],
        policy
    );
    assert_eq!(device.titles().await, ["sole copy"]);
    device.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn behind_horizon_sync_restores_the_base_and_preserves_unsent_work() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?;
    a.add(1, "base").await;
    let b = Device::join(
        &root.path().join("b"),
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    let c = Device::join(
        &root.path().join("c"),
        &a.invite().await,
        Network::Owned(hub.connector("c")),
    )?;
    b.sync().await.expect("initial sync");
    c.sync().await.expect("initial sync");
    c.add(99, "unsent").await;
    for handle in 2..=12 {
        a.add(handle, &format!("row {handle}")).await;
    }
    b.sync().await.expect("custody sync");
    let status = a
        .ok(
            json!({"type": "retention", "collection": "items", "policy": {
        "required": [], "behind": [], "copies": 1, "window": 0
    }, "maintain": true}),
        )
        .await;
    assert!(!status["horizon"].as_array().expect("horizon").is_empty());
    a.add(13, "tail").await;
    c.sync().await.expect("rebootstrap and publish");
    assert_eq!(a.items().await, c.items().await);
    assert!(c.titles().await.contains(&"unsent".to_owned()));
    assert!(c.titles().await.contains(&"tail".to_owned()));
    let status = c
        .ok(json!({"type": "retention", "collection": "items"}))
        .await;
    assert!(
        !status["horizon"]
            .as_array()
            .expect("imported horizon")
            .is_empty()
    );
    c.add(99, "unsent").await;
    assert_eq!(a.items().await, c.items().await);
    c.close().await;
    let c = Device::open(&root.path().join("c"), None)?;
    assert_eq!(a.items().await, c.items().await);
    c.close().await;
    b.close().await;
    a.close().await;
    Ok(())
}

/// Checkpoint pages take a whole frame; large rows still split so each page's
/// plaintext fits one package.
#[tokio::test(flavor = "multi_thread")]
async fn join_restores_large_rows_across_checkpoint_pages() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?;
    for handle in 0..40 {
        a.add(handle, &format!("{handle}:{}", "x".repeat(12_000)))
            .await;
    }
    let b = Device::join(
        &root.path().join("b"),
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    b.sync().await.expect("join sync");
    assert_eq!(b.titles().await.len(), 40);
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn post_revocation_join_reads_earlier_rows_without_rotated_out_keys() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?;
    a.add(1, "before revocation").await;
    let revoked = Device::join(
        &root.path().join("revoked"),
        &a.invite().await,
        Network::Owned(hub.connector("revoked")),
    )?;
    revoked.sync().await.expect("initial bootstrap");
    a.ok(json!({"type": "revoke", "node": revoked.node().await}))
        .await;
    let member_path = root.path().join("member");
    let member = Device::join(
        &member_path,
        &a.invite_as(json!({"kind": "reader", "scopes": ["items"]}))
            .await,
        Network::Owned(hub.connector("member")),
    )?;
    member
        .sync()
        .await
        .expect("checkpoint join under current policy");
    assert_eq!(member.titles().await, ["before revocation"]);
    assert!(!member_path.join("bootstrap.json").exists());
    let identity: serde_json::Value =
        serde_json::from_slice(&std::fs::read(member_path.join("identity.json"))?)?;
    assert_eq!(
        identity["keys"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|epoch| epoch["generation"].clone())
            .collect::<Vec<_>>(),
        vec![json!(2)]
    );
    let file = rusqlite::Connection::open(member_path.join("rocky.sqlite"))?;
    let imported: i64 =
        file.query_row("SELECT COUNT(*) FROM secure_checkpoint_rows", [], |row| {
            row.get(0)
        })?;
    assert_eq!(imported, 1);
    let history: i64 = file.query_row("SELECT COUNT(*) FROM secure_operations", [], |row| {
        row.get(0)
    })?;
    assert_eq!(
        history, 0,
        "earlier ciphertext was replaced by the checkpoint"
    );
    member.ok(json!({"type": "renew"})).await;
    let renewed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(member_path.join("identity.json"))?)?;
    assert_eq!(
        renewed["keys"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|epoch| epoch["generation"].clone())
            .collect::<Vec<_>>(),
        vec![json!(2)]
    );
    a.add(2, "tail").await;
    member.sync().await.expect("ordinary tail sync");
    assert_eq!(member.titles().await, ["before revocation", "tail"]);
    assert!(revoked.sync().await.is_err());
    member.close().await;
    let member = Device::open(&member_path, None)?;
    assert_eq!(member.titles().await, ["before revocation", "tail"]);
    member.close().await;
    revoked.close().await;
    a.close().await;
    Ok(())
}

/// Controls sync admission independently of enrollment, without wall-clock sleeps.
struct Gate {
    inner: Arc<dyn rocky_sdk::transport::Transport>,
    allowed: tokio::sync::watch::Sender<bool>,
    attempts: tokio::sync::watch::Sender<u64>,
    refuse: std::sync::atomic::AtomicBool,
    max_attempts: std::sync::atomic::AtomicU64,
    pause_on_attempt: std::sync::atomic::AtomicU64,
}
impl rocky_sdk::transport::Transport for Gate {
    fn kind(&self) -> &str {
        self.inner.kind()
    }
    fn address(&self) -> rocky_sdk::transport::BoxFuture<'_, Result<String, Failure>> {
        self.inner.address()
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: rocky_sdk::transport::Protocol,
    ) -> rocky_sdk::transport::BoxFuture<'a, Result<rocky_sdk::transport::Stream, Failure>> {
        Box::pin(async move {
            if protocol == rocky_sdk::transport::Protocol::Sync {
                self.attempts.send_modify(|count| *count += 1);
                if self.refuse.load(std::sync::atomic::Ordering::SeqCst)
                    || *self.attempts.borrow()
                        > self.max_attempts.load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Err("injected sync refusal".into());
                }
                if *self.attempts.borrow()
                    == self
                        .pause_on_attempt
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    self.allowed.send_replace(false);
                }
                self.allowed
                    .subscribe()
                    .wait_for(|allowed| *allowed)
                    .await?;
            }
            self.inner.connect(address, protocol).await
        })
    }
    fn accept(
        &self,
    ) -> rocky_sdk::transport::BoxFuture<
        '_,
        Option<(rocky_sdk::transport::Protocol, rocky_sdk::transport::Stream)>,
    > {
        self.inner.accept()
    }
    fn listen(&self, listening: bool) {
        self.inner.listen(listening);
    }
    fn close(&self) -> rocky_sdk::transport::BoxFuture<'_, ()> {
        self.inner.close()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn readiness_distinguishes_checkpoint_join_sync_and_ready_and_resumes_a_failed_join()
-> Result<(), Failure> {
    use rocky_sdk::transport::{Connector, Transport};
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?;
    a.add(1, "base").await;
    let gate = Arc::new(Gate {
        inner: hub.connector("member").start(false, [7; 32]).await?,
        allowed: tokio::sync::watch::channel(false).0,
        attempts: tokio::sync::watch::channel(0).0,
        refuse: std::sync::atomic::AtomicBool::new(false),
        max_attempts: std::sync::atomic::AtomicU64::new(u64::MAX),
        pause_on_attempt: std::sync::atomic::AtomicU64::new(u64::MAX),
    });
    let mut attempts = gate.attempts.subscribe();
    let path = root.path().join("member");
    let member = Device::join(&path, &a.invite().await, Network::Borrowed(gate.clone()))?;
    attempts.wait_for(|count| *count > 0).await?;
    let status = member.ok(json!({"type": "status"})).await;
    assert_eq!(status["state"], "bootstrapping");
    assert_eq!(status["ready"], false);
    gate.allowed.send_replace(true);
    member.sync().await.expect("finish join");
    let ready = member
        .ok(json!({"type": "status_next", "after": status["token"]}))
        .await;
    assert_eq!(ready["state"], "ready");
    assert_eq!(ready["ready"], true);
    gate.allowed.send_replace(false);
    let previous = *attempts.borrow_and_update();
    let sync = member.sync();
    let observe = async {
        attempts.wait_for(|count| *count > previous).await?;
        let status = member.ok(json!({"type": "status"})).await;
        assert_eq!(status["state"], "syncing");
        gate.allowed.send_replace(true);
        Ok::<_, Failure>(())
    };
    let (synced, observed) = tokio::join!(sync, observe);
    synced.expect("ordinary sync");
    observed?;
    member.close().await;
    gate.close().await;

    // Enrollment succeeds, but every sync fails: checkpoint intent survives reopen.
    let gate = Arc::new(Gate {
        inner: hub.connector("retry").start(false, [8; 32]).await?,
        allowed: tokio::sync::watch::channel(true).0,
        attempts: tokio::sync::watch::channel(0).0,
        refuse: std::sync::atomic::AtomicBool::new(true),
        max_attempts: std::sync::atomic::AtomicU64::new(u64::MAX),
        pause_on_attempt: std::sync::atomic::AtomicU64::new(u64::MAX),
    });
    let path = root.path().join("retry");
    let retry = Device::join(&path, &a.invite().await, Network::Borrowed(gate.clone()))?;
    assert!(retry.sync().await.is_err());
    assert_eq!(
        retry.ok(json!({"type": "status"})).await["state"],
        "bootstrapping"
    );
    retry.close().await;
    let retry = Device::open(&path, None)?;
    assert_eq!(
        retry.ok(json!({"type": "status"})).await["state"],
        "bootstrapping"
    );
    gate.refuse
        .store(false, std::sync::atomic::Ordering::SeqCst);
    retry
        .database
        .attach(Network::Borrowed(gate.clone()))
        .await?;
    retry.sync().await.expect("resume checkpoint-first join");
    assert_eq!(retry.ok(json!({"type": "status"})).await["state"], "ready");
    assert_eq!(retry.titles().await, ["base"]);
    retry.close().await;
    gate.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn renewal_after_missed_key_epochs_uses_a_checkpoint_without_acquiring_old_keys()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?;
    a.add(1, "epoch one").await;
    let b_path = root.path().join("b");
    let b = Device::join(
        &b_path,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    b.sync().await.expect("initial join");
    for (name, handle, title) in [("c", 2, "epoch two"), ("d", 3, "epoch three")] {
        let revoked = Device::join(
            &root.path().join(name),
            &a.invite().await,
            Network::Owned(hub.connector(name)),
        )?;
        revoked.sync().await.expect("join");
        a.ok(json!({"type": "revoke", "node": revoked.node().await}))
            .await;
        a.add(handle, title).await;
        revoked.close().await;
    }
    b.ok(json!({"type": "renew"})).await;
    assert_eq!(
        b.ok(json!({"type": "status"})).await["state"],
        "bootstrapping"
    );
    let identity: serde_json::Value =
        serde_json::from_slice(&std::fs::read(b_path.join("identity.json"))?)?;
    assert_eq!(
        identity["keys"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|epoch| epoch["generation"].clone())
            .collect::<Vec<_>>(),
        vec![json!(1), json!(3)]
    );
    b.sync().await.expect("checkpoint refresh after renewal");
    assert_eq!(a.items().await, b.items().await);
    assert_eq!(b.titles().await.len(), 3);
    assert_eq!(b.ok(json!({"type": "status"})).await["state"], "ready");
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_join_resumes_only_incomplete_scopes_after_reopen() -> Result<(), Failure> {
    use rocky_sdk::{
        Database, Open,
        transport::{Connector, Transport},
    };
    use std::sync::atomic::Ordering;
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = r#"{"application":"shopping","version":1,"format":4,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"notes","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"body","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
    let a_path = root.path().join("a");
    let a = Device::new(Database::create(
        Open {
            directory: &a_path,
            schema,
            secrets: None,
            network: Some(Network::Owned(hub.connector("a"))),
            commands: None,
        },
        None,
    )?)?;
    a.add(1, "first scope").await;
    let gate = Arc::new(Gate {
        inner: hub.connector("member").start(false, [9; 32]).await?,
        allowed: tokio::sync::watch::channel(true).0,
        attempts: tokio::sync::watch::channel(0).0,
        refuse: std::sync::atomic::AtomicBool::new(false),
        max_attempts: std::sync::atomic::AtomicU64::new(1),
        pause_on_attempt: std::sync::atomic::AtomicU64::new(u64::MAX),
    });
    let mut attempts = gate.attempts.subscribe();
    let path = root.path().join("member");
    let invitation = a
        .invite_as(json!({"kind": "writer", "scopes": ["items", "notes"]}))
        .await;
    let member = Device::new(Database::join(
        Open {
            directory: &path,
            schema,
            secrets: None,
            network: Some(Network::Borrowed(gate.clone())),
            commands: None,
        },
        &invitation,
    )?)?;
    attempts.wait_for(|count| *count >= 2).await?;
    assert!(member.sync().await.is_err());
    let progress: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path.join("bootstrap.json"))?)?;
    assert_eq!(
        progress["pending"]
            .as_array()
            .expect("pending scopes")
            .len(),
        1
    );
    assert_eq!(member.titles().await, ["first scope"]);
    member.close().await;
    let member = Device::new(Database::open(Open {
        directory: &path,
        schema,
        secrets: None,
        network: None,
        commands: None,
    })?)?;
    assert_eq!(
        member.ok(json!({"type": "status"})).await["state"],
        "bootstrapping"
    );
    let before = *attempts.borrow_and_update();
    gate.max_attempts.store(u64::MAX, Ordering::SeqCst);
    member
        .database
        .attach(Network::Borrowed(gate.clone()))
        .await?;
    member
        .sync()
        .await
        .expect("resume only the remaining scope");
    // One pending scope session, then one ordinary reconciliation session.
    assert_eq!(*attempts.borrow() - before, 2);
    assert_eq!(member.ok(json!({"type": "status"})).await["state"], "ready");
    assert!(!path.join("bootstrap.json").exists());
    member.close().await;
    gate.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_old_bootstrap_cannot_clear_newer_authority_intent() -> Result<(), Failure> {
    use rocky_sdk::{
        Database, Open,
        transport::{Connector, Transport},
    };
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let schema = r#"{"application":"shopping","version":1,"format":4,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"notes","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"body","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
    let a_path = root.path().join("a");
    let a = Device::new(Database::create(
        Open {
            directory: &a_path,
            schema,
            secrets: None,
            network: Some(Network::Owned(hub.connector("a"))),
            commands: None,
        },
        None,
    )?)?;
    a.add(1, "base").await;
    let gate = Arc::new(Gate {
        inner: hub.connector("member").start(false, [10; 32]).await?,
        allowed: tokio::sync::watch::channel(true).0,
        attempts: tokio::sync::watch::channel(0).0,
        refuse: std::sync::atomic::AtomicBool::new(false),
        max_attempts: std::sync::atomic::AtomicU64::new(u64::MAX),
        pause_on_attempt: std::sync::atomic::AtomicU64::new(2),
    });
    let mut attempts = gate.attempts.subscribe();
    let path = root.path().join("member");
    let invitation = a
        .invite_as(json!({"kind": "writer", "scopes": ["items", "notes"]}))
        .await;
    let member = Device::new(Database::join(
        Open {
            directory: &path,
            schema,
            secrets: None,
            network: Some(Network::Borrowed(gate.clone())),
            commands: None,
        },
        &invitation,
    )?)?;
    attempts.wait_for(|count| *count >= 2).await?;
    // Fault at the renewal durability boundary: intent is newer than the identity.
    rocky_credentials::write_private(&path.join("bootstrap.json"), b"2")?;
    gate.allowed.send_replace(true);
    assert!(member.sync().await.is_err());
    assert_eq!(std::fs::read(path.join("bootstrap.json"))?, b"2");
    assert_eq!(
        member.ok(json!({"type": "status"})).await["state"],
        "bootstrapping"
    );
    // Once the issuer supplies epoch 2, renewal resets all scopes and catches up.
    let c_path = root.path().join("revoked");
    let invitation = a
        .invite_as(json!({"kind": "reader", "scopes": ["notes"]}))
        .await;
    let c = Device::new(Database::join(
        Open {
            directory: &c_path,
            schema,
            secrets: None,
            network: Some(Network::Owned(hub.connector("revoked"))),
            commands: None,
        },
        &invitation,
    )?)?;
    c.sync().await.expect("join");
    a.ok(json!({"type": "revoke", "node": c.node().await}))
        .await;
    member.ok(json!({"type": "renew"})).await;
    // Retry after restoring authority explicitly signals connectivity again.
    member
        .ok(json!({"type": "connection", "online": true}))
        .await;
    member.sync().await.expect("fresh intent under epoch 2");
    assert_eq!(member.ok(json!({"type": "status"})).await["state"], "ready");
    assert_eq!(member.titles().await, ["base"]);
    member.close().await;
    c.close().await;
    gate.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn horizon_sdk_declares_finalizes_maintains_and_preserves_reopen_state() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let device = Device::create(root.path(), None)?;
    device.add(101, "base").await;
    let nodes: Vec<_> = (0..65)
        .map(|_| rocky_sdk::credentials::PrivateKey::new().public_key())
        .collect();
    let error = device
        .call(
            json!({"type":"horizon","collection":"items","action":"declare","participants":nodes}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("horizon_participants_full"));
    let unknown = rocky_sdk::credentials::PrivateKey::new().public_key();
    let error = device.call(json!({"type":"horizon","collection":"items","action":"declare","participants":[unknown]})).await.unwrap_err();
    assert!(error.to_string().contains("no registry record"));
    assert!(device.call(json!({"type":"horizon","collection":"items","action":"maintain","max_operations":0})).await.is_err());
    let declared = device
        .ok(json!({"type":"horizon","collection":"items","action":"declare","participants":[]}))
        .await;
    assert_eq!(declared["sequence"], 1);
    assert_eq!(declared["finalized"], false);
    let finalized = device
        .ok(json!({"type":"horizon","collection":"items","action":"finalize"}))
        .await;
    assert_eq!(finalized["finalized"], true);
    device.add(102, "fresh").await;
    device
        .ok(json!({"type":"horizon","collection":"items","action":"maintain","max_operations":1}))
        .await;
    let final_status = device
        .ok(json!({"type":"horizon","collection":"items","action":"maintain","max_operations":1}))
        .await;
    assert_eq!(final_status["sequence"], 2);
    assert_eq!(final_status["finalized"], true);
    assert_eq!(final_status["operations_since_finalization"], 0);
    device.close().await;
    let reopened = Device::open(root.path(), None)?;
    let status = reopened
        .ok(json!({"type":"horizon","collection":"items"}))
        .await;
    assert_eq!(status["statement"], final_status["statement"]);
    assert_eq!(status["finalized"], true);
    assert_eq!(reopened.titles().await, ["base", "fresh"]);
    reopened.close().await;
    Ok(())
}
