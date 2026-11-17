use crate::support::snapshot as snap;
use rocky_sdk::{Database, Network, Open, Session, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

const SCHEMA: &str = r#"{"format":6,"application":"shopping","version":3,"tables":[
 {"name":"lists","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],
  "primary_key":["id"],"partition_by":["id"],"partition_group":"list"},
 {"name":"categories","class":"replayed","columns":[{"name":"list","type":"blob","not_null":true},{"name":"name","type":"text","not_null":true}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list",
  "foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"}]},
 {"name":"items","class":"replayed","columns":[
   {"name":"list","type":"blob","not_null":true},{"name":"title","type":"text","not_null":true},
   {"name":"completed","type":"integer","not_null":true},{"name":"category","type":"blob"}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list",
  "indexes":[{"name":"index_items_list_title","unique":true,"columns":[{"name":"list","descending":false},{"name":"title","descending":false}]}],
  "foreign_keys":[{"columns":["list"],"table":"lists","on_delete":"cascade"},
   {"columns":["list","category"],"table":"categories","on_delete":"set_null"}]}
]}"#;
const TABLES: [&str; 3] = ["lists", "categories", "items"];

type Failure = Box<dyn std::error::Error + Send + Sync>;

struct Peer {
    database: Arc<Database>,
    session: Session,
}
impl Peer {
    fn new(database: Arc<Database>) -> Result<Self, Failure> {
        Ok(Self {
            session: database.session(),

            database,
        })
    }

    async fn invite(&self, profile: Value) -> Result<String, Failure> {
        Ok(snap::value(
            self.session.invite(
                Some(
                    serde_json::from_value::<rocky_sdk::GrantProfile>(json!(profile))
                        .expect("typed fixture"),
                ),
                None,
            ),
        )
        .await?["uri"]
            .as_str()
            .ok_or("uri")?
            .to_owned())
    }

