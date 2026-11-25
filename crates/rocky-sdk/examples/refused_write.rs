use rocky_sdk::{Affinity, Column, Configuration, ErrorKind, Schema, Table, Unsupported, params};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = Schema::builder("notes", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()?;
    let directory = std::env::temp_dir().join(format!("rocky-refused-{}", std::process::id()));
    let database = Configuration::new(&directory, &schema)?
        .create()
        .await?
        .database;

    let mut transaction = database.transaction().await?;
    let stamped = "INSERT INTO notes(body) VALUES(datetime('now'))";
    let Err(error) = transaction.execute(stamped, params![]).await else {
        return Err("a clock value in a replayed write was accepted".into());
    };
    match error.kind() {
        ErrorKind::Sql {
            unsupported: Some(Unsupported::NonDeterministic(function)),
            ..
        } => {
            println!("{function}() differs on every device; bind its value instead: {error}");
        }
        ErrorKind::Denied { need, reason, .. } => println!("cannot {need}: {reason}"),
        _ if error.retryable() => println!("retry later: {error}"),
        _ => return Err(error.into()),
    }
    transaction.rollback().await?;
    database.close().await?;
    Ok(std::fs::remove_dir_all(directory)?)
}
