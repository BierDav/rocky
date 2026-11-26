use crate::common;
use crate::support::snapshot as snap;
use rocky_credentials::{ProviderError, PublicKey};
use rocky_sdk::{Access, Database, Invite, Network, RoleName, SecretStore, transport::memory::Hub};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

use crate::support;

fn in_list(table: &str, column: rocky_sdk::Column) -> rocky_sdk::Table {
    rocky_sdk::Table::replayed(table)
        .column(common::required("list", rocky_sdk::Affinity::Text))
        .column(common::required("key", rocky_sdk::Affinity::Text))
        .column(column)
        .primary_key(["list", "key"])
        .partition_by(["list"])
        .partition_group("list")
}
fn group_role(name: &'static str, tables: &[&str], write: bool) -> rocky_sdk::Role {
    tables
        .iter()
        .fold(
            rocky_sdk::Role::new(rocky_sdk::RoleName::from_static(name)),
            |role, table| match write {
                true => role.write(*table),
                false => role.read(*table),
            },
        )
        .per_partition("list")
}
fn group_schema(version: u32, attachments: bool) -> rocky_sdk::Schema {
    let full: &[&str] = match attachments {
        true => &["lists", "items", "attachments"],
        false => &["lists", "items"],
    };
    let mut schema = rocky_sdk::Schema::builder("groupsecrets", version)
        .table(
            rocky_sdk::Table::replayed("notes")
                .column(common::required("key", rocky_sdk::Affinity::Text))
                .primary_key(["key"]),
        )
        .table(
            rocky_sdk::Table::replayed("lists")
                .column(common::required("name", rocky_sdk::Affinity::Text))
                .column(rocky_sdk::Column::new("title", rocky_sdk::Affinity::Text))
                .primary_key(["name"])
                .partition_by(["name"])
                .partition_group("list"),
        )
        .table(in_list(
            "items",
            rocky_sdk::Column::new("title", rocky_sdk::Affinity::Text),
        ));
    if attachments {
        schema = schema.table(in_list(
            "attachments",
            rocky_sdk::Column::new("bytes", rocky_sdk::Affinity::Blob),
        ));
    }
    common::build(
        schema
            .role(group_role("reader", full, false))
            .role(group_role("partial", &["lists", "items"], false))
            .role(group_role("writer", full, true)),
    )
}
fn groups() -> rocky_sdk::Schema {
    group_schema(1, true)
}
const TABLES: [&str; 3] = ["lists", "items", "attachments"];

#[derive(Clone, Copy)]
struct NodeKeys {
    identity: PublicKey,
    node: PublicKey,
}

#[derive(Default)]
struct Store {
    secrets: Mutex<BTreeMap<String, Vec<u8>>>,
    budget: Mutex<Option<usize>>,
}
impl Store {
    fn spend(&self) -> Result<(), ProviderError> {
        let mut budget = self.budget.lock().unwrap();
        match budget.as_mut() {
            Some(0) => Err("injected key provider failure".into()),
            Some(left) => {
                *left -= 1;
                Ok(())
            }
            None => Ok(()),
        }
    }
    fn limit(&self, budget: Option<usize>) {
        *self.budget.lock().unwrap() = budget;
    }
    fn group_secrets(&self) -> usize {
        let secrets = self.secrets.lock().unwrap();
        secrets
            .keys()
            .filter(|name| name.contains(".group."))
            .count()
    }
}
impl SecretStore for Store {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, ProviderError> {
        self.spend()?;
        Ok(self.secrets.lock().unwrap().get(name).cloned())
    }
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), ProviderError> {
        self.spend()?;
        self.secrets
            .lock()
            .unwrap()
            .insert(name.into(), secret.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), ProviderError> {
        self.spend()?;
        self.secrets.lock().unwrap().remove(name);
        Ok(())
    }
}

