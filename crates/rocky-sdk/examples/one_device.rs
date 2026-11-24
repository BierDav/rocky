use rocky_sdk::{Affinity, Column, Configuration, Schema, Table, params};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = Schema::builder("notes", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()?;
    let directory = std::env::temp_dir().join(format!("rocky-one-device-{}", std::process::id()));
    let database = Configuration::new(&directory, &schema)?
        .create()
        .await?
        .database;

    let mut transaction = database.transaction().await?;
    let id = transaction.next_id("notes", vec![]).await?;
    let insert = "INSERT INTO notes(id, body) VALUES(?, ?)";
    transaction
        .execute(insert, params![id, "Hello, Rocky"])
        .await?;
    transaction.commit().await?;

    let rows = database.query("SELECT body FROM notes", params![]).await?;
    let note: String = rows
        .collect()
        .await?
        .first()
        .ok_or("no note")?
        .get("body")?;
    println!("{note}");
    database.close().await?;
    Ok(std::fs::remove_dir_all(directory)?)
}
