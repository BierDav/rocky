#[cfg(target_family = "wasm")]
fn main() {}

#[cfg(not(target_family = "wasm"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    native::main().await
}

#[cfg(not(target_family = "wasm"))]
mod native {
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};
    use rocky_sdk::{
        Access, Affinity, Column, Configuration, Database, Invitation, Network, Schema, Table,
        params,
    };

    type Failure = Box<dyn std::error::Error + Send + Sync>;
    use serde_json::json;
    use std::net::{Ipv4Addr, SocketAddr};

    pub async fn main() -> Result<(), Failure> {
        let mut config = ServerConfig::default();
        config.relay = Some(RelayConfig::new((Ipv4Addr::LOCALHOST, 0)));
        let relay = Server::spawn(config).await?;
        let url = format!(
            "http://{}",
            relay.http_addr().ok_or("the relay has no address")?
        );
        let directory = tempfile::tempdir()?;

        let schema = Schema::builder("browser_sync", 1)
            .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
            .build()?;
        let relayed = owner(
            &directory.path().join("relayed"),
            &schema,
            rocky_iroh::Options {
                relay: Some(url.clone()),
                lookup: Some("disabled".into()),
                topic: false,
                local: false,
                bind: None,
            }
            .into(),
        )
        .await?;
        let websocket = owner(
            &directory.path().join("websocket"),
            &schema,
            rocky_tcp::Options::new(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .websocket()
                .into(),
        )
        .await?;
        println!(
            "{}",
            json!({
                "relay": url,
                "invitation": relayed.1.code.to_string(),
                "websocket_invitation": websocket.1.code.to_string(),
            })
        );

        tokio::task::spawn_blocking(|| std::io::stdin().read_line(&mut String::new())).await??;
        let checked = (check(&relayed.0).await, check(&websocket.0).await);
        relayed.0.close().await?;
        websocket.0.close().await?;
        checked.0?;
        checked.1?;
        eprintln!("browser-peer: the browser's writes arrived over the relay and the WebSocket");
        Ok(())
    }

    async fn owner(
        path: &std::path::Path,
        schema: &Schema,
        network: Network,
    ) -> Result<(Database, Invitation), Failure> {
        let db = Configuration::new(path, schema)?
            .network(network)
            .create()
            .await?
            .database;
        let mut tx = db.transaction().await?;
        tx.execute("INSERT INTO notes(body) VALUES(?)", params!["from native"])
            .await?;
        tx.commit().await?;
        let invitation = db.invite(rocky_sdk::Invite::new(Access::Full)).await?;
        Ok((db, invitation))
    }

    async fn check(db: &Database) -> Result<(), Failure> {
        let rows = db
            .query(
                "SELECT body FROM notes WHERE body LIKE 'from %' ORDER BY body",
                params![],
            )
            .await?
            .collect()
            .await?;
        let bodies = rows
            .iter()
            .map(|row| row.get::<String>("body"))
            .collect::<Result<Vec<_>, _>>()?;
        if bodies != ["from browser", "from native"] {
            return Err(format!("the native node holds {bodies:?}").into());
        }
        Ok(())
    }
}
