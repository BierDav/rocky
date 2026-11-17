use crate::support;
use rocky_sdk::{Database, Network, Open, Session, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use support::secrets;
use support::snapshot as snap;

const SCHEMA: &str = r#"{"format":6,"application":"lists","version":1,"tables":[
 {"name":"lists","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],
  "primary_key":["id"],"partition_by":["id"],"partition_group":"list"},
 {"name":"categories","class":"replayed","columns":[{"name":"list","type":"blob","not_null":true},{"name":"name","type":"text","not_null":true}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list",
  "foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"}]},
 {"name":"items","class":"replayed","columns":[
   {"name":"list","type":"blob","not_null":true},{"name":"title","type":"text","not_null":true},{"name":"category","type":"blob"}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list",
  "indexes":[{"name":"index_items_list_title","unique":true,"columns":[{"name":"list","descending":false},{"name":"title","descending":false}]}],
  "foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"},
   {"columns":["list","category"],"table":"categories","on_delete":"set_null"}]}
]}"#;
const TABLES: [&str; 3] = ["lists", "categories", "items"];
const ORDERED: &str = r#"{"format":6,"application":"lists","version":2,"tables":[
 {"name":"lists","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],
  "primary_key":["id"],"partition_by":["id"],"partition_group":"list"},
 {"name":"categories","class":"replayed","columns":[{"name":"list","type":"blob","not_null":true},{"name":"name","type":"text","not_null":true}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list",
  "foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"}]},
 {"name":"items","class":"replayed","columns":[
   {"name":"list","type":"blob","not_null":true},{"name":"title","type":"text","not_null":true},{"name":"category","type":"blob"},
   {"name":"position","type":"text","not_null":true,"default":""}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list",
  "indexes":[{"name":"index_items_list_title","unique":true,"columns":[{"name":"list","descending":false},{"name":"title","descending":false}]}],
  "foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"},
   {"columns":["list","category"],"table":"categories","on_delete":"set_null"}]},
 {"name":"item_categories","granted_like":"items","class":"replayed","columns":[
   {"name":"list","type":"blob","not_null":true},{"name":"item","type":"blob","not_null":true},{"name":"category","type":"blob","not_null":true}],
  "primary_key":["list","item","category"],"partition_by":["list"],"partition_group":"list",
  "foreign_keys":[{"columns":["list","item"],"table":"items","on_delete":"cascade"},
   {"columns":["list","category"],"table":"categories","on_delete":"cascade"}]}
],
"migrations":[{"version":2,"statements":[
  "UPDATE items SET position = printf('%09d5', (SELECT count(*) FROM items AS earlier WHERE earlier.list = items.list AND earlier.id < items.id))",
  {"insert":"item_categories","select":"SELECT list, id AS item, category FROM items WHERE category IS NOT NULL"},
  "UPDATE items SET category = NULL WHERE category IS NOT NULL"]}]}"#;

type Failure = Box<dyn std::error::Error + Send + Sync>;

fn identity(directory: &Path) -> Arc<dyn rocky_sdk::IdentityProvider> {
    Arc::new(rocky_sdk::IdentityKey::from_secret(rocky_core::commitment(
        "fixture.lists.identity",
        directory.to_string_lossy().as_bytes(),
    )))
}

struct Peer {
    identity: Option<Arc<dyn rocky_sdk::IdentityProvider>>,
    database: Arc<Database>,
    session: Session,
}
async fn only_explicit_syncs(peers: &[&Peer]) -> Result<(), Failure> {
    for peer in peers {
        snap::value(peer.session.set_automatic_sync(false)).await?;
    }
    Ok(())
}
impl Peer {
    fn new(database: Arc<Database>) -> Result<Self, Failure> {
        Ok(Self {
            identity: None,
            session: database.session(),
            database,
        })
    }
    fn with_identity(mut self, directory: &Path) -> Self {
        self.identity = Some(identity(directory));
        self
    }

