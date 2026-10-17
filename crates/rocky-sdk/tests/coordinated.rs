//! Coordinated writes through the JSON contract: two trusted devices issue
//! invoices concurrently over live links and number them without gaps or
//! duplicates; without a coordinator the write fails typed. See
//! `docs/runtime/coordinated-writes.md`.
mod common;
use common::{Device, Failure, introduce};
use rocky_coordinator::coordinated::MemoryLog;
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::{Value, json};
use std::sync::Arc;

fn open<'a>(directory: &'a std::path::Path, network: Option<Network>) -> Open<'a> {
    Open {
        identity: None,
        directory,
        schema: common::SCHEMA,
        secrets: None,
        network,
        coordination: None,
    }
}
fn domain(device: &Device) -> rocky_core::Scope {
    serde_json::from_str(&device.database.scope()).expect("scope")
}
/// The log of the tip `device` follows.
async fn tip_log(device: &Device) -> rocky_core::TipLog {
    let status = device.ok(json!({"type": "coordinated"})).await;
    let tip: [u8; 32] = data_encoding::HEXLOWER
        .decode(status["tip"].as_str().expect("tip").as_bytes())
        .expect("tip hex")
        .try_into()
        .expect("tip");
    rocky_core::TipLog {
        scope: domain(device),
        tip: rocky_core::TipId(tip),
    }
}
async fn node_key(device: &Device) -> rocky_sdk::credentials::PublicKey {
    let hex = device.node().await;
    let bytes: [u8; 32] = data_encoding::HEXLOWER
        .decode(hex.as_bytes())
        .expect("node hex")
        .try_into()
        .expect("node key");
    rocky_sdk::credentials::PublicKey::from_bytes(&bytes).expect("node key")
}
/// Waits until `device` holds an open live link to `other`.
async fn linked(device: &Device, other: &Device) {
    let node = other.node().await;
    for _ in 0..200 {
        if device.peer(&node).await["link"]["state"] == "open" {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("no live link");
}
/// Issues the next invoice: reads the committed count inside a coordinated
/// transaction and inserts the following number. Retries lost races.
async fn invoice(device: &Device, handle: u8, log: &MemoryLog) -> Value {
    let mut last_error = None;
    for attempt in 0..200u8 {
        let begun = device
            .call(json!({"type": "sql", "sql": "BEGIN", "coordinated": true}))
            .await;
        if let Err(error) = begun {
            assert_eq!(error["retryable"], true, "{error}");
            last_error = Some(error);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            continue;
        }
        let count = device
            .ok(json!({"type": "sql", "sql": "SELECT COUNT(*) FROM items"}))
            .await["rows"][0][0]["value"]
            .as_str()
            .expect("count")
            .parse::<u64>()
            .expect("count");
        let id = device
            .ok(json!({"type": "next_id", "table": "items"}))
            .await["id"]
            .clone();
        device
            .ok(json!({
                "type": "sql",
                "sql": "INSERT INTO items(key,completed,title,id) VALUES(?,0,?,?)",
                "values": [
                    {"type": "text", "value": format!("{:04}", count + 1)},
                    {"type": "text", "value": format!("invoice {}", count + 1)},
                    {"type": "integer", "value": id},
                ],
            }))
            .await;
        let mut key = [handle; 32];
        key[31] = attempt;
        match device
            .call(json!({"type": "sql", "sql": "COMMIT", "handle": key, "coordinated": true}))
            .await
        {
            Ok(committed) => return committed["committed"].clone(),
            Err(error) => {
                assert!(
                    ["conflict", "aborted", "undecided", "behind"]
                        .contains(&error["code"].as_str().unwrap_or("")),
                    "{error}"
                );
                last_error = Some(error);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
    }
    panic!(
        "no invoice committed for handle {handle}: {last_error:?}; head: {:?}",
        log.head(tip_log(device).await)
    )
}
/// The invoice numbers once every frame they depend on arrived by sync.
async fn keys(device: &Device) -> Vec<String> {
    let query = json!({"type": "read", "query": {"sql": "SELECT key FROM items ORDER BY key", "values": [], "max_rows": 1024}});
    let mut page = device.call(query.clone()).await;
    for _ in 0..200 {
        match &page {
            Err(error) if error["retryable"] == true => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                page = device.call(query.clone()).await;
            }
            _ => break,
        }
    }
    page.expect("keys")["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| row[0]["value"].as_str().expect("key").to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_devices_number_concurrent_invoices_without_gaps() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::new(Database::create(open(&a_dir, network("a")), None)?)?;
    let b = Device::new(Database::join(
        open(&b_dir, network("b")),
        &a.invite_as(json!({"kind": "writer", "scopes": ["items"]}))
            .await,
    )?)?;
    // Without a composed log a coordinated write is refused, never queued.
    let refused = a
        .call(json!({"type": "sql", "sql": "BEGIN", "coordinated": true}))
        .await
        .expect_err("no log");
    assert_eq!(refused["code"], "rejected");
    let log = MemoryLog::new();
    log.configure(domain(&a), 2)?;
    for device in [&a, &b] {
        let client = log.client(node_key(device).await);
        device.database.coordinate(Some(Arc::new(client))).await?;
    }
    introduce(&a, &b).await;
    // Automatic sync keeps live links open between the trusted devices.
    for device in [&a, &b] {
        device.ok(json!({"type": "sync_start"})).await;
    }
    linked(&a, &b).await;
    linked(&b, &a).await;
    a.sync().await.expect("initial sync from b");
    b.sync().await.expect("initial sync from a");
    // Both join; the first alone is below the minimum.
    let below = a
        .call(json!({"type": "sql", "sql": "BEGIN", "coordinated": true}))
        .await
        .expect_err("below minimum");
    assert_eq!(below["code"], "below_minimum", "{below}");

    let log = &log;
    let issue = |device: &'static Device, base: u8| async move {
        let mut positions = Vec::new();
        for handle in base..base + 3 {
            let committed = invoice(device, handle, log).await;
            positions.push(committed["position"].as_str().expect("position").to_owned());
        }
        positions
    };
    let (a, b): (&'static Device, &'static Device) =
        (Box::leak(Box::new(a)), Box::leak(Box::new(b)));
    let (first, second) = tokio::join!(issue(a, 0), issue(b, 16));
    let mut positions: Vec<u64> = first
        .iter()
        .chain(&second)
        .map(|position| position.parse().expect("position"))
        .collect();
    positions.sort_unstable();
    assert_eq!(positions, (1..=6).collect::<Vec<_>>());
    let numbers: Vec<String> = (1..=6).map(|number| format!("{number:04}")).collect();
    for device in [a, b] {
        let status = device.ok(json!({"type": "coordinated"})).await;
        assert_eq!(status["undecided"], 0, "{status}");
        assert_eq!(status["position"], "6", "{status}");
        assert_eq!(keys(device).await, numbers);
    }
    // Both follow one tip, whose log ordered every write; no other tip's has any.
    let followed = tip_log(a).await;
    assert_eq!(tip_log(b).await, followed);
    assert_eq!(log.head(followed)?.commit, 6);
    let other = rocky_core::TipLog {
        scope: followed.scope,
        tip: rocky_core::TipId([0; 32]),
    };
    assert_eq!(log.head(other)?.commit, 0);
    a.close().await;
    b.close().await;
    Ok(())
}

async fn tips(device: &Device, action: &str) -> Result<Value, Value> {
    device.call(json!({"type": "tips", "action": action})).await
}

/// Phase 19 through the JSON contract: a writer's devices commit two
/// invoices alone and the owner holds only the first when the writer is
/// lost. Coordinated writes stay refused until the owner recovers; then
/// they continue on the continuation and every bookmark on the tip follows.
#[tokio::test(flavor = "multi_thread")]
async fn the_owner_recovers_a_stuck_tip_and_writes_continue_on_its_continuation()
-> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::new(Database::create(open(&a_dir, network("a")), None)?)?;
    let b = Device::new(Database::join(
        open(&b_dir, network("b")),
        &a.invite_as(json!({"kind": "writer", "scopes": ["items"]}))
            .await,
    )?)?;
    for name in ["main", "release"] {
        a.ok(json!({"type": "tips", "action": "create", "name": name}))
            .await;
    }
    a.ok(json!({"type": "tips", "action": "follow", "name": "main"}))
        .await;
    introduce(&a, &b).await;
    b.sync().await.expect("graph");
    b.ok(json!({"type": "tips", "action": "follow", "name": "main"}))
        .await;
    let log = MemoryLog::new();
    log.configure(domain(&a), 1)?;
    a.ok(json!({"type":"tips","action":"genesis_minimum","minimum_in_sync":1}))
        .await;
    let owner = a.ok(json!({"type": "session"})).await["principal"]
        .as_str()
        .expect("owner")
        .to_owned();
    let owner: [u8; 32] = data_encoding::HEXLOWER
        .decode(owner.as_bytes())?
        .try_into()
        .map_err(|_| "owner key")?;
    log.set_owner(
        domain(&a),
        rocky_sdk::credentials::PublicKey::from_bytes(&owner)?,
    );
    b.database
        .coordinate(Some(Arc::new(log.client(node_key(&b).await))))
        .await?;
    invoice(&b, 1, &log).await;
    a.sync().await.expect("the first invoice");
    invoice(&b, 2, &log).await;
    let stuck = tip_log(&b).await;
    // Only the owner recovers.
    let refused = tips(&b, "recover").await.expect_err("not the owner");
    assert_eq!(refused["code"], "rejected", "{refused}");
    b.close().await;
    // A genuinely trusted identity, not merely a composed log client,
    // supplies finality while the owner keeps the single recovery action.
    let c = Device::new(Database::join(
        open(&root.path().join("c"), network("c")),
        &a.invite_as(json!({"kind":"writer","scopes":["items"]}))
            .await,
    )?)?;
    introduce(&a, &c).await;
    let principal = c.ok(json!({"type":"session"})).await["principal"].clone();
    let grant = a
        .ok(json!({"type":"tips","action":"trust","identity":principal}))
        .await;
    c.ok(json!({"type":"tips","action":"install_trust","grant":grant["grant"]}))
        .await;
    c.sync().await.expect("trusted sync");
    c.ok(json!({"type":"tips","action":"follow","name":"main"}))
        .await;
    c.database
        .coordinate(Some(Arc::new(log.client(node_key(&c).await))))
        .await?;
    // The owner is behind the log; nobody holds entry 2.
    a.database
        .coordinate(Some(Arc::new(log.client(node_key(&a).await))))
        .await?;
    let behind = a
        .call(json!({"type": "sql", "sql": "BEGIN", "coordinated": true}))
        .await
        .expect_err("behind");
    assert_eq!(behind["code"], "behind", "{behind}");
    assert_eq!(behind["retryable"], true);
    let pending = tips(&a, "recover")
        .await
        .expect_err("needs a trusted acknowledgment");
    assert_eq!(pending["code"], "recovery_pending", "{pending}");
    assert_eq!(pending["retryable"], true, "{pending}");
    for _ in 0..3 {
        c.sync().await.expect("trusted sync");
        a.sync().await.expect("owner sync");
    }

    let status = tips(&a, "status").await.expect("recovery status");
    let inspection = a.ok(json!({"type": "tips", "action": "inspect"})).await;
    let seal = inspection["seals"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|seal| seal["recovery"].is_object())
        .expect("the recovery seal")
        .clone();
    assert_eq!(seal["recovery"], json!({"retained": 1, "replaced": 2}));
    assert_eq!(seal["authority"], "owner");
    assert_eq!(status["tip"], seal["id"]);
    let bookmarks = inspection["bookmarks"].as_array().expect("bookmarks");
    for name in ["main", "release"] {
        let bookmark = bookmarks
            .iter()
            .find(|bookmark| bookmark["name"] == name)
            .expect("bookmark");
        assert_eq!(bookmark["tip"], seal["id"], "{name}");
    }
    let head = log.head(stuck)?;
    assert_eq!(
        data_encoding::HEXLOWER.encode(&head.terminal.expect("ended").digest),
        seal["id"]
    );
    // The retained invoice stays; the next one takes the abandoned number.
    let committed = invoice(&a, 3, &log).await;
    assert_eq!(committed["position"], "1");
    assert_eq!(keys(&a).await, ["0001", "0002"]);
    // An owner recovery may retain the complete log while clearing its
    // membership; collecting everything is not a refusal.
    let pending = tips(&a, "recover").await.expect_err("needs finality");
    assert_eq!(pending["code"], "recovery_pending", "{pending}");
    for _ in 0..3 {
        c.sync().await.expect("trusted sync");
        a.sync().await.expect("owner sync");
    }
    assert_eq!(keys(&a).await, ["0001", "0002"]);
    c.close().await;
    a.close().await;
    Ok(())
}

/// The common schema with a required `position` a migration fills.
const POSITIONED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}],"migrations":[{"version":2,"statements":["UPDATE items SET position = title"]}]}"#;

