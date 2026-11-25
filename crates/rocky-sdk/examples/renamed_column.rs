use rocky_sdk::{
    Access, Affinity, Column, Configuration, Database, IdentityProvider, Invite, Network, Schema,
    SyncMode, Table, params, transport::memory::Hub,
};
use std::{path::Path, sync::Arc};

fn budget(version: u32) -> Result<Schema, rocky_sdk::Error> {
    let memo = match version {
        1 => Column::new("note", Affinity::Text),
        _ => Column::new("memo", Affinity::Text).renamed_from("note"),
    };
    let expenses = Table::replayed("expenses")
        .column(Column::new("cents", Affinity::Integer).required())
        .column(memo);
    Schema::builder("budget", version).table(expenses).build()
}

fn configure(
    hub: &Arc<Hub>,
    directory: &Path,
    device: &str,
    schema: &Schema,
    sync: SyncMode,
) -> Result<Configuration, rocky_sdk::Error> {
    Ok(Configuration::new(directory.join(device), schema)?
        .network(Network::Owned(hub.connector(device)))
        .sync(sync))
}

async fn spend(database: &Database, cents: i64, note: &str) -> Result<(), rocky_sdk::Error> {
    let mut transaction = database.transaction().await?;
    let id = transaction.next_id("expenses", vec![]).await?;
    let insert = "INSERT INTO expenses(id, cents, note) VALUES(?, ?, ?)";
    transaction
        .execute(insert, params![id, cents, note])
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn memos(database: &Database) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let rows = database
        .query("SELECT memo FROM expenses ORDER BY memo", params![])
        .await?;
    let mut memos = Vec::new();
    for row in rows.collect().await? {
        memos.push(row.get("memo")?);
    }
    Ok(memos)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("rocky-renamed-{}", std::process::id()));
    let hub = Arc::new(Hub::default());
    let (v1, v2) = (budget(1)?, budget(2)?);

    let alice = configure(&hub, &directory, "alice", &v1, SyncMode::Manual)?
        .create()
        .await?;
    let alice_key: Arc<dyn IdentityProvider> = Arc::new(alice.owner);
    let invitation = alice.database.invite(Invite::new(Access::Full)).await?;
    let bob = configure(&hub, &directory, "bob", &v1, SyncMode::Manual)?
        .join(&invitation.code)
        .await?;
    let bob_key: Arc<dyn IdentityProvider> = Arc::new(bob.identity.ok_or("Bob has a person key")?);
    spend(&alice.database, 4_200, "groceries").await?;
    bob.database.sync_now().await?;
    bob.database.close().await?;
    alice.database.close().await?;

    let alice = configure(&hub, &directory, "alice", &v2, SyncMode::Manual)?
        .identity(alice_key)
        .open()
        .await?;
    println!(
        "Alice runs version {}",
        alice.status().await?.schema_version()
    );

    let offline_bob = configure(&hub, &directory, "bob", &v1, SyncMode::Off)?
        .identity(bob_key.clone())
        .open()
        .await?;
    spend(&offline_bob, 1_950, "train ticket").await?;
    offline_bob.close().await?;

    let bob = configure(&hub, &directory, "bob", &v2, SyncMode::Manual)?
        .identity(bob_key)
        .open()
        .await?;
    bob.add_peer(&alice.contact().await?.uri).await?;
    alice.add_peer(&bob.contact().await?.uri).await?;
    bob.sync_now().await?;
    alice.sync_now().await?;
    bob.sync_now().await?;

    let expected = ["groceries", "train ticket"];
    for (device, database) in [("Alice", &alice), ("Bob", &bob)] {
        let memos = memos(database).await?;
        println!("{device} reads memos: {memos:?}");
        if memos != expected {
            return Err(format!("{device} lost a write made under the old name").into());
        }
    }
    bob.close().await?;
    alice.close().await?;
    Ok(std::fs::remove_dir_all(directory)?)
}
