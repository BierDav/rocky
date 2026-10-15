//! A list shared with an enrolled writer: one envelope, delivered by sync,
//! opens the list, its categories and items, which then bootstrap from a
//! checkpoint and sync. See `docs/runtime/partitioned-scopes.md#group-secrets`.
use rocky_sdk::{Database, Network, Open, Session, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
mod support;
use support::secrets;

const SCHEMA: &str = r#"{"format":6,"application":"lists","version":1,"tables":[
 {"name":"lists","class":"mergeable","columns":[{"name":"title","type":"text","not_null":true}],
  "primary_key":["id"],"partition_by":["id"],"partition_group":"list"},
 {"name":"categories","class":"mergeable","columns":[{"name":"list","type":"integer","not_null":true},{"name":"name","type":"text","not_null":true}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list",
  "foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"}]},
 {"name":"items","class":"mergeable","columns":[
   {"name":"list","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true},{"name":"category","type":"integer"}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list",
  "indexes":[{"name":"index_items_list_title","unique":true,"columns":[{"name":"list","descending":false},{"name":"title","descending":false}],"on_conflict":"latest_wins"}],
  "foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"},
   {"columns":["list","category"],"table":"categories","on_delete":"set_null"}]}
]}"#;
const TABLES: [&str; 3] = ["lists", "categories", "items"];
/// Version 2: ordered items in any number of categories, migrated from one
/// category per item, as the shopping example's version 7.
const ORDERED: &str = r#"{"format":6,"application":"lists","version":2,"tables":[
 {"name":"lists","class":"mergeable","columns":[{"name":"title","type":"text","not_null":true}],
  "primary_key":["id"],"partition_by":["id"],"partition_group":"list"},
 {"name":"categories","class":"mergeable","columns":[{"name":"list","type":"integer","not_null":true},{"name":"name","type":"text","not_null":true}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list",
  "foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"}]},
 {"name":"items","class":"mergeable","columns":[
   {"name":"list","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true},{"name":"category","type":"integer"},
   {"name":"position","type":"text","not_null":true,"default":""}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list",
  "indexes":[{"name":"index_items_list_title","unique":true,"columns":[{"name":"list","descending":false},{"name":"title","descending":false}],"on_conflict":"latest_wins"}],
  "foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"},
   {"columns":["list","category"],"table":"categories","on_delete":"set_null"}]},
 {"name":"item_categories","class":"mergeable","columns":[
   {"name":"list","type":"integer","not_null":true},{"name":"item","type":"integer","not_null":true},{"name":"category","type":"integer","not_null":true}],
  "primary_key":["list","item","category"],"partition_by":["list"],"partition_group":"list",
  "foreign_keys":[{"columns":["list","item"],"table":"items","on_delete":"cascade"},
   {"columns":["list","category"],"table":"categories","on_delete":"cascade"}]}
],
"granted_like":{"item_categories":"items"},
"migrations":[{"version":2,"statements":[
  "UPDATE items SET position = printf('%09d5', (SELECT count(*) FROM items AS earlier WHERE earlier.list = items.list AND earlier.id < items.id))",
  {"insert":"item_categories","select":"SELECT list, id AS item, category FROM items WHERE category IS NOT NULL"},
  "UPDATE items SET category = NULL WHERE category IS NOT NULL"]}]}"#;

type Failure = Box<dyn std::error::Error + Send + Sync>;

struct Peer {
    database: Arc<Database>,
    session: Session,
    scope: Value,
}
impl Peer {
    fn new(database: Arc<Database>) -> Result<Self, Failure> {
        Ok(Self {
            session: database.session(),
            scope: serde_json::from_str(&database.scope())?,
            database,
        })
    }
    async fn call(&self, request: Value) -> Result<Value, Failure> {
        let reply: Value = serde_json::from_str(
            &self
                .session
                .call(json!({"version":1,"scope":self.scope,"request":request}).to_string())
                .await?,
        )?;
        if reply["status"] == "ok" {
            Ok(reply["value"].clone())
        } else {
            Err(format!("{request}: {}", reply["error"]).into())
        }
    }

