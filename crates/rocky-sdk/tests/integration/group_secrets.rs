use crate::common;
use crate::support::snapshot as snap;
use rocky_sdk::{
    Database, Network, NodeKeys, Open, SecretStore, Session, credentials::ProviderError,
    transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

use crate::support;

const SCHEMA: &str = r#"{"format":6,"application":"groupsecrets","version":1,"tables":[
{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]},
{"name":"lists","class":"replayed","columns":[{"name":"name","type":"text","not_null":true},{"name":"title","type":"text"}],"primary_key":["name"],"partition_by":["name"],"partition_group":"list"},
{"name":"items","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"title","type":"text"}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"},
{"name":"attachments","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"bytes","type":"blob"}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}]}"#;
const TABLES: [&str; 3] = ["lists", "items", "attachments"];

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
    scope: rocky_sdk::Scope,
    store: Arc<Store>,
    hub: Arc<Hub>,
}
fn open<'a>(directory: &'a Path, schema: &'a str, store: &Arc<Store>, hub: &Arc<Hub>) -> Open<'a> {
    let name = directory.file_name().unwrap().to_str().unwrap();
    Open {
        identity: common::prepared_identity(directory),
        directory,
        schema: crate::support::snapshot::schema(schema),
        secrets: Some(store.clone() as Arc<dyn SecretStore>),
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}
fn open_with_identity<'a>(
    directory: &'a Path,
    store: &Arc<Store>,
    hub: &Arc<Hub>,
    identity: &Arc<dyn rocky_sdk::IdentityProvider>,
) -> Open<'a> {
    let mut open = open(directory, SCHEMA, store, hub);
    open.identity = Some(Arc::clone(identity));
    open
}
impl Peer {
    fn new(database: Arc<Database>, store: Arc<Store>, hub: Arc<Hub>) -> Self {
        Self {
            session: database.session(),
            scope: database.scope(),
            database,
            store,
            hub,
        }
    }
    fn create(directory: &Path) -> Self {
        let (store, hub) = (Arc::new(Store::default()), Arc::new(Hub::default()));
        let database = Database::create(open(directory, SCHEMA, &store, &hub), None)
            .unwrap()
            .database;
        Self::new(database, store, hub)
    }
    fn reopen(directory: &Path, schema: &str, store: Arc<Store>, hub: Arc<Hub>) -> Self {
        let database = Database::open(open(directory, schema, &store, &hub))
            .unwrap()
            .database;
        Self::new(database, store, hub)
    }

