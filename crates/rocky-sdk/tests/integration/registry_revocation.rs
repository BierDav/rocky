use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_credentials::CredentialChain;
use rocky_sdk::transport::memory::Hub;
use serde_json::json;
use std::sync::Arc;

use crate::common;

pub(crate) mod fixture;
use fixture::*;

#[tokio::test]
async fn another_issuer_revokes_a_writer_and_its_recorded_partition_precisely_with_enroller_stopped()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    let m = join(root, &hub, "m", &a, role("item_writer")).await?;
    let (node, member) = (m.node().await, m.identity().await);
    let before = m.add(1, "before").await;
    assert!(!before.is_empty());
    sync(&m)
        .await
        .map_err(|error| format!("member before revocation: {error}"))?;
    grant(&a, &member, "L").await;
    common::introduce(&m, &b).await;
    sync(&b)
        .await
        .map_err(|error| format!("registry before revocation: {error}"))?;
    assert_ne!(
        listed(&b, &member).await,
        serde_json::Value::Null,
        "b lists the member before removal"
    );
    let old_chain: CredentialChain =
        serde_json::from_value(identity(root, "m")?["credentials"].clone())?;
    a.close().await;
    snap::ok(b.database.remove(
        rocky_sdk::PersonId::new(snap::public_key(&(member))),
        rocky_sdk::Removal::Departed,
    ))
    .await;
    let state = identity(root, "b")?;
    assert_eq!(
        state["keys"],
        json!({"issuer_registry":[1],"items":[1,2],"notes":[1],"parts":[1],"labels":[1]})
    );
    assert_eq!(epoch(&b, "L").await, json!([1, 2]));
    assert_eq!(epoch(&b, "M").await, json!([1]));
    let set: rocky_credentials::RevocationSet =
        serde_json::from_value(state["revocations"].clone())?;
    let statement = set.statements().next().ok_or("statement")?;
    assert!(statement.statement.identities.contains(&member.parse()?));
    let capability = old_chain
        .0
        .iter()
        .rev()
        .find(|link| link.capability.kind == rocky_core::PrincipalKind::User)
        .ok_or("member capability")?
        .id()?;
    assert!(
        statement.statement.capabilities.contains(&capability),
        "revoking an identity names its recorded capability, so no node of it renews again"
    );
    assert!(set.ends_chain(&old_chain)?);
    let leaf = old_chain.held().ok_or("identity capability")?;
    let publisher = state["publisher"].as_str().ok_or("publisher")?.parse()?;
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
                table,
                &set,
                time
            )
            .is_err(),
        "the verifier for incoming frames refuses the revoked node"
    );

    assert_eq!(
        listed(&b, &member).await,
        serde_json::Value::Null,
        "a removed person is no longer listed"
    );
    assert!(
        snap::observe(b.database.remove(
            rocky_sdk::PersonId::new(snap::public_key(&(node))),
            rocky_sdk::Removal::Departed
        ))
        .await
        .is_err(),
        "an issuer never revokes a node"
    );
    assert!(
        snap::observe(b.database.remove(
            rocky_sdk::PersonId::new(snap::public_key(&(member))),
            rocky_sdk::Removal::Departed
        ))
        .await
        .is_err(),
        "an issuer never revokes an identity twice"
    );
    b.seal_and_compact().await;
    let w = join(root, &hub, "w", &b, rocky_sdk::Access::Full).await?;
    w.add(2, "after").await;
    sync(&w)
        .await
        .map_err(|error| format!("remaining writer after revocation: {error}"))?;
    assert_eq!(
        w.titles().await,
        ["after", "before"],
        "a device joining after the rotation reads earlier rows from a sealed base"
    );
    assert!(common::exchange_failed(&m.sync().await));
    assert_eq!(m.titles().await, ["before"]);
    assert_eq!(identity(root, "m")?["keys"]["items"], json!([1]));
    let a = Device::new(open(&root.join("a"), &hub, "a").open().await?)?;
    sync(&a)
        .await
        .map_err(|error| format!("enrolling issuer after revocation: {error}"))?;
    assert_eq!(
        listed(&a, &member).await,
        serde_json::Value::Null,
        "the returning enrolling issuer learns the statement and tombstone in one sync"
    );
    assert!(
        snap::observe(a.database.grant_partition(
            rocky_sdk::PersonId::new(snap::public_key(&member)),
            &rocky_sdk::Partition::new("list", values("M"))
        ))
        .await
        .is_err()
    );
    for device in [&a, &b, &m, &w] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn a_partition_only_record_rotates_its_group_and_no_other_scope() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    let person = join(root, &hub, "p", &a, role("lister")).await?;
    let keys = person.identity().await;
    person.close().await;
    grant(&a, &keys.clone(), "L").await;
    sync(&b).await?;
    a.close().await;
    let before = identity(root, "b")?["keys"].clone();
    snap::ok(b.database.remove(
        rocky_sdk::PersonId::new(snap::public_key(&(keys.clone()))),
        rocky_sdk::Removal::Departed,
    ))
    .await;
    assert_eq!(identity(root, "b")?["keys"], before);
    assert_eq!(epoch(&b, "L").await, json!([1, 2]));
    assert_eq!(epoch(&b, "M").await, json!([1]));
    assert_eq!(
        listed(&b, &keys.clone()).await,
        serde_json::Value::Null,
        "a removed person is no longer listed"
    );
    b.close().await;
    Ok(())
}