    async fn node(&self) -> Result<Value, Failure> {
        Ok(snap::value(self.session.info()).await?["node"].clone())
    }
    async fn online(&self, online: bool) -> Result<(), Failure> {
        snap::value(self.session.set_networking_enabled(online))
            .await
            .map(drop)
    }
    async fn sync(&self) -> Result<(), Failure> {
        let status = snap::value(self.session.sync_once()).await?;
        if !status["peers"].as_array().is_some_and(|peers| {
            peers.iter().any(|peer| {
                peer["attempts"].as_array().is_some_and(|attempts| {
                    attempts.iter().any(|attempt| {
                        attempt["operation"] == "reconcile" && attempt["outcome"] == "completed"
                    })
                })
            })
        }) {
            return Err(format!("sync: {status}").into());
        }
        Ok(())
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
    async fn create_list(&self, title: &str) -> Result<String, Failure> {
        snap::value(self.session.next_partition("lists")).await?;
        self.insert(
            "lists",
            None,
            &format!("INSERT INTO lists(id,title) VALUES(?,'{title}')"),
        )
        .await
    }
    async fn add(
        &self,
        list: &str,
        title: &str,
        category: Option<&str>,
    ) -> Result<String, Failure> {
        let category = category.map_or("NULL".to_owned(), |id| format!("X'{id}'"));
        self.insert(
            "items",
            Some(list),
            &format!(
                "INSERT INTO items(id,list,title,completed,category) VALUES(?,X'{list}','{title}',0,{category})"
            ),
        )
        .await
    }
    async fn move_item(&self, from: &str, id: &str, to: &str) -> Result<String, Failure> {
        snap::value(self.session.execute("BEGIN", Vec::new())).await?;
        let moved = snap::value(
            self.session
                .next_partition_id("items", (values(to)).clone()),
        )
        .await?["id"]
            .clone();
        snap::value(self.session.execute(&(snap::text(&(format!("INSERT INTO items(id,list,title,completed) SELECT ?,X'{to}',title,completed FROM items WHERE list=X'{from}' AND id=X'{id}'")))), vec![snap::row_id(&(moved))]))
            .await?;
        self.execute(&format!(
            "DELETE FROM items WHERE list=X'{from}' AND id=X'{id}'"
        ))
        .await?;
        snap::value(self.session.execute("COMMIT", Vec::new())).await?;
        Ok(moved.as_str().ok_or("id")?.to_owned())
    }
    async fn execute(&self, sql: &str) -> Result<(), Failure> {
        snap::value(self.session.execute(&(snap::text(&(sql))), Vec::new()))
            .await
            .map(drop)
    }
    async fn rows(&self, sql: &str) -> Result<Vec<Vec<Value>>, Failure> {
        let page = snap::value(self.session.read(rocky_sdk::ReadQuery {
            sql: snap::text(&(sql)),
            values: vec![],
            max_rows: 256,
            max_bytes: None,
            cursor: None,
        }))
        .await?;
        Ok(page["rows"]
            .as_array()
            .ok_or("rows")?
            .iter()
            .map(|row| {
                row.as_array()
                    .map(|cells| cells.iter().map(snap::cell_value).collect())
                    .unwrap_or_default()
            })
            .collect())
    }
    async fn items(&self) -> Result<Vec<Vec<Value>>, Failure> {
        self.rows("SELECT list,title,completed,category FROM items ORDER BY list,title")
            .await
    }
    async fn share(&self, list: &str, reader: &Peer) -> Result<(), Failure> {
        snap::value(
            self.session.share_partition(
                rocky_sdk::PartitionTarget::Group {
                    group: "list".to_owned(),
                    values: (values(list)).clone(),
                },
                serde_json::from_value::<
                    std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>,
                >(json!([reader.node().await?]))
                .expect("typed fixture"),
                None,
            ),
        )
        .await
        .map(drop)
    }
}
fn values(list: &str) -> Vec<rocky_sdk::SqlValue> {
    vec![snap::row_id(&(list))]
}
fn open<'a>(directory: &'a Path, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    Open {
        identity: None,
        directory,
        schema: SCHEMA,
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}
async fn introduce(left: &Peer, right: &Peer) -> Result<(), Failure> {
    let l = snap::value(left.session.contact_info()).await?["uri"].clone();
    let r = snap::value(right.session.contact_info()).await?["uri"].clone();
    snap::value(async {
        let value = left.session.add_peer(&(snap::text(&(r)))).await?;
        Ok(json!({"node":value}))
    })
    .await?;
    snap::value(async {
        let value = right.session.add_peer(&(snap::text(&(l)))).await?;
        Ok(json!({"node":value}))
    })
    .await?;
    Ok(())
}

struct Topology {
    _root: tempfile::TempDir,
    hub: Arc<Hub>,
    a: Peer,
    b: Peer,
    custody: Peer,
}
async fn topology() -> Result<Topology, Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let a = Peer::new(Database::create(open(&root.path().join("a"), &hub, "a"), None)?.database)?;
    let custody = Peer::new(
        Database::join(
            open(&root.path().join("custody"), &hub, "custody"),
            &a.invite(json!({"kind":"custody","scopes":TABLES})).await?,
        )?
        .database,
    )?;
    let b = Peer::new(
        Database::join(
            open(&root.path().join("b"), &hub, "b"),
            &a.invite(json!({"kind":"grants","grants":[
                {"scope":"lists","rights":["read"]},
                {"scope":"categories","rights":["read","insert","update","delete"]},
                {"scope":"items","rights":["read","insert","update","delete"]}
            ]}))
            .await?,
        )?
        .database,
    )?;
    introduce(&a, &custody).await?;
    introduce(&b, &custody).await?;
    Ok(Topology {
        _root: root,
        hub,
        a,
        b,
        custody,
    })
}
async fn relay(from: &Peer, to: &Peer) -> Result<(), Failure> {
    to.online(false).await?;
    from.online(true).await?;
    from.sync().await?;
    from.online(false).await?;
    to.online(true).await?;
    to.sync().await
}

