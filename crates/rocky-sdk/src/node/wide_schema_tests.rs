use crate::test_snapshot as snap;
use crate::{
    Configuration, Database, Network, Schema, enrollment::Settings, transport::memory::Hub,
};
use rocky_credentials::CredentialChain;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
type Failure = crate::Failure;

struct Device {
    database: Database,
}
impl Device {
    fn new(database: Database) -> Self {
        Self { database }
    }

    async fn contact(&self) -> String {
        snap::ok(self.database.contact()).await["uri"]
            .as_str()
            .unwrap()
            .into()
    }
    async fn sync(&self) -> Result<Value, Value> {
        snap::observe(self.database.sync_now()).await
    }
    async fn add_peer(&self, contact: &str) -> Result<Value, Value> {
        snap::observe(async {
            let value = self.database.add_peer(&(snap::text(&(contact)))).await?;
            Ok::<_, crate::Error>(json!({"node":value}))
        })
        .await
    }
}

fn schema(count: usize) -> String {
    let tables: Vec<_> = (0..count)
        .map(|i| {
            json!({"name":format!("table_{i:03}"),"class":"replayed",
            "columns":[{"name":"book","type":"text","not_null":true},
                {"name":"title","type":"text","not_null":true}],
            "primary_key":["book","id"],"partition_by":["book"],"partition_group":"book"})
        })
        .collect();
    json!({"format":10,"application":"wide_grants","version":1,"tables":tables}).to_string()
}
fn open(
    directory: &Path,
    schema: &crate::Schema,
    hub: &Arc<Hub>,
    name: &str,
) -> Result<Configuration, Failure> {
    Ok(Configuration::new(directory, schema)?
        .network(Network::Owned(hub.connector(name)))
        .sync(crate::SyncMode::Manual))
}
fn settings(directory: &Path) -> Settings {
    Settings::load(&directory.join("rocky.sqlite")).unwrap()
}
fn grant_count(chain: &CredentialChain) -> usize {
    chain
        .0
        .iter()
        .map(|link| {
            link.capability.grants.len() + link.capability.extended().unwrap().partitions.len()
        })
        .max()
        .unwrap()
}
fn metric(label: &str, chain: &CredentialChain) -> usize {
    let bytes = rocky_credentials::encode(chain).unwrap().len();
    let held = grant_count(chain);
    assert!(held <= rocky_credentials::MAX_GRANTS);
    assert!(bytes <= rocky_credentials::MAX_CREDENTIAL_BYTES);
    println!("{label}: grants={held}, chain_bytes={bytes}");
    bytes
}
async fn assert_epochs(device: &Device, count: usize, expected: &[u64]) {
    let status = snap::ok(device.database.partition_status(&rocky_sdk::Partition::new(
        "book",
        vec![rocky_sdk::SqlValue::Text("A".into())],
    )))
    .await;
    let tables = status["tables"].as_array().unwrap();
    assert_eq!(tables.len(), count);
    for table in tables {
        let epochs: Vec<_> = table["epochs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|epoch| epoch["generation"].as_u64().unwrap())
            .collect();
        assert_eq!(epochs, expected, "{table}");
    }
}
async fn exercise(count: usize) -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let schema = Schema::parse(&schema(count))?;
    assert_eq!(schema.tables.len(), count);
    let hub = Arc::new(Hub::default());
    let owner_dir = root.path().join("owner");
    let owner = Device::new(
        open(&owner_dir, &schema, &hub, "owner")?
            .create()
            .await?
            .database,
    );
    let label = format!("{count} tables");
    let before = metric(&format!("{label} owner"), &settings(&owner_dir).credentials);
    assert_eq!(
        grant_count(&settings(&owner_dir).credentials),
        1,
        "an admin holds full access as one flag plus the registry"
    );
    let contact = owner.contact().await;
    let mut members = Vec::new();
    for (kind, profile) in [
        ("writer", crate::Access::Full),
        ("reader", crate::Access::Read),
    ] {
        let dir = root.path().join(kind);
        let invitation = owner.database.invite(crate::Invite::new(profile)).await?;
        let device = Device::new(
            open(&dir, &schema, &hub, kind)?
                .join(&invitation.code)
                .await?
                .database,
        );
        device.add_peer(&contact).await.map_err(|e| e.to_string())?;
        let provisioned = device.database.me().await?.person;
        metric(&format!("{label} {kind}"), &settings(&dir).credentials);
        assert_eq!(
            grant_count(&settings(&dir).credentials),
            0,
            "whole access names no table"
        );
        members.push((device, provisioned, dir));
    }
    snap::ok(owner.database.create_partition(&crate::Partition::new(
        "book",
        vec![rocky_sdk::SqlValue::Text("A".to_owned())],
    )))
    .await;
    let saved = settings(&owner_dir);
    let delivery = saved
        .deliveries
        .values()
        .find_map(|pending| pending.sealed().transpose())
        .transpose()?
        .unwrap();
    println!(
        "{label}: share_payload_bytes={}, share_frame_bytes={}",
        delivery.payload_len(),
        delivery.to_bytes()?.len()
    );
    assert!(delivery.payload_len() <= rocky_credentials::MAX_DELIVERY_PAYLOAD);
    assert!(delivery.to_bytes()?.len() < rocky_secure::MAX_SECURE_FRAME);
    for (device, _, _) in &members {
        device.sync().await.map_err(|e| e.to_string())?;
        assert_epochs(device, count, &[1]).await;
    }
    let table = format!("table_{:03}", count - 1);
    let writer = &members[0].0;
    writer
        .database
        .write(async |tx| {
            let id = tx
                .next_id_in(&table, vec![rocky_sdk::SqlValue::Text("A".to_owned())])
                .await?;
            tx.execute(
                &format!("INSERT INTO {table}(id,book,title) VALUES(?,'A','written')"),
                vec![id.sql()],
            )
            .await
        })
        .await?;
    let writer_sync = writer.sync().await.map_err(|e| e.to_string())?;
    owner
        .add_peer(&writer.contact().await)
        .await
        .map_err(|e| e.to_string())?;
    let owner_sync = owner.sync().await.map_err(|e| e.to_string())?;
    let reader_sync = members[1].0.sync().await.map_err(|e| e.to_string())?;
    let rows = members[1]
        .0
        .database
        .query(&format!("SELECT title FROM {table}"), Vec::new())
        .await?
        .collect()
        .await?;
    let title: Option<String> = rows.first().and_then(|row| row.get_at(0).ok());
    if title.as_deref() != Some("written") {
        println!(
            "writer sync: {writer_sync}; owner sync: {owner_sync}; reader sync: {reader_sync}"
        );
        println!(
            "writer peers: {}; owner peers: {}; reader peers: {}",
            snap::ok(async {
                let network = writer.database.status().await?.network;
                Ok::<_, crate::Error>(json!({"peers":network.peers,"discovery":network.discovery}))
            })
            .await,
            snap::ok(async {
                let network = owner.database.status().await?.network;
                Ok::<_, crate::Error>(json!({"peers":network.peers,"discovery":network.discovery}))
            })
            .await,
            snap::ok(async {
                let network = members[1].0.database.status().await?.network;
                Ok::<_, crate::Error>(json!({"peers":network.peers,"discovery":network.discovery}))
            })
            .await
        );
        println!(
            "owner activity: {}",
            snap::ok(owner.database.activity(0)).await
        );
    }
    assert_eq!(title.as_deref(), Some("written"));
    let forged = members[1]
        .0
        .database
        .write(async |tx| {
            tx.execute(&format!("UPDATE {table} SET title='forged'"), Vec::new())
                .await
        })
        .await;
    assert!(forged.is_err());
    snap::ok(owner.database.remove(members[0].1)).await;
    members[1].0.sync().await.map_err(|e| e.to_string())?;
    assert_epochs(&members[1].0, count, &[1, 2]).await;
    assert_epochs(&members[0].0, count, &[1]).await;
    let after = metric(
        &format!("{label} owner after rotation"),
        &settings(&owner_dir).credentials,
    );
    assert_eq!(after, before);
    for (device, _, _) in members {
        device.database.close().await?;
    }
    owner.database.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_hundred_tables_share_and_rotate_under_full_access() -> Result<(), Failure> {
    exercise(100).await
}

#[tokio::test]
async fn a_wide_schema_under_full_access_stays_within_the_grant_budget() -> Result<(), Failure> {
    exercise(rocky_credentials::MAX_GRANTS + 1).await
}

#[tokio::test]
async fn local_sql_sessions_cover_more_than_256_authorized_scopes() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let schema = crate::Schema::parse(&schema(160))?;
    let device = Device::new(
        Configuration::new(directory.path(), &schema)?
            .create()
            .await?
            .database,
    );
    snap::ok(device.database.create_partition(&crate::Partition::new(
        "book",
        vec![rocky_sdk::SqlValue::Text("A".to_owned())],
    )))
    .await;
    device
        .database
        .write(async |tx| {
            let id = tx
                .next_id_in("table_159", vec![rocky_sdk::SqlValue::Text("A".to_owned())])
                .await?;
            tx.execute(
                "INSERT INTO table_159(id,book,title) VALUES(?,'A','wide')",
                vec![id.sql()],
            )
            .await
        })
        .await?;
    let rows = device
        .database
        .query("SELECT title FROM table_159", Vec::new())
        .await?
        .collect()
        .await?;
    assert_eq!(rows[0].get_at::<String>(0)?, "wide");
    device.database.close().await?;
    Ok(())
}
