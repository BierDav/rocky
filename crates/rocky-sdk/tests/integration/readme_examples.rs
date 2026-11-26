use crate::common::Failure;
use rocky_sdk::{
    Affinity, Column, Database, ErrorKind, Index, Role, RoleName, Schema, Table, params,
};
use std::path::Path;

const DOCTOR: RoleName = RoleName::from_static("doctor");

fn clinic() -> Result<Schema, rocky_sdk::Error> {
    let charts = Table::replayed("charts")
        .column(Column::new("ward", Affinity::Text).required())
        .column(Column::new("note", Affinity::Text).required())
        .primary_key(["ward", "id"])
        .partition_by(["ward"]);
    let doctor = Role::new(DOCTOR)
        .read("wards")
        .append("charts")
        .update_columns("charts", ["note"])
        .per_partition("charts");
    Schema::builder("clinic", 1)
        .table(Table::replayed("wards").column(Column::new("name", Affinity::Text).required()))
        .table(charts)
        .role(doctor)
        .build()
}

fn ledger() -> Result<Schema, rocky_sdk::Error> {
    let accounts = Table::replayed("accounts")
        .column(Column::new("name", Affinity::Text).required())
        .column(
            Column::new("currency", Affinity::Text)
                .required()
                .default("EUR"),
        )
        .index(Index::new("accounts_by_name", ["name"]).unique());
    let entries = Table::replayed("entries")
        .column(
            Column::new("account", Affinity::Blob)
                .required()
                .references("accounts"),
        )
        .column(Column::new("cents", Affinity::Integer).required())
        .column(Column::new("booked", Affinity::Integer).required())
        .index(Index::new("entries_by_account", ["account"]).descending("booked"));
    Schema::builder("ledger", 1)
        .table(accounts)
        .table(entries)
        .build()
}

fn notes(version: u32) -> Result<Schema, rocky_sdk::Error> {
    let mut notes =
        Table::replayed("notes").column(Column::new("title", Affinity::Text).required());
    let mut schema = Schema::builder("notes", version);
    if version >= 2 {
        notes = notes.column(Column::new("slug", Affinity::Text));
        schema = schema.migration(2, [FILL_SLUGS]);
    }
    schema.table(notes).build()
}

const FILL_SLUGS: &str = "UPDATE notes SET slug = lower(title) WHERE slug IS NULL";

async fn fill_slugs(database: &Database) -> Result<(), rocky_sdk::Error> {
    database
        .write(async |tx| tx.execute(FILL_SLUGS, params![]).await)
        .await
}

mod one_device {
    include!("../../examples/one_device.rs");
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        main()
    }
}

mod refused_write {
    include!("../../examples/refused_write.rs");
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        main()
    }
}

mod renamed_column {
    include!("../../examples/renamed_column.rs");
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        main()
    }
}

mod two_devices {
    include!("../../examples/two_devices.rs");
    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        main()
    }
}

fn repository() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

#[test]
fn every_rust_block_in_the_readme_appears_verbatim_in_its_example_file() -> Result<(), Failure> {
    let readme = std::fs::read_to_string(repository().join("README.md"))?;
    let mut lines = readme.lines();
    let mut checked = Vec::new();
    while let Some(line) = lines.next() {
        let Some(info) = line.strip_prefix("```rust") else {
            continue;
        };
        let file = info.trim();
        assert!(
            !file.is_empty(),
            "a Rust block in the README must name its example file after ```rust"
        );
        let block: String = lines
            .by_ref()
            .take_while(|line| *line != "```")
            .map(|line| format!("{line}\n"))
            .collect();
        let source = std::fs::read_to_string(repository().join(file))?;
        assert!(
            source.contains(&block),
            "the README's block for {file} differs from the file; copy one into the other"
        );
        checked.push(file.to_owned());
    }
    assert!(
        checked.len() >= 2,
        "the README shows the one- and two-device examples: {checked:?}"
    );
    Ok(())
}

#[test]
fn the_readme_role_recipe_names_its_partition_group_after_the_table() -> Result<(), Failure> {
    clinic()?;
    Ok(())
}

