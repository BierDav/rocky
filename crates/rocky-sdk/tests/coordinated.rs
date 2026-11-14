//! Seals of a tip with a composed coordinated log through the JSON
//! contract: a private close ends the source tip's log only once relayed,
//! and its finality releases the staged migration.
// The SDK's blocking constructors, native transports and paused clocks.
#![cfg(not(target_family = "wasm"))]
mod common;
use common::identity::snapshot as snap;
use common::{Device, Failure, introduce};
use rocky_coordinator::coordinated::MemoryLog;
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::json;
use std::sync::Arc;

fn open<'a>(directory: &'a std::path::Path, network: Option<Network>) -> Open<'a> {
    Open {
        identity: Some(common::external_identity(directory)),
        directory,
        schema: common::SCHEMA,
        secrets: None,
        network,
        coordination: None,
    }
}
fn domain(device: &Device) -> rocky_core::Scope {
    device.database.scope()
}
/// The log of the tip `device` follows.
async fn tip_log(device: &Device) -> rocky_core::TipLog {
    let status = snap::ok(device.session.coordinated_status()).await;
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
/// Writes the next numbered invoice as an ordinary transaction.
async fn invoice(device: &Device, handle: u8) {
    snap::ok(device.session.execute("BEGIN", Vec::new())).await;
    let count = snap::ok(
        device
            .session
            .execute("SELECT COUNT(*) FROM items", Vec::new()),
    )
    .await["rows"][0][0]["value"]
        .as_str()
        .expect("count")
        .parse::<u64>()
        .expect("count");
    let id = snap::ok(device.session.next_id("items")).await["id"].clone();
    snap::ok(device.session.execute(
        "INSERT INTO items(key,completed,title,id) VALUES(?,0,?,?)",
        vec![
            rocky_sdk::SqlValue::Text(snap::text(&(format!("{:04}", count + 1)))),
            rocky_sdk::SqlValue::Text(snap::text(&(format!("invoice {}", count + 1)))),
            snap::row_id(&(id)),
        ],
    ))
    .await;
    snap::ok(device.session.commit([handle; 32])).await;
}
/// The invoice numbers once every frame they depend on arrived by sync.
async fn keys(device: &Device) -> Vec<String> {
    let query = rocky_sdk::ReadQuery::new("SELECT key FROM items ORDER BY key", 1024);
    let mut page = snap::observe(device.session.read(query.clone())).await;
    for _ in 0..200 {
        match &page {
            Err(error) if error["retryable"] == true => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                page = snap::observe(device.session.read(query.clone())).await;
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

const POSITIONED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}],"migrations":[{"version":2,"statements":["UPDATE items SET position = title"]}]}"#;

/// The owner closes, opens and migrates
/// offline. Its stage stays private until a trusted node that follows the
/// source tip ends that tip's log with exactly the close and acknowledges
/// it; the owner then releases. The source tip's writes past the close
/// replay in the successor.
#[tokio::test(flavor = "multi_thread")]
async fn an_offline_migration_is_released_by_the_exact_close_s_finality() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, c_dir) = (root.path().join("a"), root.path().join("c"));
    let a = Device::new(Database::create(open(&a_dir, network("a")), None)?)?
        .with_identity(common::external_identity(&a_dir));
    a.follow_main_migrating().await;
    let log = MemoryLog::new();
    log.configure(domain(&a), 1)?;
    let owner = snap::ok(a.session.info()).await["principal"]
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
        &a.invite_as(json!({"kind":"issuer"})).await,
    )?)?
    .with_identity(common::external_identity(&c_dir));
    introduce(&a, &c).await;
    let principal = snap::ok(c.session.info()).await["principal"].clone();
    let grant = snap::ok(
        a.session
            .trust(&(snap::text(&(principal))), (a.root()).clone()),
    )
    .await;
    snap::ok(c.session.install_trust(&(snap::text(&(grant["grant"]))))).await;
    c.sync().await.expect("trusted sync");
    let a_node = node_key(&a).await;
    for device in [&a, &c] {
        device
            .database
            .coordinate(Some(Arc::new(log.client(node_key(device).await))))
            .await?;
    }
    invoice(&c, 1).await;
    a.sync().await.expect("the first invoice");
    let source = tip_log(&a).await;
    a.close().await;
    // While the owner is away, the trusted node writes a second invoice.
    invoice(&c, 2).await;

    // Offline, the owner stages its close, opening and move and migrates.
    let a = Device::new(Database::open(Open {
        schema: POSITIONED,
        coordination: Some(Arc::new(log.client(a_node))),
        ..open(&a_dir, None)
    })?)?
    .with_identity(common::external_identity(&a_dir));
    let status = snap::ok(a.session.generation()).await;
    assert_eq!(status["current"]["schema"], 2, "{status}");
    // Nothing of the stage left the device: the source log is open.
    assert!(log.head(source)?.terminal.is_none());
    a.close().await;

    // The candidate already fixed its parent. Source writes still commit
    // while its successor remains private.
    invoice(&c, 3).await;
    c.add(4, "ordinary meanwhile").await;

    // Reconnected, it relays the exact close; the trusted node ends the
    // log with it and acknowledges, and the owner releases.
    let a = Device::new(Database::open(Open {
        schema: POSITIONED,
        coordination: Some(Arc::new(log.client(a_node))),
        ..open(&a_dir, network("a"))
    })?)?
    .with_identity(common::external_identity(&a_dir));
    introduce(&a, &c).await;
    for _ in 0..4 {
        let _ = a.sync().await;
        let _ = c.sync().await;
    }
    let head = log.head(source)?;
    let inspected = snap::ok(a.session.inspect_tips()).await;
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
    // The trusted node follows `main` to the migration's tip. Its writes
    // past the close are late: they replay there in stamp order, so the
    // replayed migration fills invoice 2 and later rows keep the default.
    let status = snap::ok(c.session.generation()).await;
    assert_eq!(status["current"]["schema"], 2, "{status}");
    assert_eq!(status["held"], 0, "{status}");
    let text = |value: &str| json!({"type": "text", "value": value});
    for device in [&a, &c] {
        let rows = snap::ok(device.session.read(rocky_sdk::ReadQuery {
            sql: "SELECT title, position FROM items ORDER BY title".to_owned(),
            values: vec![],
            max_rows: 8,
            max_bytes: None,
            cursor: None,
        }))
        .await["rows"]
            .clone();
        assert_eq!(
            rows,
            json!([
                [text("invoice 1"), text("invoice 1")],
                [text("invoice 2"), text("invoice 2")],
                [text("invoice 3"), text("")],
                [text("ordinary meanwhile"), text("")]
            ])
        );
    }
    invoice(&c, 5).await;
    a.sync().await.expect("the successor invoice");
    let keys = keys(&a).await;
    assert!(
        keys.len() == 5 && keys.contains(&"0005".to_owned()),
        "{keys:?}"
    );
    c.close().await;
    a.close().await;
    Ok(())
}
