use crate::support::snapshot as snap;
use rocky_credentials::{RootEscrow, RootKey};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

use crate::common;
use common::{Device, Failure};

const SCHEMA: &str = r#"{"format":6,"application":"recovery","version":1,"tables":[
{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]},
{"name":"lists","class":"replayed","columns":[{"name":"name","type":"text","not_null":true},{"name":"title","type":"text"}],"primary_key":["name"],"partition_by":["name"],"partition_group":"list"},
{"name":"items","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}]}"#;
const LISTS: [&str; 3] = ["acme-payroll", "acme-board", "acme-legal"];

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
fn values(list: &str) -> Vec<rocky_sdk::SqlValue> {
    vec![rocky_sdk::SqlValue::Text(snap::text(&(list)))]
}
async fn sql(device: &Device, statement: String, id: Value) {
    snap::ok(
        device
            .session
            .execute(&(snap::text(&(statement))), vec![snap::row_id(&(id))]),
    )
    .await;
}
async fn write_item(device: &Device, list: &str, key: &str) {
    let listed = snap::ok(device.session.read(rocky_sdk::ReadQuery {
        sql: snap::text(&(format!("SELECT name FROM lists WHERE name='{list}'"))),
        values: vec![],
        max_rows: 1,
        max_bytes: None,
        cursor: None,
    }))
    .await;
    snap::ok(device.session.execute("BEGIN", Vec::new())).await;
    if listed["rows"].as_array().is_none_or(Vec::is_empty) {
        let id = snap::ok(
            device
                .session
                .next_partition_id("lists", (values(list)).clone()),
        )
        .await;
        sql(
            device,
            format!("INSERT INTO lists(name,title,id) VALUES('{list}','{list}',?)"),
            id["id"].clone(),
        )
        .await;
    }
    let id = snap::ok(
        device
            .session
            .next_partition_id("items", (values(list)).clone()),
    )
    .await;
    sql(
        device,
        format!("INSERT INTO items(list,key,id) VALUES('{list}','{key}',?)"),
        id["id"].clone(),
    )
    .await;
    snap::ok(device.session.execute("COMMIT", Vec::new())).await;
}
async fn write_note(device: &Device, key: &str) {
    snap::ok(device.session.execute("BEGIN", Vec::new())).await;
    let id = snap::ok(device.session.next_id("notes")).await;
    sql(
        device,
        format!("INSERT INTO notes(key,id) VALUES('{key}',?)"),
        id["id"].clone(),
    )
    .await;
    snap::ok(device.session.execute("COMMIT", Vec::new())).await;
}
async fn column(device: &Device, query: &str) -> Vec<String> {
    let read = snap::ok(device.session.read(rocky_sdk::ReadQuery {
        sql: snap::text(&(query)),
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
async fn everything(device: &Device) -> Vec<String> {
    let mut rows = column(device, "SELECT list || '/' || key FROM items ORDER BY 1").await;
    rows.extend(column(device, "SELECT 'note/' || key FROM notes ORDER BY 1").await);
    rows
}
fn identity(directory: &Path) -> Result<Value, Failure> {
    Ok(serde_json::from_slice(&std::fs::read(
        directory.join("identity.json"),
    )?)?)
}
fn escrow(directory: &Path) -> Result<Vec<(String, RootEscrow)>, Failure> {
    assert!(identity(directory)?.get("escrow").is_none());
    let file = rusqlite::Connection::open_with_flags(
        directory.join("rocky.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut statement = file.prepare("SELECT scope,record FROM secure_escrow")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    rows.map(|row| {
        let (scope, record) = row?;
        let is_root_record = scope.is_empty();
        let record = if is_root_record {
            RootEscrow::from_bytes(&record)?
        } else {
            rocky_credentials::VouchedEscrow::from_bytes(&record)?
                .record()
                .clone()
        };
        Ok((data_encoding::HEXLOWER.encode(&record.tag()), record))
    })
    .collect()
}
fn secrets(directory: &Path) -> Result<Vec<[u8; 32]>, Failure> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(directory.join("secrets"))? {
        if let Ok(secret) = <[u8; 32]>::try_from(std::fs::read(entry?.path())?) {
            found.push(secret);
        }
    }
    Ok(found)
}
async fn rotate(issuer: &Device, directory: &Path, list: &str) -> Result<(), Failure> {
    let device = common::node_keys(directory, None)?;
    snap::ok(issuer.session.grant(
        device,
        rocky_sdk::GrantProfile::Reader {
            scopes: ["notes".to_owned()].into_iter().collect(),
        },
        rocky_sdk::GrantTerms {
            ttl: None,
            delegations: None,
        },
    ))
    .await;
    let address = snap::ok(
        issuer
            .session
            .partition_address("items", (values(list)).clone()),
    )
    .await;
    let part = &address["tuple"][0];
    let grant = json!([{"domain":issuer.scope(),"table":address["scope"]["table"],"selector":[[part[0],{"Only":[part[1]]}]],"rows":"All","rights":["Read"],"fields":[]}]);
    snap::ok(
        issuer.session.grant_partitions(
            device,
            serde_json::from_value::<Vec<rocky_sdk::credentials::PartitionGrant>>(json!(grant))
                .expect("typed fixture"),
            1,
        ),
    )
    .await;
    snap::ok(
        issuer
            .session
            .revoke_identity(&(snap::text(&(device.identity.to_string())))),
    )
    .await;
    Ok(())
}
async fn principal(device: &Device) -> String {
    snap::ok(device.session.info()).await["principal"]
        .as_str()
        .unwrap_or_default()
        .into()
}
fn names(identities: &[rocky_credentials::PublicKey]) -> Vec<String> {
    identities.iter().map(ToString::to_string).collect()
}
fn generations(identity: &Value, collection: &str) -> Vec<u64> {
    identity["keys"][collection]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|epoch| epoch["generation"].as_u64())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_root_phrase_and_a_backup_recover_every_partition_with_every_device_lost()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let creator = root.path().join("creator");
    let opened = Database::create(open(&creator, &hub), None)?;
    let phrase = opened.root.as_ref().ok_or("root")?.phrase();
    let issuer = Device::new(opened)?;
    let lost = principal(&issuer).await;
    for list in LISTS {
        snap::ok(
            issuer
                .session
                .create_partition(rocky_sdk::PartitionTarget::Group {
                    group: "list".to_owned(),
                    values: (values(list)).clone(),
                }),
        )
        .await;
        write_item(&issuer, list, "before").await;
    }
    write_note(&issuer, "before").await;
    rotate(&issuer, &root.path().join("reader"), LISTS[0]).await?;
    let held = identity(&creator)?;
    assert_eq!(
        generations(&held, "notes"),
        [1, 2],
        "revoking a reader of notes and the payroll list rotates both keys"
    );
    write_item(&issuer, LISTS[0], "after").await;
    write_note(&issuer, "after").await;
    let expected = everything(&issuer).await;
    assert_eq!(expected.len(), 6, "{expected:?}");

    let archive = root.path().join("creator.backup");
    issuer.database.backup(&archive).await?;
    issuer.close().await;
    drop(issuer);
    let kept = escrow(&creator)?;
    std::fs::remove_dir_all(&creator)?;
    std::fs::remove_dir_all(root.path().join("reader"))?;

    let mistyped_phrase_root = RootKey::generate();
    let refused = Database::recover(
        open(&root.path().join("wrong"), &hub),
        &archive,
        &mistyped_phrase_root,
    )
    .err()
    .ok_or("another root recovered")?;
    assert!(
        refused.to_string().contains("not the backup's domain root"),
        "{refused}"
    );

    let recovered = root.path().join("recovered");
    let opened = Database::recover(
        open(&recovered, &hub),
        &archive,
        &RootKey::from_phrase(&phrase)?,
    )?;
    assert!(opened.root.is_none(), "the root is never handed back");
    assert_eq!(names(&opened.revoked), [lost]);
    let device = Device::new(opened)?;
    assert_eq!(everything(&device).await, expected);
    let saved = identity(&recovered)?;
    assert_eq!(
        generations(&saved, "notes"),
        [1, 2, 3],
        "revoking the lost creator rotates every key"
    );
    let now = escrow(&recovered)?;
    assert!(
        kept.iter()
            .all(|(tag, _)| now.iter().any(|(held, _)| held == tag)),
        "the records stay sealed to the root"
    );
    assert!(now.len() > kept.len(), "the rotated epochs are escrowed");
    assert_ne!(
        saved["identity"], held["identity"],
        "the recovered node is a trusted node of its own identity"
    );
    write_item(&device, LISTS[1], "recovered").await;
    write_note(&device, "recovered").await;
    assert_eq!(everything(&device).await.len(), 8);
    let status = snap::ok(
        device
            .session
            .partition_status(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values(LISTS[0])).clone(),
            }),
    )
    .await;
    let epochs: Vec<_> = status["epochs"]
        .as_array()
        .ok_or("epochs")?
        .iter()
        .map(|epoch| epoch["generation"].clone())
        .collect();
    assert_eq!(epochs, [json!(1), json!(2), json!(3)]);
    device.follow_main().await;
    device.seal_and_compact().await;
    let joined = Device::new(Database::join(
        open(&root.path().join("joined"), &hub),
        &device
            .invite_as(json!({"kind":"reader","scopes":["notes"]}))
            .await,
    )?)?;
    common::introduce(&device, &joined).await;
    joined.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(
        column(&joined, "SELECT key FROM notes ORDER BY key").await,
        ["after", "before", "recovered"]
    );
    joined.close().await;
    device.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_trusted_node_gains_nothing_from_escrow_records() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let creator = root.path().join("creator");
    let opened = Database::create(open(&creator, &hub), None)?;
    let phrase = opened.root.as_ref().ok_or("root")?.phrase();
    let issuer = Device::new(opened)?;
    issuer.follow_main().await;
    for list in LISTS {
        snap::ok(
            issuer
                .session
                .create_partition(rocky_sdk::PartitionTarget::Group {
                    group: "list".to_owned(),
                    values: (values(list)).clone(),
                }),
        )
        .await;
        write_item(&issuer, list, "before").await;
    }
    let trusted = root.path().join("trusted");
    let t = Device::new(Database::join(
        open(&trusted, &hub),
        &issuer.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    let t_identity = snap::ok(t.session.info()).await["principal"].clone();
    let grant = snap::ok(
        issuer
            .session
            .trust(&(snap::text(&(t_identity))), (issuer.root()).clone()),
    )
    .await;
    snap::ok(t.session.install_trust(&(snap::text(&(grant["grant"]))))).await;
    let t_records = escrow(&trusted)?;
    assert!(t_records.len() > LISTS.len(), "it escrows what it holds");
    let stolen = secrets(&trusted)?;
    assert!(
        stolen.len() >= 4,
        "node, identity, transport, naming and keys"
    );
    t.close().await;
    drop(t);
    snap::ok(issuer.session.revoke_identity_with_root(
        &(snap::text(&(t_identity))),
        rocky_sdk::RevocationMode::Compromised,
        Some((issuer.root()).clone()),
        true,
    ))
    .await;
    write_item(&issuer, LISTS[0], "after").await;

    let after = escrow(&creator)?;
    let saved = identity(&creator)?;
    let bytes32 = |value: &Value| -> Vec<u8> {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .map(|byte| byte as u8)
            .collect()
    };
    let named: Vec<Vec<u8>> = saved["groups"]
        .as_object()
        .ok_or("groups")?
        .values()
        .flat_map(|record| [bytes32(&record["address"]), bytes32(&record["name"])])
        .chain(LISTS.iter().map(|list| list.as_bytes().to_vec()))
        .collect();
    assert_eq!(named.len(), 3 * LISTS.len());
    assert!(named.iter().all(|name| name.len() >= 10));
    for (_, record) in t_records.iter().chain(&after) {
        for secret in &stolen {
            assert!(
                record.open(&RootKey::from_secret(*secret)).is_err(),
                "no secret the compromised node held opens a record"
            );
        }
        let bytes = record.to_bytes()?;
        for name in &named {
            assert!(
                !bytes
                    .windows(name.len())
                    .any(|window| window == name.as_slice()),
                "no record carries a list's name, group address or naming secret"
            );
        }
    }
    let fresh: Vec<_> = after
        .iter()
        .filter(|(tag, _)| t_records.iter().all(|(held, _)| held != tag))
        .collect();
    assert!(!fresh.is_empty(), "the rotation escrowed new epochs");

    let archive = root.path().join("creator.backup");
    issuer.database.backup(&archive).await?;
    for secret in &stolen {
        let refused = Database::recover(
            open(&root.path().join("thief"), &hub),
            &archive,
            &RootKey::from_secret(*secret),
        )
        .err()
        .ok_or("a stolen key recovered")?;
        assert!(
            refused.to_string().contains("not the backup's domain root"),
            "a stolen key is not the root and recovers nothing: {refused}"
        );
    }
    let root_key = RootKey::from_phrase(&phrase)?;
    for (_, record) in &after {
        record.open(&root_key)?;
    }
    issuer.close().await;
    Ok(())
}

async fn writer(device: &Device) -> String {
    snap::ok(device.session.info()).await["writer"]
        .as_str()
        .unwrap_or_default()
        .into()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recovery_writes_under_a_key_no_trusted_node_holds() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let creator = root.path().join("creator");
    let opened = Database::create(open(&creator, &hub), None)?;
    let phrase = opened.root.as_ref().ok_or("root")?.phrase();
    let issuer = Device::new(opened)?;
    write_note(&issuer, "before").await;
    let archive = root.path().join("creator.backup");
    issuer.database.backup(&archive).await?;
    let later = root.path().join("later");
    let enrolled = Device::new(Database::join(
        open(&later, &hub),
        &issuer.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    let (handed, kept) = (writer(&enrolled).await, writer(&issuer).await);
    enrolled.close().await;
    issuer.close().await;
    std::fs::remove_dir_all(&creator)?;

    let recovered = root.path().join("recovered");
    let device = Device::new(Database::recover(
        open(&recovered, &hub),
        &archive,
        &RootKey::from_phrase(&phrase)?,
    )?)?;
    let taken = writer(&device).await;
    assert!(!taken.is_empty() && taken != handed && taken != kept);
    write_note(&device, "recovered").await;
    assert_eq!(
        column(&device, "SELECT key FROM notes ORDER BY key").await,
        ["before", "recovered"]
    );
    device.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recovery_revokes_every_lost_trusted_identity_it_knows() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let creator = root.path().join("creator");
    let opened = Database::create(open(&creator, &hub), None)?;
    let phrase = opened.root.as_ref().ok_or("root")?.phrase();
    let issuer = Device::new(opened)?;
    write_note(&issuer, "before").await;
    let trusted = Device::new(Database::join(
        open(&root.path().join("trusted"), &hub),
        &issuer.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    let second = principal(&trusted).await;
    let grant = snap::ok(
        issuer
            .session
            .trust(&(snap::text(&(second))), (issuer.root()).clone()),
    )
    .await;
    snap::ok(
        trusted
            .session
            .install_trust(&(snap::text(&(grant["grant"])))),
    )
    .await;
    let mut lost = vec![principal(&issuer).await, second];
    lost.sort();
    let archive = root.path().join("creator.backup");
    issuer.database.backup(&archive).await?;
    trusted.close().await;
    issuer.close().await;

    let recovered = root.path().join("recovered");
    let opened = Database::recover(
        open(&recovered, &hub),
        &archive,
        &RootKey::from_phrase(&phrase)?,
    )?;
    let mut revoked = names(&opened.revoked);
    revoked.sort();
    assert_eq!(revoked, lost);
    let device = Device::new(opened)?;
    let saved = identity(&recovered)?;
    assert!(
        generations(&saved, "notes").len() >= 2,
        "one more key epoch serves writes after both revocations"
    );
    write_note(&device, "recovered").await;
    assert_eq!(
        column(&device, "SELECT key FROM notes ORDER BY key").await,
        ["before", "recovered"]
    );
    for identity in &lost {
        let refused = device
            .database
            .session()
            .revoke_trusted(
                identity,
                rocky_sdk::RevocationMode::Compromised,
                Arc::new(RootKey::from_phrase(&phrase)?),
            )
            .await
            .err()
            .ok_or("revoked twice")?;
        assert!(refused.to_string().contains("already revoked"), "{refused}");
    }
    device.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_custodians_store_and_the_root_recover_what_it_holds() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let creator = root.path().join("creator");
    let opened = Database::create(open(&creator, &hub), None)?;
    let phrase = opened.root.as_ref().ok_or("root")?.phrase();
    let issuer = Device::new(opened)?;
    let lost = principal(&issuer).await;
    let created = snap::ok(
        issuer
            .session
            .create_partition(rocky_sdk::PartitionTarget::Group {
                group: "list".to_owned(),
                values: (values(LISTS[0])).clone(),
            }),
    )
    .await;
    let custody = root.path().join("custody");
    let custodian = Device::new(Database::join(
        open(&custody, &hub),
        &issuer
            .invite_as(json!({"kind":"custody","scopes":["notes","lists","items"]}))
            .await,
    )?)?;
    for table in ["lists", "items"] {
        snap::ok(
            custodian.session.register_partition(
                rocky_sdk::PartitionTarget::Table {
                    table: snap::text(&(table)),
                    values: (values(LISTS[0])).clone(),
                },
                rocky_sdk::PartitionSelection {
                    name: Some(snap::digest(&(created["name"]))),
                    omit: serde_json::from_value::<std::collections::BTreeSet<String>>(json!([]))
                        .expect("typed fixture"),
                },
            ),
        )
        .await;
    }
    common::introduce(&issuer, &custodian).await;
    write_item(&issuer, LISTS[0], "before").await;
    write_note(&issuer, "before").await;
    issuer.sync().await.map_err(|error| error.to_string())?;
    rotate(&issuer, &root.path().join("reader"), LISTS[0]).await?;
    write_item(&issuer, LISTS[0], "after").await;
    write_note(&issuer, "after").await;
    issuer.sync().await.map_err(|error| error.to_string())?;
    let expected = everything(&issuer).await;
    assert_eq!(expected.len(), 4, "{expected:?}");

    let held = identity(&custody)?;
    assert_eq!(held["keys"], json!({}), "the custodian holds no key");
    let carried = escrow(&custody)?;
    assert!(
        !carried.is_empty(),
        "the custodian holds the records of the epochs it stores"
    );
    let name = created["name"].as_str().ok_or("name")?;
    for (_, record) in &carried {
        let bytes = record.to_bytes()?;
        for named in [LISTS[0].as_bytes(), name.as_bytes()] {
            assert!(!bytes.windows(named.len()).any(|window| window == named));
        }
    }
    let archive = root.path().join("custody.backup");
    custodian.database.backup(&archive).await?;
    custodian.close().await;
    issuer.close().await;
    for lost in ["creator", "custody", "reader"] {
        std::fs::remove_dir_all(root.path().join(lost))?;
    }

    let recovered = root.path().join("recovered");
    let opened = Database::recover(
        open(&recovered, &hub),
        &archive,
        &RootKey::from_phrase(&phrase)?,
    )?;
    assert_eq!(
        names(&opened.revoked),
        [lost],
        "the custodian's registry knows the lost creator"
    );
    let device = Device::new(opened)?;
    assert_eq!(everything(&device).await, expected);
    write_item(&device, LISTS[0], "recovered").await;
    write_note(&device, "recovered").await;
    assert_eq!(everything(&device).await.len(), 6);
    device.close().await;
    Ok(())
}

async fn revocation_reason(device: &Device) -> Value {
    snap::ok(device.session.status()).await["authority"]["verified_revocation"]["reason"].clone()
}
async fn alerts(device: &Device, kind: &str) -> Vec<Value> {
    snap::ok(device.session.status()).await["alerts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|alert| alert["kind"]["type"] == kind)
        .map(|alert| alert["kind"].clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recovery_ends_trusted_identities_its_backup_never_knew() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let creator = root.path().join("creator");
    let opened = Database::create(open(&creator, &hub), None)?;
    let phrase = opened.root.as_ref().ok_or("root")?.phrase();
    let issuer = Device::new(opened)?;
    let lost = principal(&issuer).await;
    write_note(&issuer, "before").await;
    let archive = root.path().join("creator.backup");
    issuer.database.backup(&archive).await?;
    let trusted = Device::new(Database::join(
        open(&root.path().join("trusted"), &hub),
        &issuer.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    let promoted = principal(&trusted).await;
    let grant = snap::ok(
        issuer
            .session
            .trust(&(snap::text(&(promoted))), (issuer.root()).clone()),
    )
    .await;
    snap::ok(
        trusted
            .session
            .install_trust(&(snap::text(&(grant["grant"])))),
    )
    .await;
    let member = Device::new(Database::join(
        open(&root.path().join("member"), &hub),
        &trusted
            .invite_as(json!({"kind":"reader","scopes":["notes"]}))
            .await,
    )?)?;
    issuer.close().await;
    drop(issuer);
    std::fs::remove_dir_all(&creator)?;

    let opened = Database::recover(
        open(&root.path().join("recovered"), &hub),
        &archive,
        &RootKey::from_phrase(&phrase)?,
    )?;
    assert!(
        names(&opened.revoked).iter().all(|name| *name == lost),
        "only what the backup's registry names is revoked by name: the creator at most"
    );
    let device = Device::new(opened)?;
    let kept = principal(&device).await;
    for ended in [&trusted, &member] {
        common::introduce(&device, ended).await;
        let _ = ended.sync().await;
        let _ = snap::observe(async {
            ended.session.renew().await?;
            ended.session.info().await
        })
        .await;
        assert_eq!(
            revocation_reason(ended).await,
            json!({"type":"recovery","recovered":kept})
        );
    }
    let trusted_after_recovery = Device::new(Database::join(
        open(&root.path().join("again"), &hub),
        &device.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    let identity = principal(&trusted_after_recovery).await;
    let grant = snap::ok(device.session.trust(
        &(snap::text(&(identity))),
        std::sync::Arc::new(rocky_sdk::RootKey::from_phrase(&snap::text(&(phrase))).expect("root")),
    ))
    .await;
    snap::ok(
        trusted_after_recovery
            .session
            .install_trust(&(snap::text(&(grant["grant"])))),
    )
    .await;
    write_note(&trusted_after_recovery, "again").await;
    common::introduce(&device, &trusted_after_recovery).await;
    trusted_after_recovery
        .sync()
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        revocation_reason(&trusted_after_recovery).await,
        Value::Null,
        "the recovery leaves a grant the root adds afterwards"
    );
    assert_eq!(
        column(&device, "SELECT key FROM notes ORDER BY key").await,
        ["again", "before"]
    );
    for device in [&trusted, &member, &trusted_after_recovery, &device] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn of_two_recoveries_that_meet_the_later_ends_the_earlier() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let creator = root.path().join("creator");
    let opened = Database::create(open(&creator, &hub), None)?;
    let phrase = opened.root.as_ref().ok_or("root")?.phrase();
    let issuer = Device::new(opened)?;
    write_note(&issuer, "before").await;
    let archive = root.path().join("creator.backup");
    issuer.database.backup(&archive).await?;
    issuer.close().await;
    drop(issuer);
    std::fs::remove_dir_all(&creator)?;
    let mut recovered = Vec::new();
    for name in ["first", "second"] {
        let directory = root.path().join(name);
        let device = Device::new(Database::recover(
            open(&directory, &hub),
            &archive,
            &RootKey::from_phrase(&phrase)?,
        )?)?;
        write_note(&device, name).await;
        recovered.push(device);
    }
    let [first, second] = &recovered[..] else {
        return Err("two recoveries".into());
    };
    common::introduce(first, second).await;
    for _ in 0..2 {
        let _ = first.sync().await;
        let _ = second.sync().await;
    }
    let (first_reason, second_reason) = (
        revocation_reason(first).await,
        revocation_reason(second).await,
    );
    let (kept, ended, written) = match (first_reason.is_null(), second_reason.is_null()) {
        (true, false) => (first, second, "first"),
        (false, true) => (second, first, "second"),
        _ => return Err(format!("{first_reason} {second_reason}").into()),
    };
    let (kept_identity, ended_identity) = (principal(kept).await, principal(ended).await);
    assert_eq!(
        revocation_reason(ended).await,
        json!({"type":"recovery","recovered":kept_identity})
    );
    assert_eq!(
        alerts(kept, "recoveries_met").await,
        [json!({"type":"recoveries_met","kept":kept_identity,"ended":ended_identity})]
    );
    write_note(ended, "late").await;
    let _ = ended.sync().await;
    let _ = kept.sync().await;
    let notes = column(kept, "SELECT key FROM notes ORDER BY key").await;
    let own = if written == "first" {
        "first"
    } else {
        "second"
    };
    assert_eq!(
        notes,
        ["before", own],
        "the ended recovery's writes never reach the kept one: {notes:?}"
    );
    first.close().await;
    second.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn survivors_report_the_trusted_identities_a_recovery_ended() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let creator = root.path().join("creator");
    let opened = Database::create(open(&creator, &hub), None)?;
    let phrase = opened.root.as_ref().ok_or("root")?.phrase();
    let issuer = Device::new(opened)?;
    write_note(&issuer, "before").await;
    let trusted = Device::new(Database::join(
        open(&root.path().join("trusted"), &hub),
        &issuer.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    let second = principal(&trusted).await;
    let grant = snap::ok(
        issuer
            .session
            .trust(&(snap::text(&(second))), (issuer.root()).clone()),
    )
    .await;
    snap::ok(
        trusted
            .session
            .install_trust(&(snap::text(&(grant["grant"])))),
    )
    .await;
    issuer
        .session
        .create_bookmark("main", None)
        .await
        .expect("bookmark");
    issuer.follow("main").await.expect("follow");
    for (device, minimum) in [(&issuer, 1), (&trusted, 2)] {
        snap::ok(device.session.set_genesis_minimum(minimum)).await;
    }
    common::introduce(&issuer, &trusted).await;
    trusted.sync().await.map_err(|error| error.to_string())?;
    let first = principal(&issuer).await;
    let archive = root.path().join("creator.backup");
    issuer.database.backup(&archive).await?;
    trusted.close().await;
    issuer.close().await;

    let opened = Database::recover(
        open(&root.path().join("recovered"), &hub),
        &archive,
        &RootKey::from_phrase(&phrase)?,
    )?;
    let device = Device::new(opened)?;
    let kept = principal(&device).await;
    let expected = [
        json!({"type":"recovery_ended","recovered":kept,"ended":first}),
        json!({"type":"recovery_ended","recovered":kept,"ended":second}),
    ];
    let sorted = |mut alerts: Vec<Value>| {
        alerts.sort_by_key(|alert| alert["ended"].as_str().map(str::to_owned));
        alerts
    };
    let expected = sorted(expected.to_vec());
    assert_eq!(sorted(alerts(&device, "recovery_ended").await), expected);

    let again = Device::new(Database::join(
        open(&root.path().join("again"), &hub),
        &device.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    let identity = principal(&again).await;
    let grant = snap::ok(device.session.trust(
        &(snap::text(&(identity))),
        std::sync::Arc::new(rocky_sdk::RootKey::from_phrase(&snap::text(&(phrase))).expect("root")),
    ))
    .await;
    snap::ok(
        again
            .session
            .install_trust(&(snap::text(&(grant["grant"])))),
    )
    .await;
    common::introduce(&device, &again).await;
    again.sync().await.map_err(|error| error.to_string())?;
    assert_eq!(revocation_reason(&again).await, Value::Null);
    assert_eq!(sorted(alerts(&again, "recovery_ended").await), expected);
    let member_without_registry = Device::new(Database::join(
        open(&root.path().join("member"), &hub),
        &device
            .invite_as(json!({"kind":"reader","scopes":["notes"]}))
            .await,
    )?)?;
    common::introduce(&device, &member_without_registry).await;
    member_without_registry
        .sync()
        .await
        .map_err(|error| error.to_string())?;
    assert_eq!(
        sorted(alerts(&member_without_registry, "recovery_ended").await),
        expected,
        "a member holds no registry: the tip statements alone name the ended identities"
    );
    for device in [&member_without_registry, &again, &device] {
        device.close().await;
    }
    Ok(())
}