    async fn status(&self, list: &str) -> Value {
        snap::ok(
            self.session
                .partition_status(rocky_sdk::PartitionTarget::Group {
                    group: "list".to_owned(),
                    values: (values(list)).clone(),
                }),
        )
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
    async fn grants(&self, list: &str, tables: &[&str], rights: &[&str]) -> Value {
        let mut grants = Vec::new();
        for table in tables {
            let address = snap::ok(
                self.session
                    .partition_address(&(snap::text(&(table))), (values(list)).clone()),
            )
            .await;
            let part = &address["tuple"][0];
            grants.push(json!({"domain":self.scope,"table":address["scope"]["table"],"selector":[[part[0],{"Only":[part[1]]}]],"rows":"All","rights":rights,"fields":[]}));
        }
        grants.into()
    }
    async fn grant(&self, device: &NodeKeys, grants: Value) -> Result<Value, String> {
        snap::string(
            self.session.grant_partitions(
                serde_json::from_value::<rocky_sdk::NodeKeys>(json!(device))
                    .expect("typed fixture"),
                serde_json::from_value::<Vec<rocky_sdk::credentials::PartitionGrant>>(json!(
                    grants
                ))
                .expect("typed fixture"),
                1,
            ),
        )
        .await
        .map(|granted| granted["supplied"].clone())
    }
    async fn share(&self, list: &str, recipients: &[&NodeKeys]) -> Result<Value, String> {
        let recipients: Vec<_> = recipients.iter().map(|device| device.node).collect();
        snap::string(
            self.session.share_partition(
                rocky_sdk::PartitionTarget::Group {
                    group: "list".to_owned(),
                    values: (values(list)).clone(),
                },
                serde_json::from_value::<
                    std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>,
                >(json!(recipients))
                .expect("typed fixture"),
                None,
            ),
        )
        .await
    }
    async fn fetch(&self, from: &Peer) -> Result<Value, String> {
        let contact = snap::ok(from.session.contact_info()).await;
        snap::string(async {
            let value = self
                .session
                .add_peer(&(snap::text(&(contact["uri"]))))
                .await?;
            Ok(json!({"node":value}))
        })
        .await?;
        snap::string(self.session.sync_once()).await
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
    async fn events(&self, kind: &str) -> Vec<Value> {
        snap::ok(self.session.activity(0)).await["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == kind)
            .cloned()
            .collect()
    }
}
fn values(list: &str) -> Vec<rocky_sdk::SqlValue> {
    vec![rocky_sdk::SqlValue::Text(snap::text(&(list)))]
}
fn uniform(tables: &BTreeMap<String, Value>, epochs: Value) -> bool {
    tables.len() == TABLES.len() && tables.values().all(|held| *held == epochs)
}
fn prepare(directory: &Path) -> (Arc<Store>, NodeKeys) {
    let store = Arc::new(Store::default());
    let device = common::node_keys(directory, Some(store.clone() as Arc<dyn SecretStore>)).unwrap();
    (store, device)
}
fn install(directory: &Path, store: Arc<Store>, supplied: Value, hub: &Arc<Hub>) -> Peer {
    let database = Database::install(
        open(directory, SCHEMA, &store, hub),
        serde_json::from_value(supplied).unwrap(),
    )
    .unwrap()
    .database;
    Peer::new(database, store, hub.clone())
}
async fn granted(
    issuer: &Peer,
    directory: &Path,
    list: &str,
    tables: &[&str],
    rights: &[&str],
) -> (Peer, NodeKeys) {
    let (store, device) = prepare(directory);
    let grants = issuer.grants(list, tables, rights).await;
    let supplied = issuer.grant(&device, grants).await.unwrap();
    (install(directory, store, supplied, &issuer.hub), device)
}

#[tokio::test]
async fn one_envelope_shares_a_group_and_installs_only_with_read_on_every_table() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer"));
    let created = snap::ok(
        issuer
            .session
            .create_partition(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values("A")).clone(),
            }),
    )
    .await;
    assert_eq!(created["epoch"]["generation"], 1);
    assert_eq!(
        snap::ok(
            issuer
                .session
                .create_partition(rocky_sdk::PartitionTarget::Table {
                    table: "items".to_owned(),
                    values: (values("A")).clone()
                })
        )
        .await,
        created,
        "creating again is idempotent: the same secret and epoch"
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
    assert_eq!(shared["recipients"], json!([device.node.to_hex()]));
    assert_eq!(
        issuer.recipients("A").await[&device.node.to_hex()],
        "pending"
    );

    let (reader_lacking_one_table, partial_device) = granted(
        &issuer,
        &root.path().join("partial"),
        "A",
        &["lists", "items"],
        &["Read"],
    )
    .await;
    issuer.share("A", &[&partial_device]).await.unwrap();
    reader_lacking_one_table.fetch(&issuer).await.unwrap();
    let refused = reader_lacking_one_table.events("envelope_refused").await;
    assert_eq!(refused.len(), 1, "only its own envelope reached it");
    assert!(
        refused[0]["error"]
            .as_str()
            .unwrap()
            .contains("attachments lacks it"),
        "{refused:?}"
    );
    assert_eq!(
        reader_lacking_one_table.store.group_secrets(),
        0,
        "a refused envelope leaves no secret"
    );
    let (secrets, tables) = reader_lacking_one_table.epochs("A").await;
    assert_eq!(secrets, json!([]), "a refused envelope leaves no epoch");
    assert!(uniform(&tables, json!([])));
    assert_eq!(
        reader_lacking_one_table.status("A").await["tables"][0]["holding"],
        Value::Null,
        "a refused envelope leaves no registration"
    );
    let states = issuer.recipients("A").await;
    assert_eq!(
        states[&partial_device.node.to_hex()],
        "pending",
        "the sender keeps a refused envelope"
    );
    assert_eq!(states[&device.node.to_hex()], "pending");

    reader.fetch(&issuer).await.unwrap();
    assert_eq!(reader.events("partition_key_installed").await.len(), 1);
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
        SCHEMA,
        reader.store.clone(),
        reader.hub.clone(),
    );
    assert!(uniform(&reader.epochs("A").await.1, json!([1])));
    for peer in [reader, reader_lacking_one_table, issuer] {
        peer.database.close().await.unwrap();
    }
}