#[tokio::test]
async fn the_readme_ledger_refuses_deleting_an_account_its_entries_reference() -> Result<(), Failure>
{
    let directory = tempfile::tempdir()?;
    let database = rocky_sdk::Configuration::new(directory.path(), &ledger()?)?
        .create()
        .await?
        .database;
    let account = database
        .write(async |tx| {
            let account = tx.next_id("accounts", Vec::new()).await?;
            tx.execute(
                "INSERT INTO accounts(id, name) VALUES(?, 'Cash')",
                params![account],
            )
            .await?;
            tx.execute(
                "INSERT INTO entries(account, cents, booked) VALUES(?, 250, 1)",
                params![account],
            )
            .await?;
            Ok::<_, rocky_sdk::Error>(account)
        })
        .await?;
    let currency: String = database
        .query("SELECT currency FROM accounts", params![])
        .await?
        .collect()
        .await?
        .first()
        .ok_or("no account")?
        .get("currency")?;
    assert_eq!(currency, "EUR");
    let refused = database
        .write(async |tx| {
            tx.execute("DELETE FROM accounts WHERE id = ?", params![account])
                .await
        })
        .await
        .err()
        .ok_or("an account its entries reference was deleted")?;
    assert!(
        matches!(refused.kind(), ErrorKind::Sql { .. }),
        "{refused:?}"
    );
    database.close().await?;
    Ok(())
}

async fn note(database: &Database, title: &str) -> Result<(), rocky_sdk::Error> {
    database
        .write(async |tx| {
            tx.execute("INSERT INTO notes(title) VALUES(?)", params![title])
                .await
        })
        .await
}
async fn slugs(database: &Database) -> Result<Vec<Option<String>>, Failure> {
    Ok(database
        .query("SELECT slug FROM notes ORDER BY title", params![])
        .await?
        .collect()
        .await?
        .iter()
        .map(|row| row.get("slug"))
        .collect::<Result<_, _>>()?)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_readme_slug_rule_fills_a_late_write_from_an_older_version_after_sync()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = std::sync::Arc::new(rocky_sdk::transport::memory::Hub::default());
    let (owner_path, member_path) = (root.path().join("owner"), root.path().join("member"));
    let configure = |path: &Path, name: &str, schema: &Schema| {
        crate::common::configuration(
            path,
            schema,
            None,
            Some(crate::common::external_identity(path)),
            Some(rocky_sdk::Network::Owned(hub.connector(name))),
        )
    };
    let owner = crate::common::Device::new(
        crate::common::create_as(
            configure(&owner_path, "owner", &notes(1)?),
            crate::common::external_key(&owner_path),
        )
        .await?,
    )?;
    let invitation = owner.invite_as(rocky_sdk::Access::Full).await;
    let member = crate::common::Device::new(
        configure(&member_path, "member", &notes(1)?)
            .join(&invitation)
            .await?,
    )?;
    crate::common::introduce(&owner, &member).await;
    note(&owner.database, "Early").await?;
    owner.close().await;
    let owner =
        crate::common::Device::new(configure(&owner_path, "owner", &notes(2)?).open().await?)?;
    assert_eq!(slugs(&owner.database).await?, [Some("early".into())]);
    note(&member.database, "Late").await?;
    for device in [&member, &owner, &member] {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    assert_eq!(
        slugs(&owner.database).await?,
        [Some("early".into()), None],
        "the migration ran once at publish, so the late write holds only the column default"
    );
    fill_slugs(&owner.database).await?;
    assert_eq!(
        slugs(&owner.database).await?,
        [Some("early".into()), Some("late".into())]
    );
    owner.close().await;
    member.close().await;
    Ok(())
}

#[test]
fn the_one_device_example_reads_back_its_note() -> Result<(), Box<dyn std::error::Error>> {
    one_device::run()
}

#[test]
fn the_two_device_example_syncs_a_note_over_loopback_tcp() -> Result<(), Box<dyn std::error::Error>>
{
    two_devices::run()
}

#[test]
fn the_refused_write_example_matches_a_clock_value_in_a_replayed_write()
-> Result<(), Box<dyn std::error::Error>> {
    refused_write::run()
}

#[test]
fn the_renamed_column_example_lands_an_offline_write_under_the_new_name()
-> Result<(), Box<dyn std::error::Error>> {
    renamed_column::run()
}