struct Peer {
    database: Database,
    store: Arc<Store>,
    hub: Arc<Hub>,
}
fn open(
    directory: &Path,
    schema: &rocky_sdk::Schema,
    store: &Arc<Store>,
    hub: &Arc<Hub>,
) -> rocky_sdk::Configuration {
    let name = directory.file_name().unwrap().to_str().unwrap();
    crate::common::configuration(
        directory,
        schema,
        Some(store.clone() as Arc<dyn SecretStore>),
        common::prepared_identity(directory),
        Some(Network::Owned(hub.connector(name))),
    )
}
fn open_with_identity(
    directory: &Path,
    store: &Arc<Store>,
    hub: &Arc<Hub>,
    identity: &Arc<dyn rocky_sdk::IdentityProvider>,
) -> rocky_sdk::Configuration {
    open(directory, &groups(), store, hub).identity(Arc::clone(identity))
}
impl Peer {
    fn new(database: Database, store: Arc<Store>, hub: Arc<Hub>) -> Self {
        Self {
            database,
            store,
            hub,
        }
    }
    async fn create(directory: &Path) -> Self {
        let (store, hub) = (Arc::new(Store::default()), Arc::new(Hub::default()));
        let database = open(directory, &groups(), &store, &hub)
            .create()
            .await
            .unwrap()
            .database;
        Self::new(database, store, hub)
    }
    async fn reopen(
        directory: &Path,
        schema: &rocky_sdk::Schema,
        store: Arc<Store>,
        hub: Arc<Hub>,
    ) -> Self {
        let database = open(directory, schema, &store, &hub).open().await.unwrap();
        Self::new(database, store, hub)
    }

    async fn status(&self, list: &str) -> Value {
        snap::ok(self.database.partition_status(&rocky_sdk::Partition::new(
            "list".to_owned(),
            (values(list)).clone(),
        )))
        .await
    }
    async fn epochs(&self, list: &str) -> (Value, BTreeMap<String, Value>) {
        let status = self.status(list).await;
        let tables = status["tables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|table| {
                (
                    table["table"].as_str().unwrap().to_owned(),
                    json!(
                        table["epochs"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|epoch| epoch["generation"].clone())
                            .collect::<Vec<_>>()
                    ),
                )
            })
            .collect();
        (
            json!(
                status["epochs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|epoch| epoch["generation"].clone())
                    .collect::<Vec<_>>()
            ),
            tables,
        )
    }
    async fn grant(&self, identity: PublicKey, list: &str) -> Result<Value, String> {
        snap::string(self.database.grant_partition(
            rocky_sdk::PersonId::new(identity),
            &rocky_sdk::Partition::new("list", values(list)),
        ))
        .await
    }
    async fn enroll(&self, directory: &Path, role: &'static str) -> (Peer, NodeKeys) {
        self.enroll_as(directory, Access::Role(RoleName::from_static(role)))
            .await
    }
    async fn enroll_as(&self, directory: &Path, access: Access) -> (Peer, NodeKeys) {
        let invitation = self
            .database
            .invite(rocky_sdk::Invite::new(access))
            .await
            .unwrap();
        let store = Arc::new(Store::default());
        let database = open(directory, &groups(), &store, &self.hub)
            .join(&invitation.code)
            .await
            .unwrap()
            .database;
        let authority = database.status().await.unwrap().authority;
        (
            Peer::new(database, store, self.hub.clone()),
            NodeKeys {
                identity: authority.principal.key(),
                node: authority.node.key(),
            },
        )
    }
    async fn fetch(&self, from: &Peer) -> Result<Value, String> {
        let contact = snap::ok(from.database.contact()).await;
        snap::string(async {
            let value = self
                .database
                .add_peer(&(snap::text(&(contact["uri"]))))
                .await?;
            Ok::<_, rocky_sdk::Error>(json!({"node":value}))
        })
        .await?;
        snap::string(self.database.sync_now()).await
    }
    async fn recipients(&self, list: &str) -> BTreeMap<String, Value> {
        let mut recipients = self.status(list).await["recipients"]
            .as_array()
            .unwrap()
            .clone();
        recipients.sort_by_key(|recipient| recipient["epoch"]["generation"].as_u64());
        recipients
            .iter()
            .map(|recipient| {
                (
                    recipient["node"].as_str().unwrap().to_owned(),
                    recipient["state"].clone(),
                )
            })
            .collect()
    }
}
fn values(list: &str) -> Vec<rocky_sdk::SqlValue> {
    vec![rocky_sdk::SqlValue::Text(snap::text(&(list)))]
}
fn uniform(tables: &BTreeMap<String, Value>, epochs: Value) -> bool {
    tables.len() == TABLES.len() && tables.values().all(|held| *held == epochs)
}
async fn granted(
    issuer: &Peer,
    directory: &Path,
    list: &str,
    role: &'static str,
) -> (Peer, NodeKeys) {
    let (peer, device) = issuer.enroll(directory, role).await;
    issuer.grant(device.identity, list).await.unwrap();
    (peer, device)
}

