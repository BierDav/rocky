use crate::common;
use crate::support::snapshot as snap;
use common::{Device, Failure, SCHEMA, introduce};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

const SECOND: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text","not_null":false},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"tags","granted_like":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
const GRANTED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text","not_null":false},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"tags","granted_like":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
const REQUIRED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}],"migrations":[{"version":2,"statements":["UPDATE items SET position = printf('%04d', (SELECT count(*) FROM items AS earlier WHERE earlier.title < items.title))"]}]}"#;
const NULLABLE: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":false},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
const BARE: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
const UNIQUE: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"],"indexes":[{"name":"items_title","unique":true,"columns":[{"name":"title","descending":false}]}]}]}"#;
const UNIQUE_REQUIRED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"position","type":"text","not_null":true,"default":""},{"name":"title","type":"text","not_null":true}],"primary_key":["key"],"indexes":[{"name":"items_title","unique":true,"columns":[{"name":"title","descending":false}]}]}],"migrations":[{"version":2,"statements":["UPDATE items SET position = upper(title)"]}]}"#;
const THIRD: &str = r#"{"application":"shopping","version":3,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text","not_null":false},{"name":"rank","type":"integer","not_null":false},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"tags","granted_like":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

fn reopen(options: Open<'_>) -> Result<Device, Failure> {
    let signer = common::external_identity(options.directory);
    Device::new(Database::open(options)?).map(|device| device.with_identity(signer))
}

