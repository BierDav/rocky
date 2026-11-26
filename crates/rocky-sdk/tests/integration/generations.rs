use crate::common;
use crate::support::snapshot as snap;
use common::{Device, Failure, build, introduce, items, required, shopping};
use rocky_sdk::{
    Affinity, Column, ErrorKind, ForeignKeyName, Index, Network, RefusedChange, Schema,
    SchemaError, Table, transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

fn note() -> Column {
    Column::new("note", Affinity::Text)
}
fn position() -> Column {
    Column::new("position", Affinity::Text)
        .required()
        .default("")
}
fn tags() -> Table {
    Table::replayed("tags")
        .column(required("key", Affinity::Text))
        .column(required("label", Affinity::Text))
        .primary_key(["key"])
}
fn second() -> Schema {
    build(shopping(2).table(items().column(note())).table(tags()))
}
fn required_position() -> Schema {
    build(shopping(2).table(items().column(position())).migration(
        2,
        ["UPDATE items SET position = printf('%04d', (SELECT count(*) FROM items AS earlier WHERE earlier.title < items.title))"],
    ))
}
fn nullable_position() -> Schema {
    build(shopping(1).table(items().column(Column::new("position", Affinity::Text))))
}
fn bare_position() -> Schema {
    build(shopping(2).table(items().column(position())))
}
fn unique_titles() -> Index {
    Index::new("items_title", ["title"]).unique()
}
fn unique() -> Schema {
    build(shopping(1).table(items().index(unique_titles())))
}
fn unique_required() -> Schema {
    build(
        shopping(2)
            .table(items().column(position()).index(unique_titles()))
            .migration(2, ["UPDATE items SET position = upper(title)"]),
    )
}
fn third() -> Schema {
    build(
        shopping(3)
            .table(
                items()
                    .column(note())
                    .column(Column::new("rank", Affinity::Integer)),
            )
            .table(tags()),
    )
}

async fn reopen(
    directory: &Path,
    schema: &Schema,
    network: Option<Network>,
) -> Result<Device, Failure> {
    Device::new(open(directory, schema, network).open().await?)
}

fn open(directory: &Path, schema: &Schema, network: Option<Network>) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        schema,
        None,
        Some(common::external_identity(directory)),
        network,
    )
}
async fn generation(device: &Device) -> Value {
    snap::ok(async {
        device
            .database
            .status()
            .await
            .map(|status| status.data().generation)
    })
    .await
}
async fn rows(device: &Device, sql: &str) -> Value {
    snap::ok(snap::read(
        &device.database,
        &snap::text(&(sql)),
        Vec::new(),
    ))
    .await["rows"]
        .clone()
}
fn text(value: &str) -> rocky_sdk::SqlValue {
    rocky_sdk::SqlValue::Text(snap::text(&(value)))
}

