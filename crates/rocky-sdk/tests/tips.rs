//! Seals, tips and bookmarks through the JSON contract and `Session`. See
//! `docs/runtime/tips-and-bookmarks.md`.
mod common;
use common::{Device, Failure, introduce};
use rocky_sdk::{Network, transport::memory::Hub};
use serde_json::{Value, json};
use std::sync::Arc;

async fn tips(device: &Device, action: &str, body: Value) -> Result<Value, Value> {
    let mut request = json!({"type": "tips", "action": action});
    for (key, value) in body.as_object().into_iter().flatten() {
        request[key] = value.clone();
    }
    device.call(request).await
}
async fn ok(device: &Device, action: &str, body: Value) -> Value {
    tips(device, action, body)
        .await
        .unwrap_or_else(|error| panic!("{action} failed: {error}"))
}
fn message(error: Value) -> String {
    error["message"].as_str().unwrap_or_default().to_owned()
}
/// The owner's first bookmark, on the genesis tip, followed.
async fn main(device: &Device) -> Value {
    ok(device, "create", json!({"name": "main"})).await;
    ok(device, "follow", json!({"name": "main"})).await
}
fn seal_of(inspection: &Value, tip: &str) -> Value {
    inspection["seals"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|seal| seal["tip"] == tip && seal["canonical"] == true)
        .cloned()
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_continue_seal_moves_the_bookmark_and_keeps_every_row() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    let before = ok(&a, "status", json!({})).await;
    assert_eq!(before["bookmark"], Value::Null);
    assert!(message(tips(&a, "seal", json!({})).await.unwrap_err()).contains("follow"));
    let genesis = main(&a).await["tip"].clone();
    assert_eq!(genesis, before["tip"]);
    a.add(1, "milk").await;
    a.add(2, "eggs").await;
    let written = ok(&a, "status", json!({})).await;
    assert_eq!(written["bookmark"], "main");
    assert_eq!(written["operations_since_seal"], 2);
    assert_eq!(written["evidence_releasable"], Value::Null);

    let sealed = ok(&a, "seal", json!({})).await;
    assert_ne!(sealed["tip"], genesis);
    assert_eq!(sealed["bookmark"], "main");
    assert_eq!(sealed["operations_since_seal"], 0);
    assert_eq!(a.titles().await, ["eggs", "milk"]);
    a.add(3, "bread").await;
    assert_eq!(
        ok(&a, "status", json!({})).await["operations_since_seal"],
        1
    );

    let inspection = ok(&a, "inspect", json!({})).await;
    assert_eq!(inspection["genesis"], genesis);
    let seal = seal_of(&inspection, genesis.as_str().ok_or("tip")?);
    assert_eq!(seal["action"], "continue");
    assert_eq!(seal["id"], sealed["tip"]);
    assert_eq!(seal["frontier"].as_array().ok_or("frontier")?.len(), 1);
    let main = &inspection["bookmarks"][0];
    assert_eq!(
        (main["name"].clone(), main["tip"].clone()),
        (json!("main"), sealed["tip"].clone())
    );
    assert_eq!(main["moves"].as_array().ok_or("moves")?.len(), 2);
    let local = &inspection["nodes"][0];
    assert_eq!(local["followed_tip"], sealed["tip"]);
    assert_eq!(local["kept_seals"], json!([]));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_close_seal_makes_the_bookmark_read_only() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    let tip = main(&a).await["tip"].clone();
    a.add(1, "milk").await;
    let closed = ok(&a, "seal", json!({"close": true})).await;
    assert_eq!(closed["tip"], tip);
    let write = a
        .call(json!({
            "type": "mutate",
            "handle": vec![2; 32],
            "payload": json!({"type": "create", "collection": "items", "values": {"title": "eggs", "completed": false}}).to_string().into_bytes(),
            "entity": null,
            "baseline": null,
        }))
        .await;
    assert!(write.is_err(), "a closed tip took a write");
    assert_eq!(a.titles().await, ["milk"]);
    let inspection = ok(&a, "inspect", json!({})).await;
    assert_eq!(
        seal_of(&inspection, tip.as_str().ok_or("tip")?)["action"],
        "close"
    );
    assert!(message(tips(&a, "seal", json!({})).await.unwrap_err()).contains("sealed"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_and_authority_are_checked() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?;
    let b = Device::join(
        &root.path().join("b"),
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    main(&a).await;
    let sealed = ok(&a, "seal", json!({})).await;
    b.sync().await.expect("sync");
    ok(&b, "follow", json!({"name": "main"})).await;
    assert_eq!(ok(&b, "status", json!({})).await["tip"], sealed["tip"]);

    // A writer neither seals nor moves bookmarks.
    for (action, body) in [
        ("seal", json!({})),
        ("create", json!({"name": "mine"})),
        ("delete", json!({"name": "main"})),
    ] {
        let refused = message(tips(&b, action, body).await.unwrap_err());
        assert!(refused.contains("only the owner"), "{action}: {refused}");
    }

    // An issuer seals and moves forward, but opens no tip and moves nothing back.
    let c = Device::join(
        &root.path().join("c"),
        &a.invite_as(json!({"kind": "issuer"})).await,
        Network::Owned(hub.connector("c")),
    )?;
    introduce(&a, &c).await;
    c.sync().await.expect("issuer sync");
    ok(&c, "follow", json!({"name": "main"})).await;
    c.add(1, "issued").await;
    let resealed = ok(&c, "seal", json!({})).await;
    assert_ne!(resealed["tip"], sealed["tip"]);
    let issued = ok(&c, "inspect", json!({})).await;
    let reseal = seal_of(&issued, sealed["tip"].as_str().ok_or("tip")?)["id"].clone();
    let opened = message(
        tips(&c, "open", json!({"seal": reseal, "minimum_in_sync": 1}))
            .await
            .unwrap_err(),
    );
    assert!(opened.contains("trusted"), "{opened}");
    assert!(
        tips(&c, "move", json!({"name": "main", "tip": sealed["tip"]}))
            .await
            .is_err()
    );
    assert_eq!(ok(&c, "status", json!({})).await["tip"], resealed["tip"]);
    let inspection = ok(&a, "inspect", json!({})).await;
    let seal = seal_of(
        &inspection,
        inspection["genesis"].as_str().ok_or("genesis")?,
    )["id"]
        .clone();
    for (action, body) in [
        ("unknown", json!({})),
        ("create", json!({"name": "main"})),
        ("move", json!({"name": "absent", "tip": sealed["tip"]})),
        ("move", json!({"name": "main", "tip": sealed["tip"]})),
        ("open", json!({"seal": seal})),
        (
            "open",
            json!({"seal": "00".repeat(32), "minimum_in_sync": 1}),
        ),
        ("drop", json!({"tip": sealed["tip"]})),
        ("follow", json!({"name": "absent"})),
        ("seal", json!({"close": "yes"})),
    ] {
        assert!(
            tips(&a, action, body.clone()).await.is_err(),
            "{action} {body}"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_branch_writes_under_its_own_node_and_keeps_it_across_reopen() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    let genesis = main(&a).await["tip"].as_str().ok_or("tip")?.to_owned();
    a.add(1, "milk").await;
    let main_tip = ok(&a, "seal", json!({})).await["tip"].clone();
    let seal = seal_of(&ok(&a, "inspect", json!({})).await, &genesis)["id"].clone();
    let dev = ok(&a, "open", json!({"seal": seal, "minimum_in_sync": 1})).await["tip"].clone();
    ok(&a, "create", json!({"name": "dev", "tip": dev})).await;
    let followed = ok(&a, "follow", json!({"name": "dev"})).await;
    assert_eq!(
        (followed["bookmark"].clone(), followed["tip"].clone()),
        (json!("dev"), dev.clone())
    );
    assert_eq!(a.titles().await, ["milk"]);
    a.add(2, "dev only").await;
    a.close().await;

    let a = Device::open(root.path(), None)?;
    assert_eq!(ok(&a, "status", json!({})).await["tip"], dev);
    a.add(3, "after reopen").await;
    assert_eq!(a.titles().await, ["after reopen", "dev only", "milk"]);

    ok(&a, "follow", json!({"name": "main"})).await;
    assert_eq!(a.titles().await, ["milk"]);
    a.add(4, "main again").await;
    assert!(tips(&a, "drop", json!({"tip": main_tip})).await.is_err());
    // Dropping would lose writes no other device holds yet.
    let unsent = message(tips(&a, "drop", json!({"tip": dev})).await.unwrap_err());
    assert!(unsent.contains("evidence"), "{unsent}");
    let nodes = ok(&a, "inspect", json!({})).await["nodes"].clone();
    assert!(
        nodes[0]["kept_tips"]
            .as_array()
            .ok_or("kept")?
            .contains(&dev)
    );
    assert_eq!(a.titles().await, ["main again", "milk"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn moving_main_back_closes_its_tip_and_holds_the_abandoned_writes() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    let genesis = main(&a).await["tip"].as_str().ok_or("tip")?.to_owned();
    a.add(1, "kept").await;
    let continued = ok(&a, "seal", json!({})).await["tip"].clone();
    a.add(2, "abandoned").await;
    let seal = seal_of(&ok(&a, "inspect", json!({})).await, &genesis)["id"].clone();
    let earlier = ok(&a, "open", json!({"seal": seal, "minimum_in_sync": 1})).await["tip"].clone();

    let moved = ok(&a, "move", json!({"name": "main", "tip": earlier})).await;
    assert_eq!(moved["tip"], earlier);
    let inspection = ok(&a, "inspect", json!({})).await;
    assert_eq!(
        seal_of(&inspection, continued.as_str().ok_or("tip")?)["action"],
        "close"
    );
    assert_eq!(a.titles().await, ["kept"]);
    assert_eq!(a.ok(json!({"type": "held_changes"})).await["count"], 1);
    a.add(3, "rewritten").await;
    assert_eq!(a.titles().await, ["kept", "rewritten"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn devices_on_main_never_receive_a_branch_frames() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?;
    let b = Device::join(
        &root.path().join("b"),
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    let genesis = main(&a).await["tip"].as_str().ok_or("tip")?.to_owned();
    a.add(1, "shared").await;
    // Peers exchange frames only while they follow the same tip.
    b.sync().await.expect("graph sync");
    ok(&b, "follow", json!({"name": "main"})).await;
    let sealed = ok(&a, "seal", json!({})).await;
    b.sync().await.expect("seal sync");
    assert_eq!(ok(&b, "status", json!({})).await["tip"], sealed["tip"]);
    let seal = seal_of(&ok(&a, "inspect", json!({})).await, &genesis)["id"].clone();
    let dev = ok(&a, "open", json!({"seal": seal, "minimum_in_sync": 1})).await["tip"].clone();
    ok(&a, "create", json!({"name": "dev", "tip": dev})).await;
    ok(&a, "follow", json!({"name": "dev"})).await;
    a.add(2, "dev only").await;
    b.add(3, "main only").await;
    b.sync().await.expect("sync");
    assert_eq!(b.titles().await, ["main only", "shared"]);
    assert_eq!(a.titles().await, ["dev only", "shared"]);
    let names: Vec<_> = ok(&b, "inspect", json!({})).await["bookmarks"]
        .as_array()
        .ok_or("bookmarks")?
        .iter()
        .map(|bookmark| bookmark["name"].clone())
        .collect();
    assert_eq!(names, [json!("dev"), json!("main")]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_api_follows_the_json_contract() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    let session = a.database.session();
    let genesis = session.create_bookmark("main", None).await?.tip;
    assert_eq!(
        session.follow("main").await?.bookmark.as_deref(),
        Some("main")
    );
    let sealed = session.seal(false).await?;
    let inspection = session.inspect_tips().await?;
    let seal = inspection
        .seals
        .iter()
        .find(|seal| seal.tip == genesis)
        .ok_or("seal")?;
    assert_eq!(seal.id, sealed.tip);
    let dev = session.open_tip(&seal.id, 1).await?;
    session.create_bookmark("dev", Some(&dev)).await?;
    // Off an open tip this device does not follow, a sideways move is refused.
    assert!(session.move_bookmark("dev", &sealed.tip).await.is_err());
    session.create_bookmark("release", Some(&genesis)).await?;
    let release = session.move_bookmark("release", &sealed.tip).await?;
    assert_eq!(release.tip, sealed.tip);
    assert_eq!(session.delete_bookmark("dev").await?.tip, sealed.tip);
    let bookmarks = session.inspect_tips().await?.bookmarks;
    assert_eq!(
        bookmarks.iter().find(|b| b.name == "dev").ok_or("dev")?.tip,
        None
    );
    assert!(session.drop_tip(&dev).await.is_ok());
    assert_eq!(session.tip_status().await?.tip, sealed.tip);
    Ok(())
}

/// Coordinated seals: a seal or a move off an open tip ends that tip's log
/// first. See `docs/runtime/coordinated-writes.md#seals-and-moves`.
mod coordinated {
    use super::*;
    use rocky_coordinator::coordinated::{MemoryLog, MemoryLogClient};
    use rocky_core::{
        Append, Commitment, CoordinationFuture, DomainLog, LogCut, LogHead, LogPending, LogResult,
        LogTerminal, NodeKey, TipId, TipLog,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    /// A client whose next `seal` waits until the test releases it, so
    /// another node can end the log in between.
    struct Gated {
        inner: MemoryLogClient,
        armed: AtomicBool,
        entered: Notify,
        release: Notify,
    }
    impl DomainLog for Gated {
        fn node(&self) -> NodeKey {
            self.inner.node()
        }
        fn log_head(&self, log: TipLog) -> CoordinationFuture<'_, LogHead> {
            self.inner.log_head(log)
        }
        fn configure(&self, log: TipLog, minimum: usize) -> CoordinationFuture<'_, ()> {
            self.inner.configure(log, minimum)
        }
        fn join(
            &self,
            log: TipLog,
            position: u64,
            running_hash: Commitment,
            sequence: u64,
        ) -> CoordinationFuture<'_, u64> {
            self.inner.join(log, position, running_hash, sequence)
        }
        fn append(&self, log: TipLog, append: Append) -> CoordinationFuture<'_, LogPending> {
            self.inner.append(log, append)
        }
        fn acknowledge(
            &self,
            log: TipLog,
            pending: LogPending,
        ) -> CoordinationFuture<'_, LogResult> {
            self.inner.acknowledge(log, pending)
        }
        fn cut(&self, log: TipLog, cut: LogCut) -> CoordinationFuture<'_, LogResult> {
            self.inner.cut(log, cut)
        }
        fn seal(&self, log: TipLog, terminal: LogTerminal) -> CoordinationFuture<'_, LogResult> {
            Box::pin(async move {
                if self.armed.swap(false, Ordering::SeqCst) {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
                self.inner.seal(log, terminal).await
            })
        }
    }

    fn domain(device: &Device) -> rocky_core::Scope {
        serde_json::from_str(&device.database.scope()).expect("scope")
    }
    async fn node_key(device: &Device) -> rocky_sdk::credentials::PublicKey {
        device.node().await.parse().expect("node key")
    }
    fn tip(hex: &Value) -> TipId {
        TipId(
            data_encoding::HEXLOWER
                .decode(hex.as_str().expect("tip").as_bytes())
                .expect("hex")
                .try_into()
                .expect("tip"),
        )
    }
    fn log_of(device: &Device, tip_hex: &Value) -> TipLog {
        TipLog {
            scope: domain(device),
            tip: tip(tip_hex),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_seal_ends_its_tips_coordinated_log() -> Result<(), Failure> {
        let root = tempfile::tempdir()?;
        let a = Device::create(root.path(), None)?;
        let log = MemoryLog::new();
        log.configure(domain(&a), 1)?;
        a.database
            .coordinate(Some(Arc::new(log.client(node_key(&a).await))))
            .await?;
        let genesis = main(&a).await["tip"].clone();
        a.add(1, "milk").await;
        let sealed = ok(&a, "seal", json!({})).await;
        let seal = seal_of(
            &ok(&a, "inspect", json!({})).await,
            genesis.as_str().ok_or("tip")?,
        );
        let terminal = log
            .head(log_of(&a, &genesis))?
            .terminal
            .ok_or("log not ended")?;
        assert_eq!(hex(&terminal.digest), seal["id"]);
        assert_eq!(terminal.continuation, Some(tip(&sealed["tip"])));
        // Writes continue on the continuation, whose own log is open.
        a.add(2, "eggs").await;
        assert_eq!(a.titles().await, ["eggs", "milk"]);
        assert_eq!(log.head(log_of(&a, &sealed["tip"]))?.terminal, None);
        a.close().await;
        Ok(())
    }
    fn hex(digest: &Commitment) -> Value {
        json!(data_encoding::HEXLOWER.encode(digest))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_seal_another_node_ended_first_fails_sealed() -> Result<(), Failure> {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let a = Device::create(
            &root.path().join("a"),
            Some(Network::Owned(hub.connector("a"))),
        )?;
        // A joiner seals only a tip it holds history of, not the one it imported.
        main(&a).await;
        let open = ok(&a, "seal", json!({})).await["tip"].clone();
        let b = Device::join(
            &root.path().join("b"),
            &a.invite_as(json!({"kind": "issuer"})).await,
            Network::Owned(hub.connector("b")),
        )?;
        introduce(&a, &b).await;
        let log = MemoryLog::new();
        log.configure(domain(&a), 1)?;
        let gated = Arc::new(Gated {
            inner: log.client(node_key(&a).await),
            armed: AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
        });
        a.database.coordinate(Some(gated.clone())).await?;
        b.database
            .coordinate(Some(Arc::new(log.client(node_key(&b).await))))
            .await?;
        b.sync().await.expect("graph sync");
        ok(&b, "follow", json!({"name": "main"})).await;

        // a's seal passed its checks and waits at the coordinator while b
        // ends the log and hands a its seal.
        gated.armed.store(true, Ordering::SeqCst);
        let a = Arc::new(a);
        let sealing = tokio::spawn({
            let a = a.clone();
            async move { tips(&a, "seal", json!({})).await }
        });
        gated.entered.notified().await;
        let theirs = ok(&b, "seal", json!({})).await;
        let _ = b.sync().await;
        gated.release.notify_one();
        let refused = sealing.await?.expect_err("a lost the race");
        assert_eq!(refused["code"], "sealed", "{refused}");
        assert_eq!(refused["retryable"], false);
        // The log ended with b's seal, and a follows b's continuation.
        let terminal = log
            .head(log_of(&a, &open))?
            .terminal
            .ok_or("log not ended")?;
        assert_eq!(terminal.continuation, Some(tip(&theirs["tip"])));
        // a published nothing: the first seal and b's.
        let inspection = ok(&a, "inspect", json!({})).await;
        assert_eq!(inspection["seals"].as_array().map(Vec::len), Some(2));
        assert_eq!(ok(&a, "status", json!({})).await["tip"], theirs["tip"]);
        b.close().await;
        a.close().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unreachable_coordinator_refuses_seals_and_moves_and_publishes_nothing()
    -> Result<(), Failure> {
        let root = tempfile::tempdir()?;
        let a = Device::create(root.path(), None)?;
        let log = MemoryLog::new();
        log.configure(domain(&a), 1)?;
        a.database
            .coordinate(Some(Arc::new(log.client(node_key(&a).await))))
            .await?;
        let genesis = main(&a).await["tip"].clone();
        let first = ok(&a, "seal", json!({})).await;
        a.add(1, "milk").await;

        log.set_available(false);
        for (action, body) in [
            ("seal", json!({})),
            ("seal", json!({"close": true})),
            // Moving `main` back off its open tip needs that tip's close seal.
            ("move", json!({"name": "main", "tip": genesis})),
        ] {
            let refused = tips(&a, action, body).await.expect_err("unconfirmed");
            assert_eq!(refused["code"], "coordinator_unavailable", "{refused}");
            assert_eq!(refused["retryable"], true);
        }
        let inspection = ok(&a, "inspect", json!({})).await;
        assert_eq!(inspection["seals"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            inspection["bookmarks"][0]["moves"].as_array().map(Vec::len),
            Some(2)
        );
        let status = ok(&a, "status", json!({})).await;
        assert_eq!(status["tip"], first["tip"]);

        // Once the coordinator answers, the same call succeeds.
        log.set_available(true);
        assert_eq!(log.head(log_of(&a, &first["tip"]))?.terminal, None);
        let sealed = ok(&a, "seal", json!({})).await;
        assert_ne!(sealed["tip"], first["tip"]);
        assert!(log.head(log_of(&a, &first["tip"]))?.terminal.is_some());
        assert_eq!(a.titles().await, ["milk"]);
        a.close().await;
        Ok(())
    }
}

/// A trusted identity through the SDK: the owner grants it, its device
/// installs the grant, and it opens tips and moves bookmarks back.
mod trusted {
    use super::*;

    struct Household {
        _root: tempfile::TempDir,
        owner: Device,
        devices: Vec<Device>,
    }
    /// The owner on `main` and `count` writers following it, every pair
    /// introduced, then one row and a seal. The writers join before any
    /// row: a device that imported a checkpoint neither seals nor switches
    /// tips until phase 15 places checkpoints at seals.
    async fn household(count: usize) -> Result<Household, Failure> {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let owner = Device::create(
            &root.path().join("owner"),
            Some(Network::Owned(hub.connector("owner"))),
        )?;
        main(&owner).await;
        let mut devices = Vec::new();
        for index in 0..count {
            let name = format!("d{index}");
            let device = Device::join(
                &root.path().join(&name),
                &owner.invite().await,
                Network::Owned(hub.connector(&name)),
            )?;
            introduce(&owner, &device).await;
            for other in &devices {
                introduce(other, &device).await;
            }
            device.sync().await.expect("join sync");
            ok(&device, "follow", json!({"name": "main"})).await;
            devices.push(device);
        }
        owner.add(1, "before").await;
        ok(&owner, "seal", json!({})).await;
        let all: Vec<&Device> = std::iter::once(&owner).chain(&devices).collect();
        settle(&all).await;
        for device in &devices {
            assert_eq!(device.titles().await, ["before"]);
        }
        Ok(Household {
            _root: root,
            owner,
            devices,
        })
    }
    async fn identity(device: &Device) -> String {
        device.ok(json!({"type": "session"})).await["principal"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
    /// The owner grants `device`'s identity trust and the device installs it.
    async fn trust(owner: &Device, device: &Device) -> Value {
        let granted = ok(owner, "trust", json!({"identity": identity(device).await})).await;
        ok(device, "install_trust", json!({"grant": granted["grant"]})).await;
        granted
    }
    /// Syncs every device until the last round moved nothing new.
    async fn settle(devices: &[&Device]) {
        for _ in 0..3 {
            for device in devices {
                let _ = device.sync().await;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_trusted_identity_opens_a_branch_that_devices_on_main_never_see()
    -> Result<(), Failure> {
        let house = household(3).await?;
        let owner = &house.owner;
        let [t, u, m] = &house.devices[..] else {
            return Err("devices".into());
        };
        let inspection = ok(t, "inspect", json!({})).await;
        let main_tip = ok(t, "status", json!({})).await["tip"].clone();
        let seal = inspection["seals"]
            .as_array()
            .and_then(|seals| seals.iter().find(|seal| seal["id"] == main_tip))
            .ok_or("main's latest seal")?["id"]
            .clone();
        // Without the grant a writer opens nothing.
        let refused = tips(t, "open", json!({"seal": seal, "minimum_in_sync": 1}))
            .await
            .expect_err("untrusted");
        assert!(message(refused).contains("only the owner"));
        trust(owner, t).await;

        let dev = ok(t, "open", json!({"seal": seal, "minimum_in_sync": 1})).await["tip"].clone();
        ok(t, "create", json!({"name": "dev", "tip": dev})).await;
        ok(t, "follow", json!({"name": "dev"})).await;
        u.sync().await.expect("graph sync");
        ok(u, "follow", json!({"name": "dev"})).await;
        t.add(2, "t on dev").await;
        u.add(3, "u on dev").await;
        m.add(4, "m on main").await;
        settle(&[t, u, m, owner]).await;
        for device in [t, u] {
            assert_eq!(device.titles().await, ["before", "t on dev", "u on dev"]);
        }
        for device in [m, owner] {
            assert_eq!(device.titles().await, ["before", "m on main"]);
        }

        // u returns to main, drops dev, and its writes there reach main.
        ok(u, "follow", json!({"name": "main"})).await;
        ok(u, "drop", json!({"tip": dev})).await;
        u.add(5, "u on main").await;
        settle(&[u, m, owner, t]).await;
        for device in [u, m, owner] {
            assert_eq!(device.titles().await, ["before", "m on main", "u on main"]);
        }
        assert_eq!(t.titles().await, ["before", "t on dev", "u on dev"]);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_trusted_identity_moves_main_back_and_the_abandoned_writes_are_held()
    -> Result<(), Failure> {
        let house = household(2).await?;
        let owner = &house.owner;
        let [t, m] = &house.devices[..] else {
            return Err("devices".into());
        };
        trust(owner, t).await;
        let earlier = ok(t, "status", json!({})).await["tip"].clone();
        let inspection = ok(t, "inspect", json!({})).await;
        let seal = inspection["seals"]
            .as_array()
            .and_then(|seals| seals.iter().find(|seal| seal["id"] == earlier))
            .ok_or("seal")?["id"]
            .clone();
        // main moves on; m writes on the line t then abandons.
        t.add(2, "kept").await;
        ok(t, "seal", json!({})).await;
        settle(&[t, m, owner]).await;
        m.add(3, "abandoned").await;
        settle(&[m, t, owner]).await;
        for device in [t, m, owner] {
            assert_eq!(device.titles().await, ["abandoned", "before", "kept"]);
        }
        let back = ok(t, "open", json!({"seal": seal, "minimum_in_sync": 1})).await["tip"].clone();
        let moved = ok(t, "move", json!({"name": "main", "tip": back})).await;
        assert_eq!(moved["tip"], back);
        settle(&[t, m, owner]).await;
        for device in [t, m, owner] {
            assert_eq!(ok(device, "status", json!({})).await["tip"], back);
            assert_eq!(device.titles().await, ["before"]);
            let held = device.ok(json!({"type": "held_changes"})).await;
            assert!(held["count"].as_u64() >= Some(1), "{held}");
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_a_trusted_grant_ends_the_identitys_tip_authority() -> Result<(), Failure> {
        let house = household(1).await?;
        let owner = &house.owner;
        let t = &house.devices[0];
        let granted = trust(owner, t).await;
        // Only the owner grants, and only an identity's own device installs.
        assert!(
            tips(t, "trust", json!({"identity": identity(owner).await}))
                .await
                .is_err()
        );
        assert!(
            tips(owner, "install_trust", json!({"grant": granted["grant"]}))
                .await
                .is_err()
        );
        owner
            .ok(json!({"type": "revoke", "trust": granted["capability"]}))
            .await;
        settle(&[owner, t]).await;
        let tip = ok(t, "status", json!({})).await["tip"].clone();
        let seal = ok(t, "inspect", json!({})).await["seals"]
            .as_array()
            .and_then(|seals| seals.iter().find(|seal| seal["id"] == tip))
            .ok_or("seal")?["id"]
            .clone();
        let refused = tips(t, "open", json!({"seal": seal, "minimum_in_sync": 1}))
            .await
            .expect_err("trust revoked");
        assert!(message(refused).contains("only the owner"));
        // The identity keeps writing under its other grants.
        t.add(2, "still writing").await;
        Ok(())
    }
}
