use crate::support::snapshot as snap;
use rocky_sdk::{
    ActivityKind, CallFailure, Database, Freshness, Network, Open, ResetKeep, RestoreChoice,
    transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

use crate::common;
use common::{Device, Failure};

const SCHEMA: &str = r#"{"format":6,"application":"reset","version":1,"tables":[
{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

fn open<'a>(directory: &'a Path, hub: &Arc<Hub>) -> Open<'a> {
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("node");
    Open {
        identity: None,
        directory,
        schema: crate::support::snapshot::schema(SCHEMA),
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}
async fn write_note(device: &Device, key: &str) {
    let mut tx = device.database.transaction().await.expect("begin");
    let id = tx.next_id("notes", Vec::new()).await.expect("id");
    tx.execute(
        &format!("INSERT INTO notes(key,id) VALUES('{key}',?)"),
        vec![id.sql()],
    )
    .await
    .expect("insert");
    tx.commit().await.expect("commit");
}
async fn notes(device: &Device) -> Vec<String> {
    let read = snap::ok(snap::read(
        &device.database,
        "SELECT key FROM notes ORDER BY key",
        Vec::new(),
    ))
    .await;
    read["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row[0]["value"].as_str().map(String::from))
        .collect()
}
async fn revocation_reason(device: &Device) -> Value {
    snap::ok(device.session.status()).await["authority"]["verified_revocation"]["reason"].clone()
}
async fn learn_ended(device: &Device) {
    let _ = device.sync().await;
    let _ = snap::observe(async {
        device.session.renew().await?;
        device.session.info().await
    })
    .await;
}
async fn recorded_reset(device: &Device, reset: rocky_sdk::Commitment) -> Result<bool, Failure> {
    let mut after = 0;
    loop {
        let page = device.session.activity(after).await?;
        if page.events.iter().any(
            |event| matches!(event.detail, ActivityKind::NodeReset { reset: ended, .. } if ended == reset),
        ) {
            return Ok(true);
        }
        if page.events.is_empty() || page.token == after {
            return Ok(false);
        }
        after = page.token;
    }
}
async fn refusal(device: &Device, root: &Device) -> Result<CallFailure, Failure> {
    let error = device
        .session
        .sign_out_everywhere_else(root.root(), ResetKeep::default())
        .await
        .err()
        .ok_or("the reset succeeded")?;
    Ok(CallFailure::from_failure(&error))
}
async fn refuses_as_signed_out(
    device: &Device,
    root: &Device,
    by: rocky_sdk::Commitment,
) -> Result<(), Failure> {
    let refusal = refusal(device, root).await?;
    assert_eq!(refusal.code(), "device_signed_out", "{refusal:?}");
    assert!(
        refusal
            .detail
            .contains(&data_encoding::HEXLOWER.encode(&by)),
        "{refusal:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn signing_out_everywhere_else_ends_other_devices_and_discards_what_it_never_held()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = Device::new(Database::create(
        open(&root.path().join("owner"), &hub),
        None,
    )?)?;
    write_note(&owner, "before").await;
    let laptop = Device::new(Database::join(
        open(&root.path().join("laptop"), &hub),
        &owner.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    common::introduce(&owner, &laptop).await;
    laptop.sync().await.map_err(|error| error.to_string())?;
    write_note(&laptop, "seen").await;
    laptop.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(notes(&owner).await, ["before", "seen"]);
    let written_while_the_owner_was_away = "hidden";
    write_note(&laptop, written_while_the_owner_was_away).await;
    let laptop_node = rocky_credentials::PeerId::of(laptop.session.info().await?.node);
    assert_eq!(owner.session.freshness().await?, Freshness::LocallyAnchored);
    assert_eq!(RestoreChoice::ALL[0], RestoreChoice::DEFAULT);
    assert!(
        RestoreChoice::ThisIsEverything
            .detail()
            .contains("discarded")
    );

    let receipt = owner
        .session
        .sign_out_everywhere_else(owner.root(), ResetKeep::default())
        .await?;
    assert_eq!(receipt.kept.len(), 1);
    assert!(receipt.follows.is_empty());
    assert!(receipt.fenced_logs >= 1);
    assert_eq!(
        owner.session.freshness().await?,
        Freshness::FencedByOwner {
            reset: receipt.reset
        }
    );
    learn_ended(&laptop).await;
    assert_eq!(
        revocation_reason(&laptop).await["type"],
        "reset",
        "the laptop learns it was signed out by the reset"
    );
    assert!(recorded_reset(&laptop, receipt.reset).await?);
    assert_eq!(
        laptop.session.freshness().await?,
        Freshness::LocallyAnchored
    );
    let _ = owner.sync().await;
    assert_eq!(
        notes(&owner).await,
        ["before", "seen"],
        "the fence keeps what the owner held and discards what was written while it was away"
    );
    refuses_as_signed_out(&laptop, &owner, receipt.reset).await?;
    let error = owner
        .session
        .sign_out_everywhere_else(
            owner.root(),
            ResetKeep {
                also: [laptop_node].into(),
                ..ResetKeep::default()
            },
        )
        .await
        .err()
        .ok_or("a reset kept an ended custodian")?;
    assert_eq!(
        CallFailure::from_failure(&error).code(),
        "custodian_signed_out"
    );
    let again = owner
        .session
        .sign_out_everywhere_else(owner.root(), ResetKeep::default())
        .await?;
    assert_eq!(
        again.follows,
        [receipt.reset].into(),
        "a second reset follows the first"
    );
    write_note(&owner, "after").await;
    assert_eq!(notes(&owner).await, ["after", "before", "seen"]);
    for device in [&laptop, &owner] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_another_enrolled_resets_and_the_root_signed_device_it_ends_cannot()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = Device::new(Database::create(
        open(&root.path().join("owner"), &hub),
        None,
    )?)?;
    write_note(&owner, "before").await;
    let laptop = Device::new(Database::join(
        open(&root.path().join("laptop"), &hub),
        &owner.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    common::introduce(&owner, &laptop).await;
    laptop.sync().await.map_err(|error| error.to_string())?;
    let receipt = laptop
        .session
        .sign_out_everywhere_else(owner.root(), ResetKeep::default())
        .await?;
    snap::ok(laptop.session.status()).await;
    write_note(&laptop, "after").await;
    assert_eq!(
        notes(&laptop).await,
        ["after", "before"],
        "the resetting device's identity and grants survive though the ended node signed them"
    );
    assert_eq!(
        laptop.session.freshness().await?,
        Freshness::FencedByOwner {
            reset: receipt.reset
        }
    );
    learn_ended(&owner).await;
    assert_eq!(
        revocation_reason(&owner).await["type"],
        "reset",
        "the root-signed device the reset ends is ended too"
    );
    refuses_as_signed_out(&owner, &owner, receipt.reset).await?;
    for device in [&laptop, &owner] {
        device.close().await;
    }
    Ok(())
}

fn join(directory: &Path, hub: &Arc<Hub>, invitation: &str) -> Result<Device, Failure> {
    Device::new(Database::join(open(directory, hub), invitation)?)
}
async fn node_of(device: &Device) -> Result<rocky_credentials::PeerId, Failure> {
    Ok(rocky_credentials::PeerId::of(
        device.session.info().await?.node,
    ))
}

#[tokio::test(flavor = "multi_thread")]
async fn devices_invited_after_a_reset_sync_and_write_and_a_signed_out_device_admits_none()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = Device::new(Database::create(
        open(&root.path().join("owner"), &hub),
        None,
    )?)?;
    write_note(&owner, "before").await;
    let laptop = join(
        &root.path().join("laptop"),
        &hub,
        &owner.invite_as(json!({"kind": "issuer"})).await,
    )?;
    common::introduce(&owner, &laptop).await;
    laptop.sync().await.map_err(|error| error.to_string())?;
    let receipt = owner
        .session
        .sign_out_everywhere_else(owner.root(), ResetKeep::default())
        .await?;
    learn_ended(&laptop).await;
    refuses_as_signed_out(&laptop, &owner, receipt.reset).await?;

    let phone = join(
        &root.path().join("phone"),
        &hub,
        &owner.invite_as(json!({"kind": "issuer"})).await,
    )?;
    common::introduce(&owner, &phone).await;
    phone.sync().await.map_err(|error| error.to_string())?;
    write_note(&phone, "phone").await;
    phone.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        notes(&owner).await,
        ["before", "phone"],
        "a device joins through the owner's invitation after the reset"
    );
    assert_eq!(phone.session.freshness().await?, Freshness::LocallyAnchored);
    let tablet = join(
        &root.path().join("tablet"),
        &hub,
        &phone
            .invite_as(json!({"kind": "writer", "scopes": ["notes"]}))
            .await,
    )?;
    common::introduce(&phone, &tablet).await;
    tablet.sync().await.map_err(|error| error.to_string())?;
    write_note(&tablet, "tablet").await;
    tablet.sync().await.map_err(|error| error.to_string())?;
    phone.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        notes(&owner).await,
        ["before", "phone", "tablet"],
        "a device admitted after the reset admits others"
    );
    assert_eq!(notes(&tablet).await, ["before", "phone", "tablet"]);

    let enrolled_by_signed_out_laptop = join(
        &root.path().join("stray"),
        &hub,
        &laptop
            .invite_as(json!({"kind": "writer", "scopes": ["notes"]}))
            .await,
    )?;
    let contact = enrolled_by_signed_out_laptop.contact().await;
    for kept in [&owner, &phone] {
        let refused = kept
            .add_peer(&contact)
            .await
            .err()
            .ok_or("a kept device added the stray")?;
        assert_eq!(
            refused["code"], "rejected",
            "kept devices refuse a device a signed-out one enrolled: {refused}"
        );
    }
    let _ = async {
        let mut tx = enrolled_by_signed_out_laptop.database.transaction().await?;
        let id = tx.next_id("notes", Vec::new()).await?;
        tx.execute(
            "INSERT INTO notes(key,id) VALUES('stray',?)",
            vec![id.sql()],
        )
        .await?;
        tx.commit().await?;
        Ok::<_, Failure>(())
    }
    .await;
    common::introduce(&laptop, &enrolled_by_signed_out_laptop).await;
    let _ = enrolled_by_signed_out_laptop.sync().await;
    let _ = laptop.sync().await;
    let _ = owner.sync().await;
    assert_eq!(
        notes(&owner).await,
        ["before", "phone", "tablet"],
        "writes of a device a signed-out one enrolled never reach kept devices"
    );
    for device in [
        &enrolled_by_signed_out_laptop,
        &tablet,
        &phone,
        &laptop,
        &owner,
    ] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reset_keeps_the_custodians_it_knows_unless_told_otherwise() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let owner = Device::new(Database::create(
        open(&root.path().join("owner"), &hub),
        None,
    )?)?;
    let custody = json!({"kind": "custody", "scopes": ["notes"]});
    let server = join(
        &root.path().join("server"),
        &hub,
        &owner.invite_as(custody.clone()).await,
    )?;
    let backup = join(
        &root.path().join("backup"),
        &hub,
        &owner.invite_as(custody).await,
    )?;
    let (own, server_node, backup_node) = (
        node_of(&owner).await?,
        node_of(&server).await?,
        node_of(&backup).await?,
    );
    let receipt = owner
        .session
        .sign_out_everywhere_else(owner.root(), ResetKeep::default())
        .await?;
    assert_eq!(receipt.kept, [own, server_node, backup_node].into());
    let again = owner
        .session
        .sign_out_everywhere_else(
            owner.root(),
            ResetKeep {
                except: [backup_node].into(),
                ..ResetKeep::default()
            },
        )
        .await?;
    assert_eq!(
        again.kept,
        [own, server_node].into(),
        "excepting one device signs it out and the rest stay kept"
    );
    for device in [&backup, &server, &owner] {
        device.close().await;
    }
    Ok(())
}
