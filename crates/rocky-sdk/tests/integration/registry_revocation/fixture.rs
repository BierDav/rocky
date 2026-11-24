use super::{Device, Failure};
use crate::common;
use crate::support::snapshot as snap;
use rocky_credentials::testing::MemoryKeys;
use rocky_sdk::{Network, SecretStore, transport::memory::Hub};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
};

fn in_list(table: &str) -> rocky_sdk::Table {
    rocky_sdk::Table::replayed(table)
        .column(common::required("list", rocky_sdk::Affinity::Text))
        .column(common::required("key", rocky_sdk::Affinity::Text))
        .primary_key(["list", "key"])
        .partition_by(["list"])
        .partition_group("list")
}
fn revocation() -> rocky_sdk::Schema {
    let role = |name| rocky_sdk::Role::new(rocky_sdk::RoleName::from_static(name));
    common::build(
        rocky_sdk::Schema::builder("revocation", 1)
            .table(common::items())
            .table(
                rocky_sdk::Table::replayed("notes")
                    .column(common::required("key", rocky_sdk::Affinity::Text))
                    .primary_key(["key"]),
            )
            .table(in_list("parts"))
            .table(in_list("labels"))
            .role(
                role("item_writer")
                    .write("items")
                    .read("parts")
                    .read("labels")
                    .per_partition("list"),
            )
            .role(
                role("lister")
                    .read("parts")
                    .read("labels")
                    .per_partition("list"),
            ),
    )
}

static KEYS: LazyLock<Mutex<HashMap<PathBuf, Arc<MemoryKeys>>>> = LazyLock::new(Default::default);

pub(crate) fn keys(directory: &Path) -> Arc<dyn SecretStore> {
    KEYS.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(directory.to_path_buf())
        .or_default()
        .clone()
}
pub(crate) fn open(root: &Path, hub: &Arc<Hub>, name: &str) -> rocky_sdk::Configuration {
    crate::common::configuration(
        root,
        &revocation(),
        Some(keys(root)),
        common::prepared_identity(root),
        Some(Network::Owned(hub.connector(name))),
    )
}
pub(crate) fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    let mut identity = common::authority_json(&root.join(name), "identity")?;
    for epochs in identity["keys"].as_object_mut().ok_or("keys")?.values_mut() {
        for epoch in epochs.as_array_mut().ok_or("epochs")? {
            *epoch = epoch["generation"].clone();
        }
    }
    Ok(identity)
}
pub(crate) async fn sync(device: &Device) -> Result<(), Failure> {
    device.sync().await.map_err(|error| error.to_string())?;
    Ok(())
}
pub(crate) async fn settle(device: &Device) {
    let mut changes = device.database.status_changes();
    let mut status = snap::ok(changes.next()).await;
    while status["data"]["bootstrap_pending"] == true {
        status = snap::ok(changes.next()).await;
    }
}
pub(crate) async fn join(
    root: &Path,
    hub: &Arc<Hub>,
    name: &str,
    issuer: &Device,
    access: rocky_sdk::Access,
) -> Result<Device, Failure> {
    let invite = issuer.invite_as(access).await;
    let device = Device::new(open(&root.join(name), hub, name).join(&invite).await?)?;
    issuer.database.add_peer(&device.contact().await).await?;
    settle(&device).await;
    Ok(device)
}
pub(crate) async fn listed(device: &Device, identity: &str) -> Value {
    snap::ok(device.database.people()).await["identities"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|record| record["identity"] == identity)
        .cloned()
        .unwrap_or_default()
}
pub(crate) fn values(list: &str) -> Vec<rocky_sdk::SqlValue> {
    vec![rocky_sdk::SqlValue::Text(snap::text(&(list)))]
}
pub(crate) async fn epoch(device: &Device, list: &str) -> Value {
    let status = snap::ok(device.database.partition_status(&rocky_sdk::Partition::new(
        "list".to_owned(),
        (values(list)).clone(),
    )))
    .await;
    for table in status["tables"].as_array().into_iter().flatten() {
        assert_eq!(table["epochs"], status["epochs"], "{status}");
    }
    json!(
        status["epochs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|epoch| epoch["generation"].clone())
            .collect::<Vec<_>>()
    )
}
pub(crate) fn role(name: &'static str) -> rocky_sdk::Access {
    rocky_sdk::Access::Role(rocky_sdk::RoleName::from_static(name))
}
pub(crate) async fn grant(issuer: &Device, identity: &str, list: &str) {
    snap::ok(issuer.database.grant_partition(
        rocky_sdk::PersonId::new(identity.parse().expect("public key")),
        &rocky_sdk::Partition::new("list", values(list)),
    ))
    .await;
}
pub(crate) struct Domain {
    pub(super) a: Device,
    pub(super) b: Device,
}
impl Domain {
    pub(super) async fn new(root: &Path, hub: &Arc<Hub>) -> Result<Self, Failure> {
        let a = Device::new(open(&root.join("a"), hub, "a").create().await?)?;
        let b = join(root, hub, "b", &a, rocky_sdk::Access::Full).await?;
        a.admit(&b).await;
        for list in ["L", "M"] {
            snap::ok(
                a.database
                    .create_partition(&rocky_sdk::Partition::new("list", (values(list)).clone())),
            )
            .await;
        }
        sync(&b).await?;
        Ok(Self { a, b })
    }
}
