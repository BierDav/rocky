use common::{Device, Failure};
use rocky_sdk::{Network, RevocationStatus, SqlValue, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

use crate::common;

const SCHEMA: &str = r#"{"application":"shopping","version":1,"format":7,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"text","type":"text","not_null":true}],"primary_key":["key"]}],"roles":[{"name":"item_reader","tables":[{"table":"items","access":"read"}]},{"name":"note_reader","tables":[{"table":"notes","access":"read"}]}]}"#;

fn open(directory: &Path, hub: &Arc<Hub>, name: &str) -> rocky_sdk::Configuration {
    crate::common::configuration(
        directory,
        crate::support::snapshot::schema(SCHEMA),
        None,
        None,
        Some(Network::Owned(hub.connector(name))),
    )
}
async fn create(root: &Path, hub: &Arc<Hub>, name: &str) -> Result<Device, Failure> {
    Device::new(open(&root.join(name), hub, name).create().await?)
}
async fn join(
    root: &Path,
    hub: &Arc<Hub>,
    name: &str,
    issuer: &Device,
    profile: rocky_sdk::Access,
) -> Result<Device, Failure> {
    let invitation = issuer
        .database
        .invite(rocky_sdk::Invite::new(profile))
        .await?
        .code;
    let device = Device::new(open(&root.join(name), hub, name).join(&invitation).await?)?;
    issuer.database.add_peer(&device.contact().await).await?;
    Ok(device)
}
fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    let mut identity = common::authority_json(&root.join(name), "identity")?;
    for epochs in identity["keys"].as_object_mut().ok_or("keys")?.values_mut() {
        for epoch in epochs.as_array_mut().ok_or("epochs")? {
            *epoch = epoch["generation"].clone();
        }
    }
    Ok(identity)
}
async fn revocations(device: &Device) -> RevocationStatus {
    device
        .database
        .info()
        .await
        .expect("device info")
        .revocations
}
const INSERT_ITEM: &str = "INSERT INTO items(key,id,title,completed) VALUES(?,?,?,0)";
const INSERT_NOTE: &str = "INSERT INTO notes(key,id,text) VALUES(?,?,?)";
async fn create_row(device: &Device, handle: u8, table: &str, sql: &str, text: &str) {
    let handle = [handle; 32];
    let mut tx = device.database.transaction().await.expect("begin");
    let id = tx.next_id(table, Vec::new()).await.expect("id");
    tx.execute(
        sql,
        vec![
            SqlValue::Text(data_encoding::HEXLOWER.encode(&handle)),
            id.sql(),
            SqlValue::Text(text.into()),
        ],
    )
    .await
    .expect("insert");
    tx.commit().await.expect("commit");
}
async fn texts(device: &Device, sql: &str) -> Vec<String> {
    device
        .database
        .query(sql, Vec::new())
        .await
        .expect("read")
        .collect()
        .await
        .expect("collect")
        .into_iter()
        .map(|row| row.get_at::<String>(0).expect("text"))
        .collect()
}

struct Domain {
    a: Device,
    b: Device,
    x: Device,
    y: Device,
    m: Device,
    n: Device,
}
impl Domain {
    async fn new(root: &Path, hub: &Arc<Hub>) -> Result<Self, Failure> {
        let a = create(root, hub, "a").await?;
        let b = join(root, hub, "b", &a, rocky_sdk::Access::Full).await?;
        a.admit(&b).await;
        let writer = rocky_sdk::Access::Full;
        let reader = |role| rocky_sdk::Access::Role(rocky_sdk::RoleName::from_static(role));
        let x = join(root, hub, "x", &a, reader("item_reader")).await?;
        let y = join(root, hub, "y", &b, reader("note_reader")).await?;
        let m = join(root, hub, "m", &a, writer.clone()).await?;
        let n = join(root, hub, "n", &b, writer).await?;
        common::introduce(&m, &n).await;
        create_row(&a, 1, "items", INSERT_ITEM, "before").await;
        create_row(&b, 2, "notes", INSERT_NOTE, "before").await;
        for device in [&b, &a, &x, &y, &m, &n] {
            device.sync().await.map_err(|error| error.to_string())?;
        }
        Ok(Self { a, b, x, y, m, n })
    }
    async fn close(self) {
        for device in [&self.a, &self.b, &self.x, &self.y, &self.m, &self.n] {
            device.close().await;
        }
    }
}