    async fn sync_once(&self) -> Result<rocky_sdk::ReconciliationReport, Failure> {
        match &self.identity {
            Some(identity) => self.session.sync_once_with_identity(identity.clone()).await,
            None => self.session.sync_once().await,
        }
    }
    async fn follow(&self, name: &str) -> Result<rocky_sdk::TipStatus, Failure> {
        match &self.identity {
            Some(identity) => {
                self.session
                    .follow_with_identity(name, identity.clone())
                    .await
            }
            None => self.session.follow(name).await,
        }
    }
    async fn insert(&self, table: &str, list: Option<&str>, sql: &str) -> Result<String, Failure> {
        snap::value(self.session.execute("BEGIN", Vec::new())).await?;
        let id = match list {
            Some(list) => self.session.next_partition_id(table, values(list)).await?,
            None => self.session.next_id(table).await?,
        }
        .to_string();
        snap::value(
            self.session
                .execute(&(snap::text(&(sql))), vec![snap::row_id(&(id))]),
        )
        .await?;
        snap::value(self.session.execute("COMMIT", Vec::new())).await?;
        Ok(id)
    }
    async fn column(&self, sql: &str) -> Result<Vec<Value>, Failure> {
        let page = snap::value(self.session.read(rocky_sdk::ReadQuery {
            sql: snap::text(&(sql)),
            values: vec![],
            max_rows: 64,
            max_bytes: None,
            cursor: None,
        }))
        .await?;
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
fn open<'a>(directory: &'a Path, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    at(directory, hub, name, SCHEMA)
}
fn at<'a>(directory: &'a Path, hub: &Arc<Hub>, name: &str, schema: &'a str) -> Open<'a> {
    Open {
        identity: Some(identity(directory)),
        directory,
        schema,
        secrets: Some(secrets::keys(directory)),
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}

async fn checkpoint_writer(
    root: &Path,
    hub: &Arc<Hub>,
    schema: &str,
) -> Result<(Peer, Peer), Failure> {
    let issuer_path = root.join("issuer");
    let mut options = open(&issuer_path, hub, "issuer");
    options.schema = schema;
    let issuer = Peer::new(Database::create(options, None)?.database)?;
    issuer.session.create_bookmark("main", None).await?;
    issuer.follow("main").await?;
    snap::value(issuer.session.next_partition("lists")).await?;
    let base = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Base list')",
        )
        .await?;
    snap::value(issuer.session.seal(false)).await?;
    snap::value(issuer.session.compact(0)).await?;
    let invite = snap::value(
        issuer.session.invite(
            Some(rocky_sdk::GrantProfile::Writer {
                scopes: (TABLES)
                    .into_iter()
                    .map(|scope| scope.to_string())
                    .collect(),
            }),
            None,
        ),
    )
    .await?;
    let writer_path = root.join("writer");
    let mut options = open(&writer_path, hub, "writer");
    options.schema = schema;
    let writer =
        Peer::new(Database::join(options, invite["uri"].as_str().ok_or("uri")?)?.database)?;
    let writer_node = snap::value(writer.session.info()).await?["node"].clone();
    snap::value(issuer.session.share_partition(rocky_sdk::PartitionTarget::Group { group: "list".to_owned(), values: (values(&base)).clone() }, serde_json::from_value::<std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>>(json!([writer_node])).expect("typed fixture"), None)).await?;
    snap::value(writer.sync_once()).await?;
    assert_eq!(
        writer.column("SELECT title FROM lists").await?,
        [json!("Base list")]
    );
    snap::value(writer.session.execute("BEGIN", Vec::new()))
        .await
        .map_err(|error| format!("before preparing a new partition: {error}"))?;
    snap::value(writer.session.execute("ROLLBACK", Vec::new())).await?;
    Ok((issuer, writer))
}

#[tokio::test]
async fn an_unused_own_partition_prepared_before_a_seal_remains_creatable() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer = Peer::new(
        Database::create(
            at(&root.path().join("issuer"), &hub, "issuer", ORDERED),
            None,
        )?
        .database,
    )?;
    issuer.session.create_bookmark("main", None).await?;
    issuer.follow("main").await?;
    snap::value(issuer.session.next_partition("lists")).await?;
    let base = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Base list')",
        )
        .await?;
    let invite = snap::value(
        issuer.session.invite(
            Some(rocky_sdk::GrantProfile::Writer {
                scopes: (TABLES)
                    .into_iter()
                    .map(|scope| scope.to_string())
                    .collect(),
            }),
            None,
        ),
    )
    .await?;
    let writer = Peer::new(
        Database::join(
            at(&root.path().join("writer"), &hub, "writer", ORDERED),
            invite["uri"].as_str().ok_or("uri")?,
        )?
        .database,
    )?;
    let node = snap::value(writer.session.info()).await?["node"].clone();
    snap::value(issuer.session.share_partition(rocky_sdk::PartitionTarget::Group { group: "list".to_owned(), values: (values(&base)).clone() }, serde_json::from_value::<std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>>(json!([node])).expect("typed fixture"), None)).await?;
    snap::value(writer.sync_once()).await?;
    let prepared = snap::value(writer.session.next_partition("lists")).await?;
    snap::value(issuer.session.seal(false)).await?;
    snap::value(issuer.session.compact(0)).await?;
    snap::value(writer.sync_once()).await?;
    let status = snap::value(writer.session.status()).await?;
    assert!(
        status["data"]["blockers"]
            .as_array()
            .ok_or("blockers")?
            .iter()
            .all(|blocker| blocker["kind"]["type"] != "seal_rebootstrap"),
        "an unused partition is not unverified history: {status}"
    );
    let again = snap::value(writer.session.next_partition("lists")).await?;
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
    let mut schema: Value = serde_json::from_str(SCHEMA)?;
    schema["tables"][2]["foreign_keys"] = json!([]);
    let (issuer, writer) = checkpoint_writer(root.path(), &hub, &schema.to_string()).await?;
    snap::value(writer.session.next_partition("lists")).await?;
    let list = writer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Prepared')",
        )
        .await?;
    let status = snap::value(writer.session.status()).await?;
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
    let (issuer, writer) = checkpoint_writer(root.path(), &hub, ORDERED).await?;
    assert!(
        snap::value(
            writer
                .session
                .create_partition(rocky_sdk::PartitionTarget::Group {
                    group: "list".to_owned(),
                    values: values(&format!("{}{:016x}", "ab".repeat(32), 123))
                })
        )
        .await
        .is_err()
    );
    let prepared = snap::value(writer.session.next_partition("lists")).await?;
    let Peer {
        database, session, ..
    } = writer;
    drop(session);
    database.close().await?;
    let writer =
        Peer::new(Database::open(open(&root.path().join("writer"), &hub, "writer"))?.database)?;
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
    let issuer_node = snap::value(issuer.session.info()).await?["node"].clone();
    snap::value(writer.session.share_partition(rocky_sdk::PartitionTarget::Group { group: "list".to_owned(), values: (values(&list)).clone() }, serde_json::from_value::<std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>>(json!([issuer_node])).expect("typed fixture"), None)).await?;
    snap::value(writer.sync_once()).await?;
    snap::value(issuer.sync_once()).await?;
    assert_eq!(
        issuer
            .column("SELECT title FROM lists ORDER BY title")
            .await?,
        [json!("Base list"), json!("Member list")]
    );
    let read_invite = snap::value(
        issuer.session.invite(
            Some(rocky_sdk::GrantProfile::Reader {
                scopes: (TABLES)
                    .into_iter()
                    .map(|scope| scope.to_string())
                    .collect(),
            }),
            None,
        ),
    )
    .await?;
    let reader = Peer::new(
        Database::join(
            open(&root.path().join("reader"), &hub, "reader"),
            read_invite["uri"].as_str().ok_or("uri")?,
        )?
        .database,
    )?;
    assert!(
        snap::value(reader.session.next_partition("lists"))
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
    let issuer =
        Peer::new(Database::create(open(&root.path().join("a"), &hub, "a"), None)?.database)?;
    let invitation = snap::value(
        issuer.session.invite(
            Some(rocky_sdk::GrantProfile::Writer {
                scopes: (TABLES)
                    .into_iter()
                    .map(|scope| scope.to_string())
                    .collect(),
            }),
            None,
        ),
    )
    .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let writer =
        Peer::new(Database::join(open(&root.path().join("b"), &hub, "b"), &invitation)?.database)?;
    only_explicit_syncs(&[&issuer, &writer]).await?;

    let prepared = snap::value(issuer.session.next_partition("lists")).await?;
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

    let node = snap::value(writer.session.info()).await?["node"].clone();
    snap::value(issuer.session.share_partition(rocky_sdk::PartitionTarget::Group { group: "list".to_owned(), values: (values(&list)).clone() }, serde_json::from_value::<std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>>(json!([node])).expect("typed fixture"), None))
        .await?;
    snap::value(writer.sync_once()).await.map_err(|error| {
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

    snap::value(writer.session.execute(
        &(snap::text(
            &(format!("DELETE FROM categories WHERE list = X'{list}' AND id = X'{category}'")),
        )),
        Vec::new(),
    ))
    .await?;
    snap::value(writer.sync_once()).await?;
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
    let activity = snap::value(peer.session.activity(0)).await?;
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
    issuer.session.create_bookmark("main", None).await?;
    issuer.follow("main").await?;
    let invitation = snap::value(
        issuer.session.invite(
            Some(rocky_sdk::GrantProfile::Writer {
                scopes: (TABLES)
                    .into_iter()
                    .map(|scope| scope.to_string())
                    .collect(),
            }),
            None,
        ),
    )
    .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let writer =
        Peer::new(Database::join(open(&root.path().join("b"), &hub, "b"), &invitation)?.database)?;
    snap::value(writer.sync_once()).await?;
    snap::value(issuer.session.next_partition("lists")).await?;
    let list = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Groceries')",
        )
        .await?;
    let node = snap::value(writer.session.info()).await?["node"].clone();
    snap::value(issuer.session.seal(false)).await?;
    snap::value(issuer.session.compact(0)).await?;
    snap::value(issuer.session.share_partition(rocky_sdk::PartitionTarget::Group { group: "list".to_owned(), values: (values(&list)).clone() }, serde_json::from_value::<std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>>(json!([node])).expect("typed fixture"), None))
        .await?;
    snap::value(writer.sync_once()).await?;
    writer
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title) VALUES(?,X'{list}','Milk')"),
        )
        .await?;
    snap::value(writer.sync_once()).await?;
    assert_eq!(
        issuer
            .column(&format!("SELECT title FROM items WHERE list = X'{list}'"))
            .await?,
        [json!("Milk")]
    );

    let served = kinds(&snap::value(issuer.session.activity(0)).await?);
    for kind in [
        "invitation_created",
        "identity_admitted",
        "node_linked",
        "partition_created",
        "partition_shared",
    ] {
        assert!(
            served.contains(&kind.to_owned()),
            "issuer lacks {kind}: {served:?}"
        );
    }
    let activity = snap::value(writer.session.activity(0)).await?;
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
    let waiting = snap::value(writer.session.next_activity(snap::integer(&(token)) as u64));
    let (next, _) = tokio::join!(
        waiting,
        snap::value(writer.session.set_networking_enabled(false))
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
    let issuer =
        Peer::new(Database::create(open(&root.path().join("a"), &hub, "a"), None)?.database)?;
    issuer.session.create_bookmark("main", None).await?;
    issuer.follow("main").await?;
    let invitation = snap::value(
        issuer.session.invite(
            Some(rocky_sdk::GrantProfile::Writer {
                scopes: (TABLES)
                    .into_iter()
                    .map(|scope| scope.to_string())
                    .collect(),
            }),
            None,
        ),
    )
    .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let writer =
        Peer::new(Database::join(open(&root.path().join("b"), &hub, "b"), &invitation)?.database)?;
    snap::value(writer.sync_once()).await?;
    let policy = json!({"required":[],"behind":[],"copies":1,"window":0});
    snap::value(issuer.session.set_retention(
        "items",
        serde_json::from_value::<rocky_sdk::RetentionPolicy>(json!(policy)).expect("typed fixture"),
    ))
    .await?;
    snap::value(issuer.session.next_partition("lists")).await?;
    let list = issuer
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Groceries')",
        )
        .await?;
    let node = snap::value(writer.session.info()).await?["node"].clone();
    snap::value(issuer.session.share_partition(rocky_sdk::PartitionTarget::Group { group: "list".to_owned(), values: (values(&list)).clone() }, serde_json::from_value::<std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>>(json!([node])).expect("typed fixture"), None))
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
    snap::value(
        issuer
            .session
            .execute("DELETE FROM items WHERE title='Eggs'", Vec::new()),
    )
    .await?;
    snap::value(writer.sync_once()).await?;
    let titles = format!("SELECT title FROM items WHERE list = X'{list}' ORDER BY title");
    assert_eq!(
        writer.column(&titles).await?,
        [json!("Flour"), json!("Milk")]
    );
    snap::value(issuer.session.seal(false)).await?;
    let compacted = snap::value(issuer.session.compact(0)).await?;
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
async fn an_issuer_keeping_every_seal_migrates_its_lists() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = root.path().join("a");
    let issuer =
        Peer::new(Database::create(open(&a, &hub, "a"), None)?.database)?.with_identity(&a);
    issuer.session.create_bookmark("main", None).await?;
    issuer.follow("main").await?;
    snap::value(issuer.session.set_genesis_minimum(1)).await?;
    snap::value(
        issuer.session.set_kept_seals(
            serde_json::from_value::<rocky_sdk::KeptSeals>(json!({"every":true}))
                .expect("typed fixture"),
        ),
    )
    .await?;
    snap::value(issuer.session.next_partition("lists")).await?;
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
    let kept = snap::value(issuer.session.seal(false)).await?["tip"].clone();
    snap::value(issuer.session.compact(1)).await?;
    issuer.database.close().await?;

    let issuer = Peer::new(Database::open(at(&a, &hub, "a", ORDERED))?.database)?.with_identity(&a);
    assert_eq!(
        issuer
            .column(&format!(
                "SELECT title FROM items WHERE list = X'{list}' ORDER BY position"
            ))
            .await?,
        [json!("Milk"), json!("Bread")]
    );
    let inspected = snap::value(issuer.session.inspect_tips()).await?;
    let kept_seals = inspected["nodes"][0]["kept_seals"].clone();
    assert!(
        kept_seals
            .as_array()
            .is_some_and(|seals| seals.contains(&kept)),
        "the seal stays kept across the migration: {inspected}"
    );
    issuer.database.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_list_received_from_a_checkpoint_keeps_its_rows_after_reopening() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let issuer =
        Peer::new(Database::create(open(&root.path().join("a"), &hub, "a"), None)?.database)?;
    let invitation = snap::value(
        issuer.session.invite(
            Some(rocky_sdk::GrantProfile::Writer {
                scopes: (TABLES)
                    .into_iter()
                    .map(|scope| scope.to_string())
                    .collect(),
            }),
            None,
        ),
    )
    .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let writer =
        Peer::new(Database::join(open(&root.path().join("b"), &hub, "b"), &invitation)?.database)?;
    snap::value(issuer.session.next_partition("lists")).await?;
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
    let node = snap::value(writer.session.info()).await?["node"].clone();
    snap::value(issuer.session.share_partition(rocky_sdk::PartitionTarget::Group { group: "list".to_owned(), values: (values(&list)).clone() }, serde_json::from_value::<std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>>(json!([node])).expect("typed fixture"), None))
        .await?;
    snap::value(writer.sync_once()).await?;
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

    let Peer {
        database, session, ..
    } = writer;
    drop(session);
    database.close().await?;
    let reopened = Peer::new(Database::open(open(&root.path().join("b"), &hub, "b"))?.database)?;
    assert_eq!(
        reopened.column(&titles).await?,
        [json!("Eggs"), json!("Milk")],
        "after reopening, the items' parent list anchors them again"
    );
    let Peer {
        database, session, ..
    } = issuer;
    drop(session);
    database.close().await?;
    let issuer = Peer::new(Database::open(open(&root.path().join("a"), &hub, "a"))?.database)?;
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
    let a = Peer::new(Database::create(open(&root.path().join("a"), &hub, "a"), None)?.database)?;
    snap::value(a.session.next_partition("lists")).await?;
    let list = a
        .insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,'Own list')",
        )
        .await?;
    let invitation = snap::value(
        a.session
            .invite(Some(rocky_sdk::GrantProfile::Issuer), None),
    )
    .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let b =
        Peer::new(Database::join(open(&root.path().join("b"), &hub, "b"), &invitation)?.database)?;
    snap::value(b.sync_once()).await?;
    assert_eq!(
        b.column("SELECT title FROM lists").await?,
        [json!("Own list")]
    );
    let node = snap::value(a.session.info()).await?["node"].clone();
    snap::value(b.session.share_partition(rocky_sdk::PartitionTarget::Group { group: "list".to_owned(), values: (values(&list)).clone() }, serde_json::from_value::<std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>>(json!([node])).expect("typed fixture"), None))
    .await?;
    snap::value(a.sync_once()).await?;
    let activity = snap::value(a.session.activity(0)).await?;
    assert!(
        !kinds(&activity).contains(&"partition_bootstrap".to_owned()),
        "{activity}"
    );
    assert!(!root.path().join("a/bootstrap.json").exists());
    assert_eq!(
        snap::value(a.session.status()).await?["data"]["bootstrap_pending"],
        false
    );
    for peer in [b, a] {
        peer.database.close().await?;
    }
    Ok(())
}