#[tokio::test]
async fn one_envelope_grants_a_group_and_only_a_role_reading_every_table_is_granted() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer")).await;
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", (values("A")).clone())),
    )
    .await;
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", (values("A")).clone())),
    )
    .await;
    assert_eq!(issuer.store.group_secrets(), 1);
    let (secrets, tables) = issuer.epochs("A").await;
    assert_eq!(secrets, json!([1]));
    assert!(uniform(&tables, json!([1])));

    let (reader, device) = granted(&issuer, &root.path().join("reader"), "A", "reader").await;
    assert_eq!(
        issuer.recipients("A").await[&device.node.to_hex()],
        "pending"
    );

    let (reader_lacking_one_table, partial_device) =
        issuer.enroll(&root.path().join("partial"), "partial").await;
    let refused_grant = issuer
        .grant(partial_device.identity, "A")
        .await
        .unwrap_err();
    assert!(refused_grant.contains("every table"), "{refused_grant}");
    reader_lacking_one_table.fetch(&issuer).await.unwrap();
    assert_eq!(
        reader_lacking_one_table.store.group_secrets(),
        0,
        "a role lacking one table of the group receives no secret"
    );
    assert_eq!(
        issuer
            .recipients("A")
            .await
            .get(&partial_device.node.to_hex()),
        None,
        "no envelope is sealed to it"
    );

    reader.fetch(&issuer).await.unwrap();
    let (secrets, tables) = reader.epochs("A").await;
    assert_eq!(secrets, json!([1]));
    assert!(
        uniform(&tables, json!([1])),
        "the recipient installs every table's key from the one envelope: {tables:?}"
    );
    assert_eq!(
        issuer.recipients("A").await[&device.node.to_hex()],
        "delivered"
    );
    assert_eq!(
        reader.status("A").await["recipients"],
        json!([]),
        "the status of deliveries is the sharer's alone"
    );
    assert_eq!(
        reader.store.group_secrets(),
        1,
        "one secret is stored; table keys are derived, never stored"
    );
    reader.database.close().await.unwrap();
    let reader = Peer::reopen(
        &root.path().join("reader"),
        &groups(),
        reader.store.clone(),
        reader.hub.clone(),
    )
    .await;
    assert!(uniform(&reader.epochs("A").await.1, json!([1])));
    for peer in [reader, reader_lacking_one_table, issuer] {
        peer.database.close().await.unwrap();
    }
}

