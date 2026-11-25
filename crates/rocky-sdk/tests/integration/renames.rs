use crate::common::{self, Device, Failure, build, introduce, required, shopping};
use rocky_sdk::{
    Affinity, Column, Commit, Configuration, DropReason, ErrorKind, IndexColumn, IndexDefinition,
    MigrationStep, Network, Role, RoleName, Schema, Table, TransactionOutcome, TransactionToken,
    params, transport::memory::Hub,
};
use std::{path::PathBuf, sync::Arc};

fn titled(title: Column) -> Table {
    Table::replayed("items")
        .column(required("key", Affinity::Text))
        .column(required("completed", Affinity::Integer))
        .column(title)
        .primary_key(["key"])
}
fn v1() -> Schema {
    common::schema()
}
fn labelled() -> Schema {
    build(shopping(2).table(titled(
        required("label", Affinity::Text).renamed_from("title"),
    )))
}
fn named() -> Schema {
    build(
        shopping(3).table(titled(
            required("name", Affinity::Text)
                .renamed_from("title")
                .renamed_from("label"),
        )),
    )
}

struct Household {
    root: crate::support::Directory,
    hub: Arc<Hub>,
    owner: Option<Device>,
    member: Option<Device>,
}
impl Household {
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
    fn network(&self, name: &str) -> Network {
        Network::Owned(self.hub.connector(name))
    }
    fn configuration(&self, name: &str, schema: &Schema) -> Configuration {
        common::configuration(
            &self.path(name),
            schema,
            None,
            Some(common::external_identity(&self.path(name))),
            Some(self.network(name)),
        )
    }
    async fn create() -> Result<Self, Failure> {
        let root = crate::support::small_directory()?;
        let hub = Arc::new(Hub::default());
        let mut household = Self {
            root,
            hub,
            owner: None,
            member: None,
        };
        let owner = Device::new(
            common::create_as(
                household.configuration("owner", &v1()),
                common::external_key(&household.path("owner")),
            )
            .await?,
        )?;
        let member = Device::new(
            household
                .configuration("member", &v1())
                .join(&owner.invite().await)
                .await?,
        )?;
        introduce(&owner, &member).await;
        write(
            &owner,
            "INSERT INTO items(id,key,completed,title) VALUES(?,'k1',0,'apples')",
        )
        .await?;
        sync(&member).await;
        household.owner = Some(owner);
        household.member = Some(member);
        Ok(household)
    }
    fn owner(&self) -> &Device {
        self.owner.as_ref().expect("the owner is open")
    }
    fn member(&self) -> &Device {
        self.member.as_ref().expect("the member is open")
    }
    async fn reopen_owner(&mut self, schema: &Schema) -> Result<(), Failure> {
        if let Some(owner) = self.owner.take() {
            owner.close().await;
        }
        self.owner = Some(Device::new(
            self.configuration("owner", schema).open().await?,
        )?);
        Ok(())
    }
    async fn reopen_owner_confirmed(&mut self, schema: &Schema) -> Result<(), Failure> {
        if let Some(owner) = self.owner.take() {
            owner.close().await;
        }
        self.owner = Some(Device::new(
            common::confirmed(self.configuration("owner", schema)).await?,
        )?);
        Ok(())
    }
    async fn reopen_member(&mut self, schema: &Schema) -> Result<(), Failure> {
        if let Some(member) = self.member.take() {
            member.close().await;
        }
        self.member = Some(Device::new(
            self.configuration("member", schema).open().await?,
        )?);
        Ok(())
    }
    async fn reconnect(&self) {
        introduce(self.owner(), self.member()).await;
        for _ in 0..3 {
            sync(self.member()).await;
            sync(self.owner()).await;
        }
    }
    async fn close(mut self) {
        for device in [self.member.take(), self.owner.take()]
            .into_iter()
            .flatten()
        {
            device.close().await;
        }
    }
}

