//! Consumes Kotlin qualification output; run through `./kotlin check qualify -m schema-qualification`.
use rocky_core::{DomainId, LogicalSchema, Profile, SchemaDefinition, TypedRows};
use rocky_sdk::{Database, Failure, Open, TypedApp};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn descriptor(path: &Path) -> Result<LogicalSchema, Failure> {
    Ok(SchemaDefinition::parse(&fs::read(path)?)?.build()?)
}

#[tokio::main]
async fn main() -> Result<(), Failure> {
    let directory = std::env::args()
        .nth(1)
        .ok_or("definition output directory")?;
    let directory = Path::new(&directory);
    for (hand, imported) in [
        ("samples", "samples-import"),
        ("shopping-hand", "shopping-import"),
        ("market-hand", "market-import"),
        ("constraints-hand", "constraints-import"),
        ("partitions-hand", "partitions-import"),
        ("existence-hand", "existence-import"),
    ] {
        let hand = descriptor(&directory.join(format!("{hand}.json")))?;
        let imported = descriptor(&directory.join(format!("{imported}.json")))?;
        assert_eq!(hand.canonical()?, imported.canonical()?);
        assert_eq!(hand.fingerprint()?, imported.fingerprint()?);
        assert_eq!(
            hand.replicated_fingerprint(),
            imported.replicated_fingerprint()
        );
        if hand
            .tables
            .iter()
            .any(|table| !table.foreign_keys.is_empty())
        {
            assert!(hand.supported_on(Profile::Trusted).is_err());
        } else {
            hand.supported_on(Profile::Trusted)?;
        }
        hand.supported_on(Profile::Secure)?;
        if hand
            .tables
            .iter()
            .any(|table| !table.foreign_keys.is_empty())
        {
            assert!(TypedApp::new(hand.clone(), &[DomainId([1; 16])]).is_err());
        } else {
            TypedApp::new(hand.clone(), &[DomainId([1; 16])])?;
        }
        if hand.existence_tables().next().is_some() {
            // The reference crosses reader scopes; installation lists the log.
            let names: Vec<_> = hand
                .existence_tables()
                .map(|table| table.sql_name.as_str())
                .collect();
            assert_eq!(names, ["parents"]);
        }
        TypedRows::new(imported, vec![DomainId([1; 16])])?;
    }
    let scratch = tempfile::tempdir()?;
    for (hand, imported) in [
        ("samples", "samples-import"),
        ("shopping-hand", "shopping-import"),
        ("market-hand", "market-import"),
        ("constraints-hand", "constraints-import"),
        ("partitions-hand", "partitions-import"),
        ("existence-hand", "existence-import"),
    ] {
        let path = scratch.path().join(hand);
        let handwritten = fs::read_to_string(directory.join(format!("{hand}-wire.json")))?;
        let imported = fs::read_to_string(directory.join(format!("{imported}-wire.json")))?;
        let options = |schema| Open {
            directory: &path,
            schema,
            secrets: None,
            network: None,
            commands: None,
        };
        let database = Database::create(options(&handwritten), None)?.database;
        let scope: Value = serde_json::from_str(&database.scope())?;
        let table = if hand == "samples" {
            "samples"
        } else if hand == "shopping-hand" {
            "shopping_items"
        } else if hand == "constraints-hand" || hand == "existence-hand" {
            "children"
        } else if hand == "partitions-hand" {
            "partition_samples"
        } else {
            "listings"
        };
        let call = json!({"version":1,"scope":scope,"request":{"type":"read","query":{
            "sql":format!("SELECT * FROM {table}"),"values":[],"max_rows":10
        }}});
        let reply: Value = serde_json::from_str(&database.session().call(call.to_string()).await?)?;
        assert_eq!(reply["status"], "ok", "{reply}");
        assert_eq!(reply["value"]["rows"], json!([]));
        database.close().await?;
        // Import has exactly the same SDK schema identity as hand declarations.
        Database::open(options(&imported))?.database.close().await?;
        if hand == "samples" {
            let connection = rusqlite::Connection::open(path.join("rocky.sqlite"))?;
            let sql: String = connection.query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND sql LIKE '%reading%' AND sql LIKE '%PRIMARY KEY%'",
                [], |row| row.get(0),
            )?;
            assert!(
                sql.contains("PRIMARY KEY(\"sensor\",\"sequence\")"),
                "{sql}"
            );
            let indexes: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND sql LIKE '%reading%DESC%'",
                [], |row| row.get(0),
            )?;
            assert_eq!(indexes, 1);
        }
    }
    println!(
        "six hand/import descriptor and fingerprint pairs passed; all SDK schemas installed and reopened"
    );
    Ok(())
}
