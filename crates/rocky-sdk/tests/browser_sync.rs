//! A browser node syncs with a native node over a local iroh relay, which is
//! the only path a browser has. `scripts/browser-sync` starts the native side
//! (`examples/browser_peer.rs`) and passes its relay and invitation here at
//! build time; the native side then checks that this node's write arrived.
#![cfg(target_family = "wasm")]
use rocky_platform::fs::Scratch;
use rocky_sdk::{
    Affinity, Column, Configuration, Failure, Network, Schema, Table, params,
    transport::TransportOptions,
};
use std::sync::Arc;
use wasm_bindgen_test::wasm_bindgen_test;

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
    let network = TransportOptions::Iroh {
        relay: Some(relay.into()),
        lookup: Some("disabled".into()),
        topic: false,
        local: false,
        bind: None,
    };
    let configuration =
        Configuration::new(directory.path(), &schema)?.network(Network::Owned(Arc::new(network)));
    let db = configuration.join(invitation).await?.database;
    let session = db.session();
    session.set_automatic_sync(false).await?;
    session.sync_once().await?;
    let bodies = |rows: Vec<rocky_sdk::Row>| {
        rows.iter()
            .map(|row| row.get::<String>("body"))
            .collect::<Result<Vec<_>, _>>()
    };
    let rows = db
        .query("SELECT body FROM notes", params![])
        .await?
        .collect()
        .await?;
    assert_eq!(bodies(rows)?, ["from native"]);

    let mut tx = db.transaction().await?;
    let id = tx.next_id("notes", vec![]).await?;
    tx.execute(
        "INSERT INTO notes(id,body) VALUES(?,?)",
        params![id, "from browser"],
    )
    .await?;
    tx.commit().await?;
    session.sync_once().await?;
    db.close().await?;

    // Reopened from OPFS, the node still holds both writes.
    let db = configuration.open().await?.database;
    let rows = db
        .query("SELECT body FROM notes ORDER BY body", params![])
        .await?
        .collect()
        .await?;
    assert_eq!(bodies(rows)?, ["from browser", "from native"]);
    db.close().await
}