#[tokio::test]
async fn revoking_an_issuer_rotates_the_registry_and_refuses_its_new_key_at_remaining_issuers()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    snap::ok(a.database.make_admin(
        rocky_sdk::PersonId::new(snap::public_key(&(b.identity().await))),
        (a.root()).clone(),
    ))
    .await;
    snap::ok(b.database.renew()).await;
    sync(&b).await?;
    let refused = snap::observe(b.database.remove(
        rocky_sdk::PersonId::new(snap::public_key(&(a.identity().await))),
        rocky_sdk::Removal::Departed,
    ))
    .await
    .expect_err("revoking the creating device's trusted node needs the root");
    assert_eq!(refused["code"], "owner_required", "{refused}");
    snap::ok(b.database.revoke_admin(
        rocky_sdk::PersonId::new(snap::public_key(&(a.identity().await))),
        rocky_sdk::AdminRevocation::Remove(rocky_sdk::Removal::Compromised),
        (a.root()).clone(),
    ))
    .await;
    assert_eq!(
        identity(root, "b")?["keys"]["issuer_registry"],
        json!([1, 2])
    );
    let chain: CredentialChain =
        serde_json::from_value(identity(root, "a")?["credentials"].clone())?;
    assert!(
        common::exchange_failed(&a.sync().await),
        "a revoked issuer's renewal request is refused"
    );
    assert_eq!(
        identity(root, "a")?["keys"]["issuer_registry"],
        json!([1]),
        "self-renewal retains only the revoked issuer's old keys"
    );
    let set: rocky_credentials::RevocationSet =
        serde_json::from_value(identity(root, "b")?["revocations"].clone())?;
    assert!(set.revoking(&chain)?.is_some());
    a.close().await;
    b.close().await;
    Ok(())
}

#[tokio::test]
async fn two_issuers_revoking_the_same_non_reader_leave_two_statements_and_one_tombstone()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    let person = join(root, &hub, "p", &a, rocky_sdk::Access::Store).await?;
    let keys = person.identity().await;
    person.close().await;
    sync(&b).await?;
    let before_a = identity(root, "a")?["keys"].clone();
    let before_b = identity(root, "b")?["keys"].clone();
    snap::ok(a.database.remove(
        rocky_sdk::PersonId::new(snap::public_key(&(keys.clone()))),
        rocky_sdk::Removal::Departed,
    ))
    .await;
    let by_a = snap::ok(a.database.info()).await;
    snap::ok(b.database.remove(
        rocky_sdk::PersonId::new(snap::public_key(&(keys.clone()))),
        rocky_sdk::Removal::Departed,
    ))
    .await;
    let by_b = snap::ok(b.database.info()).await;
    assert_ne!(by_a["revocations"]["digest"], by_b["revocations"]["digest"]);
    assert_eq!(identity(root, "a")?["keys"], before_a);
    assert_eq!(identity(root, "b")?["keys"], before_b);
    sync(&a).await?;
    assert_eq!(
        snap::ok(a.database.info()).await["revocations"]["statements"],
        2,
        "the handshake gives each issuer the other's statement"
    );
    sync(&b).await?;
    sync(&a).await?;
    for (name, device) in [("a", &a), ("b", &b)] {
        assert_eq!(
            snap::ok(device.database.info()).await["revocations"]["statements"],
            2
        );
        assert_eq!(
            listed(device, &keys.clone()).await,
            serde_json::Value::Null,
            "a removed person is no longer listed"
        );
        let conn = rusqlite::Connection::open_with_flags(
            root.join(name).join("rocky.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM issuer_registry WHERE identity=? AND entry='revoked'",
            [keys.clone()],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
    }
    a.close().await;
    b.close().await;
    Ok(())
}