#[tokio::test]
async fn revocation_rotates_the_group_once_and_shares_only_the_current_epoch_to_live_devices() {
    let root = support::small_directory().unwrap();
    let issuer = Peer::create(&root.path().join("issuer")).await;
    for list in ["A", "B"] {
        snap::ok(
            issuer
                .database
                .create_partition(&rocky_sdk::Partition::new("list", (values(list)).clone())),
        )
        .await;
    }
    let (removed, removed_device) =
        granted(&issuer, &root.path().join("removed"), "A", "reader").await;
    let (remaining, remaining_device) =
        granted(&issuer, &root.path().join("remaining"), "A", "reader").await;
    for peer in [&removed, &remaining] {
        peer.fetch(&issuer).await.unwrap();
    }

    snap::ok(
        issuer
            .database
            .remove(rocky_sdk::PersonId::new(snap::public_key(
                &(removed_device.identity.to_string()),
            ))),
    )
    .await;
    let revoked = snap::ok(snap::device(&issuer.database)).await;
    assert_eq!(
        revoked["revocations"]["statements"], 1,
        "the issuer revokes a device it knows only through its partition grant"
    );
    for (list, expected) in [("A", json!([1, 2])), ("B", json!([1]))] {
        let (secrets, tables) = issuer.epochs(list).await;
        assert_eq!(secrets, expected, "{list}");
        assert!(uniform(&tables, expected), "{list}: {tables:?}");
    }
    assert_eq!(issuer.store.group_secrets(), 3);
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", (values("C")).clone())),
    )
    .await;
    let created = snap::ok(
        issuer
            .database
            .partition_status(&rocky_sdk::Partition::new("list".to_owned(), values("C"))),
    )
    .await;
    assert_eq!(
        created["current"]["generation"], 1,
        "key epochs follow each scope, not the revocations"
    );

    let refused = issuer
        .grant(removed_device.identity, "A")
        .await
        .unwrap_err();
    assert!(
        refused.contains("removed"),
        "a removed person gets no grant: {refused}"
    );

    let states = issuer.recipients("A").await;
    assert_eq!(states.get(&removed_device.node.to_hex()), None);
    assert_eq!(
        states[&remaining_device.node.to_hex()],
        "pending",
        "the rotation seals the new epoch to the recorded remaining reader without a share"
    );
    assert!(removed.fetch(&issuer).await.is_err(), "revoked");
    remaining.fetch(&issuer).await.unwrap();
    let (secrets, tables) = remaining.epochs("A").await;
    assert_eq!(secrets, json!([1, 2]));
    assert!(uniform(&tables, json!([1, 2])));
    let (secrets, tables) = removed.epochs("A").await;
    assert_eq!(secrets, json!([1]));
    assert!(
        uniform(&tables, json!([])),
        "the removed device holds no new key and drops the keys it held"
    );

    issuer.database.close().await.unwrap();
    let issuer = Peer::reopen(
        &root.path().join("issuer"),
        &groups(),
        issuer.store.clone(),
        issuer.hub.clone(),
    )
    .await;
    assert!(uniform(&issuer.epochs("A").await.1, json!([1, 2])));
    for peer in [issuer, removed, remaining] {
        peer.database.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_partition_granted_after_enrollment_rotates_with_its_device() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer")).await;
    for list in ["A", "B"] {
        snap::ok(
            issuer
                .database
                .create_partition(&rocky_sdk::Partition::new("list", (values(list)).clone())),
        )
        .await;
    }
    let (device_peer, device) = issuer.enroll(&root.path().join("device"), "reader").await;
    issuer.grant(device.identity, "A").await.unwrap();
    snap::ok(
        issuer
            .database
            .remove(rocky_sdk::PersonId::new(snap::public_key(
                &(device.identity.to_string()),
            ))),
    )
    .await;
    let (secrets, tables) = issuer.epochs("A").await;
    assert_eq!(secrets, json!([1, 2]), "the granted partition rotates");
    assert!(uniform(&tables, json!([1, 2])));
    let (secrets, tables) = issuer.epochs("B").await;
    assert_eq!(
        secrets,
        json!([1]),
        "a partition the device could not read keeps its epoch"
    );
    assert!(uniform(&tables, json!([1])));
    assert!(issuer.grant(device.identity, "B").await.is_err());
    device_peer.database.close().await.unwrap();
    issuer.database.close().await.unwrap();
}

#[tokio::test]
async fn an_interrupted_rotation_resumes_to_one_epoch_across_the_group() {
    let root = tempfile::tempdir().unwrap();
    let mut interrupted = 0;
    for budget in 0.. {
        let directory = root.path().join(format!("issuer{budget}"));
        let issuer = Peer::create(&directory).await;
        snap::ok(
            issuer
                .database
                .create_partition(&rocky_sdk::Partition::new("list", (values("A")).clone())),
        )
        .await;
        let (device_peer, device) = issuer
            .enroll(&root.path().join(format!("device{budget}")), "reader")
            .await;
        issuer.grant(device.identity, "A").await.unwrap();
        device_peer.database.close().await.unwrap();
        issuer.store.limit(Some(budget));
        let revoked = snap::string(issuer.database.remove(rocky_sdk::PersonId::new(
            snap::public_key(&(device.identity.to_string())),
        )))
        .await;
        issuer.store.limit(None);
        let (store, hub) = (issuer.store.clone(), issuer.hub.clone());
        issuer.database.close().await.unwrap();
        let issuer = Peer::reopen(&directory, &groups(), store, hub).await;
        let (secrets, tables) = issuer.epochs("A").await;
        assert_eq!(secrets, json!([1, 2]), "budget {budget}");
        assert!(
            uniform(&tables, json!([1, 2])),
            "budget {budget}: {tables:?}"
        );
        assert_eq!(issuer.store.group_secrets(), 2);
        issuer.database.close().await.unwrap();
        if revoked.is_ok() {
            break;
        }
        interrupted += 1;
    }
    assert!(
        interrupted >= 5,
        "only {interrupted} steps were interrupted"
    );
}

