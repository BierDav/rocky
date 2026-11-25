use crate::support;
use rocky_sdk::{
    Affinity, Column, Database, DeleteAction, ForeignKeyDefinition, IndexColumn, IndexDefinition,
    MigrationStep, Network, Schema, SchemaBuilder, Table, transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use support::secrets;
use support::snapshot as snap;

fn reference(columns: &[&str], table: &str, on_delete: DeleteAction) -> ForeignKeyDefinition {
    ForeignKeyDefinition {
        columns: columns.iter().map(|column| (*column).into()).collect(),
        table: table.into(),
        on_delete,
        deferred: false,
    }
}
fn required(name: &str, affinity: Affinity) -> Column {
    Column::new(name, affinity).required()
}
fn in_list(table: &str) -> Table {
    Table::replayed(table)
        .column(required("list", Affinity::Blob))
        .primary_key(["list", "id"])
        .partition_by(["list"])
        .partition_group("list")
}
fn items() -> Table {
    in_list("items")
        .column(required("title", Affinity::Text))
        .column(Column::new("category", Affinity::Blob))
        .index(IndexDefinition {
            name: "index_items_list_title".into(),
            columns: ["list", "title"]
                .map(|name| IndexColumn {
                    name: name.into(),
                    descending: false,
                })
                .into(),
            unique: true,
            expressions: Vec::new(),
            predicate: None,
        })
}
fn referencing_items() -> Table {
    items()
        .foreign_key(reference(&["list"], "lists", DeleteAction::Cascade))
        .foreign_key(reference(
            &["list", "category"],
            "categories",
            DeleteAction::SetNull,
        ))
}
fn lists(version: u32, items: Table) -> SchemaBuilder {
    Schema::builder("lists", version)
        .table(
            Table::replayed("lists")
                .column(required("title", Affinity::Text))
                .partition_by(["id"])
                .partition_group("list"),
        )
        .table(
            in_list("categories")
                .column(required("name", Affinity::Text))
                .foreign_key(reference(&["list"], "lists", DeleteAction::Cascade)),
        )
        .table(items)
}
fn position() -> Column {
    required("position", Affinity::Text).default("")
}
const POSITION_BY_ID: &str = "UPDATE items SET position = printf('%09d5', (SELECT count(*) FROM items AS earlier WHERE earlier.list = items.list AND earlier.id < items.id))";
fn build(schema: SchemaBuilder) -> Schema {
    schema.build().expect("lists schema")
}
fn first() -> Schema {
    build(lists(1, referencing_items()))
}
fn ordered() -> Schema {
    build(
        lists(2, referencing_items().column(position()))
            .table(
                in_list("item_categories")
                    .column(required("item", Affinity::Blob))
                    .column(required("category", Affinity::Blob))
                    .primary_key(["list", "item", "category"])
                    .foreign_key(reference(&["list", "item"], "items", DeleteAction::Cascade))
                    .foreign_key(reference(
                        &["list", "category"],
                        "categories",
                        DeleteAction::Cascade,
                    )),
            )
            .migration(
                2,
                [
                    POSITION_BY_ID.into(),
                    MigrationStep::insert(
                        "item_categories",
                        "SELECT list, id AS item, category FROM items WHERE category IS NOT NULL",
                    ),
                    "UPDATE items SET category = NULL WHERE category IS NOT NULL".into(),
                ],
            ),
    )
}

type Failure = Box<dyn std::error::Error + Send + Sync>;

fn key(directory: &Path) -> rocky_sdk::IdentityKey {
    rocky_sdk::IdentityKey::from_secret(rocky_core::commitment(
        "fixture.lists.identity",
        directory.to_string_lossy().as_bytes(),
    ))
}
fn identity(directory: &Path) -> Arc<dyn rocky_sdk::IdentityProvider> {
    Arc::new(key(directory))
}

struct Peer {
    database: Database,
}
async fn only_explicit_syncs(peers: &[&Peer]) -> Result<(), Failure> {
    for peer in peers {
        snap::value(peer.database.set_sync(rocky_sdk::SyncMode::Manual)).await?;
    }
    Ok(())
}
impl Peer {
    fn new(database: Database) -> Result<Self, Failure> {
        Ok(Self { database })
    }

    async fn sync_now(&self) -> Result<Option<rocky_sdk::status::SyncReport>, rocky_sdk::Error> {
        crate::support::peerless_ok(self.database.sync_now().await)
    }
    async fn insert(&self, table: &str, list: Option<&str>, sql: &str) -> Result<String, Failure> {
        let mut tx = self.database.transaction().await?;
        let id = tx
            .next_id(table, list.map(values).unwrap_or_default())
            .await?;
        tx.execute(sql, vec![id.sql()]).await?;
        tx.commit().await?;
        Ok(id.to_string())
    }
    async fn execute(&self, sql: &str) -> Result<(), Failure> {
        let mut tx = self.database.transaction().await?;
        tx.execute(sql, Vec::new()).await?;
        tx.commit().await?;
        Ok(())
    }
    async fn column(&self, sql: &str) -> Result<Vec<Value>, Failure> {
        let page = snap::read(&self.database, sql, Vec::new()).await?;
        Ok(page["rows"]
            .as_array()
            .ok_or("rows")?
            .iter()
            .map(|row| snap::cell_value(&row[0]))
            .collect())
    }
}
fn values(list: &str) -> Vec<rocky_sdk::SqlValue> {
    vec![snap::row_id(&(list))]
}
fn open(directory: &Path, hub: &Arc<Hub>, name: &str) -> rocky_sdk::Configuration {
    at(directory, hub, name, &first())
}
fn at(directory: &Path, hub: &Arc<Hub>, name: &str, schema: &Schema) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        schema,
        Some(secrets::keys(directory)),
        Some(identity(directory)),
        Some(Network::Owned(hub.connector(name))),
    )
}
fn at_without_identity(
    directory: &Path,
    hub: &Arc<Hub>,
    name: &str,
    schema: &Schema,
) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        schema,
        Some(secrets::keys(directory)),
        None,
        Some(Network::Owned(hub.connector(name))),
    )
}