async fn tip(device: &Device) -> Value {
    snap::ok(device.database.tip_status()).await["tip"].clone()
}
async fn tag(device: &Device, handle: u8, label: &str) {
    snap::ok(snap::mutate(
        &device.database,
        vec![handle; 32],
        snap::Action::Create {
            table: "tags".to_owned(),
            values: vec![("label".to_owned(), snap::field(json!(label)))],
        },
        None,
        None,
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_old_member_follows_main_to_a_published_schema_and_reports_behind() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a")).await?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )
    .await?;
    introduce(&a, &b).await;
    b.sync().await.expect("initial idle sync");
    let before = tip(&b).await;
    a.close().await;
    let a = reopen(&a_dir, &second(), network("a")).await?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    let published = tip(&a).await;
    assert_ne!(
        published, before,
        "an issuer declaring the second schema publishes a `continue` seal and moves `main`"
    );
    b.sync().await.expect("follow the published schema");
    let status = generation(&b).await;
    assert_eq!(
        status["current"]["schema"], 2,
        "a member declaring the first schema installs the second by following `main`: {status}"
    );
    assert_eq!(
        status["application"], "behind",
        "a member declaring the first schema reads the second as an older application"
    );
    assert_eq!(status["pending"], Value::Null);
    assert_eq!(tip(&b).await, published);
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    assert_eq!(
        rows(&a, "SELECT title, note FROM items").await,
        json!([[snap::cell(text("apples")), {"type": "null"}]])
    );
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_ahead_waits_for_the_issuer_and_its_unsent_writes_cross_the_seal()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a")).await?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )
    .await?;
    introduce(&a, &b).await;
    let apples = b.add(1, "apples").await;
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples"]);
    let unsent_when_issuer_publishes = "bread";
    b.add(2, unsent_when_issuer_publishes).await;
    a.close().await;

    let a = reopen(&a_dir, &second(), network("a")).await?;
    let status = generation(&a).await;
    assert_eq!(status["current"], json!({"schema": 2}));
    assert_eq!(status["application"], "current");
    assert_eq!(status["rebuilding"], Value::Null);
    assert!(status.get("previous").is_none() && status.get("baseline").is_none());
    assert_eq!(
        rows(&a, "SELECT title, note FROM items").await,
        json!([[snap::cell(text("apples")), {"type": "null"}]])
    );
    a.set(3, &apples, "note", json!("fresh")).await;
    tag(&a, 4, "fruit").await;

    b.close().await;
    let ahead = reopen(&b_dir, &second(), None).await?;
    let status = generation(&ahead).await;
    assert_eq!(
        status["current"]["schema"], 1,
        "a member declaring a newer schema never publishes it"
    );
    assert_eq!(status["application"], "ahead");
    assert_eq!(
        status["waiting_for_issuer"], true,
        "a member ahead waits for its bookmark to reach its schema"
    );
    let refused = ahead
        .database
        .query("SELECT title FROM items", Vec::new())
        .await
        .err()
        .ok_or("a member ahead of the installed schema read its rows")?;
    assert_eq!(
        refused.kind(),
        &ErrorKind::SchemaMismatch {
            installed: 1,
            declared: 2
        },
        "a member ahead reads nothing until an admin publishes its schema: {refused}"
    );
    ahead.close().await;

    let b = reopen(&b_dir, &second(), network("b")).await?;
    b.sync().await.expect("sync");
    let status = generation(&b).await;
    assert_eq!(
        status["current"]["schema"], 2,
        "following `main` switches a member ahead"
    );
    assert_eq!(status["application"], "current");
    assert_eq!(status["waiting_for_issuer"], false);
    b.sync().await.expect("sync");
    for device in [&a, &b] {
        assert_eq!(
            device.titles().await,
            ["apples", unsent_when_issuer_publishes],
            "a write from before the seal is a late frame across a `continue` and applies everywhere"
        );
    }
    assert_eq!(
        rows(&b, "SELECT note FROM items WHERE title='apples'").await,
        json!([[snap::cell(text("fresh"))]])
    );
    b.close().await;
    let b = reopen(&b_dir, &second(), network("b")).await?;
    assert_eq!(generation(&b).await["application"], "current");
    assert_eq!(
        rows(&b, "SELECT label FROM tags").await,
        json!([[snap::cell(text("fruit"))]]),
        "a profile naming `items` reads `tags`, which is granted like `items`"
    );
    b.close().await;

    a.close().await;
    let refused = open(&a_dir, &retyped(3), network("a")).open().await;
    let error = refused.err().expect("refused").to_string();
    assert!(error.contains("changed type"), "{error}");
    let a = reopen(&a_dir, &second(), network("a")).await?;
    assert_eq!(
        generation(&a).await["current"]["schema"],
        2,
        "a change no seal can carry is refused before anything changes"
    );
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_table_added_later_reaches_existing_full_members() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a")).await?;
    let invitation = a.invite().await;
    let b = Device::join(&b_dir, &invitation, Network::Owned(hub.connector("b"))).await?;
    introduce(&a, &b).await;
    b.close().await;
    a.close().await;

    let a = reopen(&a_dir, &second(), network("a")).await?;
    let b = reopen(&b_dir, &second(), network("b")).await?;
    b.sync().await.expect("sync");
    assert!(
        common::authority_json(&b_dir, "identity")?["keys"]["tags"].is_array(),
        "full access covers a table added later"
    );
    snap::ok(snap::mutate(
        &b.database,
        vec![5; 32],
        snap::Action::Create {
            table: "tags".to_owned(),
            values: vec![("label".to_owned(), snap::field(json!("fruit")))],
        },
        None,
        None,
    ))
    .await;
    b.sync().await.expect("sync");
    assert_eq!(
        rows(&a, "SELECT label FROM tags").await,
        json!([[snap::cell(text("fruit"))]])
    );
    b.close().await;
    a.close().await;

    let a = reopen(&a_dir, &second(), network("a")).await?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_old_device_offline_across_a_migration_has_its_late_write_replayed_in_the_successor()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let dirs = |name: &str| root.path().join(name);
    let a = Device::create(&dirs("a"), network("a")).await?;
    let b = Device::join(
        &dirs("b"),
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )
    .await?;
    let c = Device::join(
        &dirs("c"),
        &a.invite().await,
        Network::Owned(hub.connector("c")),
    )
    .await?;
    introduce(&a, &b).await;
    introduce(&a, &c).await;
    a.add(1, "cherries").await;
    a.add(2, "apples").await;
    b.add(3, "bread").await;
    b.sync().await.expect("sync");
    c.sync().await.expect("sync");
    b.close().await;
    a.close().await;

    let a = reopen(&dirs("a"), &required_position(), network("a")).await?;
    let status = generation(&a).await;
    assert_eq!(status["current"]["schema"], 2, "{status}");
    assert_eq!(status["application"], "current");
    let positions = json!([
        [snap::cell(text("apples")), snap::cell(text("0000"))],
        [snap::cell(text("bread")), snap::cell(text("0001"))],
        [snap::cell(text("cherries")), snap::cell(text("0002"))]
    ]);
    assert_eq!(
        rows(&a, "SELECT title, position FROM items ORDER BY position").await,
        positions,
        "the issuer migrates on the new tip it opens after closing `main`"
    );

    c.add(6, "stale").await;
    c.sync().await.expect("sync");
    c.sync().await.expect("sync");
    let status = generation(&c).await;
    assert_eq!(status["current"]["schema"], 2, "{status}");
    assert_eq!(status["application"], "behind");
    assert_eq!(
        snap::ok(c.database.held_changes()).await["count"],
        0,
        "an old application keeps its own state without holding its late write"
    );
    a.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread", "cherries", "stale"]);
    assert_eq!(
        rows(&a, "SELECT position FROM items WHERE title='stale'").await,
        json!([[snap::cell(text(""))]]),
        "an old application's late write replays in the migration's tip under its schema"
    );
    c.close().await;

    let b = reopen(&dirs("b"), &required_position(), network("b")).await?;
    b.sync().await.expect("sync");
    assert_eq!(generation(&b).await["application"], "current");
    snap::ok(snap::mutate(
        &b.database,
        vec![5; 32],
        snap::Action::Create {
            table: "items".to_owned(),
            values: vec![
                ("title".to_owned(), snap::field(json!("dates"))),
                ("completed".to_owned(), snap::field(json!(false))),
                ("position".to_owned(), snap::field(json!("0003"))),
            ],
        },
        None,
        None,
    ))
    .await;
    b.sync().await.expect("sync");
    assert_eq!(
        a.titles().await,
        ["apples", "bread", "cherries", "dates", "stale"],
        "an updated member writes the new tip directly"
    );
    assert_eq!(
        rows(&b, "SELECT count(*) FROM items").await,
        json!([[{"type": "integer", "value": "5"}]]),
        "the migration's rows are written once"
    );
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn requiring_a_nullable_field_converts_retained_nulls_on_every_follower()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let dirs = |name: &str| root.path().join(name);
    let a = Device::new(
        common::create_as(
            open(&dirs("a"), &nullable_position(), network("a")),
            common::external_key(&dirs("a")),
        )
        .await?,
    )?;
    let b = Device::new(
        open(&dirs("b"), &nullable_position(), network("b"))
            .join(&a.invite().await)
            .await?,
    )?;
    let c = Device::new(
        open(&dirs("c"), &nullable_position(), network("c"))
            .join(&a.invite().await)
            .await?,
    )?;
    introduce(&a, &b).await;
    introduce(&a, &c).await;
    a.add(1, "apples").await;
    b.sync().await.expect("sync");
    c.sync().await.expect("sync");
    b.add(2, "bread").await;
    b.sync().await.expect("sync");
    c.sync().await.expect("sync");
    a.close().await;

    let a = reopen(&dirs("a"), &bare_position(), network("a")).await?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    assert_eq!(
        rows(&a, "SELECT title, position FROM items ORDER BY title").await,
        json!([
            [snap::cell(text("apples")), snap::cell(text(""))],
            [snap::cell(text("bread")), snap::cell(text(""))]
        ]),
        "requiring a nullable field rewrites stored NULLs as the default, without a migration"
    );

    assert_eq!(
        rows(&b, "SELECT position FROM items WHERE title='bread'").await,
        json!([[{"type": "null"}]])
    );
    b.close().await;
    c.close().await;
    let b = reopen(&dirs("b"), &bare_position(), network("b")).await?;
    let c = reopen(&dirs("c"), &bare_position(), network("c")).await?;
    b.sync().await.expect("sync");
    c.sync().await.expect("sync");
    for device in [&a, &b, &c] {
        assert_eq!(generation(device).await["current"]["schema"], 2);
        assert_eq!(
            rows(device, "SELECT title, position FROM items ORDER BY title").await,
            json!([
                [snap::cell(text("apples")), snap::cell(text(""))],
                [snap::cell(text("bread")), snap::cell(text(""))]
            ]),
            "a retained generation 1 NULL reads as the default after following to the new schema"
        );
    }
    c.close().await;
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn requiring_a_field_rebuilds_tables_with_unique_indexes() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::new(
        common::create_as(
            open(root.path(), &unique(), None),
            common::external_key(root.path()),
        )
        .await?,
    )?;
    a.add(1, "apples").await;
    a.add(2, "bread").await;
    a.close().await;
    let a = reopen(root.path(), &unique_required(), None).await?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    assert_eq!(
        rows(&a, "SELECT title, position FROM items ORDER BY position").await,
        json!([
            [snap::cell(text("apples")), snap::cell(text("APPLES"))],
            [snap::cell(text("bread")), snap::cell(text("BREAD"))]
        ])
    );
    let duplicate = snap::observe(snap::mutate(
        &a.database,
        vec![3; 32],
        snap::Action::Create {
            table: "items".to_owned(),
            values: vec![
                ("title".to_owned(), snap::field(json!("apples"))),
                ("completed".to_owned(), snap::field(json!(false))),
                ("position".to_owned(), snap::field(json!("Z"))),
            ],
        },
        None,
        None,
    ))
    .await
    .expect_err("unique");
    assert!(
        duplicate["message"]
            .as_str()
            .is_some_and(|message| message.contains("UNIQUE")),
        "the rebuilt index still refuses a local duplicate: {duplicate}"
    );
    snap::ok(snap::mutate(
        &a.database,
        vec![4; 32],
        snap::Action::Create {
            table: "items".to_owned(),
            values: vec![
                ("title".to_owned(), snap::field(json!("cherries"))),
                ("completed".to_owned(), snap::field(json!(false))),
                ("position".to_owned(), snap::field(json!("CHERRIES"))),
            ],
        },
        None,
        None,
    ))
    .await;
    assert_eq!(a.titles().await, ["apples", "bread", "cherries"]);
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_schema_change_rebuilds_tables_and_keeps_the_generation() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None).await?;
    a.add(1, "apples").await;
    a.close().await;
    let bumped = build(shopping(7).table(items()));
    let a = reopen(root.path(), &bumped, None).await?;
    let status = generation(&a).await;
    assert_eq!(
        status["current"],
        json!({"schema": 1}),
        "the application's own version is local: the manifest stays"
    );
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    Ok(())
}

fn untitled_items() -> Table {
    Table::replayed("items")
        .column(required("key", Affinity::Text))
        .column(required("completed", Affinity::Integer))
        .primary_key(["key"])
}
fn retyped(version: u32) -> Schema {
    build(shopping(version).table(untitled_items().column(required("title", Affinity::Integer))))
}
fn renamed() -> Schema {
    build(shopping(2).table(untitled_items().column(required("label", Affinity::Text))))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_keeps_its_type_and_a_rename_is_another_field() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None).await?;
    a.add(1, "apples").await;
    a.close().await;
    for (schema, refusal) in [
        (
            retyped(2),
            RefusedChange::ChangedType {
                table: "items".into(),
                column: "title".into(),
            },
        ),
        (
            renamed(),
            RefusedChange::RequiredWithoutDefault {
                table: "items".into(),
                column: "label".into(),
            },
        ),
    ] {
        let error = open(root.path(), &schema, None)
            .open()
            .await
            .err()
            .ok_or("refused")?;
        assert!(
            matches!(
                error.kind(),
                ErrorKind::Schema(schema) if **schema == SchemaError::Change(refusal.clone())
            ),
            "a retyped name is refused, and a rename adds a required field that needs a default: {error:?}"
        );
    }
    let a = reopen(root.path(), &common::schema(), None).await?;
    assert_eq!(generation(&a).await["current"]["schema"], 1);
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_version_that_changes_or_drops_a_declared_key_fails_to_open() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None).await?;
    a.add(1, "apples").await;
    a.close().await;
    let widened = build(shopping(2).table(items().primary_key(["key", "title"])));
    let generated = build(
        shopping(2).table(
            Table::replayed("items")
                .column(required("key", Affinity::Text))
                .column(required("completed", Affinity::Integer))
                .column(required("title", Affinity::Text)),
        ),
    );
    for schema in [widened, generated] {
        let error = open(root.path(), &schema, None)
            .open()
            .await
            .err()
            .expect("refused")
            .to_string();
        assert!(
            error.contains(
                "the primary key of items changed; a row's key never changes, so declare a new table"
            ),
            "{error}"
        );
    }
    let a = reopen(root.path(), &common::schema(), None).await?;
    assert_eq!(generation(&a).await["current"]["schema"], 1);
    assert_eq!(a.titles().await, ["apples"]);
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_application_follows_two_published_schemas_and_keeps_its_unsent_rows()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a")).await?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )
    .await?;
    introduce(&a, &b).await;
    let apples = b.add(1, "apples").await;
    b.sync().await.expect("sync");
    a.close().await;
    reopen(&a_dir, &second(), None).await?.close().await;
    let a = reopen(&a_dir, &third(), network("a")).await?;
    assert_eq!(generation(&a).await["current"]["schema"], 3);
    a.set(2, &apples, "rank", json!(7)).await;
    let written_offline_before_both_seals = "bread";
    b.add(3, written_offline_before_both_seals).await;

    b.sync().await.expect("sync");
    let status = generation(&b).await;
    assert_eq!(
        status["current"],
        json!({"schema": 3}),
        "following `main` walks both seals at once: {status}"
    );
    assert_eq!(status["rebuilding"], Value::Null);
    assert_eq!(status["application"], "behind");
    b.sync().await.expect("sync");
    for device in [&a, &b] {
        assert_eq!(
            device.titles().await,
            ["apples", written_offline_before_both_seals],
            "a row written before both seals crosses both `continue` seals"
        );
    }
    assert_eq!(
        rows(&b, "SELECT rank FROM items WHERE title='apples'").await,
        json!([[{"type": "integer", "value": "7"}]])
    );
    b.close().await;
    let b = reopen(&b_dir, &common::schema(), network("b")).await?;
    assert_eq!(generation(&b).await["current"]["schema"], 3);
    assert_eq!(b.titles().await, ["apples", "bread"]);
    b.close().await;
    a.close().await;
    Ok(())
}