async fn sync(device: &Device) {
    crate::support::peerless_ok(device.sync_now().await).expect("sync");
}
async fn write(device: &Device, sql: &str) -> Result<TransactionToken, Failure> {
    let mut transaction = device.database.transaction().await?;
    let id = transaction.next_id("items", Vec::new()).await?;
    let bound = if sql.contains('?') {
        params![id]
    } else {
        params![]
    };
    transaction.execute(sql, bound).await?;
    match transaction.commit().await? {
        Commit::Recorded(token) => Ok(token),
        Commit::NoWrites => Err("the write changed nothing".into()),
    }
}
async fn update(device: &Device, sql: &str) -> Result<TransactionToken, Failure> {
    let mut transaction = device.database.transaction().await?;
    transaction.execute(sql, params![]).await?;
    match transaction.commit().await? {
        Commit::Recorded(token) => Ok(token),
        Commit::NoWrites => Err("the write changed nothing".into()),
    }
}
async fn texts(device: &Device, sql: &str, columns: &[&str]) -> Result<Vec<Vec<String>>, Failure> {
    let rows = device
        .database
        .query(sql, params![])
        .await?
        .collect()
        .await?;
    rows.iter()
        .map(|row| {
            columns
                .iter()
                .map(|column| Ok(row.get::<String>(column)?))
                .collect()
        })
        .collect()
}
async fn schema_version(device: &Device) -> Result<u32, Failure> {
    Ok(device.database.status().await?.schema_version())
}
fn pairs(rows: &[(&str, &str)]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|(key, value)| vec![(*key).to_owned(), (*value).to_owned()])
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_write_to_a_renamed_column_lands_in_its_new_name() -> Result<(), Failure> {
    let mut household = Household::create().await?;
    household.reopen_owner(&labelled()).await?;
    assert_eq!(schema_version(household.owner()).await?, 2);
    update(
        household.member(),
        "UPDATE items SET title = 'pears' WHERE key = 'k1'",
    )
    .await?;
    let late = write(
        household.member(),
        "INSERT INTO items(id,key,completed,title) VALUES(?,'k2',0,'bread')",
    )
    .await?;
    household.reconnect().await;

    let landed = pairs(&[("k1", "pears"), ("k2", "bread")]);
    let read = "SELECT key, label FROM items ORDER BY key";
    assert_eq!(
        texts(household.owner(), read, &["key", "label"]).await?,
        landed
    );
    assert_eq!(schema_version(household.member()).await?, 2);
    let old_reader = household
        .member()
        .database
        .query("SELECT key, title FROM items", params![])
        .await
        .err()
        .ok_or("a v1 read of a renamed column succeeded")?;
    assert!(
        matches!(
            old_reader.kind(),
            ErrorKind::UpgradeRequired {
                current: 2,
                declared: 1
            }
        ),
        "a v1 application on a v2 database is told to upgrade: {old_reader}"
    );
    assert!(
        matches!(
            household
                .member()
                .database
                .transaction_outcome(late)
                .await?,
            Some(TransactionOutcome::Applied { .. })
        ),
        "the late write applied"
    );
    household.reopen_member(&labelled()).await?;
    assert_eq!(
        texts(household.member(), read, &["key", "label"]).await?,
        landed
    );
    household.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_chain_maps_a_v1_write_to_the_v3_column() -> Result<(), Failure> {
    let mut household = Household::create().await?;
    household.reopen_owner(&labelled()).await?;
    household.reopen_owner(&named()).await?;
    assert_eq!(schema_version(household.owner()).await?, 3);
    update(
        household.member(),
        "UPDATE items SET title = 'pears' WHERE key = 'k1'",
    )
    .await?;
    household.reopen_member(&named()).await?;
    household.reconnect().await;
    for device in [household.owner(), household.member()] {
        assert_eq!(
            texts(device, "SELECT key, name FROM items", &["key", "name"]).await?,
            pairs(&[("k1", "pears")])
        );
    }
    household.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_write_to_a_renamed_table_lands_in_its_new_name() -> Result<(), Failure> {
    let mut household = Household::create().await?;
    let entries = build(
        shopping(2).table(
            Table::replayed("entries")
                .renamed_from("items")
                .column(required("code", Affinity::Text).renamed_from("key"))
                .column(required("completed", Affinity::Integer))
                .column(required("title", Affinity::Text))
                .primary_key(["code"]),
        ),
    );
    household.reopen_owner(&entries).await?;
    update(
        household.member(),
        "UPDATE items SET title = 'pears' WHERE key = 'k1'",
    )
    .await?;
    write(
        household.member(),
        "INSERT INTO items(id,key,completed,title) VALUES(?,'k2',0,'bread')",
    )
    .await?;
    household.reopen_member(&entries).await?;
    household.reconnect().await;
    let read = "SELECT code, title FROM entries ORDER BY code";
    for device in [household.owner(), household.member()] {
        assert_eq!(
            texts(device, read, &["code", "title"]).await?,
            pairs(&[("k1", "pears"), ("k2", "bread")]),
            "a renamed table keeps its rows, its key column and the late writes to both"
        );
    }
    household.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_write_to_a_dropped_column_is_still_dropped_loudly() -> Result<(), Failure> {
    let mut household = Household::create().await?;
    let untitled = build(
        shopping(2).table(
            Table::replayed("items")
                .column(required("key", Affinity::Text))
                .column(required("completed", Affinity::Integer))
                .column(required("label", Affinity::Text).default(""))
                .primary_key(["key"]),
        ),
    );
    household.reopen_owner_confirmed(&untitled).await?;
    let late = update(
        household.member(),
        "UPDATE items SET title = 'pears' WHERE key = 'k1'",
    )
    .await?;
    household.reopen_member(&untitled).await?;
    household.reconnect().await;
    assert_eq!(
        household
            .member()
            .database
            .transaction_outcome(late)
            .await?,
        Some(TransactionOutcome::Dropped {
            reason: DropReason::Schema,
            sealed: false,
        }),
        "a column declared without renamed_from is a new field, so the old one's writes drop"
    );
    household.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_that_forgets_an_earlier_name_is_refused_before_it_publishes()
-> Result<(), Failure> {
    let mut household = Household::create().await?;
    household.reopen_owner(&labelled()).await?;
    let forgetful = build(shopping(3).table(titled(
        required("name", Affinity::Text).renamed_from("label"),
    )));
    let refused = household
        .reopen_owner(&forgetful)
        .await
        .err()
        .ok_or("a rename without its earlier names published")?;
    let error = refused
        .downcast_ref::<rocky_sdk::Error>()
        .ok_or("an SDK error")?;
    assert!(
        matches!(
            error.kind(),
            ErrorKind::Schema(schema) if matches!(**schema, rocky_sdk::SchemaError::LostEarlierName { .. })
        ),
        "{error}"
    );
    household.close().await;
    Ok(())
}

const EDITOR: RoleName = RoleName::from_static("editor");
fn unique(column: &str) -> IndexDefinition {
    IndexDefinition {
        name: "items_unique_title".into(),
        columns: vec![IndexColumn {
            name: column.into(),
            descending: false,
        }],
        unique: true,
        expressions: Vec::new(),
        predicate: None,
    }
}
fn edited(version: u32, title: Column, column: &str) -> Schema {
    build(
        shopping(version)
            .table(titled(title).index(unique(column)))
            .role(Role::new(EDITOR).update_columns("items", [column])),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn renaming_a_column_a_role_and_an_index_name_neither_removes_nor_narrows()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let configuration = |schema: &Schema| {
        common::configuration(
            root.path(),
            schema,
            None,
            Some(common::external_identity(root.path())),
            None,
        )
    };
    let owner = Device::new(
        common::create_as(
            configuration(&edited(1, required("title", Affinity::Text), "title")),
            common::external_key(root.path()),
        )
        .await?,
    )?;
    write(
        &owner,
        "INSERT INTO items(id,key,completed,title) VALUES(?,'k1',0,'apples')",
    )
    .await?;
    owner.close().await;
    let renamed = edited(
        2,
        required("label", Affinity::Text).renamed_from("title"),
        "label",
    );
    let owner = Device::new(configuration(&renamed).open().await?)?;
    assert_eq!(
        schema_version(&owner).await?,
        2,
        "the rename published without a confirmation"
    );
    assert_eq!(
        texts(&owner, "SELECT key, label FROM items", &["key", "label"]).await?,
        pairs(&[("k1", "apples")])
    );
    let duplicate = write(
        &owner,
        "INSERT INTO items(id,key,completed,label) VALUES(?,'k2',0,'apples')",
    )
    .await;
    assert!(
        duplicate.is_err(),
        "the unique index holds the renamed column"
    );
    owner.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_migration_in_the_renaming_version_reads_its_parent_under_the_old_names()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let configuration = |schema: &Schema| {
        common::configuration(
            root.path(),
            schema,
            None,
            Some(common::external_identity(root.path())),
            None,
        )
    };
    let owner = Device::new(
        common::create_as(configuration(&v1()), common::external_key(root.path())).await?,
    )?;
    write(
        &owner,
        "INSERT INTO items(id,key,completed,title) VALUES(?,'k1',0,'apples')",
    )
    .await?;
    owner.close().await;
    let copied = build(
        shopping(2)
            .table(titled(required("label", Affinity::Text).renamed_from("title")))
            .migration(
                2,
                [MigrationStep::insert(
                    "items",
                    "SELECT 'copy-' || key AS key, completed, upper(title) AS label FROM parent.items",
                )],
            ),
    );
    let owner = Device::new(configuration(&copied).open().await?)?;
    assert_eq!(
        texts(
            &owner,
            "SELECT key, label FROM items ORDER BY key",
            &["key", "label"]
        )
        .await?,
        pairs(&[("copy-k1", "APPLES"), ("k1", "apples")])
    );
    owner.close().await;
    Ok(())
}