    /// Inserts one row in one transaction; `id` reserves the next own id.
    async fn insert(&self, table: &str, list: Option<&str>, sql: &str) -> Result<String, Failure> {
        self.call(json!({"type":"sql","sql":"BEGIN"})).await?;
        let mut next = json!({"type":"next_id","table":table});
        if let Some(list) = list {
            next["partition_values"] = values(list);
        }
        let id = self.call(next).await?["id"].clone();
        self.call(json!({"type":"sql","sql":sql,"values":[{"type":"integer","value":id}]}))
            .await?;
        self.call(json!({"type":"sql","sql":"COMMIT"})).await?;
        Ok(id.as_str().ok_or("id")?.to_owned())
    }
    async fn column(&self, sql: &str) -> Result<Vec<Value>, Failure> {
        let page = self
            .call(json!({"type":"read","query":{"sql":sql,"values":[],"max_rows":64}}))
            .await?;
        Ok(page["rows"]
            .as_array()
            .ok_or("rows")?
            .iter()
            .map(|row| row[0]["value"].clone())
            .collect())
    }
}
fn values(list: &str) -> Value {
    json!([{"type":"integer","value":list}])
}
fn open<'a>(directory: &'a Path, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    at(directory, hub, name, SCHEMA)
}
fn at<'a>(directory: &'a Path, hub: &Arc<Hub>, name: &str, schema: &'a str) -> Open<'a> {
    Open {
        directory,
        schema,
        secrets: Some(secrets::keys(directory)),
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}

#[tokio::test]
async fn an_enrolled_writer_syncs_a_list_shared_with_one_envelope() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer =
        Peer::new(Database::create(open(&root.path().join("a"), &hub, "a"), None)?.database)?;
    let invitation = issuer
        .call(json!({"type":"pairing","profile":{"kind":"writer","scopes":TABLES}}))
        .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let writer =
        Peer::new(Database::join(open(&root.path().join("b"), &hub, "b"), &invitation)?.database)?;

    let prepared = issuer
        .call(json!({"type":"partition_next","table":"lists"}))
        .await?;
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
            &format!("INSERT INTO categories(id,list,name) VALUES(?,{list},'Dairy')"),
        )
        .await?;
    issuer
        .insert(
            "items",
            Some(&list),
            &format!(
                "INSERT INTO items(id,list,title,category) VALUES(?,{list},'Milk',{category})"
            ),
        )
        .await?;

    let node = writer.call(json!({"type":"session"})).await?["node"].clone();
    issuer
        .call(json!({"type":"partition_share","group":"list","values":values(&list),"recipients":[node]}))
        .await?;
    // One sync delivers the envelope; the join's own bootstrap finishes
    // although partitioned tables offer no checkpoint of their own scope,
    // and the list starts from one.
    writer.call(json!({"type":"sync"})).await?;
    assert_eq!(
        writer
            .column(&format!("SELECT title FROM lists WHERE id = {list}"))
            .await?,
        [json!("Groceries")]
    );
    assert_eq!(
        writer
            .column(&format!("SELECT category FROM items WHERE list = {list}"))
            .await?,
        [json!(category)]
    );

    // The writer deletes the category; the issuer's item keeps its list.
    writer
        .call(json!({"type":"sql","sql":format!("DELETE FROM categories WHERE list = {list} AND id = {category}")}))
        .await?;
    writer.call(json!({"type":"sync"})).await?;
    for peer in [&writer, &issuer] {
        assert_eq!(
            peer.column(&format!(
                "SELECT category IS NULL FROM items WHERE list = {list}"
            ))
            .await?,
            [json!("1")]
        );
    }
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}

