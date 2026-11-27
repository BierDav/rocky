use crate::support::snapshot as snap;
use rocky_sdk::{Database, Network, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

fn release() -> rocky_sdk::Schema {
    use rocky_sdk::{Affinity, Column, DeleteAction, ForeignKey, Index, Table};
    let required = |name: &str, affinity| Column::new(name, affinity).required();
    let reference = |columns: &[&str], table: &str, on_delete| {
        ForeignKey::new(columns.iter().copied(), table).on_delete(on_delete)
    };
    let in_list = |table: &str| {
        Table::replayed(table)
            .column(required("list", Affinity::Blob))
            .primary_key(["list", "id"])
            .partition_by(["list"])
            .partition_group("list")
            .foreign_key(reference(&["list"], "lists", DeleteAction::Cascade))
    };
    rocky_sdk::Schema::builder("shopping", 3)
        .table(
            Table::replayed("lists")
                .column(required("title", Affinity::Text))
                .partition_by(["id"])
                .partition_group("list"),
        )
        .table(in_list("categories").column(required("name", Affinity::Text)))
        .table(
            in_list("items")
                .column(required("title", Affinity::Text))
                .column(required("completed", Affinity::Integer))
                .column(Column::new("category", Affinity::Blob))
                .index(Index::new("index_items_list_title", ["list", "title"]).unique())
                .foreign_key(reference(
                    &["list", "category"],
                    "categories",
                    DeleteAction::SetNull,
                )),
        )
        .role(
            rocky_sdk::Role::new(rocky_sdk::RoleName::from_static("shopper"))
                .read("lists")
                .write("categories")
                .write("items"),
        )
        .build()
        .expect("release schema")
}

type Failure = Box<dyn std::error::Error + Send + Sync>;

struct Peer {
    database: Database,
}
impl Peer {
    fn new(database: Database) -> Result<Self, Failure> {
        Ok(Self { database })
    }

    async fn invite(
        &self,
        access: rocky_sdk::Access,
    ) -> Result<rocky_sdk::InvitationCode, Failure> {
        Ok(self
            .database
            .invite(rocky_sdk::Invite::new(access))
            .await?
            .code)
    }

    async fn online(&self, online: bool) -> Result<(), Failure> {
        snap::value(self.database.set_sync(if online {
            rocky_sdk::SyncMode::Manual
        } else {
            rocky_sdk::SyncMode::Off
        }))
        .await
        .map(drop)
    }
    async fn sync(&self) -> Result<(), Failure> {
        let status = snap::value(self.database.sync_now()).await?;
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
        let id = self
            .database
            .write(async |tx| {
                let id = tx
                    .next_id_in(table, list.map(values).unwrap_or_default())
                    .await?;
                tx.execute(sql, vec![id.sql()]).await?;
                Ok::<_, Failure>(id)
            })
            .await?;
        Ok(id.to_string())
    }
    async fn create_list(&self, title: &str) -> Result<String, Failure> {
        snap::value(self.database.next_partition("lists")).await?;
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
        let moved = self
            .database
            .write(async |tx| {
                let moved = tx.next_id_in("items", values(to)).await?;
                tx.execute(
                    &format!("INSERT INTO items(id,list,title,completed) SELECT ?,X'{to}',title,completed FROM items WHERE list=X'{from}' AND id=X'{id}'"),
                    vec![moved.sql()],
                )
                .await?;
                tx.execute(
                    &format!("DELETE FROM items WHERE list=X'{from}' AND id=X'{id}'"),
                    Vec::new(),
                )
                .await?;
                Ok::<_, Failure>(moved)
            })
            .await?;
        Ok(moved.to_string())
    }
    async fn execute(&self, sql: &str) -> Result<(), Failure> {
        self.database
            .write(async |tx| tx.execute(sql, Vec::new()).await)
            .await?;
        Ok(())
    }
    async fn rows(&self, sql: &str) -> Result<Vec<Vec<Value>>, Failure> {
        let page = snap::read(&self.database, sql, Vec::new()).await?;
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
}
fn values(list: &str) -> Vec<rocky_sdk::SqlValue> {
    vec![snap::row_id(&(list))]
}
fn open(directory: &Path, hub: &Arc<Hub>, name: &str) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        &release(),
        None,
        None,
        Some(Network::Owned(hub.connector(name))),
    )
}
async fn introduce(left: &Peer, right: &Peer) -> Result<(), Failure> {
    let l = snap::value(left.database.contact()).await?["uri"].clone();
    let r = snap::value(right.database.contact()).await?["uri"].clone();
    snap::value(async {
        let value = left.database.add_peer(&(snap::text(&(r)))).await?;
        Ok::<_, rocky_sdk::Error>(json!({"node":value}))
    })
    .await?;
    snap::value(async {
        let value = right.database.add_peer(&(snap::text(&(l)))).await?;
        Ok::<_, rocky_sdk::Error>(json!({"node":value}))
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
    let a = Peer::new(
        open(&root.path().join("a"), &hub, "a")
            .create()
            .await?
            .database,
    )?;
    let custody = Peer::new(
        open(&root.path().join("custody"), &hub, "custody")
            .fixture_person()
            .join(&a.invite(rocky_sdk::Access::Store).await?)
            .await?,
    )?;
    let b = Peer::new(
        open(&root.path().join("b"), &hub, "b")
            .fixture_person()
            .join(
                &a.invite(rocky_sdk::Access::Role(rocky_sdk::RoleName::from_static(
                    "shopper",
                )))
                .await?,
            )
            .await?,
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
        open(&_root.path().join("c"), &hub, "c")
            .fixture_person()
            .join(&a.invite(rocky_sdk::Access::Full).await?)
            .await?,
    )?;
    c.sync().await?;
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
    a.add(&list, "Milk", None).await?;
    b.online(false).await?;
    a.sync().await?;
    a.online(false).await?;
    custody.database.close().await?;
    let custody = Peer::new(
        open(&_root.path().join("custody"), &hub, "custody")
            .open()
            .await?,
    )?;
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