async fn checkpoint_writer(
    root: &Path,
    hub: &Arc<Hub>,
    schema: &Schema,
) -> Result<(Peer, Peer), Failure> {
    let issuer_path = root.join("issuer");
    let issuer = Peer::new(
        crate::common::create_as(at(&issuer_path, hub, "issuer", schema), key(&issuer_path))
            .await?
            .database,
    )?;
    snap::value(issuer.database.next_partition("lists")).await?;
    issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Base list')",
        )
        .await?;
    snap::value(issuer.database.seal()).await?;
    snap::value(issuer.database.compact(0)).await?;
    let invite = issuer
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?;
    let writer_path = root.join("writer");
    let writer = Peer::new(
        at(&writer_path, hub, "writer", schema)
            .join(&invite.code)
            .await?
            .database,
    )?;
    writer.sync_now().await?;
    assert_eq!(
        writer.column("SELECT title FROM lists").await?,
        [json!("Base list")]
    );
    writer
        .database
        .transaction()
        .await
        .map_err(|error| format!("before preparing a new partition: {error}"))?
        .rollback()
        .await?;
    Ok((issuer, writer))
}

#[tokio::test]
async fn an_unused_own_partition_prepared_before_a_seal_remains_creatable() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer = Peer::new(
        crate::common::create_as(
            at(&root.path().join("issuer"), &hub, "issuer", &ordered()),
            key(&root.path().join("issuer")),
        )
        .await?
        .database,
    )?;
    snap::value(issuer.database.next_partition("lists")).await?;
    issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Base list')",
        )
        .await?;
    let invite = issuer
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?;
    let writer = Peer::new(
        at(&root.path().join("writer"), &hub, "writer", &ordered())
            .join(&invite.code)
            .await?
            .database,
    )?;
    writer.sync_now().await?;
    let prepared = snap::value(writer.database.next_partition("lists")).await?;
    snap::value(issuer.database.seal()).await?;
    snap::value(issuer.database.compact(0)).await?;
    writer.sync_now().await?;
    let status = snap::value(writer.database.status()).await?;
    assert!(
        status["data"]["blockers"]
            .as_array()
            .ok_or("blockers")?
            .iter()
            .all(|blocker| blocker["kind"]["type"] != "seal_rebootstrap"),
        "an unused partition is not unverified history: {status}"
    );
    let again = snap::value(writer.database.next_partition("lists")).await?;
    assert_eq!(again["id"], prepared["id"]);
    let list = writer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Prepared before seal')",
        )
        .await?;
    assert_eq!(again["id"], list.as_str());
    writer.database.close().await?;
    issuer.database.close().await?;
    Ok(())
}