/// Phase 17 on a coordinated tip: the owner closes, opens and migrates
/// offline. Its stage stays private until a trusted node that follows the
/// source tip ends that tip's log with exactly the close and acknowledges
/// it; the owner then releases. The source tip's writes past the close
/// are held on the device that made them.
#[tokio::test(flavor = "multi_thread")]
async fn an_offline_coordinated_migration_is_released_by_the_exact_close_s_finality()
-> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, c_dir) = (root.path().join("a"), root.path().join("c"));
    let a = Device::new(Database::create(open(&a_dir, network("a")), None)?)?;
    a.follow_main_migrating().await;
    let log = MemoryLog::new();
    log.configure(domain(&a), 1)?;
    let owner = a.ok(json!({"type": "session"})).await["principal"]
        .as_str()
        .expect("owner")
        .to_owned();
    let owner: [u8; 32] = data_encoding::HEXLOWER
        .decode(owner.as_bytes())?
        .try_into()
        .map_err(|_| "owner key")?;
    log.set_owner(
        domain(&a),
        rocky_sdk::credentials::PublicKey::from_bytes(&owner)?,
    );
    let c = Device::new(Database::join(
        open(&c_dir, network("c")),
        &a.invite_as(json!({"kind":"writer","scopes":["items"]}))
            .await,
    )?)?;
    introduce(&a, &c).await;
    let principal = c.ok(json!({"type":"session"})).await["principal"].clone();
    let grant = a
        .ok(json!({"type":"tips","action":"trust","identity":principal}))
        .await;
    c.ok(json!({"type":"tips","action":"install_trust","grant":grant["grant"]}))
        .await;
    c.sync().await.expect("trusted sync");
    let a_node = node_key(&a).await;
    for device in [&a, &c] {
        device
            .database
            .coordinate(Some(Arc::new(log.client(node_key(device).await))))
            .await?;
    }
    invoice(&c, 1, &log).await;
    a.sync().await.expect("the first invoice");
    let source = tip_log(&a).await;
    a.close().await;
    // While the owner is away, the trusted node commits a second invoice.
    invoice(&c, 2, &log).await;

    // Offline, the owner stages its close, opening and move and migrates.
    let a = Device::new(Database::open(Open {
        schema: POSITIONED,
        coordination: Some(rocky_sdk::Coordination::Composed(Arc::new(
            log.client(a_node),
        ))),
        ..open(&a_dir, None)
    })?)?;
    let status = a.ok(json!({"type": "generation"})).await;
    assert_eq!(status["current"]["schema"], 2, "{status}");
    // Nothing of the stage left the device: the source log is open.
    assert!(log.head(source)?.terminal.is_none());
    a.close().await;

    // The candidate already fixed its parent. Both kinds of source write
    // can still commit while its successor remains private.
    invoice(&c, 3, &log).await;
    c.add(4, "ordinary meanwhile").await;

    // Reconnected, it relays the exact close; the trusted node ends the
    // log with it and acknowledges, and the owner releases.
    let a = Device::new(Database::open(Open {
        schema: POSITIONED,
        coordination: Some(rocky_sdk::Coordination::Composed(Arc::new(
            log.client(a_node),
        ))),
        ..open(&a_dir, network("a"))
    })?)?;
    introduce(&a, &c).await;
    for _ in 0..4 {
        let _ = a.sync().await;
        let _ = c.sync().await;
    }
    let head = log.head(source)?;
    let inspected = a.ok(json!({"type": "tips", "action": "inspect"})).await;
    let close = inspected["seals"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|seal| seal["action"] == "close")
        .expect("the close")
        .clone();
    assert_eq!(
        data_encoding::HEXLOWER.encode(&head.terminal.expect("ended").digest),
        close["id"]
    );
    // The trusted node follows `main` to the migration's tip. Its invoice
    // made after the close is held; the migration's rows are everyone's.
    let status = c.ok(json!({"type": "generation"})).await;
    assert_eq!(status["current"]["schema"], 2, "{status}");
    assert_eq!(status["held"], 3, "{status}");
    for device in [&a, &c] {
        assert_eq!(keys(device).await, ["0001"]);
        let rows = device
            .ok(json!({"type": "read", "query": {"sql": "SELECT title, position FROM items", "values": [], "max_rows": 8}}))
            .await["rows"]
            .clone();
        assert_eq!(
            rows,
            json!([[{"type":"text","value":"invoice 1"}, {"type":"text","value":"invoice 1"}]])
        );
    }
    // Inherited readiness accepts those durably held post-frontier frames;
    // they must not block the successor's first coordinated slot. The
    // owner's `issuer_registry` frames inside the close's frontier, which
    // `c` cannot read, are placed by their signed headers: `c` takes it.
    let committed = invoice(&c, 5, &log).await;
    assert_eq!(committed["position"], "1");
    a.sync().await.expect("the successor invoice");
    assert_eq!(keys(&a).await, ["0001", "0002"]);
    c.close().await;
    a.close().await;
    Ok(())
}
