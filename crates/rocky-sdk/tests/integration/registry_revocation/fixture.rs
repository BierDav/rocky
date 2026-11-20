use super::{Device, Failure};
use crate::common;
use crate::support::snapshot as snap;
use rocky_sdk::{
    Database, Network, Open, SecretStore, credentials::testing::MemoryKeys, transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
};

pub(crate) const SCHEMA: &str = r#"{"application":"revocation","version":1,"format":6,"tables":[
 {"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]},
 {"name":"parts","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"},
 {"name":"labels","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}]}"#;

static KEYS: LazyLock<Mutex<HashMap<PathBuf, Arc<MemoryKeys>>>> = LazyLock::new(Default::default);

pub(crate) fn keys(directory: &Path) -> Arc<dyn SecretStore> {
    KEYS.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(directory.to_path_buf())
        .or_default()
        .clone()
}
pub(crate) fn open<'a>(root: &'a Path, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    Open {
        identity: common::prepared_identity(root),
        directory: root,
        schema: crate::support::snapshot::schema(SCHEMA),
        secrets: Some(keys(root)),
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}
pub(crate) fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(root.join(name).join("identity.json"))?)?;
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
    let mut status = snap::ok(device.session.status()).await;
    while status["data"]["bootstrap_pending"] == true {
        status = snap::ok(device.session.status_after(snap::text(&(status["token"])))).await;
    }
}
pub(crate) async fn join(
    root: &Path,
    hub: &Arc<Hub>,
    name: &str,
    issuer: &Device,
    profile: Value,
) -> Result<Device, Failure> {
    let invite = issuer.invite_as(profile).await;
    let device = Device::new(Database::join(open(&root.join(name), hub, name), &invite)?)?;
    settle(&device).await;
    Ok(device)
}
pub(crate) async fn listed(device: &Device, identity: &str) -> Value {
    snap::ok(device.session.identities()).await["identities"]
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
    let status = snap::ok(
        device
            .session
            .partition_status(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values(list)).clone(),
            }),
    )
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
pub(crate) async fn grant(device: &Device, list: &str) -> Value {
    let address = snap::ok(
        device
            .session
            .partition_address("parts", (values(list)).clone()),
    )
    .await;
    let part = &address["tuple"][0];
    json!({"domain":address["scope"]["domain"],"table":address["scope"]["table"],
        "selector":[[part[0],{"Only":[part[1]]}]],"rows":"All","rights":["Read"],"fields":[]})
}
pub(crate) struct Domain {
    pub(super) a: Device,
    pub(super) b: Device,
}
impl Domain {
    pub(super) async fn new(root: &Path, hub: &Arc<Hub>) -> Result<Self, Failure> {
        let a = Device::new(Database::create(open(&root.join("a"), hub, "a"), None)?)?;
        a.follow_main().await;
        let b = join(root, hub, "b", &a, json!({"kind":"issuer"})).await?;
        for list in ["L", "M"] {
            snap::ok(
                a.session
                    .create_partition(rocky_sdk::PartitionTarget::Group {
                        group: "list".to_owned(),
                        values: (values(list)).clone(),
                    }),
            )
            .await;
            snap::ok(
                a.session.share_partition(
                    rocky_sdk::PartitionTarget::Group {
                        group: "list".to_owned(),
                        values: (values(list)).clone(),
                    },
                    serde_json::from_value::<
                        std::collections::BTreeSet<rocky_sdk::credentials::PublicKey>,
                    >(json!([b.node().await]))
                    .expect("typed fixture"),
                    None,
                ),
            )
            .await;
        }
        sync(&b).await?;
        Ok(Self { a, b })
    }
}