#[tokio::test]
async fn revocation_rotates_the_group_once_and_shares_only_the_current_epoch_to_live_devices() {
    let root = support::small_directory().unwrap();
    let issuer = Peer::create(&root.path().join("issuer"));
    for list in ["A", "B"] {
        snap::ok(
            issuer
                .session
                .create_partition(rocky_sdk::PartitionTarget::Group {
                    group: "list".to_owned(),
                    values: (values(list)).clone(),
                }),
        )
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
    issuer
        .share("A", &[&removed_device, &remaining_device])
        .await
        .unwrap();
    for peer in [&removed, &remaining] {
        peer.fetch(&issuer).await.unwrap();
    }

    let revoked = snap::ok(
        issuer
            .session
            .revoke_identity(snap::public_key(&(removed_device.identity.to_string()))),
    )
    .await;
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
    let created = snap::ok(
        issuer
            .session
            .create_partition(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values("C")).clone(),
            }),
    )
    .await;
    assert_eq!(
        created["epoch"]["generation"], 1,
        "key epochs follow each scope, not the revocations"
    );

    let refused = issuer.share("A", &[&removed_device]).await.unwrap_err();
    assert!(
        refused.contains("revoked"),
        "sharing refuses the revoked device: {refused}"
    );
    let refused =
        snap::string(
            issuer.session.share_partition(
                rocky_sdk::PartitionTarget::Group {
                    group: "list".to_owned(),
                    values: (values("A")).clone(),
                },
                serde_json::from_value::<
                    std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>,
                >(json!([remaining_device.node]))
                .expect("typed fixture"),
                Some(
                    serde_json::from_value::<rocky_core::KeyEpoch>(json!(created["epoch"]))
                        .expect("typed fixture"),
                ),
            ),
        )
        .await
        .unwrap_err();
    assert!(
        refused.contains("current epoch"),
        "sharing refuses any epoch but the current: {refused}"
    );
    let (_, fresh) = prepare(&root.path().join("fresh"));
    let renamed = NodeKeys {
        identity: removed_device.identity,
        node: fresh.node,
    };
    let refused = issuer
        .grant(&renamed, issuer.grants("A", &TABLES, &["Read"]).await)
        .await
        .unwrap_err();
    assert!(
        refused.contains("revoked"),
        "a fresh node of a revoked identity gets no grant: {refused}"
    );

    let states = issuer.recipients("A").await;
    assert_eq!(states.get(&removed_device.node.to_hex()), None);
    assert_eq!(
        states[&remaining_device.node.to_hex()],
        "pending",
        "the rotation seals the new epoch to the recorded remaining reader without a share"
    );
    let shared = issuer.share("A", &[&remaining_device]).await.unwrap();
    assert_eq!(
        shared["epoch"]["generation"], 2,
        "sharing again adds no epoch"
    );
    assert!(removed.fetch(&issuer).await.is_err(), "revoked");
    remaining.fetch(&issuer).await.unwrap();
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
    let issuer = Peer::reopen(
        &root.path().join("issuer"),
        SCHEMA,
        issuer.store.clone(),
        issuer.hub.clone(),
    );
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
        snap::ok(
            issuer
                .session
                .create_partition(rocky_sdk::PartitionTarget::Group {
                    group: "list".to_owned(),
                    values: (values(list)).clone(),
                }),
        )
        .await;
    }
    let (_, device) = prepare(&root.path().join("device"));
    snap::ok(issuer.session.grant(
        device,
        rocky_sdk::GrantProfile::Reader {
            scopes: ["notes".to_owned()].into_iter().collect(),
        },
        rocky_sdk::GrantTerms {
            ttl: None,
            delegations: None,
        },
    ))
    .await;
    issuer
        .grant(&device, issuer.grants("A", &TABLES, &["Read"]).await)
        .await
        .unwrap();
    snap::ok(
        issuer
            .session
            .revoke_identity(snap::public_key(&(device.identity.to_string()))),
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
    let renamed = NodeKeys {
        identity: device.identity,
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
        snap::ok(
            issuer
                .session
                .create_partition(rocky_sdk::PartitionTarget::Group {
                    group: "list".to_owned(),
                    values: (values("A")).clone(),
                }),
        )
        .await;
        let (_, device) = prepare(&root.path().join(format!("device{budget}")));
        issuer
            .grant(&device, issuer.grants("A", &TABLES, &["Read"]).await)
            .await
            .unwrap();
        issuer.store.limit(Some(budget));
        let revoked = snap::string(
            issuer
                .session
                .revoke_identity(snap::public_key(&(device.identity.to_string()))),
        )
        .await;
        issuer.store.limit(None);
        let (store, hub) = (issuer.store.clone(), issuer.hub.clone());
        issuer.database.close().await.unwrap();
        let issuer = Peer::reopen(&directory, SCHEMA, store, hub);
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
{"name":"attachments","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true},{"name":"bytes","type":"blob"}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}"#,
            "",
        );
    let second = SCHEMA.replace(r#""version":1"#, r#""version":2"#);
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("issuer");
    let (store, hub) = (Arc::new(Store::default()), Arc::new(Hub::default()));
    let issuer = Peer::new(
        Database::create(open(&directory, &first, &store, &hub), None)
            .unwrap()
            .database,
        store,
        hub,
    );
    issuer
        .session
        .create_bookmark("main", None)
        .await
        .expect("bookmark");
    issuer.session.follow("main").await.expect("follow");
    snap::ok(
        issuer
            .session
            .create_partition(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values("A")).clone(),
            }),
    )
    .await;
    assert_eq!(
        issuer.status("A").await["tables"].as_array().unwrap().len(),
        2
    );
    let (store, hub) = (issuer.store.clone(), issuer.hub.clone());
    issuer.database.close().await.unwrap();

    let issuer = Peer::reopen(&directory, &second, store, hub);
    let (secrets, tables) = issuer.epochs("A").await;
    assert_eq!(secrets, json!([1]));
    assert!(uniform(&tables, json!([1])), "{tables:?}");
    assert_eq!(issuer.store.group_secrets(), 1, "no new secret or envelope");
    let mut tx = issuer.database.transaction().await.unwrap();
    let id = tx.next_id("attachments", values("A")).await.unwrap();
    tx.execute(
        "INSERT INTO attachments(list,key,id) VALUES('A','photo',?)",
        vec![id.sql()],
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
async fn a_phone_holds_a_list_and_its_items_while_leaving_out_attachments() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer"));
    snap::ok(
        issuer
            .session
            .create_partition(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values("A")).clone(),
            }),
    )
    .await;
    let (phone, device) =
        granted(&issuer, &root.path().join("phone"), "A", &TABLES, &["Read"]).await;
    issuer.share("A", &[&device]).await.unwrap();
    snap::ok(
        phone.session.set_replica_role(
            serde_json::from_value::<rocky_sdk::ReplicaRoleConfig>(json!({"role":"Partial"}))
                .expect("typed fixture"),
        ),
    )
    .await;
    phone.fetch(&issuer).await.unwrap();
    for omit in [json!(["photos"]), json!(TABLES)] {
        assert!(
            snap::string(
                phone.session.subscribe_partition(
                    rocky_sdk::PartitionTarget::Group {
                        group: "list".to_owned(),
                        values: (values("A")).clone()
                    },
                    rocky_sdk::PartitionSelection {
                        name: None,
                        omit: serde_json::from_value::<std::collections::BTreeSet<String>>(json!(
                            omit
                        ))
                        .expect("typed fixture")
                    }
                )
            )
            .await
            .is_err(),
            "{omit}"
        );
    }
    snap::ok(
        phone.session.subscribe_partition(
            rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values("A")).clone(),
            },
            rocky_sdk::PartitionSelection {
                name: None,
                omit: serde_json::from_value::<std::collections::BTreeSet<String>>(json!([
                    "attachments"
                ]))
                .expect("typed fixture"),
            },
        ),
    )
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
    snap::ok(
        phone.session.unsubscribe_partition(
            rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values("A")).clone(),
            },
            serde_json::from_value::<std::collections::BTreeSet<String>>(json!([]))
                .expect("typed fixture"),
        ),
    )
    .await;
    let status = phone.status("A").await;
    assert!(
        status["tables"]
            .as_array()
            .unwrap()
            .iter()
            .all(|table| table["holding"] == "left"),
        "the whole group partition is released together: {status}"
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
    let id = tx.next_id(table, values("A")).await.unwrap();
    let sql = match table {
        "lists" => format!("INSERT INTO lists(name,title,id) VALUES('A','{key}',?)"),
        _ => format!("INSERT INTO {table}(list,key,id) VALUES('A','{key}',?)"),
    };
    tx.execute(&sql, vec![id.sql()]).await.unwrap();
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
        let contact = snap::ok(to.session.contact_info()).await;
        snap::ok(async {
            let value = from
                .session
                .add_peer(&(snap::text(&(contact["uri"]))))
                .await?;
            Ok(json!({"node":value}))
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn readers_open_every_table_from_one_envelope_and_only_live_readers_open_later_rows() {
    let root = support::small_directory().unwrap();
    let issuer = Peer::create(&root.path().join("issuer"));
    snap::ok(
        issuer
            .session
            .create_partition(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values("A")).clone(),
            }),
    )
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
        let reader = install(&directory, store, supplied, &issuer.hub);
        introduce(&issuer, &reader).await;
        readers.push((reader, device));
    }
    let devices: Vec<&NodeKeys> = readers.iter().map(|(_, device)| device).collect();
    let shared = issuer.share("A", &devices).await.unwrap();
    assert_eq!(shared["recipients"].as_array().unwrap().len(), 2);
    for (reader, _) in &readers {
        snap::ok(reader.session.sync_once()).await;
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
            .session
            .revoke_identity(snap::public_key(&(removed_device.identity.to_string()))),
    )
    .await;
    for table in TABLES {
        write(&issuer, table, "after").await;
    }
    snap::ok(remaining.session.sync_once()).await;
    let both = vec!["after".to_owned(), "before".to_owned()];
    assert_eq!(
        rows(remaining).await,
        [vec!["after".to_owned()], both.clone(), both],
        "the rotation's envelope reaches the remaining reader by sync alone"
    );
    let _ = snap::string(removed.session.sync_once()).await;
    assert_eq!(
        rows(removed).await,
        vec![vec!["before".to_owned()]; 3],
        "the removed reader is refused later rows"
    );
    let (secrets, tables) = removed.epochs("A").await;
    assert_eq!(
        secrets,
        json!([1]),
        "the removed reader holds no key of the new epoch"
    );
    assert!(uniform(&tables, json!([1])));
    for (reader, _) in readers {
        reader.database.close().await.unwrap();
    }
    issuer.database.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_identity_file_older_than_the_replica_recovers_its_groups() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer"));
    snap::ok(
        issuer
            .session
            .create_partition(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values("A")).clone(),
            }),
    )
    .await;
    for table in TABLES {
        write(&issuer, table, "before").await;
    }
    let directory = root.path().join("reader");
    let (store, device) = prepare(&directory);
    let supplied = issuer
        .grant(&device, issuer.grants("A", &TABLES, &["Read"]).await)
        .await
        .unwrap();
    let reader = install(&directory, store, supplied, &issuer.hub);
    introduce(&issuer, &reader).await;
    let older = common::authority_bytes(&directory, "identity")
        .unwrap()
        .unwrap();
    issuer.share("A", &[&device]).await.unwrap();
    snap::ok(reader.session.sync_once()).await;
    let expected = vec![vec!["before".to_owned()]; 3];
    assert_eq!(rows(&reader).await, expected);
    reader.database.close().await.unwrap();
    common::write_authority(&directory, "identity", &older)
        .await
        .unwrap();
    let reader = Peer::reopen(&directory, SCHEMA, reader.store.clone(), reader.hub.clone());
    assert_eq!(rows(&reader).await, expected);
    assert_eq!(reader.epochs("A").await.0, json!([1]));
    reader.database.close().await.unwrap();
    issuer.database.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partition_only_writer_obtains_authors_without_collection_wide_grants() {
    let root = tempfile::tempdir().unwrap();
    let issuer = Peer::create(&root.path().join("issuer"));
    snap::ok(
        issuer
            .session
            .create_partition(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values("A")).clone(),
            }),
    )
    .await;
    for table in TABLES {
        write(&issuer, table, "before").await;
    }
    let directory = root.path().join("writer");
    let (store, device) = prepare(&directory);
    let mut grants = issuer
        .grants("A", &TABLES, &["Read", "Insert", "Update", "Delete"])
        .await;
    pair_field_rights(&root.path().join("issuer"), &mut grants);
    let supplied = issuer.grant(&device, grants).await.unwrap();
    let writer = install(&directory, store, supplied, &issuer.hub);
    introduce(&issuer, &writer).await;
    issuer.share("A", &[&device]).await.unwrap();
    snap::ok(writer.session.sync_once()).await;
    snap::ok(writer.session.sync_once()).await;
    let mut tx = writer.database.transaction().await.unwrap();
    let id = tx.next_id("items", values("A")).await.unwrap();
    tx.execute(
        "INSERT INTO items(list,key,id) VALUES('A','writer',?)",
        vec![id.sql()],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    snap::ok(writer.session.sync_once()).await;
    assert_eq!(
        rows(&issuer).await,
        [vec!["before"], vec!["before", "writer"], vec!["before"]],
        "syncing the envelope obtains an author per table for a partition-only writer"
    );
    writer.database.close().await.unwrap();
    issuer.database.close().await.unwrap();
}