#[tokio::test]
async fn preparing_an_own_id_does_not_claim_an_unrelated_group_members_state() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let unreferenced = build(lists(1, items()));
    let (issuer, writer) = checkpoint_writer(root.path(), &hub, &unreferenced).await?;
    snap::value(writer.database.next_partition("lists")).await?;
    let list = writer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Prepared')",
        )
        .await?;
    let status = snap::value(writer.database.status()).await?;
    let rebootstraps: Vec<&Value> = status["data"]["blockers"]
        .as_array()
        .ok_or("blockers")?
        .iter()
        .filter(|blocker| blocker["kind"]["type"] == "seal_rebootstrap")
        .collect();
    assert!(
        !rebootstraps.is_empty(),
        "status names the unrelated member's missing verified state: {status}"
    );
    let error = writer
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(list,id,title) VALUES(X'{list}',?,'Refused')"),
        )
        .await
        .expect_err("the unrelated member still needs a verified state");
    assert!(
        error.to_string().contains("seal rebootstrap (replay="),
        "{error}"
    );
    for blocker in rebootstraps {
        assert_eq!(blocker["clears_by"]["type"], "any_peer", "{blocker}");
        let scope = blocker["scope"].as_str().ok_or("scope")?;
        assert!(error.to_string().contains(scope), "{scope}: {error}");
    }
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn an_insert_author_creates_only_its_own_partition_and_shares_its_key() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (issuer, writer) = checkpoint_writer(root.path(), &hub, &ordered()).await?;
    assert!(
        snap::value(writer.database.create_partition(&rocky_sdk::Partition::new(
            "list",
            values(&format!("{}{:016x}", "ab".repeat(32), 123))
        )))
        .await
        .is_err()
    );
    let prepared = snap::value(writer.database.next_partition("lists")).await?;
    writer.database.close().await?;
    let writer = Peer::new(
        open(&root.path().join("writer"), &hub, "writer")
            .open()
            .await?,
    )?;
    let list = writer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Member list')",
        )
        .await?;
    assert_eq!(
        prepared["id"],
        list.as_str(),
        "a prepared partition survives a restart before its first row commits"
    );
    writer.sync_now().await?;
    issuer.sync_now().await?;
    assert_eq!(
        issuer
            .column("SELECT title FROM lists ORDER BY title")
            .await?,
        [json!("Base list"), json!("Member list")]
    );
    let read_invite = issuer
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Read))
        .await?;
    let reader = Peer::new(
        open(&root.path().join("reader"), &hub, "reader")
            .join(&read_invite.code)
            .await?
            .database,
    )?;
    assert!(
        snap::value(reader.database.next_partition("lists"))
            .await
            .is_err()
    );
    for peer in [reader, writer, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn an_enrolled_writer_syncs_a_list_shared_with_one_envelope() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer = Peer::new(
        crate::common::create_as(
            open(&root.path().join("a"), &hub, "a"),
            key(&root.path().join("a")),
        )
        .await?
        .database,
    )?;
    let invitation = issuer
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;
    let writer = Peer::new(
        open(&root.path().join("b"), &hub, "b")
            .join(&invitation)
            .await?
            .database,
    )?;
    only_explicit_syncs(&[&issuer, &writer]).await?;

    let prepared = snap::value(issuer.database.next_partition("lists")).await?;
    let list = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Groceries')",
        )
        .await?;
    assert_eq!(prepared["id"], list.as_str());
    let category = issuer
        .insert(
            "categories",
            Some(&list),
            &format!("INSERT INTO categories(id,list,name) VALUES(?,X'{list}','Dairy')"),
        )
        .await?;
    issuer
        .insert(
            "items",
            Some(&list),
            &format!(
                "INSERT INTO items(id,list,title,category) VALUES(?,X'{list}','Milk',X'{category}')"
            ),
        )
        .await?;

    writer.sync_now().await.map_err(|error| {
        format!("one sync delivers the envelope and the list's checkpoint: {error}")
    })?;
    assert_eq!(
        writer
            .column(&format!("SELECT title FROM lists WHERE id = X'{list}'"))
            .await?,
        [json!("Groceries")]
    );
    assert_eq!(
        writer
            .column(&format!(
                "SELECT category FROM items WHERE list = X'{list}'"
            ))
            .await?,
        [json!(category)]
    );

    let mut tx = writer.database.transaction().await?;
    tx.execute(
        &format!("DELETE FROM categories WHERE list = X'{list}' AND id = X'{category}'"),
        Vec::new(),
    )
    .await?;
    tx.commit().await?;
    writer.sync_now().await?;
    for peer in [&writer, &issuer] {
        assert_eq!(
            peer.column(&format!(
                "SELECT category IS NULL FROM items WHERE list = X'{list}'"
            ))
            .await?,
            [json!("1")],
            "deleting a category leaves the item in its list"
        );
    }
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}