fn open<'a>(directory: &'a Path, schema: &'a str, network: Option<Network>) -> Open<'a> {
    Open {
        identity: Some(common::external_identity(directory)),
        directory,
        schema: crate::support::snapshot::schema(schema),
        secrets: None,
        network,
    }
}
async fn generation(device: &Device) -> Value {
    snap::ok(device.session.generation()).await
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
    snap::ok(device.session.tip_status()).await["tip"].clone()
}
async fn tag(device: &Device, handle: u8, label: &str) {
    snap::ok(snap::mutate(
        &device.database,
        &device.session,
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
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    b.sync().await.expect("initial idle sync");
    let before = tip(&b).await;
    a.close().await;
    let a = reopen(open(&a_dir, SECOND, network("a")))?;
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
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    let apples = b.add(1, "apples").await;
    b.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples"]);
    let unsent_when_issuer_publishes = "bread";
    b.add(2, unsent_when_issuer_publishes).await;
    a.close().await;

    let a = reopen(open(&a_dir, SECOND, network("a")))?;
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
    let ahead = reopen(open(&b_dir, SECOND, None))?;
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
    assert_eq!(
        ahead.titles().await,
        ["apples", unsent_when_issuer_publishes]
    );
    ahead.close().await;

    let b = reopen(open(&b_dir, SECOND, network("b")))?;
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
    let b = reopen(open(&b_dir, SECOND, network("b")))?;
    assert_eq!(generation(&b).await["application"], "current");
    assert_eq!(
        rows(&b, "SELECT label FROM tags").await,
        json!([[snap::cell(text("fruit"))]]),
        "a profile naming `items` reads `tags`, which is granted like `items`"
    );
    b.close().await;

    a.close().await;
    let retyped = RETYPED.replace(r#""version":2"#, r#""version":3"#);
    let refused = Database::open(open(&a_dir, &retyped, network("a")));
    let error = refused.err().expect("refused").to_string();
    assert!(error.contains("changed type"), "{error}");
    let a = reopen(open(&a_dir, SECOND, network("a")))?;
    assert_eq!(
        generation(&a).await["current"]["schema"],
        2,
        "a change no seal can carry is refused before anything changes"
    );
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_table_granted_like_another_reaches_existing_members() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a"))?;
    let raw_grants_naming_items_alone = json!({"kind": "grants", "grants": [{"scope": "items", "rights": ["read", "insert", "update", "delete"]}]});
    let invitation = a.invite_as(raw_grants_naming_items_alone).await;
    let b = Device::join(&b_dir, &invitation, Network::Owned(hub.connector("b")))?;
    introduce(&a, &b).await;
    b.close().await;
    a.close().await;

    let a = reopen(open(&a_dir, GRANTED, network("a")))?;
    let b = reopen(open(&b_dir, GRANTED, network("b")))?;
    let before = chain(&b_dir)?;
    b.sync().await.expect("sync");
    assert_ne!(
        chain(&b_dir)?,
        before,
        "the sync after adopting asks the issuer for the grant of the table that follows `items`"
    );
    snap::ok(snap::mutate(
        &b.database,
        &b.session,
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

    let ungranted = GRANTED
        .replace(r#""version":2"#, r#""version":3"#)
        .replace(r#""granted_like":"items","#, "");
    let error = Database::open(open(&a_dir, &ungranted, network("a")))
        .err()
        .expect("refused")
        .to_string();
    assert!(
        error.contains("changed whom it is granted like"),
        "changing whom a table is granted like would change what earlier grants mean: {error}"
    );
    let a = reopen(open(&a_dir, GRANTED, network("a")))?;
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
    let a = Device::create(&dirs("a"), network("a"))?;
    let b = Device::join(
        &dirs("b"),
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    let c = Device::join(
        &dirs("c"),
        &a.invite().await,
        Network::Owned(hub.connector("c")),
    )?;
    introduce(&a, &b).await;
    introduce(&a, &c).await;
    a.add(1, "cherries").await;
    a.add(2, "apples").await;
    b.add(3, "bread").await;
    b.sync().await.expect("sync");
    c.sync().await.expect("sync");
    b.close().await;
    a.close().await;

    let a = reopen(open(&dirs("a"), REQUIRED, network("a")))?;
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
        snap::ok(c.session.held_changes()).await["count"],
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

    let b = reopen(open(&dirs("b"), REQUIRED, network("b")))?;
    b.sync().await.expect("sync");
    assert_eq!(generation(&b).await["application"], "current");
    snap::ok(snap::mutate(
        &b.database,
        &b.session,
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
    let a = Device::new(Database::create(
        open(&dirs("a"), NULLABLE, network("a")),
        None,
    )?)?;
    let b = Device::new(Database::join(
        open(&dirs("b"), NULLABLE, network("b")),
        &a.invite().await,
    )?)?;
    let c = Device::new(Database::join(
        open(&dirs("c"), NULLABLE, network("c")),
        &a.invite().await,
    )?)?;
    introduce(&a, &b).await;
    introduce(&a, &c).await;
    a.add(1, "apples").await;
    b.sync().await.expect("sync");
    c.sync().await.expect("sync");
    b.add(2, "bread").await;
    b.sync().await.expect("sync");
    c.sync().await.expect("sync");
    a.close().await;

    let a = reopen(open(&dirs("a"), BARE, network("a")))?;
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
    let b = reopen(open(&dirs("b"), BARE, network("b")))?;
    let c = reopen(open(&dirs("c"), BARE, network("c")))?;
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
    let a = Device::new(Database::create(open(root.path(), UNIQUE, None), None)?)?;
    a.add(1, "apples").await;
    a.add(2, "bread").await;
    a.close().await;
    let a = reopen(open(root.path(), UNIQUE_REQUIRED, None))?;
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
        &a.session,
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
        &a.session,
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
    let a = Device::create(root.path(), None)?;
    a.add(1, "apples").await;
    a.close().await;
    let bumped = SCHEMA.replace(r#""version":1"#, r#""version":7"#);
    let bumped: &'static str = Box::leak(bumped.into_boxed_str());
    let a = reopen(open(root.path(), bumped, None))?;
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

const RETYPED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"integer","not_null":true}],"primary_key":["key"]}]}"#;
const RENAMED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"label","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_name_keeps_its_type_and_a_rename_is_another_field() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::create(root.path(), None)?;
    a.add(1, "apples").await;
    a.close().await;
    for (schema, refusal) in [(RETYPED, "changed type"), (RENAMED, "needs a default")] {
        let error = Database::open(open(root.path(), schema, None))
            .err()
            .expect("refused")
            .to_string();
        assert!(
            error.contains(refusal),
            "a retyped name is refused, and a rename adds a required field that needs a default: {error}"
        );
    }
    let a = reopen(open(root.path(), SCHEMA, None))?;
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
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    let apples = b.add(1, "apples").await;
    b.sync().await.expect("sync");
    a.close().await;
    reopen(open(&a_dir, SECOND, None))?.close().await;
    let a = reopen(open(&a_dir, THIRD, network("a")))?;
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
    let b = reopen(open(&b_dir, SCHEMA, network("b")))?;
    assert_eq!(generation(&b).await["current"]["schema"], 3);
    assert_eq!(b.titles().await, ["apples", "bread"]);
    b.close().await;
    a.close().await;
    Ok(())
}

const NOTED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"note","type":"text","not_null":false},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;
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
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    b.close().await;
    a.close().await;
    let a = reopen(open(&a_dir, NOTED, network("a")))?;
    let target = (tip(&a).await, generation(&a).await["manifest"].clone());
    let before_meeting_published_schema = root.path().join("saved");
    std::fs::create_dir(&before_meeting_published_schema)?;
    copy_replica(&b_dir, &before_meeting_published_schema)?;
    let identity = common::authority_bytes(&b_dir, "identity")?.ok_or("identity")?;
    let b = reopen(open(&b_dir, NOTED, network("b")))?;
    b.sync().await.expect("sync");
    assert_eq!(generation(&b).await["current"]["schema"], 2);
    b.close().await;
    let switched = root.path().join("switched");
    std::fs::create_dir(&switched)?;
    copy_replica(&b_dir, &switched)?;

    stop_after_saving_switch_intent(&b_dir, &identity, &target).await?;
    let b = reopen(open(&b_dir, SCHEMA, None))?;
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
    let b = reopen(open(&b_dir, SCHEMA, network("b")))?;
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
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    b.close().await;
    a.close().await;
    let a = reopen(open(&a_dir, NOTED, network("a")))?;
    let target = (tip(&a).await, generation(&a).await["manifest"].clone());
    let identity = common::authority_bytes(&b_dir, "identity")?.ok_or("identity")?;
    let b = reopen(open(&b_dir, NOTED, network("b")))?;
    b.sync().await.expect("sync");
    assert_eq!(tip(&b).await, target.0);
    snap::ok(a.session.seal()).await;
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
    let b = reopen(open(&b_dir, SCHEMA, None))?;
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
    let a = Device::create(&a_dir, network("a"))?;
    let apples = a.add(1, "apples").await;
    a.close().await;
    let a = reopen(open(&a_dir, SECOND, network("a")))?;
    a.set(2, &apples, "note", json!("fresh")).await;
    let b = Device::new(Database::join(
        open(&b_dir, SCHEMA, network("b")),
        &a.invite().await,
    )?)?;
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
    let b = reopen(open(&b_dir, SCHEMA, network("b")))?;
    assert_eq!(generation(&b).await["current"]["schema"], 2);
    b.close().await;
    a.close().await;
    Ok(())
}

fn chain(directory: &Path) -> Result<Value, Failure> {
    let identity = common::authority_json(directory, "identity")?;
    Ok(identity["credentials"].clone())
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
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::join(
        &b_dir,
        &a.invite_as(json!({"kind": "issuer"})).await,
        Network::Owned(hub.connector("b")),
    )?;
    let c = Device::join(
        &c_dir,
        &a.invite().await,
        Network::Owned(hub.connector("c")),
    )?;
    introduce(&a, &b).await;
    introduce(&a, &c).await;
    introduce(&b, &c).await;
    let principal = snap::ok(b.session.info()).await["principal"].clone();
    let trusted = snap::ok(
        a.session
            .trust(snap::public_key(&(principal)), (a.root()).clone()),
    )
    .await;
    snap::ok(b.session.install_trust(&(snap::text(&(trusted["grant"]))))).await;
    let apples = c.add(1, "apples").await;
    c.sync().await.expect("sync");
    b.sync().await.expect("sync");
    a.close().await;
    let a = reopen(open(&a_dir, SECOND, network("a")))?;
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
    let c = reopen(open(&c_dir, SECOND, network("c")))?;
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
    let a = Device::create(&a_dir, network("a"))?;
    let invitation = a.invite().await;
    a.add(1, "apples").await;
    a.close().await;
    let a = reopen(open(&a_dir, SECOND, network("a")))?;
    assert_eq!(generation(&a).await["current"]["schema"], 2);
    let b = Device::new(Database::join(
        open(&b_dir, SECOND, network("b")),
        &invitation,
    )?)?;
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
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::new(Database::join(
        open(&b_dir, SCHEMA, network("b")),
        &a.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    introduce(&a, &b).await;
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    a.close().await;
    b.close().await;
    let a = reopen(open(&a_dir, SECOND, None))?;
    let b = reopen(open(&b_dir, SECOND, None))?;
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
    let a = reopen(open(&a_dir, SECOND, network("a")))?;
    let b = reopen(open(&b_dir, SECOND, network("b")))?;
    a.add(2, "bread").await;
    tag(&a, 5, "fruit").await;
    tag(&b, 5, "dairy").await;
    b.sync().await.expect("sync");
    a.sync().await.expect("sync");
    assert_eq!(
        tip(&a).await,
        tip(&b).await,
        "the owner's seal wins: both devices end on its tip"
    );
    let inspected = snap::ok(b.session.inspect_tips()).await;
    let losers = inspected["bookmarks"][0]["losers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(losers.len(), 1, "{inspected}");
    assert_eq!(
        losers[0]["name"], "main/1",
        "the losing issuer's tip stays a branch"
    );
    for device in [&a, &b] {
        assert_eq!(
            rows(device, "SELECT label FROM tags ORDER BY label").await,
            json!([[snap::cell(text("fruit"))]]),
            "an unsent write on the losing tip goes with it: an intent has no rows to re-author"
        );
        assert_eq!(device.titles().await, ["apples", "bread"]);
        let status = generation(device).await;
        assert_eq!(status["manifest"], left["manifest"]);
        assert_eq!(status["current"]["schema"], 2);
    }
    assert_eq!(snap::ok(b.session.held_changes()).await["count"], 0);
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
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::join(
        &b_dir,
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    a.add(1, "cherries").await;
    a.add(2, "apples").await;
    b.sync().await.expect("sync");
    a.close().await;
    let a = reopen(open(&a_dir, REQUIRED, None))?;
    assert_eq!(
        generation(&a).await["current"]["schema"],
        2,
        "an offline issuer closes `main`, opens the migration's tip and migrates"
    );
    let written_past_the_closing_frontier = "bread";
    b.add(3, written_past_the_closing_frontier).await;
    a.close().await;
    let a = reopen(open(&a_dir, REQUIRED, network("a")))?;
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
    let a = Device::create(&directory, None)?;
    a.add(1, "cherries").await;
    a.add(2, "apples").await;
    snap::ok(a.session.seal()).await;
    snap::ok(a.session.compact(0)).await;
    a.close().await;
    let a = reopen(open(&directory, REQUIRED, None))?;
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

const RIVAL: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"rank","type":"integer","not_null":false},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn issuers_that_publish_different_schemas_apart_end_on_the_winners() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::create(&a_dir, network("a"))?;
    let b = Device::new(Database::join(
        open(&b_dir, SCHEMA, network("b")),
        &a.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    introduce(&a, &b).await;
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    a.close().await;
    b.close().await;
    let a = reopen(open(&a_dir, SECOND, None))?;
    let b = reopen(open(&b_dir, RIVAL, None))?;
    let winner = generation(&a).await["manifest"].clone();
    assert_ne!(winner, generation(&b).await["manifest"]);
    a.close().await;
    b.close().await;
    let a = reopen(open(&a_dir, SECOND, network("a")))?;
    let b = reopen(open(&b_dir, RIVAL, network("b")))?;
    introduce(&a, &b).await;
    for _ in 0..4 {
        b.sync().await.expect("sync");
        a.sync().await.expect("sync");
    }
    let status = generation(&b).await;
    assert_eq!(
        tip(&a).await,
        tip(&b).await,
        "the owner's tip wins: {status}"
    );
    assert_eq!(
        status["manifest"], winner,
        "the losing issuer's database follows the owner's schema: {status}"
    );
    assert_eq!(status["pending"], Value::Null, "{status}");
    assert_eq!(b.titles().await, ["apples"]);
    b.close().await;
    a.close().await;
    Ok(())
}

const LINKABLE: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"list","type":"blob"},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"lists","class":"replayed","columns":[{"name":"name","type":"text","not_null":true}],"primary_key":["id"]}]}"#;
const LINKED: &str = r#"{"application":"shopping","version":2,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"list","type":"blob"},{"name":"title","type":"text","not_null":true}],"primary_key":["key"],"foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"set_null"}]},{"name":"lists","class":"replayed","columns":[{"name":"name","type":"text","not_null":true}],"primary_key":["id"]}]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_rollout_refuses_a_reference_over_a_field_existing_rows_hold() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let a = Device::new(Database::create(open(root.path(), LINKABLE, None), None)?)?;
    a.close().await;
    let error = Database::open(open(root.path(), LINKED, None))
        .err()
        .expect("refused")
        .to_string();
    assert!(
        error.contains(
            "the application schema cannot replace the installed one: foreign key items(list) -> lists would constrain existing rows; declare it on a new optional field"
        ),
        "{error}"
    );
    let a = reopen(open(root.path(), LINKABLE, None))?;
    assert_eq!(generation(&a).await["current"]["schema"], 1);
    a.close().await;
    Ok(())
}
