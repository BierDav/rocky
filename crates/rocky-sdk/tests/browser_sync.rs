#![cfg(target_family = "wasm")]
use rocky_platform::fs::Scratch;
use rocky_sdk::{Affinity, Column, Configuration, Database, Network, Schema, Table, params};
use wasm_bindgen_test::wasm_bindgen_test;

type Failure = Box<dyn std::error::Error>;

async fn bodies(db: &Database, pattern: &str) -> Result<Vec<String>, Failure> {
    let rows = db
        .query(
            "SELECT body FROM notes WHERE body LIKE ? ORDER BY body",
            params![pattern],
        )
        .await?
        .collect()
        .await?;
    Ok(rows
        .iter()
        .map(|row| row.get::<String>("body"))
        .collect::<Result<Vec<_>, _>>()?)
}

async fn note(db: &Database, body: &str) -> Result<(), Failure> {
    let mut tx = db.transaction().await?;
    let id = tx.next_id("notes", vec![]).await?;
    tx.execute("INSERT INTO notes(id,body) VALUES(?,?)", params![id, body])
        .await?;
    tx.commit().await?;
    Ok(())
}

fn peer() -> Result<serde_json::Value, Failure> {
    Ok(serde_json::from_str(
        option_env!("ROCKY_BROWSER_PEER").ok_or("run this test through scripts/browser-sync")?,
    )?)
}

async fn join_write_and_reopen(
    directory: &std::path::Path,
    network: Network,
    invitation: &str,
) -> Result<(), Failure> {
    let schema = Schema::builder("browser_sync", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()?;
    let configuration = Configuration::new(directory, &schema)?.network(network);
    let db = configuration.join(&invitation.parse()?).await?.database;
    db.set_sync(rocky_sdk::SyncMode::Manual).await?;
    db.sync_now().await?;
    assert_eq!(bodies(&db, "from %").await?, ["from native"]);

    note(&db, "from browser").await?;
    db.sync_now().await?;
    db.close().await?;

    let db = configuration.open().await?;
    assert_eq!(
        bodies(&db, "from %").await?,
        ["from browser", "from native"],
        "reopened from OPFS, the node still holds both writes"
    );
    db.close().await?;
    Ok(())
}

#[wasm_bindgen_test]
async fn a_browser_node_syncs_with_a_native_node_over_a_relay() -> Result<(), Failure> {
    let peer = peer()?;
    let relay = peer["relay"].as_str().ok_or("no relay")?;
    let invitation = peer["invitation"].as_str().ok_or("no invitation")?;
    let directory = Scratch::new().await;
    let network = rocky_iroh::Options {
        relay: Some(relay.into()),
        lookup: Some("disabled".into()),
        topic: false,
        local: false,
        bind: None,
    };
    join_write_and_reopen(directory.path(), network.into(), invitation).await
}

#[wasm_bindgen_test]
async fn a_browser_node_dials_a_native_websocket_server() -> Result<(), Failure> {
    let peer = peer()?;
    let invitation = peer["websocket_invitation"]
        .as_str()
        .ok_or("no WebSocket invitation")?;
    let directory = Scratch::new().await;
    let network: Network = rocky_tcp::Options::browser().into();
    join_write_and_reopen(directory.path(), network, invitation).await
}
