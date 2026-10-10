//! Phase 7 acceptance on the in-process hub: precise recorded access and
//! the explicit full-rotation fallback for unrecorded devices.
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
    let node = m.node().await;
    let before = m.add(1, "before").await;
    assert!(!before.is_empty());
    sync(&m)
        .await
        .map_err(|error| format!("member before revocation: {error}"))?;
    let record = listed(&a, &node).await;
    a.ok(json!({"type":"partition_grant","device":{"node":node,"user":record["user"]},"partitions":[grant(&a,"L").await]})).await;
    common::introduce(&m, &b).await;
    sync(&b)
        .await
        .map_err(|error| format!("registry before revocation: {error}"))?;
    let recorded = listed(&b, &node).await;
    assert_eq!(recorded["pseudonyms"], record["pseudonyms"]);
    let old_chain: CredentialChain =
        serde_json::from_value(identity(root, "m")?["pseudonyms"]["items"].clone())?;
    a.close().await;
    let response = b.ok(json!({"type":"revoke","node":node})).await;
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
    assert!(
        statement
            .statement
            .users
            .contains(&recorded["user"].as_str().ok_or("user")?.parse()?)
    );
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
    for pseudonym in recorded["pseudonyms"].as_array().ok_or("pseudonyms")? {
        assert!(
            statement
                .statement
                .pseudonyms
                .contains(&pseudonym["key"].as_str().ok_or("key")?.parse()?)
        );
    }
    assert!(set.ends(old_chain.0.last().ok_or("pseudonym leaf")?)?);
    // The same verifier used for incoming frames refuses A's assigned author.
    let leaf = old_chain.0.last().ok_or("pseudonym leaf")?;
    let publisher = state["publisher"].as_str().ok_or("publisher")?.parse()?;
    let manifest = leaf.capability.manifest;
    let author = old_chain
        .authors()?
        .and_then(|range| range.single())
        .ok_or("author")?;
    let table = leaf.capability.grants.first().ok_or("grant")?.scope.table;
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    old_chain.verify_author(
        publisher,
        manifest,
        leaf.capability.subject,
        author,
        table,
        &Default::default(),
        time,
    )?;
    assert!(
        old_chain
            .verify_author(
                publisher,
                manifest,
                leaf.capability.subject,
                author,
                table,
                &set,
                time
            )
            .is_err()
    );

    assert_eq!(listed(&b, &node).await["revoked"], true);
    assert!(b.call(json!({"type":"revoke","node":node})).await.is_err());
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
    assert!(m.sync().await.is_err());
    assert_eq!(m.titles().await, ["before"]);
    assert_eq!(identity(root, "m")?["keys"]["items"], json!([1]));
    // The enrolling issuer returns, learns the statement and tombstone in one sync.
    let a = Device::new(Database::open(open(&root.join("a"), &hub, "a"))?)?;
    // Existing group-key sharing supplies the rotated secret before checkpoint sync.
    let shared = b.ok(json!({"type":"partition_share","group":"list","values":values("L"),"recipients":[a.node().await]})).await;
    a.ok(json!({"type":"partition_install","group":"list","values":values("L"),"envelope":shared["envelopes"][0]["envelope"]})).await;
    sync(&a)
        .await
        .map_err(|error| format!("enrolling issuer after revocation: {error}"))?;
    assert_eq!(listed(&a, &node).await["revoked"], true);
    assert!(a.call(json!({"type":"grant","device":{"node":node,"user":recorded["user"]},"profile":{"kind":"reader","scopes":["items"]}})).await.is_err());
    for device in [&a, &b, &m, &w] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn a_partition_only_record_rotates_its_group_and_no_other_scope() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    let keys = Database::device(&root.join("m"), Some(keys(&root.join("m"))))?;
    a.ok(json!({"type":"partition_grant","device":keys,"partitions":[grant(&a,"L").await]}))
        .await;
    sync(&b).await?;
    a.close().await;
    let before = identity(root, "b")?["keys"].clone();
    b.ok(json!({"type":"revoke","node":keys.node.to_hex()}))
        .await;
    assert_eq!(identity(root, "b")?["keys"], before);
    assert_eq!(epoch(&b, "L").await, json!([1, 2]));
    assert_eq!(epoch(&b, "M").await, json!([1]));
    assert_eq!(listed(&b, &keys.node.to_hex()).await["revoked"], true);
    b.close().await;
    Ok(())
}

#[tokio::test]
async fn an_unrecorded_delegate_uses_full_rotation_fallback_without_revoking_its_recorded_delegator()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    let member = Database::device(&root.join("m"), Some(keys(&root.join("m"))))?;
    let supplied = a.ok(json!({"type":"grant","device":member,"profile":{"kind":"reader","scopes":["items"]},"delegations":2})).await["supplied"].clone();
    let m = Device::new(Database::install(
        open(&root.join("m"), &hub, "m"),
        serde_json::from_value(supplied)?,
    )?)?;
    let delegate = Database::device(&root.join("d"), Some(keys(&root.join("d"))))?;
    m.ok(
        json!({"type":"delegate","device":delegate,"profile":{"kind":"reader","scopes":["items"]}}),
    )
    .await;
    sync(&b).await?;
    common::introduce(&m, &b).await;
    a.close().await;
    assert_eq!(listed(&b, &delegate.node.to_hex()).await, Value::Null);
    b.ok(json!({"type":"revoke","node":delegate.node.to_hex()}))
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
    assert_eq!(listed(&b, &member.node.to_hex()).await["revoked"], false);
    let set: rocky_sdk::credentials::RevocationSet =
        serde_json::from_value(identity(root, "b")?["revocations"].clone())?;
    assert!(!set.revokes(&member.node));
    assert!(!set.revokes(&member.user));
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
    sync(&b).await?;
    b.ok(json!({"type":"revoke","node":a.node().await})).await;
    assert_eq!(
        identity(root, "b")?["keys"]["issuer_registry"],
        json!([1, 2])
    );
    let chain: CredentialChain =
        serde_json::from_value(identity(root, "a")?["credentials"].clone())?;
    // A's own request for renewal is refused; self-renewal retains only its old keys.
    assert!(a.sync().await.is_err());
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
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    let keys = Database::device(&root.join("m"), Some(keys(&root.join("m"))))?;
    a.ok(json!({"type":"grant","device":keys,"profile":{"kind":"custody","scopes":["items"]}}))
        .await;
    sync(&b).await?;
    let before_a = identity(root, "a")?["keys"].clone();
    let before_b = identity(root, "b")?["keys"].clone();
    // No sync between the two calls: both decide from an unrevoked record.
    let by_a = a
        .ok(json!({"type":"revoke","node":keys.node.to_hex()}))
        .await;
    let by_b = b
        .ok(json!({"type":"revoke","node":keys.node.to_hex()}))
        .await;
    assert_ne!(by_a["statement"], by_b["statement"]);
    assert_eq!(identity(root, "a")?["keys"], before_a);
    assert_eq!(identity(root, "b")?["keys"], before_b);
    // Phase 6's renew-once exchange can learn B's statement before B has A's.
    if let Err(error) = a.sync().await {
        assert_eq!(error["code"], "revocations_differ", "{error}");
    }
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
        assert_eq!(listed(device, &keys.node.to_hex()).await["revoked"], true);
        let conn = rusqlite::Connection::open_with_flags(
            root.join(name).join("rocky.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM issuer_registry WHERE node=? AND entry='revoked'",
            [keys.node.to_hex()],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
    }
    a.close().await;
    b.close().await;
    Ok(())
}
