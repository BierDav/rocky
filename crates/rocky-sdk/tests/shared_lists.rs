//! A list shared with an enrolled writer: one envelope opens the list, its
//! categories and items, which then bootstrap from a checkpoint and sync. See
//! `docs/runtime/partitioned-scopes.md#group-secrets`.
use rocky_sdk::{Database, Network, Open, Session, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

const SCHEMA: &str = r#"{"format":4,"application":"lists","version":1,"tables":[
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
    async fn author(&self, issuer: &Peer) -> Result<(), Failure> {
        for table in TABLES {
            let request = self
                .call(json!({"type":"pseudonym_request","collection":table}))
                .await?;
            let assigned = issuer
                .call(json!({"type":"pseudonym_assign","request":request["request"]}))
                .await?;
            self.call(json!({"type":"pseudonym_install","pseudonym":assigned["pseudonym"]}))
                .await?;
        }
        Ok(())
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
    Open {
        directory,
        schema: SCHEMA,
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        commands: None,
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
    issuer.author(&issuer).await?;
    writer.author(&issuer).await?;

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
    let shared = issuer
        .call(json!({"type":"partition_share","group":"list","values":values(&list),"recipients":[node]}))
        .await?;
    let envelope = &shared["envelopes"][0]["envelope"];
    writer
        .call(json!({"type":"partition_install","group":"list","values":values(&list),"envelope":envelope}))
        .await?;
    // The join's own bootstrap finishes although partitioned tables offer
    // no checkpoint of their own scope; the list then starts from one.
    writer.call(json!({"type":"sync"})).await?;
    writer
        .call(json!({"type":"partition_bootstrap","group":"list","values":values(&list)}))
        .await?;
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
