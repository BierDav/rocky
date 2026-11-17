//! Seals, tips and bookmarks through the JSON contract and `Session`.
use crate::common;
mod schema_changes;
use crate::support::snapshot as snap;
use common::{Device, Failure, introduce};
use rocky_sdk::{Network, transport::memory::Hub};
use serde_json::{Value, json};
use std::sync::Arc;

fn message(error: Value) -> String {
    error["message"].as_str().unwrap_or_default().to_owned()
}
/// The owner's first bookmark, on the genesis tip, followed.
async fn main(device: &Device) -> Value {
    snap::ok(device.session.create_bookmark("main", None)).await;
    snap::ok(device.follow("main")).await
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
async fn a_trusted_node_only_raises_the_genesis_minimum_and_the_roots_governs()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    main(&a).await;
    let minimum = async || snap::ok(a.session.inspect_tips()).await["genesis_minimum"].clone();
    snap::ok(a.session.set_genesis_minimum(1)).await;
    snap::ok(a.session.set_genesis_minimum(3)).await;
    assert_eq!(minimum().await, 3);
    let lowered = snap::observe(a.session.set_genesis_minimum(2)).await;
    assert!(
        message(lowered.unwrap_err()).contains("stays 3"),
        "a trusted node cannot lower the genesis minimum and says so"
    );
    snap::ok(a.session.set_root_genesis_minimum((a.root()).clone(), 2)).await;
    assert_eq!(
        minimum().await,
        2,
        "the root lowers the minimum without revoking anyone"
    );
    let raised = snap::observe(a.session.set_genesis_minimum(5)).await;
    assert!(
        message(raised.unwrap_err()).contains("stays 2"),
        "once the root set the minimum, a trusted node no longer changes it"
    );
    assert_eq!(minimum().await, 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_continue_seal_moves_the_bookmark_and_keeps_every_row() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    let before = snap::ok(a.session.tip_status()).await;
    assert_eq!(before["bookmark"], Value::Null);
    assert!(message(snap::observe(a.session.seal(false)).await.unwrap_err()).contains("follow"));
    let genesis = main(&a).await["tip"].clone();
    assert_eq!(genesis, before["tip"]);
    a.add(1, "milk").await;
    a.add(2, "eggs").await;
    let written = snap::ok(a.session.tip_status()).await;
    assert_eq!(written["bookmark"], "main");
    assert_eq!(written["operations_since_seal"], 2);
    assert_eq!(written["evidence_releasable"], Value::Null);

    let sealed = snap::ok(a.session.seal(false)).await;
    assert_ne!(sealed["tip"], genesis);
    assert_eq!(sealed["bookmark"], "main");
    assert_eq!(sealed["operations_since_seal"], 0);
    assert_eq!(a.titles().await, ["eggs", "milk"]);
    a.add(3, "bread").await;
    assert_eq!(
        snap::ok(a.session.tip_status()).await["operations_since_seal"],
        1
    );

    let inspection = snap::ok(a.session.inspect_tips()).await;
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
    let closed = snap::ok(a.session.seal(true)).await;
    assert_eq!(closed["tip"], tip);
    let write = snap::observe(snap::mutate(
        &a.session,
        vec![2; 32],
        snap::Action::Create {
            table: "items".to_owned(),
            values: vec![
                ("title".to_owned(), snap::field(json!("eggs"))),
                ("completed".to_owned(), snap::field(json!(false))),
            ],
        },
        None,
        None,
    ))
    .await;
    assert!(write.is_err(), "a closed tip took a write");
    assert_eq!(a.titles().await, ["milk"]);
    let inspection = snap::ok(a.session.inspect_tips()).await;
    assert_eq!(
        seal_of(&inspection, tip.as_str().ok_or("tip")?)["action"],
        "close"
    );
    assert!(message(snap::observe(a.session.seal(false)).await.unwrap_err()).contains("sealed"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn compact_keeps_the_requested_seals_and_every_row_across_reopen() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    main(&a).await;
    let none = snap::ok(a.session.compaction()).await;
    assert_eq!(
        (none["compacted"].clone(), none["pruned"].clone()),
        (Value::Null, json!(0))
    );
    a.add(1, "milk").await;
    let eggs = a.add(2, "eggs").await;
    a.set(3, &eggs, "completed", json!(true)).await;
    let first = snap::ok(a.session.seal(false)).await["tip"].clone();
    a.add(4, "bread").await;
    let second = snap::ok(a.session.seal(false)).await["tip"].clone();

    assert_eq!(
        snap::ok(a.session.compaction()).await["compacted"],
        Value::Null
    );
    let kept = snap::ok(a.session.compact(1)).await;
    assert_eq!(kept["finalized"], second);
    assert_eq!(kept["compacted"], first);
    assert_eq!(kept["uncompacted"], json!([second]));
    assert_eq!(kept["rebootstrap"], Value::Null);
    let all = snap::ok(a.session.compact(0)).await;
    assert_eq!(all["compacted"], second);
    assert_eq!(all["uncompacted"], json!([]));
    let inspected = snap::ok(a.session.inspect_tips()).await;
    let checkpoints_beside_seal = |seal: &Value| -> Vec<Value> {
        inspected["seals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate["id"] == *seal)
            .unwrap()["checkpoints"]
            .as_array()
            .unwrap()
            .clone()
    };
    let at_first = checkpoints_beside_seal(&first);
    assert_eq!(at_first.len(), 2);
    let mut rows: Vec<_> = at_first
        .iter()
        .map(|record| record["rows"].as_u64().unwrap())
        .collect();
    rows.sort_unstable();
    assert_eq!(rows, [0, 2]);
    assert_ne!(at_first[0]["scope"], at_first[1]["scope"]);
    let at_second = checkpoints_beside_seal(&second);
    assert_eq!(at_second.len(), 1);
    assert_eq!(at_second[0]["rows"], 3);
    for record in at_first.iter().chain(&at_second) {
        assert_eq!(record["authority"], "trusted");
        assert!(
            record["signer"]
                .as_str()
                .is_some_and(|signer| signer.len() == 64)
        );
        let scope: rocky_core::ResourceScope = serde_json::from_value(record["scope"].clone())?;
        assert_eq!(scope.domain, a.database.scope());
    }
    let empty_registry = at_first.iter().find(|record| record["rows"] == 0).unwrap();
    let changed = at_first.iter().find(|record| record["rows"] == 2).unwrap();
    assert_eq!(at_second[0]["scope"], changed["scope"]);
    assert_ne!(
        at_second[0]["scope"], empty_registry["scope"],
        "the unchanged empty registry keeps its signed base at the first seal"
    );
    assert_ne!(at_second[0]["checkpoint"], changed["checkpoint"]);
    let items = ["bread/0", "eggs/1", "milk/0"];
    assert_eq!(a.items().await, items);
    a.close().await;

    let a = Device::open(root.path(), None)?;
    assert_eq!(a.items().await, items);
    let reopened = snap::ok(a.session.compaction()).await;
    assert_eq!(reopened["compacted"], second);
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
    let sealed = snap::ok(a.session.seal(false)).await;
    b.sync().await.expect("sync");
    snap::ok(b.follow("main")).await;
    assert_eq!(snap::ok(b.session.tip_status()).await["tip"], sealed["tip"]);

    let writer_attempts = [
        ("seal", snap::observe(b.session.seal(false)).await),
        (
            "create",
            snap::observe(b.session.create_bookmark("mine", None)).await,
        ),
        (
            "delete",
            snap::observe(b.session.delete_bookmark("main")).await,
        ),
    ];
    for (action, result) in writer_attempts {
        let refused = message(result.unwrap_err());
        assert!(
            refused.contains("only a trusted identity or an issuer"),
            "a writer neither seals nor moves bookmarks; {action}: {refused}"
        );
    }

    let issuer = Device::join(
        &root.path().join("c"),
        &a.invite_as(json!({"kind": "issuer"})).await,
        Network::Owned(hub.connector("c")),
    )?;
    introduce(&a, &issuer).await;
    issuer.sync().await.expect("issuer sync");
    snap::ok(issuer.follow("main")).await;
    issuer.add(1, "issued").await;
    let resealed = snap::ok(issuer.session.seal(false)).await;
    assert_ne!(
        resealed["tip"], sealed["tip"],
        "an issuer seals and moves its bookmark forward"
    );
    let issued = snap::ok(issuer.session.inspect_tips()).await;
    let reseal = seal_of(&issued, sealed["tip"].as_str().ok_or("tip")?)["id"].clone();
    let opened = message(
        snap::observe(async {
            let value = issuer.open_tip(&(snap::text(&(reseal))), 1).await?;
            Ok(json!({"tip":value}))
        })
        .await
        .unwrap_err(),
    );
    assert!(
        opened.contains("trusted"),
        "an issuer opens no tip: {opened}"
    );
    assert!(
        snap::observe(issuer.move_bookmark("main", &(snap::text(&(sealed["tip"])))))
            .await
            .is_err(),
        "an issuer moves no bookmark back"
    );
    assert_eq!(
        snap::ok(issuer.session.tip_status()).await["tip"],
        resealed["tip"]
    );
    for result in [
        snap::observe(a.session.create_bookmark("main", None)).await,
        snap::observe(a.move_bookmark("absent", &snap::text(&sealed["tip"]))).await,
        snap::observe(a.move_bookmark("main", &snap::text(&sealed["tip"]))).await,
        snap::observe(a.open_tip(&"00".repeat(32), 1)).await,
        snap::observe(a.session.drop_tip(&snap::text(&sealed["tip"]))).await,
        snap::observe(a.follow("absent")).await,
    ] {
        assert!(result.is_err(), "{result:?}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_branch_writes_under_its_own_node_and_keeps_it_across_reopen() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    let genesis = main(&a).await["tip"].as_str().ok_or("tip")?.to_owned();
    a.add(1, "milk").await;
    let main_tip = snap::ok(a.session.seal(false)).await["tip"].clone();
    let seal = seal_of(&snap::ok(a.session.inspect_tips()).await, &genesis)["id"].clone();
    let dev = snap::ok(async {
        let value = a.open_tip(&(snap::text(&(seal))), 1).await?;
        Ok(json!({"tip":value}))
    })
    .await["tip"]
        .clone();
    snap::ok(
        a.session
            .create_bookmark("dev", Some(&(snap::text(&(dev))))),
    )
    .await;
    let followed = snap::ok(a.follow("dev")).await;
    assert_eq!(
        (followed["bookmark"].clone(), followed["tip"].clone()),
        (json!("dev"), dev.clone())
    );
    assert_eq!(a.titles().await, ["milk"]);
    a.add(2, "dev only").await;
    a.close().await;

    let a = Device::open(root.path(), None)?;
    assert_eq!(snap::ok(a.session.tip_status()).await["tip"], dev);
    a.add(3, "after reopen").await;
    assert_eq!(a.titles().await, ["after reopen", "dev only", "milk"]);

    snap::ok(a.follow("main")).await;
    assert_eq!(a.titles().await, ["milk"]);
    a.add(4, "main again").await;
    assert!(
        snap::observe(a.session.drop_tip(&(snap::text(&(main_tip)))))
            .await
            .is_err()
    );
    snap::ok(a.session.drop_tip(&(snap::text(&(dev))))).await;
    let nodes = snap::ok(a.session.inspect_tips()).await["nodes"].clone();
    assert!(
        !nodes[0]["kept_tips"]
            .as_array()
            .ok_or("kept")?
            .contains(&dev),
        "with the default custody of zero, an explicit drop stops keeping the branch tip"
    );
    assert_eq!(a.titles().await, ["main again", "milk"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn moving_main_back_closes_its_tip_and_withdraws_the_abandoned_writes() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    let genesis = main(&a).await["tip"].as_str().ok_or("tip")?.to_owned();
    snap::ok(
        a.session.set_kept_seals(
            serde_json::from_value::<rocky_sdk::KeptSeals>(json!({"bookmarked": true}))
                .expect("typed fixture"),
        ),
    )
    .await;
    a.add(1, "kept").await;
    let continued = snap::ok(a.session.seal(false)).await["tip"].clone();
    a.add(2, "abandoned").await;
    let seal = seal_of(&snap::ok(a.session.inspect_tips()).await, &genesis)["id"].clone();
    let earlier = snap::ok(async {
        let value = a.open_tip(&(snap::text(&(seal))), 1).await?;
        Ok(json!({"tip":value}))
    })
    .await["tip"]
        .clone();

    let moved = snap::ok(a.move_bookmark("main", &(snap::text(&(earlier))))).await;
    assert_eq!(moved["tip"], earlier);
    let inspection = snap::ok(a.session.inspect_tips()).await;
    assert_eq!(
        seal_of(&inspection, continued.as_str().ok_or("tip")?)["action"],
        "close"
    );
    assert_eq!(a.titles().await, ["kept"]);
    assert_eq!(
        snap::ok(a.session.held_changes()).await["count"],
        0,
        "intents have no rows to re-author, so the abandoned write goes with its line"
    );
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
    b.sync().await.expect("graph sync");
    snap::ok(b.follow("main")).await;
    let sealed = snap::ok(a.session.seal(false)).await;
    b.sync().await.expect("seal sync");
    assert_eq!(snap::ok(b.session.tip_status()).await["tip"], sealed["tip"]);
    let seal = seal_of(&snap::ok(a.session.inspect_tips()).await, &genesis)["id"].clone();
    let dev = snap::ok(async {
        let value = a.open_tip(&(snap::text(&(seal))), 1).await?;
        Ok(json!({"tip":value}))
    })
    .await["tip"]
        .clone();
    snap::ok(
        a.session
            .create_bookmark("dev", Some(&(snap::text(&(dev))))),
    )
    .await;
    snap::ok(a.follow("dev")).await;
    a.add(2, "dev only").await;
    b.add(3, "main only").await;
    b.sync().await.expect("sync");
    assert_eq!(b.titles().await, ["main only", "shared"]);
    assert_eq!(a.titles().await, ["dev only", "shared"]);
    let names: Vec<_> = snap::ok(b.session.inspect_tips()).await["bookmarks"]
        .as_array()
        .ok_or("bookmarks")?
        .iter()
        .map(|bookmark| bookmark["name"].clone())
        .collect();
    assert_eq!(names, [json!("dev"), json!("main")]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn bookmark_operations_work_through_a_native_offline_session() -> Result<(), Failure> {
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
    assert!(
        session.move_bookmark("dev", &sealed.tip).await.is_err(),
        "a sideways move off an open tip this device does not follow is refused"
    );
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
/// first.
mod coordinated {
    use super::*;
    use rocky_coordinator::coordinated::MemoryLog;
    use rocky_core::{
        Append, Commitment, CoordinationFuture, DomainLog, LogHead, LogPending, LogResult,
        LogTerminal, NodeKey, TipId, TipLog,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    /// A client whose next `seal` waits until the test releases it, so
    /// another node can end the log in between.
    struct Gated {
        inner: Arc<dyn rocky_sdk::Coordinator>,
        armed: Arc<AtomicBool>,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }
    impl DomainLog for Gated {
        fn pending_status(
            &self,
            log: TipLog,
            position: u64,
            hash: rocky_core::Commitment,
        ) -> CoordinationFuture<'_, rocky_core::LogStatus> {
            Box::pin(async move { Ok(self.log_head(log).await?.status(position, hash)) })
        }
        fn identity(&self) -> rocky_core::CoordinatorIdentity {
            self.inner.identity()
        }
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
        fn recover(&self, log: TipLog, seal: Vec<u8>) -> CoordinationFuture<'_, LogResult> {
            self.inner.recover(log, seal)
        }
        fn acknowledge_terminal(
            &self,
            log: TipLog,
            acknowledgment: Vec<u8>,
        ) -> CoordinationFuture<'_, ()> {
            self.inner.acknowledge_terminal(log, acknowledgment)
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

    impl rocky_sdk::Coordinator for Gated {
        fn bundle(&self) -> Result<rocky_sdk::CoordinatorBundle, rocky_core::CoordinationError> {
            self.inner.bundle()
        }
        fn bind(
            &self,
            key: rocky_sdk::credentials::PrivateKey,
            certificate: rocky_sdk::credentials::SignedCapability,
        ) -> Result<Arc<dyn rocky_sdk::Coordinator>, rocky_core::CoordinationError> {
            Ok(Arc::new(Self {
                inner: self.inner.bind(key, certificate)?,
                armed: self.armed.clone(),
                entered: self.entered.clone(),
                release: self.release.clone(),
            }))
        }
        fn certify(&self, certificate: rocky_sdk::credentials::SignedCapability) {
            self.inner.certify(certificate);
        }
        fn observe(&self, observer: rocky_sdk::credentials::CoordinationObserver) {
            self.inner.observe(observer);
        }
        fn present_claims(&self, claims: Option<rocky_sdk::credentials::CredentialChain>) {
            self.inner.present_claims(claims);
        }
        fn claim(
            &self,
            scope: rocky_core::Scope,
            counter: rocky_sdk::counters::Name,
            claim: rocky_sdk::counters::Claim,
        ) -> CoordinationFuture<'_, u64> {
            self.inner.claim(scope, counter, claim)
        }
        fn counter(
            &self,
            scope: rocky_core::Scope,
            counter: rocky_sdk::counters::Name,
            claim: Option<rocky_sdk::counters::Claim>,
        ) -> CoordinationFuture<'_, rocky_sdk::counters::Reading> {
            self.inner.counter(scope, counter, claim)
        }
        fn revoke(
            &self,
            scope: rocky_core::Scope,
            revocations: rocky_sdk::credentials::RevocationSet,
        ) -> CoordinationFuture<'_, Commitment> {
            self.inner.revoke(scope, revocations)
        }
    }

    fn domain(device: &Device) -> rocky_core::Scope {
        device.database.scope()
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
    async fn a_final_seal_compacts_on_trusted_and_member_devices_in_one_pass() -> Result<(), Failure>
    {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let a = Device::create(
            &root.path().join("a"),
            Some(Network::Owned(hub.connector("a"))),
        )?;
        main(&a).await;
        snap::ok(a.session.set_genesis_minimum(1)).await;
        let log = MemoryLog::new();
        log.configure(domain(&a), 1)?;
        a.database
            .coordinate(Some(Arc::new(log.client(node_key(&a).await))))
            .await?;
        snap::ok(a.session.declare_coordinated(a.root())).await;
        a.add(1, "base").await;
        let b = Device::join(
            &root.path().join("b"),
            &a.invite_as(json!({"kind": "issuer"})).await,
            Network::Owned(hub.connector("b")),
        )?;
        let c = Device::join(
            &root.path().join("c"),
            &a.invite().await,
            Network::Owned(hub.connector("c")),
        )?;
        let identity = snap::ok(b.session.info()).await["principal"].clone();
        let grant = snap::ok(
            a.session
                .trust(&(snap::text(&(identity))), (a.root()).clone()),
        )
        .await;
        snap::ok(b.session.install_trust(&(snap::text(&(grant["grant"]))))).await;
        b.database
            .coordinate(Some(Arc::new(log.client(node_key(&b).await))))
            .await?;
        for peer in [&b, &c] {
            introduce(&a, peer).await;
            peer.sync().await.map_err(|error| error.to_string())?;
            snap::ok(peer.follow("main")).await;
            assert_eq!(
                snap::ok(peer.session.compaction()).await["compacted"],
                Value::Null
            );
        }
        let sealed = snap::ok(a.session.seal(false)).await["tip"].clone();
        b.sync().await.map_err(|error| error.to_string())?;
        assert_eq!(snap::ok(b.session.compaction()).await["compacted"], sealed);
        a.sync().await.map_err(|error| error.to_string())?;
        c.sync().await.map_err(|error| error.to_string())?;
        for peer in [&a, &b, &c] {
            assert_eq!(
                snap::ok(peer.session.compaction()).await["compacted"],
                sealed
            );
            assert_eq!(peer.titles().await, ["base"]);
        }
        for peer in [&a, &b, &c] {
            peer.close().await;
        }
        Ok(())
    }

    /// The owner may name only the coordinator the database's finality
    /// records name: settings naming another would make its final seals,
    /// compacted ones too, non-final everywhere.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_owner_names_only_the_coordinator_its_finality_records_name() -> Result<(), Failure>
    {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let a = Device::create(
            &root.path().join("a"),
            Some(Network::Owned(hub.connector("a"))),
        )?;
        main(&a).await;
        snap::ok(a.session.set_genesis_minimum(1)).await;
        let log = MemoryLog::new();
        log.configure(domain(&a), 1)?;
        a.database
            .coordinate(Some(Arc::new(log.client(node_key(&a).await))))
            .await?;
        a.add(1, "base").await;
        snap::ok(a.session.seal(false)).await;
        a.sync().await.map_err(|error| error.to_string())?;
        let final_seals = |inspection: rocky_sdk::TipInspection| {
            inspection
                .seals
                .into_iter()
                .filter(|seal| seal.finalized)
                .count()
        };
        assert_eq!(final_seals(a.session.inspect_tips().await?), 1);

        let other = MemoryLog::new();
        other.configure(domain(&a), 1)?;
        a.database
            .coordinate(Some(Arc::new(other.client(node_key(&a).await))))
            .await?;
        let refused = a.session.declare_coordinated(a.root()).await.unwrap_err();
        assert_eq!(
            rocky_sdk::CallFailure::from_failure(&refused).code(),
            rocky_secure::COORDINATOR_MISMATCH
        );
        let inspection = a.session.inspect_tips().await?;
        assert!(inspection.coordinator.is_none(), "nothing was signed");
        assert_eq!(final_seals(inspection), 1);

        a.database
            .coordinate(Some(Arc::new(log.client(node_key(&a).await))))
            .await?;
        snap::ok(a.session.declare_coordinated(a.root())).await;
        let inspection = a.session.inspect_tips().await?;
        assert!(inspection.coordinator.is_some());
        assert_eq!(final_seals(inspection), 1, "the seal stays final");
        a.close().await;
        Ok(())
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
        let sealed = snap::ok(a.session.seal(false)).await;
        let seal = seal_of(
            &snap::ok(a.session.inspect_tips()).await,
            genesis.as_str().ok_or("tip")?,
        );
        let terminal = log
            .head(log_of(&a, &genesis))?
            .terminal
            .ok_or("log not ended")?;
        assert_eq!(hex(&terminal.digest), seal["id"]);
        assert_eq!(terminal.continuation, Some(tip(&sealed["tip"])));

        a.add(2, "eggs").await;
        assert_eq!(a.titles().await, ["eggs", "milk"]);
        assert_eq!(
            log.head(log_of(&a, &sealed["tip"]))?.terminal,
            None,
            "writes continue on the continuation, whose own log is open"
        );
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
        main(&a).await;
        let open = snap::ok(a.session.seal(false)).await["tip"].clone();
        let b = Device::join(
            &root.path().join("b"),
            &a.invite_as(json!({"kind": "issuer"})).await,
            Network::Owned(hub.connector("b")),
        )?;
        introduce(&a, &b).await;
        let log = MemoryLog::new();
        log.configure(domain(&a), 1)?;
        let gated = Arc::new(Gated {
            inner: Arc::new(log.client(node_key(&a).await)),
            armed: Arc::new(AtomicBool::new(false)),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        });
        a.database.coordinate(Some(gated.clone())).await?;
        b.database
            .coordinate(Some(Arc::new(log.client(node_key(&b).await))))
            .await?;
        b.sync().await.expect("graph sync");
        snap::ok(b.follow("main")).await;

        gated.armed.store(true, Ordering::SeqCst);
        let a = Arc::new(a);
        let mut sealing = tokio::spawn({
            let a = a.clone();
            async move { snap::observe(a.session.seal(false)).await }
        });
        tokio::select! {
            biased;
            () = gated.entered.notified() => {},
            result = &mut sealing => return Err(format!("seal ended before the coordinator gate: {result:?}").into()),
        }
        let theirs = snap::ok(b.session.seal(false)).await;
        let _ = b.sync().await;
        gated.release.notify_one();
        let refused = sealing.await?.expect_err("a lost the race");
        assert_eq!(refused["code"], "sealed", "{refused}");
        assert_eq!(refused["retryable"], false);
        let terminal = log
            .head(log_of(&a, &open))?
            .terminal
            .ok_or("log not ended")?;
        assert_eq!(
            terminal.continuation,
            Some(tip(&theirs["tip"])),
            "the log ended with b's seal"
        );
        let inspection = snap::ok(a.session.inspect_tips()).await;
        assert_eq!(
            inspection["seals"].as_array().map(Vec::len),
            Some(2),
            "the losing node published nothing: only the first seal and the winner's"
        );
        assert_eq!(snap::ok(a.session.tip_status()).await["tip"], theirs["tip"]);
        b.close().await;
        a.close().await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_statement_after_one_waiting_on_its_log_entry_follows_it_without_a_fork()
    -> Result<(), Failure> {
        let root = tempfile::tempdir()?;
        let a = Device::create(root.path(), None)?;
        let log = MemoryLog::new();
        log.configure(domain(&a), 1)?;
        a.database
            .coordinate(Some(Arc::new(log.client(node_key(&a).await))))
            .await?;
        main(&a).await;
        a.add(1, "milk").await;
        log.set_available(false);
        let held = snap::observe(a.session.seal(false))
            .await
            .expect_err("unconfirmed");
        assert_eq!(held["code"], "coordinator_unavailable", "{held}");
        let later = snap::observe(a.session.create_bookmark("later", None))
            .await
            .expect_err("the next statement chains after the held seal and waits behind it");
        assert_eq!(later["code"], "chain_pending", "{later}");
        assert_eq!(later["retryable"], true);

        log.set_available(true);
        let sealed = snap::ok(a.session.seal(false)).await;
        assert!(
            log.head(log_of(&a, &main_tip(&a).await))?
                .terminal
                .is_none()
        );
        let inspection = snap::ok(a.session.inspect_tips()).await;
        let names: Vec<&str> = inspection["bookmarks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|bookmark| bookmark["name"].as_str())
            .collect();
        assert_eq!(
            names,
            ["later", "main"],
            "retrying the seal settles both statements in chain order: {inspection}"
        );
        assert_eq!(inspection["seals"].as_array().map(Vec::len), Some(1));
        assert_eq!(snap::ok(a.session.tip_status()).await["tip"], sealed["tip"]);
        let alerts = snap::ok(a.session.status()).await["alerts"].clone();
        assert!(
            !alerts.to_string().contains("tip_chain_equivocation"),
            "{alerts}"
        );
        a.close().await;
        Ok(())
    }
    async fn main_tip(device: &Device) -> Value {
        snap::ok(device.session.tip_status()).await["tip"].clone()
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
        let first = snap::ok(a.session.seal(false)).await;
        a.add(1, "milk").await;

        log.set_available(false);
        let first_waits_and_later_seals_of_its_tip_are_not_journaled = [
            (
                snap::observe(a.session.seal(false)).await,
                "coordinator_unavailable",
            ),
            (snap::observe(a.session.seal(true)).await, "seal_pending"),
            (
                snap::observe(a.move_bookmark("main", &snap::text(&genesis))).await,
                "seal_pending",
            ),
        ];
        for (result, code) in first_waits_and_later_seals_of_its_tip_are_not_journaled {
            let refused = result.expect_err("unconfirmed");
            assert_eq!(refused["code"], code, "{refused}");
            assert_eq!(refused["retryable"], true);
        }
        let inspection = snap::ok(a.session.inspect_tips()).await;
        assert_eq!(inspection["seals"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            inspection["bookmarks"][0]["moves"].as_array().map(Vec::len),
            Some(2)
        );
        let status = snap::ok(a.session.tip_status()).await;
        assert_eq!(status["tip"], first["tip"]);

        log.set_available(true);
        assert_eq!(log.head(log_of(&a, &first["tip"]))?.terminal, None);
        let sealed = snap::ok(a.session.seal(false)).await;
        assert_ne!(
            sealed["tip"], first["tip"],
            "once the coordinator answers, the same seal succeeds"
        );
        assert!(log.head(log_of(&a, &first["tip"]))?.terminal.is_some());
        assert_eq!(a.titles().await, ["milk"]);
        let inspection = snap::ok(a.session.inspect_tips()).await;
        let only_the_retried_seal_was_published =
            "the close seal and the move off the waiting tip were refused, never journaled";
        assert_eq!(
            inspection["seals"].as_array().map(Vec::len),
            Some(2),
            "{only_the_retried_seal_was_published}"
        );
        assert_eq!(
            inspection["bookmarks"][0]["moves"].as_array().map(Vec::len),
            Some(3),
            "{only_the_retried_seal_was_published}"
        );
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
    /// The owner on `main` and `count` devices following it, every pair
    /// introduced, then one row and a seal. Only the first device is an
    /// issuer. Devices join before any row, because a device that imported a
    /// checkpoint neither seals nor switches tips.
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
            let is_first_device_and_the_only_issuer = index == 0;
            let invitation = if is_first_device_and_the_only_issuer {
                owner.invite_as(json!({"kind": "issuer"})).await
            } else {
                owner.invite().await
            };
            let device = Device::join(
                &root.path().join(&name),
                &invitation,
                Network::Owned(hub.connector(&name)),
            )?;
            introduce(&owner, &device).await;
            for other in &devices {
                introduce(other, &device).await;
            }
            device.sync().await.expect("join sync");
            snap::ok(device.follow("main")).await;
            devices.push(device);
        }
        owner.add(1, "before").await;
        snap::ok(owner.session.seal(false)).await;
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
        snap::ok(device.session.info()).await["principal"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
    /// The owner grants `device`'s identity trust and the device installs it.
    async fn trust(owner: &Device, device: &Device) -> Value {
        let granted = snap::ok(owner.session.trust(
            &(snap::text(&(identity(device).await))),
            (owner.root()).clone(),
        ))
        .await;
        snap::ok(
            device
                .session
                .install_trust(&(snap::text(&(granted["grant"])))),
        )
        .await;
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
        let inspection = snap::ok(t.session.inspect_tips()).await;
        let main_tip = snap::ok(t.session.tip_status()).await["tip"].clone();
        let seal = inspection["seals"]
            .as_array()
            .and_then(|seals| seals.iter().find(|seal| seal["id"] == main_tip))
            .ok_or("main's latest seal")?["id"]
            .clone();
        let refused = snap::observe(async {
            let value = t.open_tip(&(snap::text(&(seal))), 1).await?;
            Ok(json!({"tip":value}))
        })
        .await
        .expect_err("without the grant an issuer opens no branch");
        let refused = message(refused);
        assert!(
            refused.contains("a trusted identity opens tips"),
            "{refused}"
        );
        trust(owner, t).await;

        let dev = snap::ok(async {
            let value = t.open_tip(&(snap::text(&(seal))), 1).await?;
            Ok(json!({"tip":value}))
        })
        .await["tip"]
            .clone();
        snap::ok(
            t.session
                .create_bookmark("dev", Some(&(snap::text(&(dev))))),
        )
        .await;
        snap::ok(t.follow("dev")).await;
        u.sync().await.expect("graph sync");
        snap::ok(u.follow("dev")).await;
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

        snap::ok(u.follow("main")).await;
        snap::ok(u.session.drop_tip(&(snap::text(&(dev))))).await;
        u.add(5, "u on main").await;
        settle(&[u, m, owner, t]).await;
        for device in [u, m, owner] {
            assert_eq!(
                device.titles().await,
                ["before", "m on main", "u on main"],
                "a device back on main after dropping dev writes to main"
            );
        }
        assert_eq!(t.titles().await, ["before", "t on dev", "u on dev"]);
        Ok(())
    }

    /// A device that compacted, and released history, at main's seal still
    /// drops a branch it wrote on: its base holds no frame of the branch.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_compacted_device_drops_a_branch_it_wrote_on() -> Result<(), Failure> {
        let house = household(2).await?;
        let owner = &house.owner;
        let [t, u] = &house.devices[..] else {
            return Err("devices".into());
        };
        trust(owner, t).await;
        let main_tip = snap::ok(t.session.tip_status()).await["tip"].clone();
        snap::ok(
            u.session.set_retention(
                "items",
                serde_json::from_value::<rocky_sdk::RetentionPolicy>(
                    json!({"required": [], "behind": [], "copies": 1, "window": 0}),
                )
                .expect("typed fixture"),
            ),
        )
        .await;
        settle(&[t, u, owner]).await;
        let compacted = snap::ok(u.session.compact(0)).await;
        assert_eq!(compacted["compacted"], main_tip, "{compacted}");
        let dev = snap::ok(async {
            let value = t.open_tip(&(snap::text(&(main_tip))), 1).await?;
            Ok(json!({"tip":value}))
        })
        .await["tip"]
            .clone();
        snap::ok(
            t.session
                .create_bookmark("dev", Some(&(snap::text(&(dev))))),
        )
        .await;
        snap::ok(t.follow("dev")).await;
        u.sync().await.expect("graph sync");
        snap::ok(u.follow("dev")).await;
        u.add(3, "u on dev").await;
        settle(&[t, u]).await;
        assert_eq!(t.titles().await, ["before", "u on dev"]);
        snap::ok(u.follow("main")).await;
        snap::ok(u.session.drop_tip(&(snap::text(&(dev))))).await;
        assert_eq!(u.titles().await, ["before"]);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_trusted_identity_moves_main_back_and_the_abandoned_writes_are_withdrawn()
    -> Result<(), Failure> {
        let house = household(2).await?;
        let owner = &house.owner;
        let [t, m] = &house.devices[..] else {
            return Err("devices".into());
        };
        trust(owner, t).await;
        snap::ok(
            t.session.set_kept_seals(
                serde_json::from_value::<rocky_sdk::KeptSeals>(json!({"every": true}))
                    .expect("typed fixture"),
            ),
        )
        .await;
        let earlier = snap::ok(t.session.tip_status()).await["tip"].clone();
        let inspection = snap::ok(t.session.inspect_tips()).await;
        let seal = inspection["seals"]
            .as_array()
            .and_then(|seals| seals.iter().find(|seal| seal["id"] == earlier))
            .ok_or("seal")?["id"]
            .clone();
        t.add(2, "kept").await;
        snap::ok(t.session.seal(false)).await;
        settle(&[t, m, owner]).await;
        m.add(3, "abandoned").await;
        settle(&[m, t, owner]).await;
        for device in [t, m, owner] {
            assert_eq!(device.titles().await, ["abandoned", "before", "kept"]);
        }
        let back = snap::ok(async {
            let value = t.open_tip(&(snap::text(&(seal))), 1).await?;
            Ok(json!({"tip":value}))
        })
        .await["tip"]
            .clone();
        let moved = snap::ok(t.move_bookmark("main", &(snap::text(&(back))))).await;
        assert_eq!(moved["tip"], back);
        settle(&[t, m, owner]).await;
        for device in [t, m, owner] {
            assert_eq!(snap::ok(device.session.tip_status()).await["tip"], back);
            assert_eq!(device.titles().await, ["before"]);
            let held = snap::ok(device.session.held_changes()).await;
            assert_eq!(held["count"], 0, "{held}");
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_a_trusted_node_ends_its_tip_authority() -> Result<(), Failure> {
        let house = household(1).await?;
        let owner = &house.owner;
        let t = &house.devices[0];
        let granted = trust(owner, t).await;
        assert!(
            snap::observe(t.session.trust(
                &(snap::text(&(identity(owner).await))),
                std::sync::Arc::new(rocky_sdk::RootKey::generate())
            ))
            .await
            .is_err(),
            "only the root grants trust"
        );
        assert!(
            snap::observe(
                owner
                    .session
                    .install_trust(&(snap::text(&(granted["grant"]))))
            )
            .await
            .is_err(),
            "only the trusted identity's own device installs its grant"
        );
        snap::ok(
            owner.session.revoke_identity_with_root(
                &(owner
                    .session
                    .identities()
                    .await?
                    .into_iter()
                    .find(|identity| {
                        identity.trusted.as_ref().is_some_and(|trust| {
                            data_encoding::HEXLOWER.encode(&trust.capability)
                                == snap::text(&(granted["capability"]))
                        })
                    })
                    .ok_or("unknown trust")?
                    .identity
                    .to_hex()),
                rocky_sdk::RevocationMode::Compromised,
                Some((owner.root()).clone()),
                true,
            ),
        )
        .await;
        settle(&[owner, t]).await;
        let tip = snap::ok(t.session.tip_status()).await["tip"].clone();
        let seal = snap::ok(t.session.inspect_tips()).await["seals"]
            .as_array()
            .and_then(|seals| seals.iter().find(|seal| seal["id"] == tip))
            .ok_or("seal")?["id"]
            .clone();
        snap::observe(async {
            let value = t.open_tip(&(snap::text(&(seal))), 1).await?;
            Ok(json!({"tip":value}))
        })
        .await
        .expect_err("the revoked trusted node learned it and opens nothing more");
        let status = snap::ok(t.session.status()).await;
        assert!(
            status["authority"]["verified_revocation"].is_object(),
            "{status}"
        );
        Ok(())
    }
}

/// Races between trusted identities resolve by authority, then digest;
/// revocation resolves them again. The issuer variants run in `rocky-secure`.
mod races {
    use super::*;

    struct Household {
        _root: tempfile::TempDir,
        hub: Arc<Hub>,
        owner: Device,
        trusted: [Device; 2],
        member: Device,
        grants: [Value; 2],
    }
    /// The owner, two trusted identities and a writer, all on `main` after
    /// one row and a seal. They join before any row.
    async fn household() -> Result<Household, Failure> {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let owner = Device::create(
            &root.path().join("owner"),
            Some(Network::Owned(hub.connector("owner"))),
        )?;
        main(&owner).await;
        let mut joined = Vec::new();
        for name in ["t0", "t1", "m"] {
            let invitation = if name == "m" {
                owner.invite().await
            } else {
                owner.invite_as(json!({"kind": "issuer"})).await
            };
            let device = Device::join(
                &root.path().join(name),
                &invitation,
                Network::Owned(hub.connector(name)),
            )?;
            introduce(&owner, &device).await;
            for other in &joined {
                introduce(other, &device).await;
            }
            device.sync().await.expect("join sync");
            snap::ok(device.follow("main")).await;
            joined.push(device);
        }
        let mut grants = Vec::new();
        for device in &joined[..2] {
            let identity = snap::ok(device.session.info()).await["principal"].clone();
            let granted = snap::ok(
                owner
                    .session
                    .trust(&(snap::text(&(identity))), (owner.root()).clone()),
            )
            .await;
            snap::ok(
                device
                    .session
                    .install_trust(&(snap::text(&(granted["grant"])))),
            )
            .await;
            grants.push(granted);
        }
        owner.add(1, "before").await;
        snap::ok(owner.session.seal(false)).await;
        let mut joined = joined.into_iter();
        let (t0, t1, member) = (
            joined.next().ok_or("t0")?,
            joined.next().ok_or("t1")?,
            joined.next().ok_or("m")?,
        );
        let house = Household {
            _root: root,
            hub,
            owner,
            trusted: [t0, t1],
            member,
            grants: [grants[0].clone(), grants[1].clone()],
        };
        let tip = snap::ok(house.owner.session.tip_status()).await["tip"].clone();
        settle_on(&house.all(), &tip, &["before"]).await;
        for device in house.all() {
            assert_eq!(device.titles().await, ["before"]);
        }
        Ok(house)
    }
    impl Household {
        fn all(&self) -> Vec<&Device> {
            vec![
                &self.owner,
                &self.trusted[0],
                &self.trusted[1],
                &self.member,
            ]
        }
    }
    async fn settle(devices: &[&Device]) {
        for _ in 0..3 {
            for device in devices {
                let _ = device.sync().await;
            }
        }
    }
    /// Stop once the observable tip and rows converge, with the same
    /// three-round bound as `settle`.
    async fn settle_on(devices: &[&Device], tip: &Value, titles: &[&str]) {
        for _ in 0..3 {
            for device in devices {
                let _ = device.sync().await;
            }
            let mut ready = true;
            for device in devices {
                if snap::ok(device.session.tip_status()).await["tip"] != *tip {
                    ready = false;
                    break;
                }
                let page = snap::observe(device.session.read(rocky_sdk::ReadQuery {
                    sql: "SELECT title FROM items ORDER BY title".to_owned(),
                    values: Vec::new(),
                    max_rows: 64,
                    max_bytes: None,
                    cursor: None,
                }))
                .await;
                let rows = page.as_ref().ok().and_then(|page| page["rows"].as_array());
                let found: Option<Vec<&str>> = rows.map(|rows| {
                    rows.iter()
                        .filter_map(|row| row[0]["value"].as_str())
                        .collect()
                });
                if found.as_deref() != Some(titles) {
                    ready = false;
                    break;
                }
            }
            if ready {
                return;
            }
        }
        panic!("devices did not converge on {tip} with {titles:?}");
    }
    fn seal_by_id<'a>(inspection: &'a Value, id: &Value) -> &'a Value {
        inspection["seals"]
            .as_array()
            .and_then(|seals| seals.iter().find(|seal| seal["id"] == *id))
            .unwrap_or(&Value::Null)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner()
    -> Result<(), Failure> {
        let house = household().await?;
        let (owner, member) = (&house.owner, &house.member);
        let tip = snap::ok(owner.session.tip_status()).await["tip"].clone();
        let continuations = [
            snap::ok(house.trusted[0].session.seal(false)).await["tip"].clone(),
            snap::ok(house.trusted[1].session.seal(false)).await["tip"].clone(),
        ];
        let lower_digest_wins = continuations[1].as_str() < continuations[0].as_str();
        let winner = usize::from(lower_digest_wins);
        settle_on(&house.all(), &continuations[winner], &["before"]).await;
        let next = continuations[1 - winner].clone();
        for device in house.all() {
            assert_eq!(
                snap::ok(device.session.tip_status()).await["tip"],
                continuations[winner]
            );
        }
        member.add(2, "on the winner").await;
        settle_on(
            &house.all(),
            &continuations[winner],
            &["before", "on the winner"],
        )
        .await;
        assert_eq!(owner.titles().await, ["before", "on the winner"]);

        let without_cut = false;
        snap::ok(
            owner.session.revoke_identity_with_root(
                &(owner
                    .session
                    .identities()
                    .await?
                    .into_iter()
                    .find(|identity| {
                        identity.trusted.as_ref().is_some_and(|trust| {
                            data_encoding::HEXLOWER.encode(&trust.capability)
                                == snap::text(&(house.grants[winner]["capability"]))
                        })
                    })
                    .ok_or("unknown trust")?
                    .identity
                    .to_hex()),
                rocky_sdk::RevocationMode::Compromised,
                Some((owner.root()).clone()),
                without_cut,
            ),
        )
        .await;
        let revoked = &house.trusted[winner];
        let rest: Vec<&Device> = house
            .all()
            .into_iter()
            .filter(|device| !std::ptr::eq(*device, revoked))
            .collect();
        settle(&rest).await;
        let followed = snap::ok(owner.session.tip_status()).await["tip"].clone();
        let rotation_resealed_next = followed != next;
        if rotation_resealed_next {
            let inspection = snap::ok(owner.session.inspect_tips()).await;
            assert_eq!(seal_by_id(&inspection, &followed)["tip"], next);
        }
        settle_on(&rest, &followed, &["before"]).await;
        for device in rest {
            assert_eq!(device.titles().await, ["before"]);
            let inspection = snap::ok(device.session.inspect_tips()).await;
            let revoked = seal_by_id(&inspection, &continuations[winner]);
            assert_eq!(
                (revoked["revoked"].clone(), revoked["canonical"].clone()),
                (json!(true), json!(false)),
                "a revocation without a cut takes the revoked seal out of resolution"
            );
            assert_eq!(seal_by_id(&inspection, &next)["canonical"], true);
            assert_eq!(seal_by_id(&inspection, &next)["tip"], tip);
        }
        let held = snap::ok(member.session.held_changes()).await;
        assert_eq!(
            held["count"], 0,
            "the write on the revoked continuation goes with it: {held}"
        );
        Ok(())
    }

    /// Alerts of `kind` in `device`'s status.
    async fn alerts(device: &Device, kind: &str) -> Vec<Value> {
        snap::ok(device.session.status()).await["alerts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|alert| alert["kind"]["type"] == kind)
            .cloned()
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_the_creator_keeps_what_it_sealed_and_main_moves_on_through_another_trusted_node()
    -> Result<(), Failure> {
        let house = household().await?;
        let (owner, member) = (&house.owner, &house.member);
        owner.add(2, "sealed by the creator").await;
        let sealed = snap::ok(owner.session.seal(false)).await["tip"].clone();
        let titles = ["before", "sealed by the creator"];
        settle_on(&house.all(), &sealed, &titles).await;

        let creator = snap::ok(owner.session.info()).await["principal"].clone();
        let (t0, t1) = (&house.trusted[0], &house.trusted[1]);
        snap::ok(t0.session.revoke_identity_with_root(
            &(snap::text(&(creator))),
            rocky_sdk::RevocationMode::Compromised,
            Some((owner.root()).clone()),
            true,
        ))
        .await;
        let rest = [t0, t1];
        settle(&rest).await;
        let followed = snap::ok(t0.session.tip_status()).await["tip"].clone();
        settle_on(&rest, &followed, &titles).await;
        for device in rest {
            let inspection = snap::ok(device.session.inspect_tips()).await;
            let kept = seal_by_id(&inspection, &sealed);
            assert_eq!(
                (kept["revoked"].clone(), kept["canonical"].clone()),
                (json!(false), json!(true)),
                "{inspection}"
            );
            assert!(alerts(device, "tip_statements_past_cut").await.is_empty());
        }
        let ended = member
            .sync()
            .await
            .expect_err("the member ends with the creator");
        assert!(ended.to_string().contains("revoked"), "{ended}");
        t0.add(3, "after").await;
        settle(&rest).await;
        let next = snap::ok(t1.session.seal(false)).await["tip"].clone();
        assert_ne!(
            next, followed,
            "main keeps moving through the other trusted node"
        );
        settle_on(&rest, &next, &["after", "before", "sealed by the creator"]).await;
        for device in house.all() {
            device.close().await;
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_statement_the_revoked_creator_signed_past_the_cut_is_deleted_and_surfaced()
    -> Result<(), Failure> {
        let house = household().await?;
        let (owner, member) = (&house.owner, &house.member);
        let (t0, t1) = (&house.trusted[0], &house.trusted[1]);
        isolate_t0(&house.hub, true);
        snap::ok(owner.session.create_bookmark("late", None)).await;
        settle(&[owner, t1, member]).await;
        assert_eq!(bookmarks(t1).await, ["late", "main"]);
        let creator = snap::ok(owner.session.info()).await["principal"].clone();
        snap::ok(t0.session.revoke_identity_with_root(
            &(snap::text(&(creator))),
            rocky_sdk::RevocationMode::Compromised,
            Some((owner.root()).clone()),
            true,
        ))
        .await;
        isolate_t0(&house.hub, false);
        settle(&[t0, t1]).await;
        let past = alerts(t1, "tip_statements_past_cut").await;
        assert_eq!(
            past.len(),
            1,
            "the device that held the statement past the cut deletes it and says so: {past:?}"
        );
        assert_eq!(past[0]["kind"]["statements"], 1, "{past:?}");
        assert_eq!(past[0]["kind"]["signer"], creator, "{past:?}");
        for device in [t0, t1] {
            let names = snap::ok(device.session.inspect_tips()).await["bookmarks"].clone();
            assert_eq!(names.as_array().map(Vec::len), Some(1), "{names}");
            assert_eq!(bookmarks(device).await, ["main"]);
        }
        assert!(
            alerts(t0, "tip_statements_past_cut").await.is_empty(),
            "the statement past the cut never reaches the device that proposed the cut"
        );
        for device in house.all() {
            device.close().await;
        }
        Ok(())
    }
    /// Cuts `t0` off from every other device, or reconnects it.
    fn isolate_t0(hub: &Hub, down: bool) {
        for other in ["owner", "t1", "m"] {
            hub.set_down("t0", other, down);
        }
    }
    async fn bookmarks(device: &Device) -> Vec<String> {
        snap::ok(device.session.inspect_tips()).await["bookmarks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|bookmark| !bookmark["tip"].is_null())
            .filter_map(|bookmark| bookmark["name"].as_str().map(str::to_owned))
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deferred_branch_switch_pulls_a_kept_seal_and_survives_reopen() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?;
    let b = Device::join(
        &root.path().join("b"),
        &a.invite_as(json!({"kind": "issuer"})).await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    main(&a).await;
    b.sync().await.expect("sync");
    snap::ok(b.follow("main")).await;
    snap::ok(
        b.session.set_kept_seals(
            serde_json::from_value::<rocky_sdk::KeptSeals>(json!({"every": true}))
                .expect("typed fixture"),
        ),
    )
    .await;
    a.add(1, "milk").await;
    let first = snap::ok(a.session.seal(false)).await["tip"].clone();
    a.add(2, "eggs").await;
    let second = snap::ok(a.session.seal(false)).await["tip"].clone();
    b.sync().await.expect("sync");
    snap::ok(b.session.compaction()).await;
    let held = |inspection: &Value, seal: &Value| {
        inspection["seals"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|known| known["id"] == *seal)
            .map(|known| known["held"].clone())
            .unwrap_or_default()
    };
    assert_eq!(
        held(&snap::ok(b.session.inspect_tips()).await, &first),
        "kept"
    );
    assert_eq!(snap::ok(a.session.compact(0)).await["compacted"], second);
    assert_eq!(
        held(&snap::ok(a.session.inspect_tips()).await, &first),
        Value::Null,
        "the owner compacted past the first seal and no longer holds it"
    );
    let open = json!({"seal": first, "minimum_in_sync": 1});
    let refused = message(
        snap::observe(a.open_tip(&snap::text(&open["seal"]), 1))
            .await
            .unwrap_err(),
    );
    assert!(refused.contains("pull"), "{refused}");

    let identity = snap::ok(b.session.info()).await["principal"].clone();
    let grant = snap::ok(
        a.session
            .trust(&(snap::text(&(identity))), (a.root()).clone()),
    )
    .await["grant"]
        .clone();
    snap::ok(b.session.install_trust(&(snap::text(&(grant))))).await;
    let branch = snap::ok(b.open_tip(&snap::text(&open["seal"]), 1)).await;
    snap::ok(
        b.session
            .create_bookmark("dev", Some(&(snap::text(&(branch))))),
    )
    .await;
    a.sync().await.expect("graph sync");
    let pending = snap::ok(a.follow_when_ready("dev")).await;
    assert_eq!(pending["pending_follow"], "dev");
    a.close().await;
    let offline = Device::open(&root.path().join("a"), None)?;
    assert_eq!(
        snap::ok(offline.session.tip_status()).await["pending_follow"],
        "dev"
    );
    offline.close().await;
    let a = Device::open(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?;
    let pulled = a.sync().await.expect("pull sync");
    let following = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let mut status = snap::ok(a.session.status()).await;
        loop {
            let tips = snap::ok(a.session.tip_status()).await;
            if tips["pending_follow"].is_null() && tips["bookmark"] == "dev" {
                break;
            }
            status = snap::ok(a.session.status_after(snap::text(&(status["token"])))).await;
        }
    })
    .await;
    if let Err(error) = following {
        return Err(format!(
            "{error}; pull report: {pulled}; tips: {}",
            snap::ok(a.session.inspect_tips()).await
        )
        .into());
    }
    assert_eq!(a.titles().await, ["milk"]);
    assert_eq!(
        held(&snap::ok(a.session.inspect_tips()).await, &first),
        "checkpoint"
    );
    let acquired_seal = snap::text(&open["seal"]);
    snap::ok(a.open_tip(&acquired_seal, 1)).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_issuer_automatically_acquires_the_target_seal_when_main_changes_lines()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?;
    let b_dir = root.path().join("b");
    let b = Device::join(
        &b_dir,
        &a.invite_as(json!({"kind":"issuer"})).await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    main(&a).await;
    snap::ok(
        a.session.set_kept_seals(
            serde_json::from_value::<rocky_sdk::KeptSeals>(json!({"every":true}))
                .expect("typed fixture"),
        ),
    )
    .await;
    a.add(1, "inherited").await;
    let first = snap::ok(a.session.seal(false)).await["tip"].clone();
    b.sync().await.expect("issuer learns first seal");
    snap::ok(b.follow("main")).await;
    a.add(2, "later line").await;
    let second = snap::ok(a.session.seal(false)).await["tip"].clone();
    b.sync().await.expect("issuer learns second seal");
    assert_eq!(snap::ok(b.session.compact(0)).await["compacted"], second);
    b.close().await;
    let branch = snap::ok(async {
        let value = a.open_tip(&(snap::text(&(first))), 1).await?;
        Ok(json!({"tip":value}))
    })
    .await["tip"]
        .clone();
    snap::ok(a.move_bookmark("main", &(snap::text(&(branch))))).await;
    let b = Device::open(&b_dir, Some(Network::Owned(hub.connector("b"))))?;
    for _ in 0..3 {
        b.sync().await.expect("automatic target-state acquisition");
    }
    assert_eq!(snap::ok(b.session.tip_status()).await["tip"], branch);
    assert_eq!(b.titles().await, ["inherited"]);
    b.add(3, "issuer continues").await;
    for _ in 0..2 {
        b.sync().await.expect("issuer publication");
        a.sync().await.expect("owner receives");
    }
    assert_eq!(a.titles().await, ["inherited", "issuer continues"]);
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_writer_compacts_a_close_with_only_registry_frames_after_the_last_item()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let a = Device::create(
        &root.path().join("a"),
        Some(Network::Owned(hub.connector("a"))),
    )?;
    main(&a).await;
    a.add(1, "milk").await;
    let writer = Device::join(
        &root.path().join("b"),
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &writer).await;
    writer
        .sync()
        .await
        .expect("enrollment writes the issuer registry after the last application frame");
    assert_eq!(writer.titles().await, ["milk"]);
    let before = snap::ok(a.session.tip_status()).await["tip"].clone();
    snap::ok(a.session.seal(true)).await;
    let inspected = snap::ok(a.session.inspect_tips()).await;
    let close = seal_of(&inspected, before.as_str().ok_or("tip")?);
    assert_eq!(close["action"], "close");
    writer
        .sync()
        .await
        .expect("closed tip and registry placements");
    let compacted = snap::ok(writer.session.compact(0)).await;
    assert_eq!(compacted["finalized"], close["id"], "{compacted}");
    assert_eq!(compacted["compacted"], close["id"], "{compacted}");
    assert_eq!(compacted["waiting"], Value::Null, "{compacted}");
    assert_eq!(writer.titles().await, ["milk"]);
    writer.close().await;
    a.close().await;
    Ok(())
}