#[tokio::test]
async fn a_table_joining_the_group_later_derives_its_key_from_the_held_secret() {
    let first = group_schema(1, false);
    let second = group_schema(2, true);
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("issuer");
    let (store, hub) = (Arc::new(Store::default()), Arc::new(Hub::default()));
    let issuer = Peer::new(
        open(&directory, &first, &store, &hub)
            .create()
            .await
            .unwrap()
            .database,
        store,
        hub,
    );
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", (values("A")).clone())),
    )
    .await;
    assert_eq!(
        issuer.status("A").await["tables"].as_array().unwrap().len(),
        2
    );
    let (store, hub) = (issuer.store.clone(), issuer.hub.clone());
    issuer.database.close().await.unwrap();

    let issuer = Peer::reopen(&directory, &second, store, hub).await;
    let (secrets, tables) = issuer.epochs("A").await;
    assert_eq!(secrets, json!([1]));
    assert!(uniform(&tables, json!([1])), "{tables:?}");
    assert_eq!(issuer.store.group_secrets(), 1, "no new secret or envelope");
    let mut tx = issuer.database.transaction().await.unwrap();
    tx.execute(
        "INSERT INTO attachments(list,key) VALUES('A','photo')",
        Vec::new(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let rows = snap::ok(snap::read(
        &issuer.database,
        "SELECT key FROM attachments WHERE list='A'",
        Vec::new(),
    ))
    .await;
    assert_eq!(
        rows["rows"][0][0]["value"], "photo",
        "the derived key seals and opens a row of the new table"
    );
    issuer.database.close().await.unwrap();
}

#[tokio::test]
async fn a_granted_phone_holds_every_table_of_its_list_and_nothing_else() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer")).await;
    for list in ["A", "B"] {
        snap::ok(
            issuer
                .database
                .create_partition(&rocky_sdk::Partition::new("list", values(list))),
        )
        .await;
    }
    let (phone, _) = granted(&issuer, &root.path().join("phone"), "A", "reader").await;
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", values("C"))),
    )
    .await;
    phone.fetch(&issuer).await.unwrap();
    let subscribed: BTreeMap<String, Value> = phone.status("A").await["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|table| {
            (
                table["table"].as_str().unwrap().to_owned(),
                table["subscribed"].clone(),
            )
        })
        .collect();
    for table in TABLES {
        assert_eq!(subscribed[table], true, "{table}");
    }
    let progress = phone.database.status().await.unwrap().data.partitions;
    assert_eq!(
        progress
            .iter()
            .map(|progress| (progress.partition.clone(), progress.held))
            .collect::<Vec<_>>(),
        [(rocky_sdk::Partition::new("list", values("A")), true)],
        "the phone holds the granted list and no other"
    );
    phone.database.close().await.unwrap();
    issuer.database.close().await.unwrap();
}

