use crate::common;
use crate::support::snapshot as snap;
use common::{Device, Failure, introduce};
use rocky_coordinator::coordinated::MemoryLog;
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::json;
use std::sync::Arc;

mod reach;
mod split;

fn open<'a>(directory: &'a std::path::Path, network: Option<Network>) -> Open<'a> {
    Open {
        identity: Some(common::external_identity(directory)),
        directory,
        schema: crate::support::snapshot::schema(common::SCHEMA),
        secrets: None,
        network,
        coordination: None,
    }
}
fn domain(device: &Device) -> rocky_core::Scope {
    device.database.scope()
}
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
async fn invoice(device: &Device, _handle: u8) {
    let mut tx = device.database.transaction().await.expect("begin");
    let count: i64 = tx
        .query("SELECT COUNT(*) FROM items", Vec::new())
        .await
        .expect("count")
        .next()
        .await
        .expect("next")
        .expect("row")
        .get_at(0)
        .expect("count");
    let id = tx.next_id("items", Vec::new()).await.expect("id");
    tx.execute(
        "INSERT INTO items(key,completed,title,id) VALUES(?,0,?,?)",
        vec![
            rocky_sdk::SqlValue::Text(format!("{:04}", count + 1)),
            rocky_sdk::SqlValue::Text(format!("invoice {}", count + 1)),
            id.sql(),
        ],
    )
    .await
    .expect("insert");
    tx.commit().await.expect("commit");
}
async fn keys(device: &Device) -> Result<Vec<String>, Failure> {
    let changes = device.database.observe_tables(None).await?;
    let outlasts_a_loaded_ci_host = std::time::Duration::from_secs(30);
    let mut awaiting = None;
    let waited = tokio::time::timeout(outlasts_a_loaded_ci_host, async {
        loop {
            match snap::observe(snap::read(
                &device.database,
                "SELECT key FROM items ORDER BY key",
                Vec::new(),
            ))
            .await
            {
                Err(error) if error["code"] == "missing_dependencies" => {
                    awaiting = Some(error.to_string());
                    changes.next().await?;
                }
                page => return page.map_err(|error| -> Failure { error.to_string().into() }),
            }
        }
    })
    .await;
    let page = waited.map_err(|_| -> Failure {
        format!(
            "the read awaited its dependencies with no table change to release it: {}",
            awaiting.unwrap_or_default()
        )
        .into()
    })??;
    Ok(page["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| row[0]["value"].as_str().expect("key").to_owned())
        .collect())
}

fn reopen_owner_positioned(
    directory: &std::path::Path,
    log: &MemoryLog,
    node: rocky_sdk::credentials::PublicKey,
    network: Option<Network>,
) -> Result<Device, Failure> {
    Ok(Device::new(Database::open(Open {
        schema: crate::support::snapshot::schema(POSITIONED),
        coordination: Some(rocky_sdk::CoordinatorSlot::Client(Arc::new(
            log.client(node),
        ))),
        ..open(directory, network)
    })?)?
    .with_identity(common::external_identity(directory)))
}

const POSITIONED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}],"migrations":[{"version":2,"statements":["UPDATE items SET position = title"]}]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn an_offline_migration_is_released_by_the_exact_close_s_finality() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
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
            .trust(snap::public_key(&(principal)), (a.root()).clone()),
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
    invoice(&c, 2).await;

    let a = reopen_owner_positioned(&a_dir, &log, a_node, None)?;
    let status = snap::ok(a.session.generation()).await;
    assert_eq!(status["current"]["schema"], 2, "{status}");
    assert!(
        log.head(source)?.terminal.is_none(),
        "nothing of the offline stage leaves the device: the source log stays open"
    );
    a.close().await;

    invoice(&c, 3).await;
    c.add(4, "ordinary meanwhile").await;

    let a = reopen_owner_positioned(&a_dir, &log, a_node, network("a"))?;
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
        close["id"],
        "the trusted node ends the source log with exactly the relayed close"
    );
    let status = snap::ok(c.session.generation()).await;
    assert_eq!(
        status["current"]["schema"], 2,
        "the trusted node follows `main` to the migration's tip: {status}"
    );
    assert_eq!(status["held"], 0, "{status}");
    let text = |value: &str| json!({"type": "text", "value": value});
    for device in [&a, &c] {
        let rows = snap::ok(snap::read(
            &device.database,
            "SELECT title, position FROM items ORDER BY title",
            Vec::new(),
        ))
        .await["rows"]
            .clone();
        assert_eq!(
            rows,
            json!([
                [text("invoice 1"), text("invoice 1")],
                [text("invoice 2"), text("invoice 2")],
                [text("invoice 3"), text("")],
                [text("ordinary meanwhile"), text("")]
            ]),
            "writes past the close replay in stamp order: the migration fills \
             invoice 2 and later rows keep the default"
        );
    }
    invoice(&c, 5).await;
    a.sync().await.expect("the successor invoice");
    let keys = keys(&a).await?;
    assert!(
        keys.len() == 5 && keys.contains(&"0005".to_owned()),
        "{keys:?}"
    );
    c.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test]
async fn a_reachable_coordinator_meets_the_claim_contract() {
    let log = MemoryLog::new();
    let key = rocky_sdk::credentials::PrivateKey::new();
    let (reach, _) = reach::Reach::new(Arc::new(log.client(key.public_key())));
    rocky_sdk::credentials::testing::claim_contract(
        &*reach,
        crate::support::CONTRACT_SCOPE,
        async |seconds| log.advance(seconds),
    )
    .await;
}

#[tokio::test]
async fn a_reachable_coordinator_meets_the_coordinator_contract() {
    let trusted = rocky_sdk::credentials::PrivateKey::new();
    let (reach, _) = reach::Reach::new(Arc::new(MemoryLog::new().client(trusted.public_key())));
    rocky_sdk::credentials::testing::coordinator_contract(
        &*reach,
        &trusted,
        crate::support::contract_database(),
        rocky_sdk::credentials::testing::CoordinatorBehaviour {
            observes_calls: false,
            denies_untrusted_claims: false,
        },
    )
    .await;
}
