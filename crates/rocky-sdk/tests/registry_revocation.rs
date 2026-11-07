//! Phase 7 acceptance on the in-process hub: precise recorded access and
//! the explicit full-rotation fallback for unrecorded devices.
// The SDK's blocking constructors, native transports and paused clocks.
#![cfg(not(target_family = "wasm"))]
use common::{Device, Failure};
use rocky_sdk::{Database, credentials::CredentialChain, transport::memory::Hub};
use serde_json::{Value, json};
use std::sync::Arc;

mod common;

#[path = "registry_revocation/fixture.rs"]
mod fixture;
use fixture::*;

#[tokio::test]
async fn another_issuer_revokes_a_writer_and_its_recorded_partition_precisely_with_enroller_stopped()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    let m = join(
        root,
        &hub,
        "m",
        &a,
        json!({"kind":"writer","scopes":["items"]}),
    )
    .await?;
    let (node, member) = (m.node().await, m.identity().await);
    let before = m.add(1, "before").await;
    assert!(!before.is_empty());
    sync(&m)
        .await
        .map_err(|error| format!("member before revocation: {error}"))?;
    a.ok(json!({"type":"partition_grant","device":{"node":node,"identity":member},"partitions":[grant(&a,"L").await]})).await;
    common::introduce(&m, &b).await;
    sync(&b)
        .await
        .map_err(|error| format!("registry before revocation: {error}"))?;
    let recorded = listed(&b, &member).await;
    let old_chain: CredentialChain =
        serde_json::from_value(identity(root, "m")?["credentials"].clone())?;
    a.close().await;
    // Revoking the identity names it and its recorded capability, so
    // no node of it renews again.
    let response = b.ok(json!({"type":"revoke","identity":member})).await;
    let state = identity(root, "b")?;
    assert_eq!(
        state["keys"],
        json!({"issuer_registry":[1],"items":[1,2],"notes":[1],"parts":[1],"labels":[1]})
    );
    assert_eq!(epoch(&b, "L").await, json!([1, 2]));
    assert_eq!(epoch(&b, "M").await, json!([1]));
    let set: rocky_sdk::credentials::RevocationSet =
        serde_json::from_value(state["revocations"].clone())?;
    let statement = set.statements().next().ok_or("statement")?;
    assert_eq!(
        data_encoding::HEXLOWER.encode(&statement.id()?),
        response["statement"]
    );
    assert!(statement.statement.identities.contains(&member.parse()?));
    let capability: [u8; 32] = data_encoding::HEXLOWER
        .decode(
            recorded["capability"]
                .as_str()
                .ok_or("capability")?
                .as_bytes(),
        )?
        .try_into()
        .map_err(|_| "capability length")?;
    assert!(statement.statement.capabilities.contains(&capability));
    assert!(set.ends_chain(&old_chain)?);
    // The same verifier used for incoming frames refuses A's assigned author.
    let leaf = old_chain.held().ok_or("identity capability")?;
    let publisher = state["publisher"].as_str().ok_or("publisher")?.parse()?;
    let author = old_chain.author_number()?;
    let table = leaf.grants.first().ok_or("grant")?.scope.table;
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    old_chain.verify_author(
        publisher,
        old_chain
            .certificate()
            .ok_or("certificate")?
            .capability
            .subject,
        author,
        table,
        &Default::default(),
        time,
    )?;
    assert!(
        old_chain
            .verify_author(
                publisher,
                old_chain
                    .certificate()
                    .ok_or("certificate")?
                    .capability
                    .subject,
                author,
                table,
                &set,
                time
            )
            .is_err()
    );

    assert_eq!(listed(&b, &member).await["revoked"], true);
    // An issuer never revokes a node, nor the identity twice.
    assert!(b.call(json!({"type":"revoke","node":node})).await.is_err());
    assert!(
        b.call(json!({"type":"revoke","identity":member}))
            .await
            .is_err()
    );
    // A device joining after the rotation reads earlier rows from a base
    // at a seal, without the rotated-out key.
    b.seal_and_compact().await;
    let w = join(
        root,
        &hub,
        "w",
        &b,
        json!({"kind":"writer","scopes":["items"]}),
    )
    .await?;
    w.add(2, "after").await;
    sync(&w)
        .await
        .map_err(|error| format!("remaining writer after revocation: {error}"))?;
    assert_eq!(w.titles().await, ["after", "before"]);
    assert!(common::exchange_failed(&m.sync().await));
    assert_eq!(m.titles().await, ["before"]);
    assert_eq!(identity(root, "m")?["keys"]["items"], json!([1]));
    // The enrolling issuer returns, learns the statement and tombstone in one sync.
    let a = Device::new(Database::open(open(&root.join("a"), &hub, "a"))?)?;
    // The rotation sealed the new secret to A, which the registry records:
    // its first sync collects it before checkpoint sync.
    sync(&a)
        .await
        .map_err(|error| format!("enrolling issuer after revocation: {error}"))?;
    assert_eq!(listed(&a, &member).await["revoked"], true);
    assert!(a.call(json!({"type":"grant","device":{"node":node,"identity":member},"profile":{"kind":"reader","scopes":["items"]}})).await.is_err());
    for device in [&a, &b, &m, &w] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn a_partition_only_record_rotates_its_group_and_no_other_scope() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    let keys = common::node_keys(&root.join("m"), Some(keys(&root.join("m"))))?;
    a.ok(json!({"type":"partition_grant","device":keys,"partitions":[grant(&a,"L").await]}))
        .await;
    sync(&b).await?;
    a.close().await;
    let before = identity(root, "b")?["keys"].clone();
    b.ok(json!({"type":"revoke","identity":keys.identity.to_hex()}))
        .await;
    assert_eq!(identity(root, "b")?["keys"], before);
    assert_eq!(epoch(&b, "L").await, json!([1, 2]));
    assert_eq!(epoch(&b, "M").await, json!([1]));
    assert_eq!(listed(&b, &keys.identity.to_hex()).await["revoked"], true);
    b.close().await;
    Ok(())
}