#[tokio::test(flavor = "multi_thread")]
async fn two_phones_never_online_together_pass_the_release_gate_through_custody()
-> Result<(), Failure> {
    let Topology { a, b, custody, .. } = &topology().await?;
    let weekly = a.create_list("Weekly").await?;
    let weekend = a.create_list("Weekend").await?;
    let party = a.create_list("Party").await?;
    let dairy = a
        .insert(
            "categories",
            Some(&weekly),
            &format!("INSERT INTO categories(id,list,name) VALUES(?,X'{weekly}','Dairy')"),
        )
        .await?;
    let milk = a.add(&weekly, "Milk", Some(&dairy)).await?;
    a.add(&weekly, "Butter", Some(&dairy)).await?;
    a.add(&party, "Cake", None).await?;
    for list in [&weekly, &weekend, &party] {
        a.share(list, b).await?;
    }
    relay(a, b).await?;
    assert_eq!(
        b.items().await?,
        a.items().await?,
        "custody holds one envelope per list as a mailbox for a device it relays to"
    );
    assert_eq!(b.items().await?.len(), 3);
    assert!(
        custody
            .rows("SELECT title FROM items")
            .await
            .map_or(true, |rows| rows.is_empty()),
        "custody holds every list's frames but cannot read them"
    );

    b.execute(&format!(
        "UPDATE items SET completed = 1 WHERE list = X'{weekly}' AND id = X'{milk}'"
    ))
    .await?;
    b.add(&weekly, "Bread", None).await?;
    b.sync().await?;
    b.online(false).await?;
    a.add(&weekly, "Bread", None).await?;
    a.move_item(&weekly, &milk, &weekend).await?;
    relay(a, b).await?;
    relay(b, a).await?;
    let moved_with_the_values_a_read_not_b_s_tick =
        vec![json!(weekend), json!("Milk"), json!("0"), Value::Null];
    let converged = vec![
        vec![json!(weekly), json!("Bread"), json!("0"), Value::Null],
        vec![json!(weekly), json!("Butter"), json!("0"), json!(dairy)],
        moved_with_the_values_a_read_not_b_s_tick,
        vec![json!(party), json!("Cake"), json!("0"), Value::Null],
    ];
    assert_eq!(
        a.items().await?,
        converged,
        "offline edits converge, and B's tick changed only the row A moved away"
    );
    assert_eq!(b.items().await?, converged);

    a.online(true).await?;
    a.execute(&format!(
        "DELETE FROM categories WHERE list = X'{weekly}' AND id = X'{dairy}'"
    ))
    .await?;
    a.execute(&format!("DELETE FROM lists WHERE id = X'{party}'"))
        .await?;
    relay(a, b).await?;
    let deleted = vec![
        vec![json!(weekly), json!("Bread"), json!("0"), Value::Null],
        vec![json!(weekly), json!("Butter"), json!("0"), Value::Null],
        vec![json!(weekend), json!("Milk"), json!("0"), Value::Null],
    ];
    assert_eq!(
        b.items().await?,
        deleted,
        "deleting a category nulls only the reference and deleting a list cascades"
    );
    assert_eq!(
        b.rows("SELECT title FROM lists ORDER BY title").await?,
        [[json!("Weekend")], [json!("Weekly")]]
    );

    b.add(&weekend, "Eggs", None).await?;
    relay(b, a).await?;
    assert_eq!(a.items().await?, b.items().await?);
    for peer in [a, b, custody] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_joining_after_a_move_starts_the_list_from_a_checkpoint() -> Result<(), Failure> {
    let Topology {
        _root,
        hub,
        a,
        b,
        custody,
    } = topology().await?;
    let weekly = a.create_list("Weekly").await?;
    let weekend = a.create_list("Weekend").await?;
    let milk = a.add(&weekly, "Milk", None).await?;
    a.move_item(&weekly, &milk, &weekend).await?;
    let c = Peer::new(
        Database::join(
            open(&_root.path().join("c"), &hub, "c"),
            &a.invite(json!({"kind":"writer","scopes":TABLES})).await?,
        )?
        .database,
    )?;
    c.sync().await?;
    for list in [&weekly, &weekend] {
        a.share(list, &c).await?;
    }
    c.sync().await?;
    assert_eq!(c.items().await?, a.items().await?);
    assert_eq!(
        c.items().await?,
        [[json!(weekend), json!("Milk"), json!("0"), Value::Null]],
        "the weekly checkpoint no longer holds the moved row"
    );
    for peer in [a, b, c, custody] {
        peer.database.close().await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn custody_keeps_offered_partitions_across_restart() -> Result<(), Failure> {
    let Topology {
        _root,
        hub,
        a,
        b,
        custody,
    } = topology().await?;
    let list = a.create_list("Weekly").await?;
    a.share(&list, &b).await?;
    a.add(&list, "Milk", None).await?;
    b.online(false).await?;
    a.sync().await?;
    a.online(false).await?;
    custody.database.close().await?;
    let custody =
        Peer::new(Database::open(open(&_root.path().join("custody"), &hub, "custody"))?.database)?;
    b.online(true).await?;
    b.sync().await?;
    assert_eq!(
        b.items().await?,
        [[json!(list), json!("Milk"), json!("0"), Value::Null]],
        "custody persists a list registered from an offer across restart"
    );
    for peer in [a, b, custody] {
        peer.database.close().await?;
    }
    Ok(())
}
