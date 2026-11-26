use rocky_sdk::{Access, Affinity, Column, Configuration, Invite, Network, Schema, Table, params};
use std::net::SocketAddr;

fn loopback() -> Network {
    let any_port = SocketAddr::from(([127, 0, 0, 1], 0));
    rocky_tcp::Options::new(any_port).into()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = Schema::builder("notes", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()?;
    let directory = std::env::temp_dir().join(format!("rocky-two-devices-{}", std::process::id()));

    let alice = Configuration::new(directory.join("alice"), &schema)?
        .network(loopback())
        .create()
        .await?;
    println!(
        "Alice's person key, for the app's secure storage: {}",
        alice.owner.phrase()
    );
    let invitation = alice.database.invite(Invite::new(Access::Full)).await?;

    let bob = Configuration::new(directory.join("bob"), &schema)?
        .network(loopback())
        .join(&invitation.code)
        .await?;

    let insert = "INSERT INTO notes(body) VALUES(?)";
    alice
        .database
        .write(async |tx| tx.execute(insert, params!["Hello from Alice"]).await)
        .await?;

    let report = bob.database.sync_now().await?;
    println!("Bob's sync: {report}");
    let rows = bob
        .database
        .query("SELECT body FROM notes", params![])
        .await?;
    let note: String = rows
        .collect()
        .await?
        .first()
        .ok_or("Bob has no note")?
        .get("body")?;
    println!("Bob reads: {note}");

    bob.database.close().await?;
    alice.database.close().await?;
    Ok(std::fs::remove_dir_all(directory)?)
}
