//! The native side of `scripts/browser-sync`: a plain-HTTP iroh relay on
//! loopback, which a browser reaches without a trusted certificate, and a
//! native node behind it. It prints the relay and an invitation as one JSON
//! line, serves until a line arrives on stdin, then checks that the browser
//! node's write reached it.
#[cfg(target_family = "wasm")]
fn main() {}

#[cfg(not(target_family = "wasm"))]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), rocky_sdk::Failure> {
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};
    use rocky_sdk::{
        Affinity, Column, Configuration, GrantProfile, Network, Schema, Table, params,
        transport::TransportOptions,
    };
    use std::{net::Ipv4Addr, sync::Arc};

    let mut config = ServerConfig::default();
    config.relay = Some(RelayConfig::new((Ipv4Addr::LOCALHOST, 0)));
    let relay = Server::spawn(config).await?;
    let url = format!(
        "http://{}",
        relay.http_addr().ok_or("the relay has no address")?
    );

    let schema = Schema::builder("browser_sync", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()?;
    let directory = tempfile::tempdir()?;
    let network = TransportOptions::Iroh {
        relay: Some(url.clone()),
        lookup: Some("disabled".into()),
        topic: false,
        local: false,
        bind: None,
    };
    let db = Configuration::new(directory.path(), &schema)?
        .network(Network::Owned(Arc::new(network)))
        .create()
        .await?
        .database;
    let mut tx = db.transaction().await?;
    let id = tx.next_id("notes", vec![]).await?;
    tx.execute(
        "INSERT INTO notes(id,body) VALUES(?,?)",
        params![id, "from native"],
    )
    .await?;
    tx.commit().await?;
    let invitation = db
        .session()
        .pairing(GrantProfile::Writer {
            scopes: ["notes".into()].into(),
        })
        .await?;
    println!(
        "{}",
        serde_json::json!({ "relay": url, "invitation": invitation.uri })
    );

    // The browser node joins, syncs, writes and syncs again before the
    // script sends the line; its sync pushed the write here.
    tokio::task::spawn_blocking(|| std::io::stdin().read_line(&mut String::new())).await??;
    let rows = db
        .query("SELECT body FROM notes ORDER BY body", params![])
        .await?
        .collect()
        .await?;
    let bodies = rows
        .iter()
        .map(|row| row.get::<String>("body"))
        .collect::<Result<Vec<_>, _>>()?;
    db.close().await?;
    if bodies != ["from browser", "from native"] {
        return Err(format!("the native node holds {bodies:?}").into());
    }
    eprintln!("browser-peer: the browser's write arrived over the relay");
    Ok(())
}
