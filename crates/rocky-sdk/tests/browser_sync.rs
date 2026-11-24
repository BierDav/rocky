#![cfg(target_family = "wasm")]
use rocky_platform::fs::Scratch;
use rocky_sdk::{
    Affinity, Column, Configuration, Database, Failure, Network, Schema, Table, params,
};
use std::sync::Arc;
use wasm_bindgen_test::wasm_bindgen_test;

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

#[wasm_bindgen_test]
async fn a_browser_node_syncs_with_a_native_node_over_a_relay() -> Result<(), Failure> {
    let peer: serde_json::Value = serde_json::from_str(
        option_env!("ROCKY_BROWSER_PEER").ok_or("run this test through scripts/browser-sync")?,
    )?;
    let relay = peer["relay"].as_str().ok_or("no relay")?;
    let invitation = peer["invitation"].as_str().ok_or("no invitation")?;
    let schema = Schema::builder("browser_sync", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()?;
    let directory = Scratch::new().await;
    let network = rocky_iroh::Options {
        relay: Some(relay.into()),
        lookup: Some("disabled".into()),
        topic: false,
        local: false,
        bind: None,
    };
    let configuration =
        Configuration::new(directory.path(), &schema)?.network(Network::Owned(Arc::new(network)));
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
