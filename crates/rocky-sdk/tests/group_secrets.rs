//! Group secrets: one envelope shares every table of a group partition,
//! revocation rotates the group as one unit, and the holding rule keeps
//! a group partition whole unless a table is left out explicitly. See
//! `docs/runtime/partitioned-scopes.md#group-secrets`.
use rocky_sdk::{
    Database, DeviceKeys, Network, Open, SecretStore, Session, credentials::ProviderError,
    transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

const SCHEMA: &str = r#"{"format":4,"application":"groupsecrets","version":1,"tables":[
{"name":"notes","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]},
{"name":"lists","class":"mergeable","columns":[{"name":"name","type":"text","not_null":true},{"name":"title","type":"text"}],"primary_key":["name"],"partition_by":["name"],"partition_group":"list"},
{"name":"items","class":"mergeable","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"title","type":"text"}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"},
{"name":"attachments","class":"mergeable","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"bytes","type":"blob"}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}]}"#;
const TABLES: [&str; 3] = ["lists", "items", "attachments"];

/// An in-memory key provider that fails every operation once its budget
/// of operations is spent, to interrupt a rotation at each step.
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
    database: Arc<Database>,
    session: Session,
    scope: Value,
    store: Arc<Store>,
}
fn open<'a>(directory: &'a Path, schema: &'a str, store: &Arc<Store>) -> Open<'a> {
    Open {
        directory,
        schema,
        secrets: Some(store.clone() as Arc<dyn SecretStore>),
        network: None,
        commands: None,
    }
}
impl Peer {
    fn new(database: Arc<Database>, store: Arc<Store>) -> Self {
        Self {
            session: database.session(),
            scope: serde_json::from_str(&database.scope()).unwrap(),
            database,
            store,
        }
    }
    fn create(directory: &Path) -> Self {
        let store = Arc::new(Store::default());
        let database = Database::create(open(directory, SCHEMA, &store), None)
            .unwrap()
            .database;
        Self::new(database, store)
    }
    fn reopen(directory: &Path, schema: &str, store: Arc<Store>) -> Self {
        let database = Database::open(open(directory, schema, &store))
            .unwrap()
            .database;
        Self::new(database, store)
    }
    async fn reply(&self, request: Value) -> Result<Value, String> {
        let reply: Value = serde_json::from_str(
            &self
                .session
                .call(json!({"version":1,"scope":self.scope,"request":request}).to_string())
                .await
                .unwrap(),
        )
        .unwrap();
        if reply["status"] == "ok" {
            Ok(reply["value"].clone())
        } else {
            Err(reply["error"].to_string())
        }
    }
    async fn ok(&self, request: Value) -> Value {
        match self.reply(request.clone()).await {
            Ok(value) => value,
            Err(error) => panic!("{request} failed: {error}"),
        }
    }
    async fn status(&self, list: &str) -> Value {
        self.ok(json!({"type":"partition_status","group":"list","values":values(list)}))
            .await
    }
    /// The held secret epochs and each table's installed key epochs.
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
    /// Selector grants of whole-row `rights` on list `list` of `tables`.
    async fn grants(&self, list: &str, tables: &[&str], rights: &[&str]) -> Value {
        let mut grants = Vec::new();
        for table in tables {
            let address = self
                .ok(json!({"type":"partition_address","table":table,"values":values(list)}))
                .await;
            let part = &address["tuple"][0];
            grants.push(json!({"domain":self.scope,"table":address["scope"]["table"],"selector":[[part[0],{"Only":[part[1]]}]],"rows":"All","rights":rights,"fields":[]}));
        }
        grants.into()
    }
    async fn grant(&self, device: &DeviceKeys, grants: Value) -> Result<Value, String> {
        self.reply(json!({"type":"partition_grant","device":device,"partitions":grants}))
            .await
            .map(|granted| granted["supplied"].clone())
    }
    async fn share(&self, list: &str, recipients: &[&DeviceKeys]) -> Result<Value, String> {
        let recipients: Vec<_> = recipients.iter().map(|device| device.node).collect();
        self.reply(json!({"type":"partition_share","group":"list","values":values(list),"recipients":recipients}))
            .await
    }
    async fn install(&self, list: &str, envelope: &Value) -> Result<Value, String> {
        self.reply(json!({"type":"partition_install","group":"list","values":values(list),"envelope":envelope}))
            .await
    }
}
fn values(list: &str) -> Value {
    json!([{"type":"text","value":list}])
}
fn uniform(tables: &BTreeMap<String, Value>, epochs: Value) -> bool {
    tables.len() == TABLES.len() && tables.values().all(|held| *held == epochs)
}
/// A device prepared for supplied credentials, without a network.
fn prepare(directory: &Path) -> (Arc<Store>, DeviceKeys) {
    let store = Arc::new(Store::default());
    let device = Database::device(directory, Some(store.clone() as Arc<dyn SecretStore>)).unwrap();
    (store, device)
}
fn install(directory: &Path, store: Arc<Store>, supplied: Value) -> Peer {
    install_on(directory, store, supplied, None)
}
fn install_on(
    directory: &Path,
    store: Arc<Store>,
    supplied: Value,
    network: Option<Network>,
) -> Peer {
    let database = Database::install(
        Open {
            network,
            ..open(directory, SCHEMA, &store)
        },
        serde_json::from_value(supplied).unwrap(),
    )
    .unwrap()
    .database;
    Peer::new(database, store)
}
/// A reader granted `rights` on list `list` of `tables`, holding no secret.
async fn granted(
    issuer: &Peer,
    directory: &Path,
    list: &str,
    tables: &[&str],
    rights: &[&str],
) -> (Peer, DeviceKeys) {
    let (store, device) = prepare(directory);
    let grants = issuer.grants(list, tables, rights).await;
    let supplied = issuer.grant(&device, grants).await.unwrap();
    (install(directory, store, supplied), device)
}

#[tokio::test]
async fn one_envelope_shares_a_group_and_installs_only_with_read_on_every_table() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer"));
    let created = issuer
        .ok(json!({"type":"partition_create","group":"list","values":values("A")}))
        .await;
    assert_eq!(created["epoch"]["generation"], 1);
    // Creating again is idempotent: the same secret and epoch.
    assert_eq!(
        issuer
            .ok(json!({"type":"partition_create","table":"items","values":values("A")}))
            .await,
        created
    );
    assert_eq!(issuer.store.group_secrets(), 1);
    let (secrets, tables) = issuer.epochs("A").await;
    assert_eq!(secrets, json!([1]));
    assert!(uniform(&tables, json!([1])));

    let (reader, device) = granted(
        &issuer,
        &root.path().join("reader"),
        "A",
        &TABLES,
        &["Read"],
    )
    .await;
    let shared = issuer.share("A", &[&device]).await.unwrap();
    let envelopes = shared["envelopes"].as_array().unwrap();
    assert_eq!(envelopes.len(), 1, "one envelope per recipient");
    assert_eq!(envelopes[0]["recipient"], device.node.to_hex());

    // A reader lacking read on one table of the group cannot install, and
    // a refused envelope leaves no secret, epoch or registration behind.
    let (partial, partial_device) = granted(
        &issuer,
        &root.path().join("partial"),
        "A",
        &["lists", "items"],
        &["Read"],
    )
    .await;
    let foreign = issuer.share("A", &[&partial_device]).await.unwrap();
    let refused = partial
        .install("A", &foreign["envelopes"][0]["envelope"])
        .await
        .unwrap_err();
    assert!(refused.contains("attachments lacks it"), "{refused}");
    assert!(
        partial
            .install("A", &envelopes[0]["envelope"])
            .await
            .is_err(),
        "another recipient's envelope"
    );
    assert_eq!(partial.store.group_secrets(), 0);
    let (secrets, tables) = partial.epochs("A").await;
    assert_eq!(secrets, json!([]));
    assert!(uniform(&tables, json!([])));
    assert_eq!(
        partial.status("A").await["tables"][0]["holding"],
        Value::Null
    );

    // The recipient installs every table's key from the one envelope.
    let installed = reader
        .install("A", &envelopes[0]["envelope"])
        .await
        .unwrap();
    assert_eq!(installed["epoch"]["generation"], 1);
    let (secrets, tables) = reader.epochs("A").await;
    assert_eq!(secrets, json!([1]));
    assert!(uniform(&tables, json!([1])), "{tables:?}");
    // One secret is stored; table keys are derived, never stored.
    assert_eq!(reader.store.group_secrets(), 1);
    reader.database.close().await.unwrap();
    let reader = Peer::reopen(&root.path().join("reader"), SCHEMA, reader.store.clone());
    assert!(uniform(&reader.epochs("A").await.1, json!([1])));
    for peer in [reader, partial, issuer] {
        peer.database.close().await.unwrap();
    }
}

#[tokio::test]
async fn revocation_rotates_the_group_once_and_shares_only_the_current_epoch_to_live_devices() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer"));
    for list in ["A", "B"] {
        issuer
            .ok(json!({"type":"partition_create","group":"list","values":values(list)}))
            .await;
    }
    let (removed, removed_device) = granted(
        &issuer,
        &root.path().join("removed"),
        "A",
        &TABLES,
        &["Read"],
    )
    .await;
    let (remaining, remaining_device) = granted(
        &issuer,
        &root.path().join("remaining"),
        "A",
        &TABLES,
        &["Read"],
    )
    .await;
    let shared = issuer
        .share("A", &[&removed_device, &remaining_device])
        .await
        .unwrap();
    for (peer, device) in [(&removed, &removed_device), (&remaining, &remaining_device)] {
        let envelope = shared["envelopes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|envelope| envelope["recipient"] == device.node.to_hex())
            .unwrap();
        peer.install("A", &envelope["envelope"]).await.unwrap();
    }

    // The issuer knows the removed device only through its partition grant.
    let revoked = issuer
        .ok(json!({"type":"revoke","node":removed_device.node.to_string()}))
        .await;
    assert_eq!(revoked["revocations"]["statements"], 1);
    for (list, expected) in [("A", json!([1, 2])), ("B", json!([1]))] {
        let (secrets, tables) = issuer.epochs(list).await;
        assert_eq!(secrets, expected, "{list}");
        assert!(uniform(&tables, expected), "{list}: {tables:?}");
    }
    assert_eq!(issuer.store.group_secrets(), 3);
    // A partition created after the rotation starts at the first epoch:
    // key epochs follow each scope, not the revocations.
    let created = issuer
        .ok(json!({"type":"partition_create","group":"list","values":values("C")}))
        .await;
    assert_eq!(created["epoch"]["generation"], 1);

    // Sharing refuses the revoked device and any epoch but the current.
    let refused = issuer.share("A", &[&removed_device]).await.unwrap_err();
    assert!(refused.contains("revoked"), "{refused}");
    let refused = issuer
        .reply(json!({"type":"partition_share","group":"list","values":values("A"),"recipients":[remaining_device.node],"epoch":created["epoch"]}))
        .await
        .unwrap_err();
    assert!(refused.contains("current epoch"), "{refused}");
    // Its user is revoked too: a fresh node key under it gets no grant.
    let (_, fresh) = prepare(&root.path().join("fresh"));
    let renamed = DeviceKeys {
        user: removed_device.user,
        node: fresh.node,
    };
    let refused = issuer
        .grant(&renamed, issuer.grants("A", &TABLES, &["Read"]).await)
        .await
        .unwrap_err();
    assert!(refused.contains("revoked"), "{refused}");

    // One new envelope per remaining reader delivers the new epoch.
    let shared = issuer.share("A", &[&remaining_device]).await.unwrap();
    assert_eq!(shared["epoch"]["generation"], 2);
    let envelope = &shared["envelopes"][0]["envelope"];
    assert!(removed.install("A", envelope).await.is_err());
    remaining.install("A", envelope).await.unwrap();
    let (secrets, tables) = remaining.epochs("A").await;
    assert_eq!(secrets, json!([1, 2]));
    assert!(uniform(&tables, json!([1, 2])));
    let (secrets, tables) = removed.epochs("A").await;
    assert_eq!(secrets, json!([1]));
    assert!(
        uniform(&tables, json!([1])),
        "the removed device holds no new key"
    );

    issuer.database.close().await.unwrap();
    let issuer = Peer::reopen(&root.path().join("issuer"), SCHEMA, issuer.store.clone());
    assert!(uniform(&issuer.epochs("A").await.1, json!([1, 2])));
    for peer in [issuer, removed, remaining] {
        peer.database.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_partition_granted_after_enrollment_rotates_with_its_device() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer"));
    for list in ["A", "B"] {
        issuer
            .ok(json!({"type":"partition_create","group":"list","values":values(list)}))
            .await;
    }
    // Enrolled to read only `notes`, then granted list A of the group.
    let (_, device) = prepare(&root.path().join("device"));
    issuer
        .ok(json!({"type":"grant","device":device,"profile":{"kind":"reader","scopes":["notes"]}}))
        .await;
    issuer
        .grant(&device, issuer.grants("A", &TABLES, &["Read"]).await)
        .await
        .unwrap();
    issuer
        .ok(json!({"type":"revoke","node":device.node.to_string()}))
        .await;
    let (secrets, tables) = issuer.epochs("A").await;
    assert_eq!(secrets, json!([1, 2]), "the granted partition rotates");
    assert!(uniform(&tables, json!([1, 2])));
    // Nothing the device could read selects B: it keeps its epoch.
    let (secrets, tables) = issuer.epochs("B").await;
    assert_eq!(secrets, json!([1]));
    assert!(uniform(&tables, json!([1])));
    let renamed = DeviceKeys {
        user: device.user,
        node: prepare(&root.path().join("fresh")).1.node,
    };
    assert!(
        issuer
            .grant(&renamed, issuer.grants("B", &TABLES, &["Read"]).await)
            .await
            .is_err()
    );
    issuer.database.close().await.unwrap();
}

#[tokio::test]
async fn an_interrupted_rotation_resumes_to_one_epoch_across_the_group() {
    let root = tempfile::tempdir().unwrap();
    let mut interrupted = 0;
    for budget in 0.. {
        let directory = root.path().join(format!("issuer{budget}"));
        let issuer = Peer::create(&directory);
        issuer
            .ok(json!({"type":"partition_create","group":"list","values":values("A")}))
            .await;
        let (_, device) = prepare(&root.path().join(format!("device{budget}")));
        issuer
            .grant(&device, issuer.grants("A", &TABLES, &["Read"]).await)
            .await
            .unwrap();
        issuer.store.limit(Some(budget));
        let revoked = issuer
            .reply(json!({"type":"revoke","node":device.node.to_string()}))
            .await;
        issuer.store.limit(None);
        let store = issuer.store.clone();
        issuer.database.close().await.unwrap();
        let issuer = Peer::reopen(&directory, SCHEMA, store);
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
    let first = SCHEMA.replace(
            r#",
{"name":"attachments","class":"mergeable","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"bytes","type":"blob"}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}"#,
            "",
        );
    let second = SCHEMA.replace(r#""version":1"#, r#""version":2"#);
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("issuer");
    let store = Arc::new(Store::default());
    let issuer = Peer::new(
        Database::create(open(&directory, &first, &store), None)
            .unwrap()
            .database,
        store,
    );
    issuer
        .ok(json!({"type":"partition_create","group":"list","values":values("A")}))
        .await;
    assert_eq!(
        issuer.status("A").await["tables"].as_array().unwrap().len(),
        2
    );
    let store = issuer.store.clone();
    issuer.database.close().await.unwrap();

    let issuer = Peer::reopen(&directory, &second, store);
    let (secrets, tables) = issuer.epochs("A").await;
    assert_eq!(secrets, json!([1]));
    assert!(uniform(&tables, json!([1])), "{tables:?}");
    assert_eq!(issuer.store.group_secrets(), 1, "no new secret or envelope");
    // The derived key seals and opens a row of the new table.
    let request = issuer
        .ok(json!({"type":"pseudonym_request","collection":"attachments"}))
        .await;
    let assigned = issuer
        .ok(json!({"type":"pseudonym_assign","request":request["request"]}))
        .await;
    issuer
        .ok(json!({"type":"pseudonym_install","pseudonym":assigned["pseudonym"]}))
        .await;
    issuer.ok(json!({"type":"sql","sql":"BEGIN"})).await;
    let id = issuer
        .ok(json!({"type":"next_id","table":"attachments","partition_values":values("A")}))
        .await;
    issuer.ok(json!({"type":"sql","sql":"INSERT INTO attachments(list,key,id) VALUES('A','photo',?)","values":[{"type":"integer","value":id["id"]}]})).await;
    issuer.ok(json!({"type":"sql","sql":"COMMIT"})).await;
    let rows = issuer
        .ok(json!({"type":"read","query":{"sql":"SELECT key FROM attachments WHERE list='A'","values":[],"max_rows":4}}))
        .await;
    assert_eq!(rows["rows"][0][0]["value"], "photo");
    issuer.database.close().await.unwrap();
}

#[tokio::test]
async fn a_phone_holds_a_list_and_its_items_while_leaving_out_attachments() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer"));
    issuer
        .ok(json!({"type":"partition_create","group":"list","values":values("A")}))
        .await;
    let (phone, device) =
        granted(&issuer, &root.path().join("phone"), "A", &TABLES, &["Read"]).await;
    let shared = issuer.share("A", &[&device]).await.unwrap();
    phone
        .install("A", &shared["envelopes"][0]["envelope"])
        .await
        .unwrap();
    phone
        .ok(json!({"type":"partition_role","role":{"role":"Partial","full_nodes":[],"custody":0}}))
        .await;
    for omit in [json!(["photos"]), json!(TABLES)] {
        assert!(
            phone
                .reply(json!({"type":"partition_subscribe","group":"list","values":values("A"),"omit":omit}))
                .await
                .is_err(),
            "{omit}"
        );
    }
    phone
        .ok(json!({"type":"partition_subscribe","group":"list","values":values("A"),"omit":["attachments"]}))
        .await;
    let holding: BTreeMap<String, Value> = phone.status("A").await["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|table| {
            (
                table["table"].as_str().unwrap().to_owned(),
                table["holding"].clone(),
            )
        })
        .collect();
    assert_eq!(holding["lists"], "held");
    assert_eq!(holding["items"], "held");
    assert_eq!(holding["attachments"], "left");
    // The whole group partition is released together.
    phone
        .ok(json!({"type":"partition_unsubscribe","group":"list","values":values("A")}))
        .await;
    let status = phone.status("A").await;
    assert!(
        status["tables"]
            .as_array()
            .unwrap()
            .iter()
            .all(|table| table["holding"] == "left"),
        "{status}"
    );
    phone.database.close().await.unwrap();
    issuer.database.close().await.unwrap();
}

/// Installs a table pseudonym and writes one row of list A.
async fn write(peer: &Peer, table: &str, key: &str) {
    let request = peer
        .ok(json!({"type":"pseudonym_request","collection":table}))
        .await;
    if request
        .get("request")
        .is_some_and(|request| !request.is_null())
    {
        let assigned = peer
            .ok(json!({"type":"pseudonym_assign","request":request["request"]}))
            .await;
        peer.ok(json!({"type":"pseudonym_install","pseudonym":assigned["pseudonym"]}))
            .await;
    }
    peer.ok(json!({"type":"sql","sql":"BEGIN"})).await;
    if table == "lists" && key != "before" {
        // One row per list: a later write updates it.
        peer.ok(
            json!({"type":"sql","sql":format!("UPDATE lists SET title='{key}' WHERE name='A'")}),
        )
        .await;
        peer.ok(json!({"type":"sql","sql":"COMMIT"})).await;
        return;
    }
    let id = peer
        .ok(json!({"type":"next_id","table":table,"partition_values":values("A")}))
        .await;
    let sql = match table {
        "lists" => format!("INSERT INTO lists(name,title,id) VALUES('A','{key}',?)"),
        _ => format!("INSERT INTO {table}(list,key,id) VALUES('A','{key}',?)"),
    };
    peer.ok(json!({"type":"sql","sql":sql,"values":[{"type":"integer","value":id["id"]}]}))
        .await;
    peer.ok(json!({"type":"sql","sql":"COMMIT"})).await;
}
/// The keys of list A's rows in every table of the group, in table order.
async fn rows(peer: &Peer) -> Vec<Vec<String>> {
    let mut all = Vec::new();
    for (table, column) in [("lists", "title"), ("items", "key"), ("attachments", "key")] {
        let column = if table == "lists" { column } else { "key" };
        let read = peer
            .ok(json!({"type":"read","query":{"sql":format!("SELECT {column} FROM {table} WHERE {} = 'A' ORDER BY {column}", if table == "lists" { "name" } else { "list" }),"values":[],"max_rows":64}}))
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
        let contact = to.ok(json!({"type":"contact"})).await;
        from.ok(json!({"type":"peer_add","contact":contact["uri"]}))
            .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn readers_open_every_table_from_one_envelope_and_only_live_readers_open_later_rows() {
    let root = tempfile::tempdir().unwrap();
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let store = Arc::new(Store::default());
    let issuer = Peer::new(
        Database::create(
            Open {
                network: network("issuer"),
                ..open(&root.path().join("issuer"), SCHEMA, &store)
            },
            None,
        )
        .unwrap()
        .database,
        store,
    );
    issuer
        .ok(json!({"type":"partition_create","group":"list","values":values("A")}))
        .await;
    for table in TABLES {
        write(&issuer, table, "before").await;
    }
    let mut readers = Vec::new();
    for name in ["removed", "remaining"] {
        let directory = root.path().join(name);
        let (store, device) = prepare(&directory);
        let supplied = issuer
            .grant(&device, issuer.grants("A", &TABLES, &["Read"]).await)
            .await
            .unwrap();
        let reader = install_on(&directory, store, supplied, network(name));
        introduce(&issuer, &reader).await;
        readers.push((reader, device));
    }
    let devices: Vec<&DeviceKeys> = readers.iter().map(|(_, device)| device).collect();
    let shared = issuer.share("A", &devices).await.unwrap();
    assert_eq!(shared["envelopes"].as_array().unwrap().len(), 2);
    for (reader, device) in &readers {
        let envelope = shared["envelopes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|envelope| envelope["recipient"] == device.node.to_hex())
            .unwrap();
        reader.install("A", &envelope["envelope"]).await.unwrap();
        assert_eq!(
            reader.ok(json!({"type":"authors"})).await["installed"],
            json!([])
        );
        reader.ok(json!({"type":"sync"})).await;
        assert_eq!(rows(reader).await, vec![vec!["before".to_owned()]; 3]);
    }

    let (removed, removed_device) = &readers[0];
    let (remaining, remaining_device) = &readers[1];
    issuer
        .ok(json!({"type":"revoke","node":removed_device.node.to_string()}))
        .await;
    for table in TABLES {
        write(&issuer, table, "after").await;
    }
    let shared = issuer.share("A", &[remaining_device]).await.unwrap();
    remaining
        .install("A", &shared["envelopes"][0]["envelope"])
        .await
        .unwrap();
    remaining.ok(json!({"type":"sync"})).await;
    let both = vec!["after".to_owned(), "before".to_owned()];
    assert_eq!(
        rows(remaining).await,
        [vec!["after".to_owned()], both.clone(), both]
    );
    // The removed reader is refused and holds no key of the new epoch.
    let _ = removed.reply(json!({"type":"sync"})).await;
    assert_eq!(rows(removed).await, vec![vec!["before".to_owned()]; 3]);
    let (secrets, tables) = removed.epochs("A").await;
    assert_eq!(secrets, json!([1]));
    assert!(uniform(&tables, json!([1])));
    for (reader, _) in readers {
        reader.database.close().await.unwrap();
    }
    issuer.database.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partition_only_writer_obtains_authors_without_collection_wide_grants() {
    let root = tempfile::tempdir().unwrap();
    let hub = Arc::new(Hub::default());
    let store = Arc::new(Store::default());
    let issuer = Peer::new(
        Database::create(
            Open {
                network: Some(Network::Owned(hub.connector("issuer"))),
                ..open(&root.path().join("issuer"), SCHEMA, &store)
            },
            None,
        )
        .unwrap()
        .database,
        store,
    );
    issuer
        .ok(json!({"type":"partition_create","group":"list","values":values("A")}))
        .await;
    for table in TABLES {
        write(&issuer, table, "before").await;
    }
    let directory = root.path().join("writer");
    let (store, device) = prepare(&directory);
    let mut grants = issuer
        .grants("A", &TABLES, &["Read", "Insert", "Update", "Delete"])
        .await;
    // A writer needs paired field rights as well as rights on its selected rows.
    let identity: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("issuer/identity.json")).unwrap())
            .unwrap();
    let credentials: rocky_sdk::credentials::CredentialChain =
        serde_json::from_value(identity["credentials"].clone()).unwrap();
    for grant in grants.as_array_mut().unwrap() {
        let full = credentials
            .grants()
            .iter()
            .find(|full| json!(full.scope.table) == grant["table"])
            .unwrap();
        grant["fields"] = json!(full.fields);
    }
    let supplied = issuer.grant(&device, grants).await.unwrap();
    let writer = install_on(
        &directory,
        store,
        supplied,
        Some(Network::Owned(hub.connector("writer"))),
    );
    introduce(&issuer, &writer).await;
    let shared = issuer.share("A", &[&device]).await.unwrap();
    writer
        .install("A", &shared["envelopes"][0]["envelope"])
        .await
        .unwrap();
    let authors = writer.ok(json!({"type":"authors"})).await;
    assert_eq!(authors["installed"].as_array().unwrap().len(), TABLES.len());
    assert_eq!(
        writer.ok(json!({"type":"authors"})).await["installed"],
        json!([])
    );
    writer.ok(json!({"type":"sync"})).await;
    writer.ok(json!({"type":"sql","sql":"BEGIN"})).await;
    let id = writer
        .ok(json!({"type":"next_id","table":"items","partition_values":values("A")}))
        .await;
    writer
        .ok(
            json!({"type":"sql","sql":"INSERT INTO items(list,key,id) VALUES('A','writer',?)",
        "values":[{"type":"integer","value":id["id"]}]}),
        )
        .await;
    writer.ok(json!({"type":"sql","sql":"COMMIT"})).await;
    writer.ok(json!({"type":"sync"})).await;
    assert_eq!(
        rows(&issuer).await,
        [vec!["before"], vec!["before", "writer"], vec!["before"]]
    );
    writer.database.close().await.unwrap();
    issuer.database.close().await.unwrap();
}