async fn write(peer: &Peer, table: &str, key: &str) {
    let mut tx = peer.database.transaction().await.unwrap();
    let updates_the_one_row_per_list = table == "lists" && key != "before";
    if updates_the_one_row_per_list {
        tx.execute(
            &format!("UPDATE lists SET title='{key}' WHERE name='A'"),
            Vec::new(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        return;
    }
    let sql = match table {
        "lists" => format!("INSERT INTO lists(name,title) VALUES('A','{key}')"),
        _ => format!("INSERT INTO {table}(list,key) VALUES('A','{key}')"),
    };
    tx.execute(&sql, Vec::new()).await.unwrap();
    tx.commit().await.unwrap();
}
async fn rows(peer: &Peer) -> Vec<Vec<String>> {
    let mut all = Vec::new();
    for (table, column) in [("lists", "title"), ("items", "key"), ("attachments", "key")] {
        let column = if table == "lists" { column } else { "key" };
        let read = snap::ok(snap::read(
            &peer.database,
            &format!(
                "SELECT {column} FROM {table} WHERE {} = 'A' ORDER BY {column}",
                if table == "lists" { "name" } else { "list" }
            ),
            Vec::new(),
        ))
        .await;
        all.push(
            read["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row[0]["value"].as_str().unwrap().to_owned())
                .collect(),
        );
    }
    all
}
async fn introduce(left: &Peer, right: &Peer) {
    for (from, to) in [(left, right), (right, left)] {
        let contact = snap::ok(to.database.contact()).await;
        snap::ok(async {
            let value = from
                .database
                .add_peer(&(snap::text(&(contact["uri"]))))
                .await?;
            Ok::<_, rocky_sdk::Error>(json!({"node":value}))
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn readers_open_every_table_from_one_envelope_and_only_live_readers_open_later_rows() {
    let root = support::small_directory().unwrap();
    let issuer = Peer::create(&root.path().join("issuer")).await;
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", (values("A")).clone())),
    )
    .await;
    for table in TABLES {
        write(&issuer, table, "before").await;
    }
    let mut readers = Vec::new();
    for name in ["removed", "remaining"] {
        let (reader, device) = granted(&issuer, &root.path().join(name), "A", "reader").await;
        introduce(&issuer, &reader).await;
        readers.push((reader, device));
    }
    for (reader, _) in &readers {
        snap::ok(reader.database.sync_now()).await;
        assert_eq!(
            rows(reader).await,
            vec![vec!["before".to_owned()]; 3],
            "one sync delivers the envelope and then the list's rows"
        );
    }

    let (removed, removed_device) = &readers[0];
    let (remaining, _) = &readers[1];
    snap::ok(
        issuer
            .database
            .remove(rocky_sdk::PersonId::new(snap::public_key(
                &(removed_device.identity.to_string()),
            ))),
    )
    .await;
    for table in TABLES {
        write(&issuer, table, "after").await;
    }
    snap::ok(remaining.database.sync_now()).await;
    let both = vec!["after".to_owned(), "before".to_owned()];
    assert_eq!(
        rows(remaining).await,
        [vec!["after".to_owned()], both.clone(), both],
        "the rotation's envelope reaches the remaining reader by sync alone"
    );
    let _ = snap::string(removed.database.sync_now()).await;
    assert_eq!(
        snap::observe(snap::read(
            &removed.database,
            "SELECT key FROM items",
            Vec::new()
        ))
        .await
        .err()
        .map(|refusal| refusal["code"].clone()),
        Some(json!("removed")),
        "the removed reader is refused later rows and purges the earlier ones"
    );
    let (secrets, tables) = removed.epochs("A").await;
    assert_eq!(
        secrets,
        json!([1]),
        "the removed reader holds no key of the new epoch"
    );
    assert!(uniform(&tables, json!([])));
    for (reader, _) in readers {
        reader.database.close().await.unwrap();
    }
    issuer.database.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_identity_file_older_than_the_replica_recovers_its_groups() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer")).await;
    let directory = root.path().join("reader");
    let (reader, _) = issuer.enroll_as(&directory, Access::Read).await;
    introduce(&issuer, &reader).await;
    let older = common::authority_bytes(&directory, "identity")
        .unwrap()
        .unwrap();
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", (values("A")).clone())),
    )
    .await;
    for table in TABLES {
        write(&issuer, table, "before").await;
    }
    snap::ok(reader.database.sync_now()).await;
    let expected = vec![vec!["before".to_owned()]; 3];
    assert_eq!(rows(&reader).await, expected);
    reader.database.close().await.unwrap();
    common::write_authority(&directory, "identity", &older)
        .await
        .unwrap();
    let reader = Peer::reopen(
        &directory,
        &groups(),
        reader.store.clone(),
        reader.hub.clone(),
    )
    .await;
    assert_eq!(rows(&reader).await, expected);
    assert_eq!(reader.epochs("A").await.0, json!([1]));
    reader.database.close().await.unwrap();
    issuer.database.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partition_only_writer_obtains_authors_without_collection_wide_grants() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer")).await;
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", (values("A")).clone())),
    )
    .await;
    for table in TABLES {
        write(&issuer, table, "before").await;
    }
    let directory = root.path().join("writer");
    let (writer, _) = granted(&issuer, &directory, "A", "writer").await;
    introduce(&issuer, &writer).await;
    snap::ok(writer.database.sync_now()).await;
    snap::ok(writer.database.sync_now()).await;
    let mut tx = writer.database.transaction().await.unwrap();
    tx.execute(
        "INSERT INTO items(list,key) VALUES('A','writer')",
        Vec::new(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    snap::ok(writer.database.sync_now()).await;
    assert_eq!(
        rows(&issuer).await,
        [vec!["before"], vec!["before", "writer"], vec!["before"]],
        "syncing the envelope obtains an author per table for a partition-only writer"
    );
    writer.database.close().await.unwrap();
    issuer.database.close().await.unwrap();
}

#[tokio::test]
async fn only_an_admin_grants_a_partition() {
    let root = support::small_directory().unwrap();
    let issuer = Peer::create(&root.path().join("issuer")).await;
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", (values("A")).clone())),
    )
    .await;
    let (member, _) = granted(&issuer, &root.path().join("member"), "A", "reader").await;
    member.fetch(&issuer).await.unwrap();
    assert_eq!(member.epochs("A").await.0, json!([1]));
    let (other, other_device) = issuer.enroll(&root.path().join("other"), "reader").await;
    let refused = member.grant(other_device.identity, "A").await.unwrap_err();
    assert!(refused.contains("only an admin"), "{refused}");
    for peer in [other, member, issuer] {
        peer.database.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partition_granted_to_an_identity_reaches_a_node_it_links_later() {
    let root = support::small_directory().unwrap();
    let issuer = Peer::create(&root.path().join("issuer")).await;
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", (values("A")).clone())),
    )
    .await;
    for table in TABLES {
        write(&issuer, table, "before").await;
    }
    let person: Arc<dyn rocky_sdk::IdentityProvider> = Arc::new(rocky_sdk::IdentityKey::generate());
    let first_directory = root.path().join("first");
    let first_store = Arc::new(Store::default());
    let invitation = issuer
        .database
        .invite(
            rocky_sdk::Invite::new(Access::Role(RoleName::from_static("reader")))
                .for_identity(rocky_sdk::PersonId::new(person.public_key().unwrap()))
                .partition(rocky_sdk::Partition::new("list", values("A"))),
        )
        .await
        .unwrap();
    let first = Peer::new(
        open_with_identity(&first_directory, &first_store, &issuer.hub, &person)
            .join(&invitation.code)
            .await
            .unwrap()
            .database,
        first_store,
        issuer.hub.clone(),
    );
    let keys = NodeKeys {
        identity: person.public_key().unwrap(),
        node: first.database.me().await.unwrap().device.key(),
    };
    introduce(&issuer, &first).await;
    snap::ok(first.database.sync_now()).await;
    assert_eq!(rows(&first).await, vec![vec!["before".to_owned()]; 3]);

    let contact = snap::ok(first.database.contact()).await["uri"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_store = Arc::new(Store::default());
    let second = Peer::new(
        common::link(
            open_with_identity(
                &root.path().join("second"),
                &second_store,
                &issuer.hub,
                &person,
            ),
            person.clone(),
            &contact,
            &first.database,
        )
        .await
        .unwrap(),
        second_store,
        issuer.hub.clone(),
    );
    introduce(&issuer, &second).await;
    snap::ok(second.database.sync_now()).await;
    assert_eq!(
        rows(&second).await,
        vec![vec!["before".to_owned()]; 3],
        "a node the identity links from the first reads without an issuer acting"
    );
    let info = snap::ok(snap::device(&second.database)).await;
    assert_eq!(info["principal"], json!(keys.identity));
    assert_ne!(info["node"], json!(keys.node));

    let listed = snap::ok(issuer.database.people()).await;
    let record = listed["identities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["identity"] == json!(keys.identity))
        .unwrap()
        .clone();
    assert_eq!(record["access"], json!({"role": "reader"}));
    assert_eq!(record["partitions"].as_array().unwrap().len(), 1);
    assert!(
        record["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["node"] == json!(keys.node)),
        "one record, by identity, holds the grant for every node of it: {record}"
    );
    let before = issuer.epochs("A").await.0;
    first
        .database
        .unlink(rocky_sdk::DeviceId::new(
            info["node"].as_str().unwrap().parse().unwrap(),
        ))
        .await
        .unwrap();
    snap::ok(first.database.sync_now()).await;
    snap::ok(issuer.database.sync_now()).await;
    let (epochs, tables) = issuer.epochs("A").await;
    assert_ne!(epochs, before, "unlinking one reader rotates its partition");
    assert!(uniform(&tables, epochs));
    snap::ok(first.database.sync_now()).await;
    assert_eq!(
        rows(&first).await,
        vec![vec!["before".to_owned()]; 3],
        "the sibling keeps the identity's grant and reads the replacement epoch"
    );
    assert!(snap::string(second.database.sync_now()).await.is_err());
    for peer in [first, second, issuer] {
        peer.database.close().await.unwrap();
    }
}

#[test]
fn the_budgeted_store_meets_the_key_provider_contract() {
    rocky_credentials::testing::key_provider_contract(&Store::default());
}

async fn full_person(issuer: &Peer, root: &Path, name: &str) -> (Peer, Peer, PublicKey) {
    let person: Arc<dyn rocky_sdk::IdentityProvider> = Arc::new(rocky_sdk::IdentityKey::generate());
    let identity = person.public_key().unwrap();
    let invitation = issuer
        .database
        .invite(Invite::new(Access::Full).for_identity(rocky_sdk::PersonId::new(identity)))
        .await
        .unwrap();
    let laptop_store = Arc::new(Store::default());
    let laptop = Peer::new(
        open_with_identity(
            &root.join(format!("{name}-laptop")),
            &laptop_store,
            &issuer.hub,
            &person,
        )
        .join(&invitation.code)
        .await
        .unwrap()
        .database,
        laptop_store,
        issuer.hub.clone(),
    );
    let contact = snap::ok(laptop.database.contact()).await["uri"]
        .as_str()
        .unwrap()
        .to_owned();
    let phone_store = Arc::new(Store::default());
    let phone = Peer::new(
        common::link(
            open_with_identity(
                &root.join(format!("{name}-phone")),
                &phone_store,
                &issuer.hub,
                &person,
            ),
            person.clone(),
            &contact,
            &laptop.database,
        )
        .await
        .unwrap(),
        phone_store,
        issuer.hub.clone(),
    );
    for device in [&laptop, &phone] {
        introduce(issuer, device).await;
        snap::ok(device.database.sync_now()).await;
    }
    (laptop, phone, identity)
}
async fn holds(peer: &Peer) -> Vec<(rocky_sdk::Partition, bool)> {
    peer.database
        .status()
        .await
        .unwrap()
        .data
        .partitions
        .into_iter()
        .map(|progress| (progress.partition, progress.held))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_members_phone_and_laptop_open_a_list_created_after_they_joined() {
    let root = support::small_directory().unwrap();
    let issuer = Peer::create(&root.path().join("issuer")).await;
    let (laptop, phone, _) = full_person(&issuer, root.path(), "member").await;
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", values("A"))),
    )
    .await;
    for table in TABLES {
        write(&issuer, table, "before").await;
    }
    for device in [&laptop, &phone] {
        snap::ok(device.database.sync_now()).await;
        assert_eq!(
            rows(device).await,
            vec![vec!["before".to_owned()]; 3],
            "every device of a full member opens the new list without an application call"
        );
        assert_eq!(
            holds(device).await,
            [(rocky_sdk::Partition::new("list", values("A")), true)]
        );
    }
    for peer in [laptop, phone, issuer] {
        peer.database.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_store_relay_holds_a_new_lists_frames_but_never_its_secret() {
    let root = support::small_directory().unwrap();
    let issuer = Peer::create(&root.path().join("issuer")).await;
    let (relay, _) = issuer
        .enroll_as(&root.path().join("relay"), Access::Store)
        .await;
    let (member, _) = issuer
        .enroll_as(&root.path().join("member"), Access::Full)
        .await;
    introduce(&issuer, &relay).await;
    introduce(&member, &relay).await;
    snap::ok(member.database.set_sync(rocky_sdk::SyncMode::Off)).await;
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", values("A"))),
    )
    .await;
    for table in TABLES {
        write(&issuer, table, "before").await;
    }
    snap::ok(issuer.database.sync_now()).await;
    issuer.database.close().await.unwrap();
    snap::ok(member.database.set_sync(rocky_sdk::SyncMode::Manual)).await;
    for _ in 0..2 {
        let _issuer_is_gone = snap::observe(member.database.sync_now()).await;
    }
    assert_eq!(
        rows(&member).await,
        vec![vec!["before".to_owned()]; 3],
        "the member opens the list with the relay as its only reachable peer"
    );
    assert_eq!(relay.store.group_secrets(), 0, "the relay holds no secret");
    assert!(holds(&relay).await.is_empty());
    for peer in [member, relay] {
        peer.database.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_persons_devices_get_nothing_created_after_the_removal() {
    let root = support::small_directory().unwrap();
    let issuer = Peer::create(&root.path().join("issuer")).await;
    let (laptop, phone, identity) = full_person(&issuer, root.path(), "member").await;
    let nodes = [
        laptop.database.me().await.unwrap().device.key().to_hex(),
        phone.database.me().await.unwrap().device.key().to_hex(),
    ];
    snap::ok(issuer.database.remove(rocky_sdk::PersonId::new(identity))).await;
    snap::ok(
        issuer
            .database
            .create_partition(&rocky_sdk::Partition::new("list", values("A"))),
    )
    .await;
    for table in TABLES {
        write(&issuer, table, "after").await;
    }
    let recipients = issuer.recipients("A").await;
    for node in &nodes {
        assert_eq!(
            recipients.get(node),
            None,
            "no envelope is sealed to {node}"
        );
    }
    for device in [&laptop, &phone] {
        let _removed = snap::observe(device.database.sync_now()).await;
        assert_eq!(device.store.group_secrets(), 0);
        assert!(holds(device).await.is_empty());
    }
    for peer in [laptop, phone, issuer] {
        peer.database.close().await.unwrap();
    }
}
