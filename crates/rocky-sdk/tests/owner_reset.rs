//! "This is everything: sign out everywhere else": the root ends every
//! node but the signer and its custodians, including ones it never saw,
//! and the fence discards what it did not hold.
#![cfg(not(target_family = "wasm"))]
use common::identity::snapshot as snap;
use rocky_sdk::{
    ActivityKind, CallFailure, Database, Freshness, Network, Open, ResetKeep, RestoreChoice,
    transport::memory::Hub,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

mod common;
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
        schema: SCHEMA,
        secrets: None,
        network: Some(Network::Owned(hub.connector(name))),
        coordination: None,
    }
}
async fn write_note(device: &Device, key: &str) {
    snap::ok(device.session.execute("BEGIN", Vec::new())).await;
    let id = snap::ok(device.session.next_id("notes")).await;
    snap::ok(device.session.execute(
        &format!("INSERT INTO notes(key,id) VALUES('{key}',?)"),
        vec![snap::row_id(&id["id"])],
    ))
    .await;
    snap::ok(device.session.execute("COMMIT", Vec::new())).await;
}
async fn notes(device: &Device) -> Vec<String> {
    let read = snap::ok(device.session.read(rocky_sdk::ReadQuery {
        sql: "SELECT key FROM notes ORDER BY key".into(),
        values: vec![],
        max_rows: 256,
        max_bytes: None,
        cursor: None,
    }))
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
/// Syncs, then renews, so `device` learns what ended it.
async fn learn_ended(device: &Device) {
    let _ = device.sync().await;
    let _ = snap::observe(async {
        device.session.renew().await?;
        device.session.info().await
    })
    .await;
}
/// Whether `device` recorded that reset `reset` ended it.
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
/// The refusal a reset on `device` ends with.
async fn refusal(device: &Device, root: &Device) -> Result<CallFailure, Failure> {
    let error = device
        .session
        .sign_out_everywhere_else(root.root(), ResetKeep::default())
        .await
        .err()
        .ok_or("the reset succeeded")?;
    Ok(CallFailure::from_failure(&error))
}
/// `device` refuses to reset: reset `by` already signed it out.
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
    // The owner holds what the laptop synced; the fence keeps it.
    write_note(&laptop, "seen").await;
    laptop.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(notes(&owner).await, ["before", "seen"]);
    // Written while the owner was away: the reset discards it everywhere.
    write_note(&laptop, "hidden").await;
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
    // The laptop learns it was signed out by the reset, and its writes stay out.
    learn_ended(&laptop).await;
    assert_eq!(revocation_reason(&laptop).await["type"], "reset");
    assert!(recorded_reset(&laptop, receipt.reset).await?);
    assert_eq!(
        laptop.session.freshness().await?,
        Freshness::LocallyAnchored
    );
    let _ = owner.sync().await;
    assert_eq!(notes(&owner).await, ["before", "seen"]);
    refuses_as_signed_out(&laptop, &owner, receipt.reset).await?;
    // An ended custodian is refused, recording nothing.
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
    // A second reset follows the first.
    let again = owner
        .session
        .sign_out_everywhere_else(owner.root(), ResetKeep::default())
        .await?;
    assert_eq!(again.follows, [receipt.reset].into());
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
    // The owner's node signed the laptop's grant; the reset ends that node.
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
    // The laptop's identity and grants survive the reset it signed.
    snap::ok(laptop.session.status()).await;
    write_note(&laptop, "after").await;
    assert_eq!(notes(&laptop).await, ["after", "before"]);
    assert_eq!(
        laptop.session.freshness().await?,
        Freshness::FencedByOwner {
            reset: receipt.reset
        }
    );
    // The owner, whose chain the root signed directly, is ended and refused.
    learn_ended(&owner).await;
    assert_eq!(revocation_reason(&owner).await["type"], "reset");
    refuses_as_signed_out(&owner, &owner, receipt.reset).await?;
    for device in [&laptop, &owner] {
        device.close().await;
    }
    Ok(())
}

/// Joins `name` through `invitation`, the normal invite flow.
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

    // A phone joins through the owner's invitation after the reset.
    let phone = join(
        &root.path().join("phone"),
        &hub,
        &owner.invite_as(json!({"kind": "issuer"})).await,
    )?;
    common::introduce(&owner, &phone).await;
    phone.sync().await.map_err(|error| error.to_string())?;
    write_note(&phone, "phone").await;
    phone.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(notes(&owner).await, ["before", "phone"]);
    assert_eq!(phone.session.freshness().await?, Freshness::LocallyAnchored);
    // A tablet the phone invites joins too: an admitted device admits.
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
    assert_eq!(notes(&owner).await, ["before", "phone", "tablet"]);
    assert_eq!(notes(&tablet).await, ["before", "phone", "tablet"]);

    // The signed-out laptop still invites, but admits nobody: the device
    // it enrolls stays ended, so kept devices refuse it as a peer and its
    // writes never reach them.
    let stray = join(
        &root.path().join("stray"),
        &hub,
        &laptop
            .invite_as(json!({"kind": "writer", "scopes": ["notes"]}))
            .await,
    )?;
    let contact = stray.contact().await;
    for kept in [&owner, &phone] {
        let refused = kept
            .add_peer(&contact)
            .await
            .err()
            .ok_or("a kept device added the stray")?;
        assert_eq!(refused["code"], "rejected", "{refused}");
    }
    let _ = snap::observe(async {
        stray.session.execute("BEGIN", Vec::new()).await?;
        let id = stray.session.next_id("notes").await?;
        stray
            .session
            .execute(
                "INSERT INTO notes(key,id) VALUES('stray',?)",
                vec![id.sql()],
            )
            .await?;
        stray.session.execute("COMMIT", Vec::new()).await
    })
    .await;
    common::introduce(&laptop, &stray).await;
    let _ = stray.sync().await;
    let _ = laptop.sync().await;
    let _ = owner.sync().await;
    assert_eq!(notes(&owner).await, ["before", "phone", "tablet"]);
    for device in [&stray, &tablet, &phone, &laptop, &owner] {
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
    // Excepting one signs it out; the rest stay kept.
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
    assert_eq!(again.kept, [own, server_node].into());
    for device in [&backup, &server, &owner] {
        device.close().await;
    }
    Ok(())
}