#[tokio::test]
async fn an_unrecorded_delegate_uses_full_rotation_fallback_without_revoking_its_recorded_delegator()
-> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    let member = common::node_keys(&root.join("m"), Some(keys(&root.join("m"))))?;
    let supplied = a.ok(json!({"type":"grant","device":member,"profile":{"kind":"reader","scopes":["items"]},"delegations":2})).await["supplied"].clone();
    let m = Device::new(Database::install(
        open(&root.join("m"), &hub, "m"),
        serde_json::from_value(supplied)?,
    )?)?;
    let delegate = common::node_keys(&root.join("d"), Some(keys(&root.join("d"))))?;
    m.ok(
        json!({"type":"delegate","device":delegate,"profile":{"kind":"reader","scopes":["items"]}}),
    )
    .await;
    sync(&b).await?;
    common::introduce(&m, &b).await;
    a.close().await;
    assert_eq!(listed(&b, &delegate.identity.to_hex()).await, Value::Null);
    b.ok(json!({"type":"revoke","identity":delegate.identity.to_hex()}))
        .await;
    for epochs in identity(root, "b")?["keys"]
        .as_object()
        .ok_or("keys")?
        .values()
    {
        assert_eq!(*epochs, json!([1, 2]));
    }
    for list in ["L", "M"] {
        assert_eq!(epoch(&b, list).await, json!([1, 2]));
    }
    assert_eq!(
        listed(&b, &member.identity.to_hex()).await["revoked"],
        false
    );
    let set: rocky_sdk::credentials::RevocationSet =
        serde_json::from_value(identity(root, "b")?["revocations"].clone())?;
    assert!(!set.revokes(&member.node));
    assert!(!set.revokes(&member.identity));
    sync(&m).await?;
    assert_eq!(identity(root, "m")?["keys"]["items"], json!([1, 2]));
    // Learning the statement also prevents handing the new key back to that node.
    assert!(m.call(json!({"type":"delegate","device":delegate,"profile":{"kind":"reader","scopes":["items"]}})).await.is_err());
    for device in [&a, &b, &m] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn revoking_an_issuer_rotates_the_registry_and_refuses_its_new_key_at_remaining_issuers()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    // B keeps authority independently of the issuer it is about to revoke.
    let grant = a
        .ok(json!({"type":"tips","action":"trust","identity":b.identity().await,"root":a.root()}))
        .await;
    b.ok(json!({"type":"tips","action":"install_trust","grant":grant["grant"]}))
        .await;
    sync(&b).await?;
    // A is the creating device's trusted node: an issuer revokes it only
    // with the root, and without it the refusal is typed.
    let refused = b
        .call(json!({"type": "revoke", "identity": a.identity().await}))
        .await
        .expect_err("needs the root");
    assert_eq!(refused["code"], "publisher_required", "{refused}");
    b.ok(json!({"type": "revoke", "identity": a.identity().await,"root":a.root()}))
        .await;
    assert_eq!(
        identity(root, "b")?["keys"]["issuer_registry"],
        json!([1, 2])
    );
    let chain: CredentialChain =
        serde_json::from_value(identity(root, "a")?["credentials"].clone())?;
    // A's own request for renewal is refused; self-renewal retains only its old keys.
    assert!(common::exchange_failed(&a.sync().await));
    assert_eq!(identity(root, "a")?["keys"]["issuer_registry"], json!([1]));
    let set: rocky_sdk::credentials::RevocationSet =
        serde_json::from_value(identity(root, "b")?["revocations"].clone())?;
    assert!(set.revoking(&chain)?.is_some());
    a.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test]
async fn two_issuers_revoking_the_same_non_reader_leave_two_statements_and_one_tombstone()
-> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    let keys = common::node_keys(&root.join("m"), Some(keys(&root.join("m"))))?;
    a.ok(json!({"type":"grant","device":keys,"profile":{"kind":"custody","scopes":["items"]}}))
        .await;
    sync(&b).await?;
    let before_a = identity(root, "a")?["keys"].clone();
    let before_b = identity(root, "b")?["keys"].clone();
    // No sync between the two calls: both decide from an unrevoked record.
    let by_a = a
        .ok(json!({"type":"revoke","identity":keys.identity.to_hex()}))
        .await;
    let by_b = b
        .ok(json!({"type":"revoke","identity":keys.identity.to_hex()}))
        .await;
    assert_ne!(by_a["statement"], by_b["statement"]);
    assert_eq!(identity(root, "a")?["keys"], before_a);
    assert_eq!(identity(root, "b")?["keys"], before_b);
    // The handshake gives each issuer the other's statement.
    sync(&a).await?;
    assert_eq!(
        a.ok(json!({"type":"session"})).await["revocations"]["statements"],
        2
    );
    sync(&b).await?;
    sync(&a).await?;
    for (name, device) in [("a", &a), ("b", &b)] {
        assert_eq!(
            device.ok(json!({"type":"session"})).await["revocations"]["statements"],
            2
        );
        assert_eq!(
            listed(device, &keys.identity.to_hex()).await["revoked"],
            true
        );
        let conn = rusqlite::Connection::open_with_flags(
            root.join(name).join("rocky.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM issuer_registry WHERE identity=? AND entry='revoked'",
            [keys.identity.to_hex()],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
    }
    a.close().await;
    b.close().await;
    Ok(())
}