async fn partitioned_revocations_merge(a_first: bool) -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b, x, y, m, n } = Domain::new(root, &hub).await?;
    let empty = revocations(&a).await;
    assert_eq!(empty.statements, 0);

    let by_a = a
        .database
        .revoke_identity(x.identity().await.parse()?)
        .await?;
    let by_b = b
        .database
        .revoke_identity(y.identity().await.parse()?)
        .await?;
    assert_ne!(by_a.statement, by_b.statement);
    assert_eq!(by_a.revocations.statements, 1);
    assert_eq!(by_b.revocations.statements, 1);
    assert_ne!(by_a.revocations.digest, by_b.revocations.digest);
    assert_eq!(
        identity(root, "a")?["keys"],
        json!({"issuer_registry": [1], "items": [1, 2], "notes": [1]}),
        "each issuer rotates only the collection its revoked device read"
    );
    assert_eq!(
        identity(root, "b")?["keys"],
        json!({"issuer_registry": [1], "items": [1], "notes": [1, 2]})
    );

    let (first, second) = if a_first { (&a, &b) } else { (&b, &a) };
    let _checkpoint_import_fails_until_both_hold_the_keys = first.sync().await;
    assert_eq!(
        revocations(first).await.statements,
        2,
        "the handshake gives each issuer the other's statement"
    );
    assert_eq!(revocations(second).await.statements, 2);
    second.sync().await.map_err(|error| error.to_string())?;
    first.sync().await.map_err(|error| error.to_string())?;
    let both = revocations(&a).await;
    assert_eq!(both.statements, 2);
    assert_eq!(revocations(&b).await, both);
    for issuer in ["a", "b"] {
        assert_eq!(
            identity(root, issuer)?["keys"],
            json!({"issuer_registry": [1], "items": [1, 2], "notes": [1, 2]}),
            "{issuer}"
        );
    }

    m.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        revocations(&m).await,
        both,
        "a member learns both statements from its issuer's handshake"
    );
    assert_eq!(
        revocations(&n).await,
        both,
        "a member's peer learns both statements from the member before meeting an issuer"
    );
    n.sync().await.map_err(|error| error.to_string())?;
    m.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        m.peer(&n.node().await).await["attempts"]
            .as_array()
            .ok_or("attempts")?
            .iter()
            .rev()
            .find(|attempt| attempt["operation"] == "reconcile")
            .ok_or("reconciliation")?["outcome"],
        "completed"
    );
    for (name, device) in [("m", &m), ("n", &n)] {
        assert_eq!(
            identity(root, name)?["keys"],
            json!({"items": [1, 2], "notes": [1, 2]}),
            "{name}"
        );
        assert!(!device.database.status().await?.data.bootstrap_pending);
    }
    create_row(&m, 3, "items", INSERT_ITEM, "after").await;
    create_row(&n, 4, "notes", INSERT_NOTE, "after").await;
    for device in [&m, &n, &m] {
        device.sync().await.map_err(|error| error.to_string())?;
    }
    assert_eq!(
        texts(&n, "SELECT title FROM items ORDER BY title").await,
        ["after", "before"]
    );
    assert_eq!(
        texts(&m, "SELECT text FROM notes ORDER BY text").await,
        ["after", "before"],
        "rows flow under both new epochs through either issuer's members"
    );
    for device in [&x, &y] {
        assert!(common::exchange_failed(&device.sync().await));
        assert!(
            device
                .database
                .status()
                .await?
                .authority
                .verified_revocation
                .is_some(),
            "each revoked device is told why by its own issuer"
        );
    }
    assert_eq!(
        texts(&x, "SELECT title FROM items ORDER BY title").await,
        ["before"]
    );
    Domain { a, b, x, y, m, n }.close().await;
    Ok(())
}

#[tokio::test]
async fn partitioned_issuers_revocations_merge_when_a_syncs_first() -> Result<(), Failure> {
    partitioned_revocations_merge(true).await
}

#[tokio::test]
async fn partitioned_issuers_revocations_merge_when_b_syncs_first() -> Result<(), Failure> {
    partitioned_revocations_merge(false).await
}

async fn conflicting_root_revocations_merge(b_first: bool) -> Result<(), Failure> {
    use rocky_sdk::{RevocationMode, RevocationReason};

    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a").await?;
    let early = Device::new(
        open(&root.join("early"), &hub, "early")
            .join(&a.invite().await)
            .await?,
    )?;
    let mut issuers = Vec::new();
    for name in ["b", "c"] {
        let invitation = a
            .database
            .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
            .await?;
        let issuer = Device::new(
            open(&root.join(name), &hub, name)
                .join(&invitation.code)
                .await?,
        )?;
        a.database.add_peer(&issuer.contact().await).await?;
        let identity = issuer.identity().await;
        let grant = a
            .database
            .trust(identity.parse()?, a.root_signer()?)
            .await?;
        issuer.database.install_trust(&grant.grant).await?;
        issuers.push(issuer);
    }
    let [b, c]: [Device; 2] = issuers.try_into().map_err(|_| "two issuers")?;
    for device in [&b, &c, &b] {
        device.sync_now().await?;
    }
    early.database.renew().await?;
    let creator = a.identity().await;
    let root_signer = a.root_signer()?;
    a.close().await;
    drop(a);

    let lost = b
        .database
        .revoke_trusted(creator.parse()?, RevocationMode::Lost, root_signer.clone())
        .await?;
    let compromised = c
        .database
        .revoke_trusted(creator.parse()?, RevocationMode::Compromised, root_signer)
        .await?;
    assert_eq!(lost.revocations.statements, 1);
    assert_eq!(compromised.revocations.statements, 1);
    assert_ne!(lost.statement, compromised.statement);

    let (first, second) = if b_first { (&b, &c) } else { (&c, &b) };
    let _ = first.sync_now().await;
    let _ = second.sync_now().await;
    let left = b.database.info().await?.revocations;
    let right = c.database.info().await?.revocations;
    assert_eq!(left.statements, 2);
    assert_eq!(left.digest, right.digest);
    assert_eq!(right.statements, 2);
    let error = early
        .database
        .renew()
        .await
        .err()
        .ok_or("renewed after cascade")?;
    assert_eq!(error.code(), "revoked", "{error}");
    let status = early.database.status().await?;
    assert_eq!(
        status
            .authority
            .verified_revocation
            .as_ref()
            .map(|notice| &notice.reason),
        Some(&RevocationReason::Cascade { trusted: creator }),
        "{status:?}"
    );
    for device in [&b, &c, &early] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn conflicting_root_modes_merge_when_lost_syncs_first() -> Result<(), Failure> {
    conflicting_root_revocations_merge(true).await
}

#[tokio::test]
async fn conflicting_root_modes_merge_when_compromised_syncs_first() -> Result<(), Failure> {
    conflicting_root_revocations_merge(false).await
}