async fn sync_each_learning_the_others_tuple(peers: &[&Peer]) -> Result<(), Failure> {
    for peer in peers {
        snap::value(peer.sync_once()).await?;
    }
    Ok(())
}
#[tokio::test]
async fn two_issuers_sync_while_each_holds_a_list_the_other_never_saw() -> Result<(), Failure> {
    let root = support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let a = Peer::new(Database::create(open(&root.path().join("a"), &hub, "a"), None)?.database)?;
    let invitation = snap::value(
        a.session
            .invite(Some(rocky_sdk::GrantProfile::Issuer), None),
    )
    .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let b =
        Peer::new(Database::join(open(&root.path().join("b"), &hub, "b"), &invitation)?.database)?;
    let mut lists = Vec::new();
    for (peer, title) in [(&a, "From a"), (&b, "From b")] {
        snap::value(peer.session.next_partition("lists")).await?;
        let sql = format!("INSERT INTO lists(id,title) VALUES(?,'{title}')");
        lists.push(peer.insert("lists", None, &sql).await?);
    }
    sync_each_learning_the_others_tuple(&[&a, &b]).await?;
    for (from, to, list) in [(&a, &b, &lists[0]), (&b, &a, &lists[1])] {
        let node = snap::value(to.session.info()).await?["node"].clone();
        snap::value(
            from.session.share_partition(
                rocky_sdk::PartitionTarget::Group {
                    group: "list".to_owned(),
                    values: (values(list)).clone(),
                },
                serde_json::from_value::<
                    std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>,
                >(json!([node]))
                .expect("typed fixture"),
                None,
            ),
        )
        .await?;
        snap::value(from.sync_once()).await?;
        snap::value(to.sync_once()).await?;
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
    let issuer =
        Peer::new(Database::create(open(&a, &hub, "a"), None)?.database)?.with_identity(&a);
    issuer.session.create_bookmark("main", None).await?;
    issuer.follow("main").await?;
    let inherited_by_migration_openings = 1;
    snap::value(
        issuer
            .session
            .set_genesis_minimum(inherited_by_migration_openings),
    )
    .await?;
    let grants_naming_every_table: Vec<Value> = TABLES
        .iter()
        .map(|table| json!({"scope": table, "rights": ["read", "insert", "update", "delete"]}))
        .collect();
    let invitation = snap::value(
        issuer.session.invite(
            Some(rocky_sdk::GrantProfile::Grants {
                grants: serde_json::from_value::<Vec<rocky_sdk::credentials::Grant>>(json!(
                    grants_naming_every_table
                ))
                .expect("typed fixture"),
            }),
            None,
        ),
    )
    .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let writer =
        Peer::new(Database::join(open(&b, &hub, "b"), &invitation)?.database)?.with_identity(&b);
    snap::value(
        writer.session.set_replica_role(
            serde_json::from_value::<rocky_sdk::ReplicaRoleConfig>(json!({"role":"Partial"}))
                .expect("typed fixture"),
        ),
    )
    .await?;
    snap::value(issuer.session.next_partition("lists")).await?;
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
    let node = snap::value(writer.session.info()).await?["node"].clone();
    snap::value(issuer.session.share_partition(rocky_sdk::PartitionTarget::Group { group: "list".to_owned(), values: (values(&list)).clone() }, serde_json::from_value::<std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>>(json!([node])).expect("typed fixture"), None))
        .await?;
    snap::value(writer.sync_once()).await?;
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }

    let issuer = Peer::new(Database::open(at(&a, &hub, "a", ORDERED))?.database)?.with_identity(&a);
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
    let older_application =
        Peer::new(Database::open(at(&b, &hub, "b", SCHEMA))?.database)?.with_identity(&b);
    snap::value(older_application.sync_once()).await?;
    older_application
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title) VALUES(?,X'{list}','Jam')"),
        )
        .await?;
    snap::value(older_application.sync_once()).await?;
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
    let writer = Peer::new(Database::open(at(&b, &hub, "b", ORDERED))?.database)?.with_identity(&b);
    snap::value(writer.sync_once()).await?;
    assert_eq!(writer.column(&linked).await?, [json!("Milk")]);
    let bread = writer
        .column(&format!(
            "SELECT id FROM items WHERE list = X'{list}' AND title = 'Bread'"
        ))
        .await?;
    let bread = bread.first().and_then(Value::as_str).ok_or("bread")?;
    writer
        .insert(
            "item_categories",
            Some(&list),
            &format!("INSERT INTO item_categories(id,list,item,category) VALUES(?,X'{list}',X'{bread}',X'{dairy}')"),
        )
        .await?;
    snap::value(writer.sync_once()).await?;
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
    let issuer =
        Peer::new(Database::create(open(&a, &hub, "a"), None)?.database)?.with_identity(&a);
    issuer.session.create_bookmark("main", None).await?;
    issuer.follow("main").await?;
    let inherited_by_migration_openings = 1;
    snap::value(
        issuer
            .session
            .set_genesis_minimum(inherited_by_migration_openings),
    )
    .await?;
    let grants_naming_every_table: Vec<Value> = TABLES
        .iter()
        .map(|table| json!({"scope": table, "rights": ["read", "insert", "update", "delete"]}))
        .collect();
    let invitation = snap::value(
        issuer.session.invite(
            Some(rocky_sdk::GrantProfile::Grants {
                grants: serde_json::from_value::<Vec<rocky_sdk::credentials::Grant>>(json!(
                    grants_naming_every_table
                ))
                .expect("typed fixture"),
            }),
            None,
        ),
    )
    .await?["uri"]
        .as_str()
        .ok_or("uri")?
        .to_owned();
    let writer =
        Peer::new(Database::join(open(&b, &hub, "b"), &invitation)?.database)?.with_identity(&b);
    snap::value(issuer.session.next_partition("lists")).await?;
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
    let node = snap::value(writer.session.info()).await?["node"].clone();
    snap::value(issuer.session.share_partition(rocky_sdk::PartitionTarget::Group { group: "list".to_owned(), values: (values(&list)).clone() }, serde_json::from_value::<std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>>(json!([node])).expect("typed fixture"), None))
        .await?;
    snap::value(writer.sync_once()).await?;
    for peer in [writer, issuer] {
        peer.database.close().await?;
    }

    let issuer = Peer::new(Database::open(at(&a, &hub, "a", ORDERED))?.database)?.with_identity(&a);
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
    let older_application =
        Peer::new(Database::open(at(&b, &hub, "b", SCHEMA))?.database)?.with_identity(&b);
    snap::value(older_application.sync_once()).await?;
    older_application
        .insert(
            "items",
            Some(&list),
            &format!("INSERT INTO items(id,list,title) VALUES(?,X'{list}','Jam')"),
        )
        .await?;
    snap::value(older_application.sync_once()).await?;
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
    let writer = Peer::new(Database::open(at(&b, &hub, "b", ORDERED))?.database)?.with_identity(&b);
    snap::value(writer.sync_once()).await?;
    assert_eq!(writer.column(&linked).await?, [json!("Milk")]);
    let bread = writer
        .column(&format!(
            "SELECT id FROM items WHERE list = X'{list}' AND title = 'Bread'"
        ))
        .await?;
    let bread = bread.first().and_then(Value::as_str).ok_or("bread")?;
    writer
        .insert(
            "item_categories",
            Some(&list),
            &format!("INSERT INTO item_categories(id,list,item,category) VALUES(?,X'{list}',X'{bread}',X'{dairy}')"),
        )
        .await?;
    snap::value(writer.sync_once()).await?;
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
