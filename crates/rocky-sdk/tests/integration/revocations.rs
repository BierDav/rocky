//! Concurrent revocations by two issuers merge into one grow-only set, on
//! the in-process hub.
use common::{Device, Failure};
use rocky_sdk::{
    Database, GrantProfile, Network, Open, ReadQuery, RevocationStatus, SqlValue,
    transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

use crate::common;

const SCHEMA: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"text","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

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
fn create(root: &Path, hub: &Arc<Hub>, name: &str) -> Result<Device, Failure> {
    Device::new(Database::create(open(&root.join(name), hub, name), None)?)
}
async fn join(
    root: &Path,
    hub: &Arc<Hub>,
    name: &str,
    issuer: &Device,
    profile: GrantProfile,
) -> Result<Device, Failure> {
    let invitation = issuer.session().invitation_with(profile).await?.uri;
    Device::new(Database::join(
        open(&root.join(name), hub, name),
        &invitation,
    )?)
}
fn identity(root: &Path, name: &str) -> Result<Value, Failure> {
    let mut identity: Value =
        serde_json::from_slice(&std::fs::read(root.join(name).join("identity.json"))?)?;
    for epochs in identity["keys"].as_object_mut().ok_or("keys")?.values_mut() {
        for epoch in epochs.as_array_mut().ok_or("epochs")? {
            *epoch = epoch["generation"].clone();
        }
    }
    Ok(identity)
}
async fn revocations(device: &Device) -> RevocationStatus {
    device.session().info().await.expect("session").revocations
}
const INSERT_ITEM: &str = "INSERT INTO items(key,id,title,completed) VALUES(?,?,?,0)";
const INSERT_NOTE: &str = "INSERT INTO notes(key,id,text) VALUES(?,?,?)";
async fn create_row(device: &Device, handle: u8, table: &str, sql: &str, text: &str) {
    let handle = [handle; 32];
    device
        .session()
        .execute("BEGIN", Vec::new())
        .await
        .expect("begin");
    let id = device.session().next_id(table).await.expect("id");
    device
        .session()
        .execute(
            sql,
            vec![
                SqlValue::Text(data_encoding::HEXLOWER.encode(&handle)),
                id.sql(),
                SqlValue::Text(text.into()),
            ],
        )
        .await
        .expect("insert");
    device.session().commit(handle).await.expect("commit");
}
async fn texts(device: &Device, sql: &str) -> Vec<String> {
    device
        .session()
        .read(ReadQuery::new(sql, 64))
        .await
        .expect("read")
        .rows
        .into_iter()
        .map(|row| row[0].to_text().expect("text"))
        .collect()
}

/// Issuers A and B, each enrolling a reader it later revokes and a writer
/// of both collections; the readers read disjoint collections.
struct Domain {
    a: Device,
    b: Device,
    /// Reads `items`; A revokes it.
    x: Device,
    /// Reads `notes`; B revokes it.
    y: Device,
    /// Writers enrolled by A and by B, each the other's peer.
    m: Device,
    n: Device,
}
impl Domain {
    async fn new(root: &Path, hub: &Arc<Hub>) -> Result<Self, Failure> {
        let a = create(root, hub, "a")?;
        let b = join(root, hub, "b", &a, GrantProfile::Issuer).await?;
        let reader = |scope: &str| GrantProfile::Reader {
            scopes: [scope.into()].into(),
        };
        let writer = GrantProfile::Writer {
            scopes: ["items".into(), "notes".into()].into(),
        };
        let x = join(root, hub, "x", &a, reader("items")).await?;
        let y = join(root, hub, "y", &b, reader("notes")).await?;
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

/// Two issuers, partitioned, each revoke a device; they meet with
/// `a_first` choosing who syncs first. Every remaining device ends with both
/// statements and one digest, and no device refuses either statement.
async fn partitioned_revocations_merge(a_first: bool) -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b, x, y, m, n } = Domain::new(root, &hub).await?;
    let empty = revocations(&a).await;
    assert_eq!(empty.statements, 0);

    let by_a = a.session().revoke_identity(&x.identity().await).await?;
    let by_b = b.session().revoke_identity(&y.identity().await).await?;
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
        assert!(!device.session().status().await?.data.bootstrap_pending);
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
                .session()
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

/// Partitioned trusted issuers sign conflicting modes for the same creator.
/// Both statements survive; the cascade and digest agree in either sync order.
async fn conflicting_root_revocations_merge(b_first: bool) -> Result<(), Failure> {
    use rocky_sdk::{CallFailure, GrantProfile, RevocationMode, RevocationReason};

    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a")?;
    a.follow_main().await;
    let early = Device::new(Database::join(
        open(&root.join("early"), &hub, "early"),
        &a.invite().await,
    )?)?;
    let mut issuers = Vec::new();
    for name in ["b", "c"] {
        let invitation = a.session().invitation_with(GrantProfile::Issuer).await?;
        let issuer = Device::new(Database::join(
            open(&root.join(name), &hub, name),
            &invitation.uri,
        )?)?;
        let identity = issuer.identity().await;
        let grant = a.session().trust(&identity, a.root_signer()?).await?;
        issuer.session().install_trust(&grant.grant).await?;
        issuers.push(issuer);
    }
    let [b, c]: [Device; 2] = issuers.try_into().map_err(|_| "two issuers")?;
    for device in [&b, &c, &b] {
        device.session().sync_once().await?;
    }
    early.session().renew().await?;
    let creator = a.identity().await;
    let root_signer = a.root_signer()?;
    a.close().await;
    drop(a);

    let lost = b
        .session()
        .revoke_trusted(&creator, RevocationMode::Lost, root_signer.clone())
        .await?;
    let compromised = c
        .session()
        .revoke_trusted(&creator, RevocationMode::Compromised, root_signer)
        .await?;
    assert_eq!(lost.revocations.statements, 1);
    assert_eq!(compromised.revocations.statements, 1);
    assert_ne!(lost.statement, compromised.statement);

    let (first, second) = if b_first { (&b, &c) } else { (&c, &b) };
    let _ = first.session().sync_once().await;
    let _ = second.session().sync_once().await;
    let left = b.session().info().await?.revocations;
    let right = c.session().info().await?.revocations;
    assert_eq!(left.statements, 2);
    assert_eq!(left.digest, right.digest);
    assert_eq!(right.statements, 2);
    let error = early
        .session()
        .renew()
        .await
        .err()
        .ok_or("renewed after cascade")?;
    assert_eq!(
        CallFailure::from_failure(&error).code(),
        "revoked",
        "{error}"
    );
    let status = early.session().status().await?;
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