fn noted() -> Schema {
    build(shopping(2).table(items().column(note())))
}
fn copy_replica(from: &Path, to: &Path) -> Result<(), Failure> {
    for name in ["rocky.sqlite", "rocky.sqlite-wal", "rocky.sqlite-shm"] {
        let _ = std::fs::remove_file(to.join(name));
        if from.join(name).exists() {
            std::fs::copy(from.join(name), to.join(name))?;
        }
    }
    Ok(())
}
fn with_pending(identity: &[u8], tip: &Value, manifest: &Value) -> Result<Vec<u8>, Failure> {
    let bytes = |hex: &Value| -> Result<Value, Failure> {
        Ok(json!(
            data_encoding::HEXLOWER.decode(hex.as_str().ok_or("hex")?.as_bytes())?
        ))
    };
    let mut saved: Value = serde_json::from_slice(identity)?;
    saved["pending"] = json!({"tip": bytes(tip)?, "manifest": bytes(manifest)?});
    Ok(serde_json::to_vec(&saved)?)
}
async fn stop_after_saving_switch_intent(
    directory: &Path,
    identity: &[u8],
    target: &(Value, Value),
) -> Result<(), Failure> {
    common::write_authority(
        directory,
        "identity",
        &with_pending(identity, &target.0, &target.1)?,
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_on_either_side_of_the_switch_resumes_on_the_followed_schema() -> Result<(), Failure>
{
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a")).await?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )
    .await?;
    introduce(&a, &b).await;
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    b.close().await;
    a.close().await;
    let a = reopen(&a_dir, &noted(), network("a")).await?;
    let target = (tip(&a).await, generation(&a).await["manifest"].clone());
    let before_meeting_published_schema = root.path().join("saved");
    std::fs::create_dir(&before_meeting_published_schema)?;
    copy_replica(&b_dir, &before_meeting_published_schema)?;
    let identity = common::authority_bytes(&b_dir, "identity")?.ok_or("identity")?;
    let b = reopen(&b_dir, &noted(), network("b")).await?;
    b.sync().await.expect("sync");
    assert_eq!(generation(&b).await["current"]["schema"], 2);
    b.close().await;
    let switched = root.path().join("switched");
    std::fs::create_dir(&switched)?;
    copy_replica(&b_dir, &switched)?;

    stop_after_saving_switch_intent(&b_dir, &identity, &target).await?;
    let b = reopen(&b_dir, &common::schema(), None).await?;
    let status = generation(&b).await;
    assert_eq!(
        status["current"]["schema"], 2,
        "a replica that committed the switch before the stop resumes on the followed schema: {status}"
    );
    assert_eq!(status["application"], "behind");
    assert_eq!(status["pending"], Value::Null);
    assert_eq!(
        rows(&b, "SELECT title, note FROM items").await,
        json!([[snap::cell(text("apples")), {"type": "null"}]])
    );
    b.close().await;

    copy_replica(&before_meeting_published_schema, &b_dir)?;
    stop_after_saving_switch_intent(&b_dir, &identity, &target).await?;
    let b = reopen(&b_dir, &common::schema(), network("b")).await?;
    assert_eq!(
        generation(&b).await["current"]["schema"],
        1,
        "a replica not yet switched waits on its schema"
    );
    b.sync().await.expect("sync");
    let status = generation(&b).await;
    assert_eq!(
        status["current"]["schema"], 2,
        "the next pass switches a replica not yet switched: {status}"
    );
    assert_eq!(status["pending"], Value::Null);
    b.add(2, "bread").await;
    b.sync().await.expect("sync");
    assert_eq!(
        a.titles().await,
        ["apples", "bread"],
        "nothing is lost across an interrupted switch"
    );
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_after_the_switch_and_a_same_schema_advance_reopens_on_the_target_schema()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a")).await?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )
    .await?;
    introduce(&a, &b).await;
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    b.close().await;
    a.close().await;
    let a = reopen(&a_dir, &noted(), network("a")).await?;
    let target = (tip(&a).await, generation(&a).await["manifest"].clone());
    let identity = common::authority_bytes(&b_dir, "identity")?.ok_or("identity")?;
    let b = reopen(&b_dir, &noted(), network("b")).await?;
    b.sync().await.expect("sync");
    assert_eq!(tip(&b).await, target.0);
    snap::ok(a.database.seal()).await;
    let advanced_with_the_same_schema = tip(&a).await;
    assert_ne!(advanced_with_the_same_schema, target.0);
    b.sync().await.expect("sync");
    assert_eq!(
        tip(&b).await,
        advanced_with_the_same_schema,
        "the member follows `main` past the target in the commit of its switch"
    );
    b.close().await;

    stop_after_saving_switch_intent(&b_dir, &identity, &target).await?;
    let b = reopen(&b_dir, &common::schema(), None).await?;
    let status = generation(&b).await;
    assert_eq!(
        status["current"]["schema"], 2,
        "a committed tip continuing from the pending target adopts its schema on reopen: {status}"
    );
    assert_eq!(status["pending"], Value::Null);
    assert_eq!(tip(&b).await, advanced_with_the_same_schema);
    assert_eq!(
        rows(&b, "SELECT title, note FROM items").await,
        json!([[snap::cell(text("apples")), {"type": "null"}]])
    );
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_application_joins_at_the_households_generation() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a")).await?;
    let apples = a.add(1, "apples").await;
    a.close().await;
    let a = reopen(&a_dir, &second(), network("a")).await?;
    a.set(2, &apples, "note", json!("fresh")).await;
    let b = Device::new(
        open(&b_dir, &common::schema(), network("b"))
            .join(&a.invite().await)
            .await?,
    )?;
    introduce(&a, &b).await;
    b.sync().await.expect("sync");
    let status = generation(&b).await;
    assert_eq!(
        status["current"]["schema"], 2,
        "a device declaring version 1 joins at the household's generation 2"
    );
    assert_eq!(status["application"], "behind");
    assert_eq!(
        rows(&b, "SELECT title, note FROM items").await,
        json!([[snap::cell(text("apples")), snap::cell(text("fresh"))]])
    );
    b.add(3, "bread").await;
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread"]);
    b.close().await;
    let b = reopen(&b_dir, &common::schema(), network("b")).await?;
    assert_eq!(generation(&b).await["current"]["schema"], 2);
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_fetches_a_required_schema_from_another_member_with_no_issuer_reachable()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir, c_dir) = (
        root.path().join("a"),
        root.path().join("b"),
        root.path().join("c"),
    );
    let a = Device::create(&a_dir, network("a")).await?;
    let b = Device::join(
        &b_dir,
        &a.invite_as(rocky_sdk::Access::Full).await,
        Network::Owned(hub.connector("b")),
    )
    .await?;
    a.admit(&b).await;
    let c = Device::join(
        &c_dir,
        &a.invite().await,
        Network::Owned(hub.connector("c")),
    )
    .await?;
    introduce(&a, &b).await;
    introduce(&a, &c).await;
    introduce(&b, &c).await;
    let principal = snap::ok(snap::device(&b.database)).await["principal"].clone();
    snap::ok(a.database.make_admin(
        rocky_sdk::PersonId::new(snap::public_key(&(principal))),
        (a.root()).clone(),
    ))
    .await;
    snap::ok(b.database.renew()).await;
    let apples = c.add(1, "apples").await;
    c.sync().await.expect("sync");
    b.sync().await.expect("sync");
    a.close().await;
    let a = reopen(&a_dir, &second(), network("a")).await?;
    a.set(2, &apples, "note", json!("fresh")).await;
    b.sync().await.expect("sync");
    assert_eq!(
        generation(&b).await["current"]["schema"],
        2,
        "the issuer meets only `b` after publishing the second schema"
    );
    assert_eq!(
        rows(&b, "SELECT note FROM items WHERE title='apples'").await,
        json!([[snap::cell(text("fresh"))]])
    );
    a.close().await;

    c.sync().await.expect("sync");
    let status = generation(&c).await;
    assert_eq!(
        status["current"]["schema"], 2,
        "`c` fetches the record its first-schema application requires from `b`: {status}"
    );
    assert_eq!(status["application"], "behind");

    c.close().await;
    let c = reopen(&c_dir, &second(), network("c")).await?;
    assert_eq!(generation(&c).await["application"], "current");
    let bread = c.add(3, "bread").await;
    c.sync().await.expect("sync");
    assert_eq!(b.titles().await, ["apples", "bread"]);
    c.set(4, &bread, "note", json!("rye")).await;
    c.sync().await.expect("sync");
    assert_eq!(
        rows(&b, "SELECT note FROM items WHERE title='bread'").await,
        json!([[snap::cell(text("rye"))]])
    );
    c.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invitation_created_before_a_schema_change_enrolls_a_device_after_it()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a")).await?;
    let invitation = a.invite().await;
    a.add(1, "apples").await;
    a.close().await;
    let a = reopen(&a_dir, &second(), network("a")).await?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    let b = Device::new(
        open(&b_dir, &second(), network("b"))
            .join(&invitation)
            .await?,
    )?;
    introduce(&a, &b).await;
    b.sync().await.expect("sync");
    let status = generation(&b).await;
    assert_eq!(
        status["current"]["schema"], 2,
        "an invitation names tables, not a generation: it enrolls into the one the issuer runs now"
    );
    assert_eq!(status["application"], "current");
    let bread = b.add(2, "bread").await;
    b.set(3, &bread, "note", json!("rye")).await;
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread"]);
    assert_eq!(
        rows(&a, "SELECT note FROM items WHERE title='bread'").await,
        json!([[snap::cell(text("rye"))]])
    );
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn issuers_that_publish_one_schema_apart_end_on_one_tip() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a")).await?;
    let b = Device::new(
        open(&b_dir, &common::schema(), network("b"))
            .join(&a.invite_as(rocky_sdk::Access::Full).await)
            .await?,
    )?;
    a.admit(&b).await;
    introduce(&a, &b).await;
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    a.close().await;
    b.close().await;
    let a = reopen(&a_dir, &second(), None).await?;
    let b = reopen(&b_dir, &second(), None).await?;
    let (left, right) = (generation(&a).await, generation(&b).await);
    assert_eq!(left["manifest"], right["manifest"]);
    assert_ne!(
        tip(&a).await,
        tip(&b).await,
        "each issuer seals `main` with the second schema on its own"
    );
    a.close().await;
    b.close().await;
    let tags = |directory: &Path| -> Result<Value, Failure> {
        let identity = common::authority_json(directory, "identity")?;
        Ok(identity["keys"]["tags"].clone())
    };
    assert_eq!(
        tags(&a_dir)?,
        tags(&b_dir)?,
        "both issuers derive the first reader key of `tags` from the publisher key"
    );
    assert_eq!(tags(&a_dir)?.as_array().map(Vec::len), Some(1));
    let a = reopen(&a_dir, &second(), network("a")).await?;
    let b = reopen(&b_dir, &second(), network("b")).await?;
    a.add(2, "bread").await;
    tag(&a, 5, "fruit").await;
    tag(&b, 5, "dairy").await;
    b.sync().await.expect("sync");
    a.sync().await.expect("sync");
    assert_eq!(
        tip(&a).await,
        tip(&b).await,
        "equally trusted issuers end on the one tip the seal order picks"
    );
    let inspected = snap::ok(b.database.inspect_tips()).await;
    let losers = inspected["bookmarks"][0]["losers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(losers.len(), 1, "{inspected}");
    assert_eq!(
        losers[0]["name"], "main/1",
        "the losing issuer's tip stays a branch"
    );
    let survivor = rows(&a, "SELECT label FROM tags ORDER BY label").await;
    assert!(
        [
            json!([[snap::cell(text("fruit"))]]),
            json!([[snap::cell(text("dairy"))]])
        ]
        .contains(&survivor),
        "an unsent write on the losing tip leaves with it: {survivor}"
    );
    let a_won = survivor == json!([[snap::cell(text("fruit"))]]);
    let titles: &[&str] = if a_won {
        &["apples", "bread"]
    } else {
        &["apples"]
    };
    for device in [&a, &b] {
        assert_eq!(
            rows(device, "SELECT label FROM tags ORDER BY label").await,
            survivor
        );
        assert_eq!(device.titles().await, titles);
        let status = generation(device).await;
        assert_eq!(status["manifest"], left["manifest"]);
        assert_eq!(status["current"]["schema"], 2);
    }
    let unsent_on_the_losing_tip = if a_won { 1 } else { 2 };
    for device in [&a, &b] {
        let held = snap::ok(device.database.held_changes()).await;
        assert_eq!(
            held["count"], unsent_on_the_losing_tip,
            "each unsent write on the losing tip is listed as held: {held}"
        );
    }
    assert_eq!(tags(&a_dir)?, tags(&b_dir)?);
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_issuer_migrating_offline_publishes_on_reconnect_and_meanwhile_writes_replay_after_it()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a")).await?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )
    .await?;
    introduce(&a, &b).await;
    a.add(1, "cherries").await;
    a.add(2, "apples").await;
    b.sync().await.expect("sync");
    a.close().await;
    let a = reopen(&a_dir, &required_position(), None).await?;
    assert_eq!(
        generation(&a).await["current"]["schema"],
        2,
        "an offline issuer closes `main`, opens the migration's tip and migrates"
    );
    let written_past_the_closing_frontier = "bread";
    b.add(3, written_past_the_closing_frontier).await;
    a.close().await;
    let a = reopen(&a_dir, &required_position(), network("a")).await?;
    introduce(&a, &b).await;
    b.sync().await.expect("sync");
    b.sync().await.expect("sync");
    a.sync().await.expect("sync");
    let status = generation(&b).await;
    assert_eq!(
        status["current"]["schema"], 2,
        "the member follows the reconnected issuer to the new schema: {status}"
    );
    assert_eq!(status["held"], 0);
    let positions = json!([
        [
            snap::cell(text(written_past_the_closing_frontier)),
            snap::cell(text(""))
        ],
        [snap::cell(text("apples")), snap::cell(text("0000"))],
        [snap::cell(text("cherries")), snap::cell(text("0001"))]
    ]);
    for device in [&a, &b] {
        assert_eq!(
            rows(
                device,
                "SELECT title, position FROM items ORDER BY position"
            )
            .await,
            positions,
            "a write made meanwhile replays after the migration, under the new schema"
        );
    }
    b.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_compacted_node_publishes_a_migration_adding_a_field() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let directory = root.path().join("a");
    let a = Device::create(&directory, None).await?;
    a.add(1, "cherries").await;
    a.add(2, "apples").await;
    snap::ok(a.database.seal()).await;
    snap::ok(a.database.compact(0)).await;
    a.close().await;
    let a = reopen(&directory, &required_position(), None).await?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    assert_eq!(
        rows(&a, "SELECT title, position FROM items ORDER BY position").await,
        json!([
            [snap::cell(text("apples")), snap::cell(text("0000"))],
            [snap::cell(text("cherries")), snap::cell(text("0001"))]
        ])
    );
    a.close().await;
    Ok(())
}

fn rival() -> Schema {
    build(shopping(2).table(items().column(Column::new("rank", Affinity::Integer))))
}

#[tokio::test(flavor = "multi_thread")]
async fn issuers_that_publish_different_schemas_apart_end_on_one_winner() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a")).await?;
    let b = Device::new(
        open(&b_dir, &common::schema(), network("b"))
            .join(&a.invite_as(rocky_sdk::Access::Full).await)
            .await?,
    )?;
    a.admit(&b).await;
    introduce(&a, &b).await;
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    a.close().await;
    b.close().await;
    let a = reopen(&a_dir, &second(), None).await?;
    let b = reopen(&b_dir, &rival(), None).await?;
    let published = [
        generation(&a).await["manifest"].clone(),
        generation(&b).await["manifest"].clone(),
    ];
    assert_ne!(published[0], published[1]);
    a.close().await;
    b.close().await;
    let a = reopen(&a_dir, &second(), network("a")).await?;
    let b = reopen(&b_dir, &rival(), network("b")).await?;
    introduce(&a, &b).await;
    for _ in 0..4 {
        b.sync().await.expect("sync");
        a.sync().await.expect("sync");
    }
    let status = generation(&b).await;
    assert_eq!(
        tip(&a).await,
        tip(&b).await,
        "equally trusted issuers end on one tip: {status}"
    );
    assert_eq!(
        status["manifest"],
        generation(&a).await["manifest"],
        "the losing issuer's database follows the winner's schema: {status}"
    );
    assert!(published.contains(&status["manifest"]), "{status}");
    for device in [&a, &b] {
        assert_eq!(generation(device).await["pending"], Value::Null);
    }
    assert_eq!(b.titles().await, ["apples"]);
    b.close().await;
    a.close().await;
    Ok(())
}

fn linking(version: u32, items: Table) -> Schema {
    build(
        shopping(version)
            .table(items)
            .table(Table::replayed("lists").column(required("name", Affinity::Text))),
    )
}
fn list_items() -> Table {
    items().column(Column::new("list", Affinity::Blob))
}
fn linkable() -> Schema {
    linking(1, list_items())
}
fn linked() -> Schema {
    linking(
        2,
        list_items().foreign_key(
            rocky_sdk::ForeignKey::new(["list"], "lists")
                .on_delete(rocky_sdk::DeleteAction::SetNull),
        ),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_schema_change_is_a_typed_schema_error() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::new(
        common::create_as(
            open(root.path(), &linkable(), None),
            common::external_key(root.path()),
        )
        .await?,
    )?;
    a.close().await;
    let error = open(root.path(), &linked(), None)
        .open()
        .await
        .err()
        .ok_or("a reference over a field existing rows hold opened")?;
    let expected = SchemaError::Change(RefusedChange::AddedReference(ForeignKeyName {
        table: "items".into(),
        columns: vec!["list".into()],
        parent: "lists".into(),
    }));
    assert!(
        matches!(error.kind(), ErrorKind::Schema(schema) if **schema == expected),
        "a refused change names its rule and foreign key: {error:?}"
    );
    assert!(!error.retryable());
    assert_eq!(
        error.to_string(),
        "the application schema cannot replace the installed one: foreign key items(list) -> lists would constrain existing rows; declare it on a new optional field"
    );
    let a = reopen(root.path(), &linkable(), None).await?;
    assert_eq!(generation(&a).await["current"]["schema"], 1);
    a.close().await;
    Ok(())
}