async fn refused(peer: &Peer) -> Result<Option<Vec<Value>>, Failure> {
    let activity = snap::value(peer.database.activity(0)).await?;
    Ok(activity["events"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|event| event["kind"] == "scopes_refused")
        .and_then(|event| event["scopes"].as_array().cloned()))
}

fn kinds(activity: &Value) -> Vec<String> {
    activity["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|event| event["kind"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn a_joined_writer_uses_its_node_number_and_activity_shows_the_list_arriving()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer = Peer::new(
        crate::common::create_as(
            open(&root.path().join("a"), &hub, "a"),
            key(&root.path().join("a")),
        )
        .await?
        .database,
    )?;
    let invitation = issuer
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;
    let writer = Peer::new(
        open(&root.path().join("b"), &hub, "b")
            .join(&invitation)
            .await?
            .database,
    )?;
    writer.sync_now().await?;
    snap::value(issuer.database.next_partition("lists")).await?;
    let list = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Groceries')",
        )
        .await?;
    snap::value(issuer.database.seal()).await?;
    snap::value(issuer.database.compact(0)).await?;
    writer.sync_now().await?;
    writer
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title) VALUES(?,X'{list}','Milk')"),
        )
        .await?;
    writer.sync_now().await?;
    assert_eq!(
        issuer
            .column(&format!("SELECT title FROM items WHERE list = X'{list}'"))
            .await?,
        [json!("Milk")]
    );

    let served = kinds(&snap::value(issuer.database.activity(0)).await?);
    for kind in [
        "invitation_redeemed",
        "partition_created",
        "partition_shared",
    ] {
        assert!(
            served.contains(&kind.to_owned()),
            "issuer lacks {kind}: {served:?}"
        );
    }
    let activity = snap::value(writer.database.activity(0)).await?;
    let seen = kinds(&activity);
    for kind in [
        "peer_reconciliation_completed",
        "partition_key_installed",
        "partition_bootstrap",
        "checkpoint_imported",
        "synced",
    ] {
        assert!(
            seen.contains(&kind.to_owned()),
            "writer lacks {kind}: {seen:?}"
        );
    }
    let synced = activity["events"]
        .as_array()
        .ok_or("events")?
        .iter()
        .filter(|event| event["kind"] == "synced")
        .collect::<Vec<_>>();
    assert!(
        synced
            .iter()
            .any(|event| event["pushed"].as_u64() > Some(0))
    );
    assert!(
        synced
            .iter()
            .any(|event| event["scopes"][0]["table"].is_string())
    );
    let token = activity["token"].clone();
    let waiting = snap::value(
        writer
            .database
            .next_activity(snap::integer(&(token)) as u64),
    );
    let (next, _) = tokio::join!(
        waiting,
        snap::value(writer.database.set_sync(rocky_sdk::SyncMode::Off))
    );
    assert_eq!(
        kinds(&next?),
        ["networking_disabled"],
        "a caught-up observer waits for the next event"
    );
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn compacting_releases_a_shared_lists_history_under_its_collections_policy()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer = Peer::new(
        crate::common::create_as(
            open(&root.path().join("a"), &hub, "a"),
            key(&root.path().join("a")),
        )
        .await?
        .database,
    )?;
    let invitation = issuer
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;
    let writer = Peer::new(
        open(&root.path().join("b"), &hub, "b")
            .join(&invitation)
            .await?
            .database,
    )?;
    writer.sync_now().await?;
    let policy = json!({"required":[],"behind":[],"copies":1,"window":0});
    snap::value(
        issuer.database.set_retention(
            "items",
            serde_json::from_value::<rocky_sdk::inspection::RetentionPolicy>(json!(policy))
                .expect("typed fixture"),
        ),
    )
    .await?;
    snap::value(issuer.database.next_partition("lists")).await?;
    let list = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Groceries')",
        )
        .await?;
    for title in ["Milk", "Eggs", "Flour"] {
        issuer
            .insert(
                "items",
                Some(&list),
                &format!("INSERT INTO items(id,list,title) VALUES(?,X'{list}','{title}')"),
            )
            .await?;
    }
    let mut tx = issuer.database.transaction().await?;
    tx.execute("DELETE FROM items WHERE title='Eggs'", Vec::new())
        .await?;
    tx.commit().await?;
    writer.sync_now().await?;
    let titles = format!("SELECT title FROM items WHERE list = X'{list}' ORDER BY title");
    assert_eq!(
        writer.column(&titles).await?,
        [json!("Flour"), json!("Milk")]
    );
    snap::value(issuer.database.seal()).await?;
    let compacted = snap::value(issuer.database.compact(0)).await?;
    assert!(
        compacted["pruned"].as_u64() >= Some(4),
        "the list's items were not released: {compacted}"
    );
    assert_eq!(
        issuer.column(&titles).await?,
        [json!("Flour"), json!("Milk")]
    );
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn an_issuer_migrates_its_lists_after_compacting() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = root.path().join("a");
    let issuer = Peer::new(
        crate::common::create_as(open(&a, &hub, "a"), key(&a))
            .await?
            .database,
    )?;
    snap::value(issuer.database.next_partition("lists")).await?;
    let list = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Weekly')",
        )
        .await?;
    for title in ["Milk", "Bread"] {
        issuer
            .insert(
                "items",
                Some(&list),
                &format!("INSERT INTO items(id,list,title) VALUES(?,X'{list}','{title}')"),
            )
            .await?;
    }
    snap::value(issuer.database.seal()).await?;
    snap::value(issuer.database.compact(0)).await?;
    issuer.database.close().await?;

    let issuer = Peer::new(at(&a, &hub, "a", &ordered()).open().await?)?;
    assert_eq!(
        issuer
            .column(&format!(
                "SELECT title FROM items WHERE list = X'{list}' ORDER BY position"
            ))
            .await?,
        [json!("Milk"), json!("Bread")]
    );
    issuer.database.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_list_received_from_a_checkpoint_keeps_its_rows_after_reopening() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer = Peer::new(
        crate::common::create_as(
            open(&root.path().join("a"), &hub, "a"),
            key(&root.path().join("a")),
        )
        .await?
        .database,
    )?;
    let invitation = issuer
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;
    let writer = Peer::new(
        open(&root.path().join("b"), &hub, "b")
            .join(&invitation)
            .await?
            .database,
    )?;
    snap::value(issuer.database.next_partition("lists")).await?;
    let list = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Groceries')",
        )
        .await?;
    issuer
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title) VALUES(?,X'{list}','Milk')"),
        )
        .await?;
    writer.sync_now().await?;
    writer
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title) VALUES(?,X'{list}','Eggs')"),
        )
        .await?;
    let titles = format!("SELECT title FROM items WHERE list = X'{list}' ORDER BY title");
    assert_eq!(
        writer.column(&titles).await?,
        [json!("Eggs"), json!("Milk")]
    );

    writer.database.close().await?;
    let reopened = Peer::new(open(&root.path().join("b"), &hub, "b").open().await?)?;
    assert_eq!(
        reopened.column(&titles).await?,
        [json!("Eggs"), json!("Milk")],
        "after reopening, the items' parent list anchors them again"
    );
    issuer.database.close().await?;
    let issuer = Peer::new(open(&root.path().join("a"), &hub, "a").open().await?)?;
    assert_eq!(
        issuer
            .column(&format!("SELECT title FROM items WHERE list = X'{list}'"))
            .await?,
        [json!("Milk")]
    );
    for peer in [reopened, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn an_issuer_does_not_bootstrap_its_own_list_when_another_issuer_shares_it_back()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Peer::new(
        crate::common::create_as(
            open(&root.path().join("a"), &hub, "a"),
            key(&root.path().join("a")),
        )
        .await?
        .database,
    )?;
    snap::value(a.database.next_partition("lists")).await?;
    a.insert(
        "lists",
        None,
        "INSERT INTO lists(id,title) VALUES(?,'Own list')",
    )
    .await?;
    let invitation = a
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;
    let b = Peer::new(
        open(&root.path().join("b"), &hub, "b")
            .join(&invitation)
            .await?
            .database,
    )?;
    b.sync_now().await?;
    assert_eq!(
        b.column("SELECT title FROM lists").await?,
        [json!("Own list")]
    );
    a.sync_now().await?;
    let activity = snap::value(a.database.activity(0)).await?;
    assert!(
        !kinds(&activity).contains(&"partition_bootstrap".to_owned()),
        "{activity}"
    );
    assert!(!root.path().join("a/bootstrap.json").exists());
    assert_eq!(
        snap::value(a.database.status()).await?["data"]["bootstrap_pending"],
        false
    );
    for peer in [b, a] {
        peer.database.close().await?;
    }
    Ok(())
}

