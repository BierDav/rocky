//! Phase 7 acceptance on the in-process hub: precise recorded access and
//! the explicit full-rotation fallback for unrecorded devices.
use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_sdk::{Database, credentials::CredentialChain, transport::memory::Hub};
use serde_json::{Value, json};
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
    snap::ok(
        a.session.grant_partitions(
            serde_json::from_value::<rocky_sdk::NodeKeys>(json!({"node":node,"identity":member}))
                .expect("typed fixture"),
            serde_json::from_value::<Vec<rocky_sdk::credentials::PartitionGrant>>(json!([grant(
                &a, "L"
            )
            .await]))
            .expect("typed fixture"),
            1,
        ),
    )
    .await;
    common::introduce(&m, &b).await;
    sync(&b)
        .await
        .map_err(|error| format!("registry before revocation: {error}"))?;
    let recorded = listed(&b, &member).await;
    let old_chain: CredentialChain =
        serde_json::from_value(identity(root, "m")?["credentials"].clone())?;
    a.close().await;
    let revocation = snap::ok(b.session.revoke_identity(&(snap::text(&(member))))).await;
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
        revocation["statement"]
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

    assert_eq!(listed(&b, &member).await["revoked"], true);
    assert!(
        snap::observe(b.session.revoke_identity(&(snap::text(&(node)))))
            .await
            .is_err(),
        "an issuer never revokes a node"
    );
    assert!(
        snap::observe(b.session.revoke_identity(&(snap::text(&(member)))))
            .await
            .is_err(),
        "an issuer never revokes an identity twice"
    );
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
    assert_eq!(
        w.titles().await,
        ["after", "before"],
        "a device joining after the rotation reads earlier rows from a sealed base"
    );
    assert!(common::exchange_failed(&m.sync().await));
    assert_eq!(m.titles().await, ["before"]);
    assert_eq!(identity(root, "m")?["keys"]["items"], json!([1]));
    let a = Device::new(Database::open(open(&root.join("a"), &hub, "a"))?)?;
    sync(&a)
        .await
        .map_err(|error| format!("enrolling issuer after revocation: {error}"))?;
    assert_eq!(
        listed(&a, &member).await["revoked"],
        true,
        "the returning enrolling issuer learns the statement and tombstone in one sync"
    );
    assert!(
        snap::observe(
            a.session.grant(
                serde_json::from_value::<rocky_sdk::NodeKeys>(
                    json!({"node":node,"identity":member})
                )
                .expect("typed fixture"),
                rocky_sdk::GrantProfile::Reader {
                    scopes: ["items".to_owned()].into_iter().collect()
                },
                rocky_sdk::GrantTerms {
                    ttl: None,
                    delegations: None,
                }
            )
        )
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
    let keys = common::node_keys(&root.join("m"), Some(keys(&root.join("m"))))?;
    snap::ok(
        a.session.grant_partitions(
            keys,
            serde_json::from_value::<Vec<rocky_sdk::credentials::PartitionGrant>>(json!([grant(
                &a, "L"
            )
            .await]))
            .expect("typed fixture"),
            1,
        ),
    )
    .await;
    sync(&b).await?;
    a.close().await;
    let before = identity(root, "b")?["keys"].clone();
    snap::ok(
        b.session
            .revoke_identity(&(snap::text(&(keys.identity.to_hex())))),
    )
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
    let root = crate::support::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    let member = common::node_keys(&root.join("m"), Some(keys(&root.join("m"))))?;
    let supplied = snap::ok(a.session.grant(
        member,
        rocky_sdk::GrantProfile::Reader {
            scopes: ["items".to_owned()].into_iter().collect(),
        },
        rocky_sdk::GrantTerms {
            ttl: None,
            delegations: Some(2),
        },
    ))
    .await["supplied"]
        .clone();
    let m = Device::new(Database::install(
        open(&root.join("m"), &hub, "m"),
        serde_json::from_value(supplied)?,
    )?)?;
    let delegate = common::node_keys(&root.join("d"), Some(keys(&root.join("d"))))?;
    snap::ok(m.session.delegate(
        delegate,
        rocky_sdk::GrantProfile::Reader {
            scopes: ["items".to_owned()].into_iter().collect(),
        },
        rocky_sdk::GrantTerms {
            ttl: None,
            delegations: None,
        },
    ))
    .await;
    sync(&b).await?;
    common::introduce(&m, &b).await;
    a.close().await;
    assert_eq!(listed(&b, &delegate.identity.to_hex()).await, Value::Null);
    snap::ok(
        b.session
            .revoke_identity(&(snap::text(&(delegate.identity.to_hex())))),
    )
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
    assert!(
        snap::observe(m.session.delegate(
            delegate,
            rocky_sdk::GrantProfile::Reader {
                scopes: ["items".to_owned()].into_iter().collect()
            },
            rocky_sdk::GrantTerms {
                ttl: None,
                delegations: None,
            }
        ))
        .await
        .is_err(),
        "a device that learned the statement never hands the new key to the revoked node"
    );
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
    let grant = snap::ok(
        a.session
            .trust(&(snap::text(&(b.identity().await))), (a.root()).clone()),
    )
    .await;
    snap::ok(b.session.install_trust(&(snap::text(&(grant["grant"]))))).await;
    sync(&b).await?;
    let refused = snap::observe(
        b.session
            .revoke_identity(&(snap::text(&(a.identity().await)))),
    )
    .await
    .expect_err("revoking the creating device's trusted node needs the root");
    assert_eq!(refused["code"], "publisher_required", "{refused}");
    snap::ok(b.session.revoke_identity_with_root(
        &(snap::text(&(a.identity().await))),
        rocky_sdk::RevocationMode::Compromised,
        Some((a.root()).clone()),
        true,
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
    let root = crate::support::small_directory()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let Domain { a, b } = Domain::new(root, &hub).await?;
    let keys = common::node_keys(&root.join("m"), Some(keys(&root.join("m"))))?;
    snap::ok(a.session.grant(
        keys,
        rocky_sdk::GrantProfile::Custody {
            scopes: ["items".to_owned()].into_iter().collect(),
        },
        rocky_sdk::GrantTerms {
            ttl: None,
            delegations: None,
        },
    ))
    .await;
    sync(&b).await?;
    let before_a = identity(root, "a")?["keys"].clone();
    let before_b = identity(root, "b")?["keys"].clone();
    let by_a = snap::ok(
        a.session
            .revoke_identity(&(snap::text(&(keys.identity.to_hex())))),
    )
    .await;
    let by_b = snap::ok(
        b.session
            .revoke_identity(&(snap::text(&(keys.identity.to_hex())))),
    )
    .await;
    assert_ne!(by_a["statement"], by_b["statement"]);
    assert_eq!(identity(root, "a")?["keys"], before_a);
    assert_eq!(identity(root, "b")?["keys"], before_b);
    sync(&a).await?;
    assert_eq!(
        snap::ok(a.session.info()).await["revocations"]["statements"],
        2,
        "the handshake gives each issuer the other's statement"
    );
    sync(&b).await?;
    sync(&a).await?;
    for (name, device) in [("a", &a), ("b", &b)] {
        assert_eq!(
            snap::ok(device.session.info()).await["revocations"]["statements"],
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