fn pair_field_rights(issuer_directory: &std::path::Path, grants: &mut Value) {
    let identity = common::authority_json(issuer_directory, "identity").unwrap();
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
}

#[tokio::test]
async fn a_member_with_a_delegated_grant_shares_without_issuer_authority() {
    let root = support::small_directory().unwrap();
    let issuer = Peer::create(&root.path().join("issuer"));
    snap::ok(
        issuer
            .session
            .create_partition(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values("A")).clone(),
            }),
    )
    .await;
    let (store, device) = prepare(&root.path().join("member"));
    let grants = issuer.grants("A", &TABLES, &["Read"]).await;
    let supplied = snap::ok(
        issuer.session.grant_partitions(
            device,
            serde_json::from_value::<Vec<rocky_sdk::credentials::PartitionGrant>>(json!(grants))
                .expect("typed fixture"),
            2,
        ),
    )
    .await;
    let member = install(
        &root.path().join("member"),
        store,
        supplied["supplied"].clone(),
        &issuer.hub,
    );
    issuer.share("A", &[&device]).await.unwrap();
    member.fetch(&issuer).await.unwrap();
    let (store, recipient) = prepare(&root.path().join("recipient"));
    let grants = member.grants("A", &TABLES, &["Read"]).await;
    let supplied = snap::ok(
        member.session.delegate_partitions(
            recipient,
            serde_json::from_value::<Vec<rocky_sdk::credentials::PartitionGrant>>(json!(grants))
                .expect("typed fixture"),
            1,
        ),
    )
    .await;
    let delegated = install(
        &root.path().join("recipient"),
        store,
        supplied["supplied"].clone(),
        &issuer.hub,
    );
    member.share("A", &[&recipient]).await.unwrap();
    delegated.fetch(&member).await.unwrap();
    assert_eq!(delegated.epochs("A").await.0, json!([1]));
    assert_eq!(
        member.recipients("A").await[&recipient.node.to_hex()],
        "delivered"
    );
    let (last, last_device) =
        granted(&issuer, &root.path().join("last"), "A", &TABLES, &["Read"]).await;
    delegated.share("A", &[&last_device]).await.unwrap();
    last.fetch(&delegated).await.unwrap();
    assert_eq!(last.epochs("A").await.0, json!([1]));
    assert_eq!(
        delegated.recipients("A").await[&last_device.node.to_hex()],
        "delivered"
    );
    for peer in [last, delegated, member, issuer] {
        peer.database.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_partition_granted_to_an_identity_reaches_a_node_it_links_later() {
    let root = support::small_directory().unwrap();
    let issuer = Peer::create(&root.path().join("issuer"));
    snap::ok(
        issuer
            .session
            .create_partition(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values("A")).clone(),
            }),
    )
    .await;
    for table in TABLES {
        write(&issuer, table, "before").await;
    }
    let person: Arc<dyn rocky_sdk::IdentityProvider> =
        Arc::new(rocky_sdk::FileIdentityProvider::open(root.path().join("person")).unwrap());
    let first_directory = root.path().join("first");
    let first_store = Arc::new(Store::default());
    let keys = Database::node_keys_of(
        &first_directory,
        Some(first_store.clone() as Arc<dyn SecretStore>),
        Some(person.clone()),
    )
    .unwrap();
    assert_eq!(keys.identity, person.public_key().unwrap());
    let supplied = issuer
        .grant(&keys, issuer.grants("A", &TABLES, &["Read"]).await)
        .await
        .unwrap();
    let first = Peer::new(
        Database::install(
            open_with_identity(&first_directory, &first_store, &issuer.hub, &person),
            serde_json::from_value(supplied).unwrap(),
        )
        .unwrap()
        .database,
        first_store,
        issuer.hub.clone(),
    );
    introduce(&issuer, &first).await;
    issuer.share("A", &[&keys]).await.unwrap();
    snap::ok(first.session.sync_once()).await;
    assert_eq!(rows(&first).await, vec![vec!["before".to_owned()]; 3]);

    let contact = snap::ok(first.session.contact_info()).await["uri"]
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
            &contact,
            &first.database.session(),
        )
        .await
        .unwrap()
        .database,
        second_store,
        issuer.hub.clone(),
    );
    introduce(&issuer, &second).await;
    snap::ok(second.session.sync_once()).await;
    assert_eq!(
        rows(&second).await,
        vec![vec!["before".to_owned()]; 3],
        "a node the identity links from the first reads without an issuer acting"
    );
    let session = snap::ok(second.session.info()).await;
    assert_eq!(session["principal"], json!(keys.identity));
    assert_ne!(session["node"], json!(keys.node));

    let listed = snap::ok(issuer.session.identities()).await;
    let record = listed["identities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["identity"] == json!(keys.identity))
        .unwrap()
        .clone();
    assert_eq!(record["kind"], "partition");
    assert_eq!(record["partitions"].as_array().unwrap().len(), TABLES.len());
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
        .session
        .unlink(
            rocky_credentials::PeerId::of(session["node"].as_str().unwrap().parse().unwrap()),
            person,
        )
        .await
        .unwrap();
    snap::ok(first.session.sync_once()).await;
    snap::ok(issuer.session.sync_once()).await;
    let (epochs, tables) = issuer.epochs("A").await;
    assert_ne!(epochs, before, "unlinking one reader rotates its partition");
    assert!(uniform(&tables, epochs));
    snap::ok(first.session.sync_once()).await;
    assert_eq!(
        rows(&first).await,
        vec![vec!["before".to_owned()]; 3],
        "the sibling keeps the identity's grant and reads the replacement epoch"
    );
    assert!(snap::string(second.session.sync_once()).await.is_err());
    for peer in [first, second, issuer] {
        peer.database.close().await.unwrap();
    }
}

#[test]
fn the_budgeted_store_meets_the_key_provider_contract() {
    rocky_sdk::credentials::testing::key_provider_contract(&Store::default());
}
