//! A browser node syncs with a native node over a local iroh relay, which is
//! the only path a browser has, and claims invoice numbers from the
//! coordination service the native side runs, through a claim right the
//! owner grants: the browser is never one of the control's trusted nodes.
//! `scripts/browser-sync` starts the native side (`examples/browser_peer.rs`)
//! and passes its relay, invitation and service bundle here at build time;
//! the native side then checks that this node's write and claims reached it.
#![cfg(target_family = "wasm")]
use rocky_platform::{fs::Scratch, time};
use rocky_sdk::{
    Affinity, Column, Configuration, Coordination, Database, Failure, Network, Schema, Table,
    TrustBundle, counters, params, transport::TransportOptions,
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
async fn a_browser_node_syncs_and_claims_with_a_native_node_over_a_relay() -> Result<(), Failure> {
    let peer: serde_json::Value = serde_json::from_str(
        option_env!("ROCKY_BROWSER_PEER").ok_or("run this test through scripts/browser-sync")?,
    )?;
    let relay = peer["relay"].as_str().ok_or("no relay")?;
    let invitation = peer["invitation"].as_str().ok_or("no invitation")?;
    let bundle: TrustBundle = serde_json::from_value(peer["bundle"].clone())?;
    let schema = Schema::builder("browser_sync", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .table(
            Table::replayed("invoices")
                .column(Column::new("claim", Affinity::Text).required())
                .column(Column::new("number", Affinity::Integer).required()),
        )
        .build()?;
    let directory = Scratch::new().await;
    let network = TransportOptions::Iroh {
        relay: Some(relay.into()),
        lookup: Some("disabled".into()),
        topic: false,
        local: false,
        bind: None,
    };
    let configuration = Configuration::new(directory.path(), &schema)?
        .network(Network::Owned(Arc::new(network)))
        .coordination(Coordination::Service(Box::new(bundle)));
    let db = configuration.join(invitation).await?.database;
    let session = db.session();
    session.set_automatic_sync(false).await?;
    session.sync_once().await?;
    assert_eq!(bodies(&db, "from %").await?, ["from native"]);

    // The owner grants this node's identity a claim right for invoices once
    // it asks; the node fetches the claim link from it when it claims.
    let scope: serde_json::Value = serde_json::from_str(&db.scope())?;
    let reply: serde_json::Value = serde_json::from_str(
        &session
            .call(
                serde_json::json!({"version": 1, "scope": scope, "request": {"type": "session"}})
                    .to_string(),
            )
            .await?,
    )?;
    let principal = reply["value"]["principal"].as_str().ok_or("no principal")?;
    note(&db, &format!("member {principal}")).await?;
    let deadline = time::Instant::now() + time::Duration::from_secs(60);
    loop {
        session.sync_once().await?;
        if !bodies(&db, &format!("granted {principal}"))
            .await?
            .is_empty()
        {
            break;
        }
        assert!(
            time::Instant::now() < deadline,
            "the owner never granted this node a claim right"
        );
        time::sleep(time::Duration::from_millis(200)).await;
    }

    // Gap-free invoice numbers: a retry with the same claim answers its
    // value, and the next claim takes the next one.
    let invoices = counters::Name::new("invoices")?;
    let first = counters::Claim::fresh()?;
    let number = db.claim(invoices.clone(), first).await?;
    assert_eq!(number, 1);
    assert_eq!(db.claim(invoices.clone(), first).await?, number);
    assert_eq!(
        db.claim(invoices.clone(), counters::Claim::fresh()?)
            .await?,
        2
    );
    // The right names only invoices; another counter is refused.
    let refused = db
        .claim(counters::Name::new("credits")?, counters::Claim::fresh()?)
        .await
        .err()
        .ok_or("a claim outside the right succeeded")?;
    assert_eq!(
        refused.downcast_ref::<rocky_sdk::CoordinationError>(),
        Some(&rocky_sdk::CoordinationError::Denied)
    );
    let mut tx = db.transaction().await?;
    let id = tx.next_id("invoices", vec![]).await?;
    tx.execute(
        "INSERT INTO invoices(id,claim,number) VALUES(?,?,?)",
        params![id, serde_json::to_string(&first)?, i64::try_from(number)?],
    )
    .await?;
    tx.commit().await?;
    note(&db, "from browser").await?;
    session.sync_once().await?;
    db.close().await?;

    // Reopened from OPFS, the node still holds both writes, its claim and
    // its claim right.
    let db = configuration.open().await?.database;
    assert_eq!(
        bodies(&db, "from %").await?,
        ["from browser", "from native"]
    );
    let reading = db.counter(invoices.clone(), Some(first)).await?;
    assert_eq!(
        (reading.value, reading.claim),
        (2, Some(counters::Lookup::Claimed(1)))
    );
    assert_eq!(db.claim(invoices, first).await?, 1);
    db.close().await
}