/// The scopes of the latest `scopes_refused` event, if any.
async fn refused(peer: &Peer) -> Result<Option<Vec<Value>>, Failure> {
    let activity = peer.call(json!({"type":"activity"})).await?;
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
    let issuer =
        Peer::new(Database::create(open(&root.path().join("a"), &hub, "a"), None)?.database)?;
    let invitation = issuer
        .call(json!({"type":"pairing","profile":{"kind":"writer","scopes":TABLES}}))
        .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let writer =
        Peer::new(Database::join(open(&root.path().join("b"), &hub, "b"), &invitation)?.database)?;
    writer.call(json!({"type":"sync"})).await?;
    issuer
        .call(json!({"type":"partition_next","table":"lists"}))
        .await?;
    let list = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Groceries')",
        )
        .await?;
    let node = writer.call(json!({"type":"session"})).await?["node"].clone();
    issuer
        .call(json!({"type":"partition_share","group":"list","values":values(&list),"recipients":[node]}))
        .await?;
    writer.call(json!({"type":"sync"})).await?;
    // Its numbered node credential lets the writer insert into the shared list.
    writer
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title) VALUES(?,{list},'Milk')"),
        )
        .await?;
    writer.call(json!({"type":"sync"})).await?;
    assert_eq!(
        issuer
            .column(&format!("SELECT title FROM items WHERE list = {list}"))
            .await?,
        [json!("Milk")]
    );

    let served = kinds(&issuer.call(json!({"type":"activity"})).await?);
    for kind in [
        "invitation_created",
        "device_enrolled",
        "partition_created",
        "partition_shared",
    ] {
        assert!(
            served.contains(&kind.to_owned()),
            "issuer lacks {kind}: {served:?}"
        );
    }
    let activity = writer.call(json!({"type":"activity"})).await?;
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
    // A caught-up observer waits for the next event.
    let token = activity["token"].clone();
    let waiting = writer.call(json!({"type":"activity_next","after":token}));
    let (next, _) = tokio::join!(
        waiting,
        writer.call(json!({"type":"networking_control","enabled":false}))
    );
    assert_eq!(kinds(&next?), ["networking_disabled"]);
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn a_list_received_from_a_checkpoint_keeps_its_rows_after_reopening() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer =
        Peer::new(Database::create(open(&root.path().join("a"), &hub, "a"), None)?.database)?;
    let invitation = issuer
        .call(json!({"type":"pairing","profile":{"kind":"writer","scopes":TABLES}}))
        .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let writer =
        Peer::new(Database::join(open(&root.path().join("b"), &hub, "b"), &invitation)?.database)?;
    issuer
        .call(json!({"type":"partition_next","table":"lists"}))
        .await?;
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
            &format!("INSERT INTO items(id,list,title) VALUES(?,{list},'Milk')"),
        )
        .await?;
    let node = writer.call(json!({"type":"session"})).await?["node"].clone();
    issuer
        .call(json!({"type":"partition_share","group":"list","values":values(&list),"recipients":[node]}))
        .await?;
    writer.call(json!({"type":"sync"})).await?;
    writer
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title) VALUES(?,{list},'Eggs')"),
        )
        .await?;
    let titles = format!("SELECT title FROM items WHERE list = {list} ORDER BY title");
    assert_eq!(
        writer.column(&titles).await?,
        [json!("Eggs"), json!("Milk")]
    );

    // Reopening installs the group keys after the projection is built; the
    // items' parent list then anchors them again, on both devices.
    let Peer {
        database, session, ..
    } = writer;
    drop(session);
    database.close().await?;
    let reopened = Peer::new(Database::open(open(&root.path().join("b"), &hub, "b"))?.database)?;
    assert_eq!(
        reopened.column(&titles).await?,
        [json!("Eggs"), json!("Milk")]
    );
    let Peer {
        database, session, ..
    } = issuer;
    drop(session);
    database.close().await?;
    let issuer = Peer::new(Database::open(open(&root.path().join("a"), &hub, "a"))?.database)?;
    assert_eq!(
        issuer
            .column(&format!("SELECT title FROM items WHERE list = {list}"))
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
    let a = Peer::new(Database::create(open(&root.path().join("a"), &hub, "a"), None)?.database)?;
    a.call(json!({"type":"partition_next","table":"lists"}))
        .await?;
    let list = a
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Own list')",
        )
        .await?;
    let invitation = a
        .call(json!({"type":"pairing","profile":{"kind":"issuer"}}))
        .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let b =
        Peer::new(Database::join(open(&root.path().join("b"), &hub, "b"), &invitation)?.database)?;
    b.call(json!({"type":"sync"})).await?;
    assert_eq!(
        b.column("SELECT title FROM lists").await?,
        [json!("Own list")]
    );
    let node = a.call(json!({"type":"session"})).await?["node"].clone();
    b.call(
        json!({"type":"partition_share","group":"list","values":values(&list),"recipients":[node]}),
    )
    .await?;
    a.call(json!({"type":"sync"})).await?;
    let activity = a.call(json!({"type":"activity"})).await?;
    assert!(
        !kinds(&activity).contains(&"partition_bootstrap".to_owned()),
        "{activity}"
    );
    assert!(!root.path().join("a/bootstrap.json").exists());
    assert_eq!(
        a.call(json!({"type":"status"})).await?["data"]["bootstrap_pending"],
        false
    );
    for peer in [b, a] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn two_issuers_sync_while_each_holds_a_list_the_other_never_saw() -> Result<(), Failure> {
    let root = support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let a = Peer::new(Database::create(open(&root.path().join("a"), &hub, "a"), None)?.database)?;
    let invitation = a
        .call(json!({"type":"pairing","profile":{"kind":"issuer"}}))
        .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let b =
        Peer::new(Database::join(open(&root.path().join("b"), &hub, "b"), &invitation)?.database)?;
    let mut lists = Vec::new();
    for (peer, title) in [(&a, "From a"), (&b, "From b")] {
        peer.call(json!({"type":"partition_next","table":"lists"}))
            .await?;
        let sql = format!("INSERT INTO lists(id,title) VALUES(?,'{title}')");
        lists.push(peer.insert("lists", None, &sql).await?);
    }
    // The first session may refuse an unknown tuple; the issuer exchange
    // installs it and retries within this sync. The peer learns ours on its sync.
    for peer in [&a, &b] {
        peer.call(json!({"type":"sync"})).await?;
    }
    for (from, to, list) in [(&a, &b, &lists[0]), (&b, &a, &lists[1])] {
        let node = to.call(json!({"type":"session"})).await?["node"].clone();
        from.call(json!({"type":"partition_share","group":"list","values":values(list),"recipients":[node]}))
            .await?;
        // Whichever dials hands over or collects the envelope.
        from.call(json!({"type":"sync"})).await?;
        to.call(json!({"type":"sync"})).await?;
    }
    for peer in [&a, &b] {
        assert_eq!(
            peer.column("SELECT title FROM lists ORDER BY title")
                .await?,
            [json!("From a"), json!("From b")]
        );
    }
    // Once both hold both lists, nothing is refused any more.
    assert!(refused(&a).await?.unwrap_or_default().is_empty());
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
    let issuer = Peer::new(Database::create(open(&a, &hub, "a"), None)?.database)?;
    // Raw grants name the three tables, as the shopping app's device kinds do.
    let grants: Vec<Value> = TABLES
        .iter()
        .map(|table| json!({"scope": table, "rights": ["read", "insert", "update", "delete"]}))
        .collect();
    let invitation = issuer
        .call(json!({"type":"pairing","profile":{"kind":"grants","grants":grants}}))
        .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let writer = Peer::new(Database::join(open(&b, &hub, "b"), &invitation)?.database)?;
    // A partial replica holds only the lists shared with it.
    writer
        .call(
            json!({"type":"partition_role","role":{"role":"Partial","full_nodes":[],"custody":0}}),
        )
        .await?;
    issuer
        .call(json!({"type":"partition_next","table":"lists"}))
        .await?;
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
            &format!("INSERT INTO categories(id,list,name) VALUES(?,{list},'Dairy')"),
        )
        .await?;
    for (title, category) in [("Milk", dairy.as_str()), ("Bread", "NULL")] {
        issuer
            .insert(
                "items",
                Some(&list),
                &format!("INSERT INTO items(id,list,title,category) VALUES(?,{list},'{title}',{category})"),
            )
            .await?;
    }
    let node = writer.call(json!({"type":"session"})).await?["node"].clone();
    issuer
        .call(json!({"type":"partition_share","group":"list","values":values(&list),"recipients":[node]}))
        .await?;
    writer.call(json!({"type":"sync"})).await?;
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }

    let issuer = Peer::new(Database::open(at(&a, &hub, "a", ORDERED))?.database)?;
    assert_eq!(
        issuer
            .column(&format!(
                "SELECT title FROM items WHERE list = {list} ORDER BY position"
            ))
            .await?,
        [json!("Milk"), json!("Bread")]
    );
    let linked = format!(
        "SELECT i.title FROM item_categories c JOIN items i ON i.list = c.list AND i.id = c.item WHERE c.list = {list} AND i.list = {list} AND c.category = {dairy} ORDER BY i.title"
    );
    assert_eq!(issuer.column(&linked).await?, [json!("Milk")]);
    let writer = Peer::new(Database::open(at(&b, &hub, "b", ORDERED))?.database)?;
    writer.call(json!({"type":"sync"})).await?;
    assert_eq!(writer.column(&linked).await?, [json!("Milk")]);
    // The writer's grants never named `item_categories`; they follow `items`.
    let bread = writer
        .column(&format!(
            "SELECT id FROM items WHERE list = {list} AND title = 'Bread'"
        ))
        .await?;
    let bread = bread.first().and_then(Value::as_str).ok_or("bread")?;
    writer
        .insert(
            "item_categories",
            Some(&list),
            &format!("INSERT INTO item_categories(id,list,item,category) VALUES(?,{list},{bread},{dairy})"),
        )
        .await?;
    writer.call(json!({"type":"sync"})).await?;
    assert_eq!(
        issuer.column(&linked).await?,
        [json!("Bread"), json!("Milk")]
    );
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }
    Ok(())
}