async fn sync_each_learning_the_others_tuple(peers: &[&Peer]) -> Result<(), Failure> {
    for peer in peers {
        peer.sync_now().await?;
    }
    Ok(())
}
#[tokio::test]
async fn two_issuers_sync_while_each_holds_a_list_the_other_never_saw() -> Result<(), Failure> {
    let root = support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let created = crate::common::create_as(
        open(&root.path().join("a"), &hub, "a"),
        key(&root.path().join("a")),
    )
    .await?;
    let owner: Arc<dyn rocky_sdk::IdentityProvider> = Arc::new(created.root);
    let a = Peer::new(created.database)?;
    let invitation = a
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;
    let b = Peer::new(
        open(&root.path().join("b"), &hub, "b")
            .join(&invitation)
            .await?
            .database,
    )?;
    a.database
        .make_admin(
            rocky_sdk::PersonId::new(b.database.me().await?.person.key()),
            owner,
        )
        .await?;
    b.database.renew().await?;
    let mut lists = Vec::new();
    for (peer, title) in [(&a, "From a"), (&b, "From b")] {
        snap::value(peer.database.next_partition("lists")).await?;
        let sql = format!("INSERT INTO lists(id,title) VALUES(?,'{title}')");
        lists.push(peer.insert("lists", None, &sql).await?);
    }
    sync_each_learning_the_others_tuple(&[&a, &b]).await?;
    for (from, to) in [(&a, &b), (&b, &a)] {
        from.sync_now().await?;
        to.sync_now().await?;
    }
    for peer in [&a, &b] {
        assert_eq!(
            peer.column("SELECT title FROM lists ORDER BY title")
                .await?,
            [json!("From a"), json!("From b")]
        );
    }
    assert!(
        refused(&a).await?.unwrap_or_default().is_empty(),
        "once both hold both lists, nothing is refused"
    );
    for peer in [a, b] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn a_cut_orders_items_and_links_categories_for_an_enrolled_writer() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (a, b) = (root.path().join("a"), root.path().join("b"));
    let issuer = Peer::new(
        crate::common::create_as(open(&a, &hub, "a"), key(&a))
            .await?
            .database,
    )?;
    let invitation = issuer
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;
    let writer = Peer::new(open(&b, &hub, "b").join(&invitation).await?.database)?;
    snap::value(issuer.database.next_partition("lists")).await?;
    let list = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Weekly')",
        )
        .await?;
    let dairy = issuer
        .insert(
            "categories",
            Some(&list),
            &format!("INSERT INTO categories(id,list,name) VALUES(?,X'{list}','Dairy')"),
        )
        .await?;
    for (title, category) in [
        ("Milk", format!("X'{dairy}'")),
        ("Bread", "NULL".to_owned()),
    ] {
        issuer
            .insert(
                "items",
                Some(&list),
                &format!("INSERT INTO items(id,list,title,category) VALUES(?,X'{list}','{title}',{category})"),
            )
            .await?;
    }
    writer.sync_now().await?;
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }

    let issuer = Peer::new(at(&a, &hub, "a", &ordered()).open().await?)?;
    assert_eq!(
        issuer
            .column(&format!(
                "SELECT title FROM items WHERE list = X'{list}' ORDER BY position"
            ))
            .await?,
        [json!("Milk"), json!("Bread")]
    );
    let linked = format!(
        "SELECT i.title FROM item_categories c JOIN items i ON i.list = c.list AND i.id = c.item WHERE c.list = X'{list}' AND i.list = X'{list}' AND c.category = X'{dairy}' ORDER BY i.title"
    );
    assert_eq!(issuer.column(&linked).await?, [json!("Milk")]);
    let older_application = Peer::new(at(&b, &hub, "b", &first()).open().await?)?;
    older_application.sync_now().await?;
    older_application
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title) VALUES(?,X'{list}','Jam')"),
        )
        .await?;
    older_application.sync_now().await?;
    assert_eq!(
        issuer
            .column(&format!(
                "SELECT title FROM items WHERE list = X'{list}' ORDER BY title"
            ))
            .await?,
        [json!("Bread"), json!("Jam"), json!("Milk")],
        "an older application follows the migration and its item reaches the issuer"
    );
    older_application.database.close().await?;
    let writer = Peer::new(at(&b, &hub, "b", &ordered()).open().await?)?;
    writer.sync_now().await?;
    assert_eq!(writer.column(&linked).await?, [json!("Milk")]);
    let bread = writer
        .column(&format!(
            "SELECT id FROM items WHERE list = X'{list}' AND title = 'Bread'"
        ))
        .await?;
    let bread = bread.first().and_then(Value::as_str).ok_or("bread")?;
    writer
        .execute(&format!(
            "INSERT INTO item_categories(list,item,category) VALUES(X'{list}',X'{bread}',X'{dairy}')"
        ))
        .await?;
    writer.sync_now().await?;
    assert_eq!(
        issuer.column(&linked).await?,
        [json!("Bread"), json!("Milk")],
        "grants that never named item_categories follow items"
    );
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn a_full_writer_on_an_older_application_writes_after_the_migration() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (a, b) = (root.path().join("a"), root.path().join("b"));
    let issuer = Peer::new(
        crate::common::create_as(open(&a, &hub, "a"), key(&a))
            .await?
            .database,
    )?;
    let invitation = issuer
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;
    let writer = Peer::new(open(&b, &hub, "b").join(&invitation).await?.database)?;
    snap::value(issuer.database.next_partition("lists")).await?;
    let list = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Weekly')",
        )
        .await?;
    let dairy = issuer
        .insert(
            "categories",
            Some(&list),
            &format!("INSERT INTO categories(id,list,name) VALUES(?,X'{list}','Dairy')"),
        )
        .await?;
    for (title, category) in [
        ("Milk", format!("X'{dairy}'")),
        ("Bread", "NULL".to_owned()),
    ] {
        issuer
            .insert(
                "items",
                Some(&list),
                &format!("INSERT INTO items(id,list,title,category) VALUES(?,X'{list}','{title}',{category})"),
            )
            .await?;
    }
    writer.sync_now().await?;
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }

    let issuer = Peer::new(at(&a, &hub, "a", &ordered()).open().await?)?;
    assert_eq!(
        issuer
            .column(&format!(
                "SELECT title FROM items WHERE list = X'{list}' ORDER BY position"
            ))
            .await?,
        [json!("Milk"), json!("Bread")]
    );
    let linked = format!(
        "SELECT i.title FROM item_categories c JOIN items i ON i.list = c.list AND i.id = c.item WHERE c.list = X'{list}' AND i.list = X'{list}' AND c.category = X'{dairy}' ORDER BY i.title"
    );
    assert_eq!(issuer.column(&linked).await?, [json!("Milk")]);
    let older_application = Peer::new(at(&b, &hub, "b", &first()).open().await?)?;
    older_application.sync_now().await?;
    older_application
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title) VALUES(?,X'{list}','Jam')"),
        )
        .await?;
    older_application.sync_now().await?;
    assert_eq!(
        issuer
            .column(&format!(
                "SELECT title FROM items WHERE list = X'{list}' ORDER BY title"
            ))
            .await?,
        [json!("Bread"), json!("Jam"), json!("Milk")],
        "an older application follows the migration and its item reaches the issuer"
    );
    older_application.database.close().await?;
    let writer = Peer::new(at(&b, &hub, "b", &ordered()).open().await?)?;
    writer.sync_now().await?;
    assert_eq!(writer.column(&linked).await?, [json!("Milk")]);
    let bread = writer
        .column(&format!(
            "SELECT id FROM items WHERE list = X'{list}' AND title = 'Bread'"
        ))
        .await?;
    let bread = bread.first().and_then(Value::as_str).ok_or("bread")?;
    writer
        .execute(&format!(
            "INSERT INTO item_categories(list,item,category) VALUES(X'{list}',X'{bread}',X'{dairy}')"
        ))
        .await?;
    writer.sync_now().await?;
    assert_eq!(
        issuer.column(&linked).await?,
        [json!("Bread"), json!("Milk")],
        "grants that never named item_categories follow items"
    );
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn an_invited_writer_without_identity_writes_after_the_migration() -> Result<(), Failure> {
    let positioned =
        build(lists(2, referencing_items().column(position())).migration(2, [POSITION_BY_ID]));
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (a, b) = (root.path().join("a"), root.path().join("b"));
    let issuer = Peer::new(
        crate::common::create_as(open(&a, &hub, "a"), key(&a))
            .await?
            .database,
    )?;
    let invitation = issuer
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;
    let writer = Peer::new(
        at_without_identity(&b, &hub, "b", &first())
            .join(&invitation)
            .await?
            .database,
    )?;
    snap::value(issuer.database.next_partition("lists")).await?;
    let list = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Weekly')",
        )
        .await?;
    let dairy = issuer
        .insert(
            "categories",
            Some(&list),
            &format!("INSERT INTO categories(id,list,name) VALUES(?,X'{list}','Dairy')"),
        )
        .await?;
    for (title, category) in [
        ("Milk", format!("X'{dairy}'")),
        ("Bread", "NULL".to_owned()),
    ] {
        issuer
            .insert(
                "items",
                Some(&list),
                &format!("INSERT INTO items(id,list,title,category) VALUES(?,X'{list}','{title}',{category})"),
            )
            .await?;
    }
    writer.sync_now().await?;
    writer
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title) VALUES(?,X'{list}','Butter')"),
        )
        .await?;
    writer.sync_now().await?;
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }

    let issuer = Peer::new(at(&a, &hub, "a", &positioned).open().await?)?;

    let older_application = Peer::new(at_without_identity(&b, &hub, "b", &first()).open().await?)?;
    older_application.sync_now().await?;
    older_application.database.renew().await?;
    older_application
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title) VALUES(?,X'{list}','Jam')"),
        )
        .await?;
    older_application.sync_now().await?;
    assert_eq!(
        issuer
            .column(&format!(
                "SELECT title FROM items WHERE list = X'{list}' ORDER BY title"
            ))
            .await?,
        [json!("Bread"), json!("Butter"), json!("Jam"), json!("Milk")],
        "an invited writer without identity on an older application writes after the migration and renewal"
    );
    older_application.database.close().await?;

    let writer = Peer::new(
        at_without_identity(&b, &hub, "b", &positioned)
            .open()
            .await?,
    )?;
    writer.sync_now().await?;
    writer.database.renew().await?;
    writer
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title,position) VALUES(?,X'{list}','Eggs','9')"),
        )
        .await?;
    writer.sync_now().await?;
    assert_eq!(
        issuer
            .column(&format!(
                "SELECT title FROM items WHERE list = X'{list}' ORDER BY title"
            ))
            .await?,
        [
            json!("Bread"),
            json!("Butter"),
            json!("Eggs"),
            json!("Jam"),
            json!("Milk")
        ],
        "an invited writer without identity writes after upgrading and renewing"
    );
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn a_migration_update_over_more_than_max_bundle_rows_applies_and_replicates()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let (a, b) = (root.path().join("a"), root.path().join("b"));
    let issuer = Peer::new(
        crate::common::create_as(open(&a, &hub, "a"), key(&a))
            .await?
            .database,
    )?;
    let invitation = issuer
        .database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;
    let member = Peer::new(open(&b, &hub, "b").join(&invitation).await?.database)?;
    snap::value(issuer.database.next_partition("lists")).await?;
    let list = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Bulk')",
        )
        .await?;
    let total = 2_053usize;
    let list_bytes = data_encoding::HEXLOWER_PERMISSIVE.decode(list.as_bytes())?;
    for chunk_start in (0..total).step_by(200) {
        let chunk_end = (chunk_start + 200).min(total);
        let mut transaction = issuer.database.transaction().await?;
        for index in chunk_start..chunk_end {
            let id = transaction
                .next_id("items", rocky_sdk::params![list_bytes.clone()])
                .await?;
            transaction
                .execute(
                    "INSERT INTO items(id,list,title) VALUES(?,?,?)",
                    rocky_sdk::params![id, list_bytes.clone(), format!("Item {index:04}")],
                )
                .await?;
        }
        transaction.commit().await?;
    }
    member.sync_now().await?;
    for peer in [member, issuer] {
        peer.database.close().await?;
    }

    let issuer = Peer::new(at(&a, &hub, "a", &ordered()).open().await?)?;
    let check_sql = format!(
        "SELECT COUNT(*) || ':' || COUNT(DISTINCT position) FROM items WHERE list = X'{list}' AND position > ''"
    );
    let expected_summary = [json!(format!("{total}:{total}"))];
    assert_eq!(
        issuer.column(&check_sql).await?,
        expected_summary,
        "migration UPDATE over more than 2048 rows must assign distinct non-empty positions on issuer"
    );
    let all_rows_sql = format!(
        "SELECT title || ':' || position FROM items WHERE list = X'{list}' ORDER BY position"
    );
    let issuer_rows = issuer.column(&all_rows_sql).await?;
    assert_eq!(issuer_rows.len(), total);

    let member = Peer::new(
        at_without_identity(&b, &hub, "b", &ordered())
            .open()
            .await?,
    )?;
    member.sync_now().await?;
    assert_eq!(
        member.column(&check_sql).await?,
        expected_summary,
        "migration UPDATE over more than 2048 rows must replicate distinct non-empty positions to member"
    );
    assert_eq!(
        member.column(&all_rows_sql).await?,
        issuer_rows,
        "member (title, position) pairs must match issuer across all 2053 rows"
    );
    for peer in [member, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}
