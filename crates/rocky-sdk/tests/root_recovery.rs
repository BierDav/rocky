//! Root recovery: every data key epoch is escrowed to the offline root in
//! records that travel with its frames, so the root phrase and any node's
//! backup, a keyless custodian's too, recover the data that store holds
//! with every trusted node and device lost, while the records give anyone
//! else nothing.
// The SDK's blocking constructors and native archives.
#![cfg(not(target_family = "wasm"))]
use rocky_credentials::{RootEscrow, RootKey};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

mod common;
use common::{Device, Failure};

const SCHEMA: &str = r#"{"format":6,"application":"recovery","version":1,"tables":[
{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]},
{"name":"lists","class":"replayed","columns":[{"name":"name","type":"text","not_null":true},{"name":"title","type":"text"}],"primary_key":["name"],"partition_by":["name"],"partition_group":"list"},
{"name":"items","class":"replayed","columns":[{"name":"list","type":"text","not_null":true},{"name":"key","type":"text","not_null":true}],"primary_key":["list","key"],"partition_by":["list"],"partition_group":"list"}]}"#;
/// Lists names no one but holders may learn.
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
fn values(list: &str) -> Value {
    json!([{"type":"text","value":list}])
}
async fn sql(device: &Device, statement: String, id: Value) {
    device
        .ok(json!({"type":"sql","sql":statement,"values":[{"type":"integer","value":id}]}))
        .await;
}
/// Writes item `key` of `list`, creating the list's row first.
async fn write_item(device: &Device, list: &str, key: &str) {
    let listed = device
        .ok(json!({"type":"read","query":{"sql":format!("SELECT name FROM lists WHERE name='{list}'"),"values":[],"max_rows":1}}))
        .await;
    device.ok(json!({"type":"sql","sql":"BEGIN"})).await;
    if listed["rows"].as_array().is_none_or(Vec::is_empty) {
        let id = device
            .ok(json!({"type":"next_id","table":"lists","partition_values":values(list)}))
            .await;
        sql(
            device,
            format!("INSERT INTO lists(name,title,id) VALUES('{list}','{list}',?)"),
            id["id"].clone(),
        )
        .await;
    }
    let id = device
        .ok(json!({"type":"next_id","table":"items","partition_values":values(list)}))
        .await;
    sql(
        device,
        format!("INSERT INTO items(list,key,id) VALUES('{list}','{key}',?)"),
        id["id"].clone(),
    )
    .await;
    device.ok(json!({"type":"sql","sql":"COMMIT"})).await;
}
async fn write_note(device: &Device, key: &str) {
    device.ok(json!({"type":"sql","sql":"BEGIN"})).await;
    let id = device.ok(json!({"type":"next_id","table":"notes"})).await;
    sql(
        device,
        format!("INSERT INTO notes(key,id) VALUES('{key}',?)"),
        id["id"].clone(),
    )
    .await;
    device.ok(json!({"type":"sql","sql":"COMMIT"})).await;
}
async fn column(device: &Device, query: &str) -> Vec<String> {
    let read = device
        .ok(json!({"type":"read","query":{"sql":query,"values":[],"max_rows":256}}))
        .await;
    read["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row[0]["value"].as_str().map(String::from))
        .collect()
}
/// Every row as `list/key` and `note/key`.
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
/// The root escrow records a node's replica holds, by tag; its identity
/// record keeps none.
fn escrow(directory: &Path) -> Result<Vec<(String, RootEscrow)>, Failure> {
    assert!(identity(directory)?.get("escrow").is_none());
    let file = rusqlite::Connection::open_with_flags(
        directory.join("rocky.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut statement = file.prepare("SELECT tag,record FROM secure_escrow")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    rows.map(|row| {
        let (tag, record) = row?;
        Ok((
            data_encoding::HEXLOWER.encode(&tag),
            RootEscrow::from_bytes(&record)?,
        ))
    })
    .collect()
}
/// Every 32-byte secret an installation's file key store holds.
fn secrets(directory: &Path) -> Result<Vec<[u8; 32]>, Failure> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(directory.join("secrets"))? {
        if let Ok(secret) = <[u8; 32]>::try_from(std::fs::read(entry?.path())?) {
            found.push(secret);
        }
    }
    Ok(found)
}
/// Revokes a device that was granted `notes` and list `list`, never
/// installed: the revocation rotates both keys.
async fn rotate(issuer: &Device, directory: &Path, list: &str) -> Result<(), Failure> {
    let device = Database::device(directory, None)?;
    issuer
        .ok(json!({"type":"grant","device":device,"profile":{"kind":"reader","scopes":["notes"]}}))
        .await;
    let address = issuer
        .ok(json!({"type":"partition_address","table":"items","values":values(list)}))
        .await;
    let part = &address["tuple"][0];
    let grant = json!([{"domain":issuer.scope(),"table":address["scope"]["table"],"selector":[[part[0],{"Only":[part[1]]}]],"rows":"All","rights":["Read"],"fields":[]}]);
    issuer
        .ok(json!({"type":"partition_grant","device":device,"partitions":grant}))
        .await;
    issuer
        .ok(json!({"type":"revoke","identity":device.user.to_string()}))
        .await;
    Ok(())
}
async fn principal(device: &Device) -> String {
    device.ok(json!({"type":"session"})).await["principal"]
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
        issuer
            .ok(json!({"type":"partition_create","group":"list","values":values(list)}))
            .await;
        write_item(&issuer, list, "before").await;
    }
    write_note(&issuer, "before").await;
    // Revoking a reader of notes and the payroll list rotates both keys;
    // later rows are sealed under the new epochs only.
    rotate(&issuer, &root.path().join("reader"), LISTS[0]).await?;
    let held = identity(&creator)?;
    assert_eq!(generations(&held, "notes"), [1, 2]);
    write_item(&issuer, LISTS[0], "after").await;
    write_note(&issuer, "after").await;
    let expected = everything(&issuer).await;
    assert_eq!(expected.len(), 6, "{expected:?}");

    let archive = root.path().join("creator.backup");
    issuer.database.backup(&archive).await?;
    issuer.close().await;
    drop(issuer);
    let kept = escrow(&creator)?;
    // Every device is lost: its keys with it. What remains is the archive,
    // which holds no key, and the phrase.
    std::fs::remove_dir_all(&creator)?;
    std::fs::remove_dir_all(root.path().join("reader"))?;

    // Another root, or a phrase the user mistyped, recovers nothing.
    let refused = Database::recover(
        open(&root.path().join("wrong"), &hub),
        &archive,
        &RootKey::generate(),
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
    // The lost creator is revoked, which rotates every key.
    assert_eq!(names(&opened.revoked), [lost]);
    let device = Device::new(opened)?;
    assert_eq!(everything(&device).await, expected);
    let saved = identity(&recovered)?;
    assert_eq!(generations(&saved, "notes"), [1, 2, 3]);
    let now = escrow(&recovered)?;
    assert!(
        kept.iter().all(|record| now.contains(record)),
        "the records stay sealed to the root"
    );
    assert!(now.len() > kept.len(), "the rotated epochs are escrowed");
    // The recovered node is a trusted node of its own identity: it writes
    // under the current keys, with author numbers no one else held.
    assert_ne!(saved["identity"], held["identity"]);
    write_item(&device, LISTS[1], "recovered").await;
    write_note(&device, "recovered").await;
    assert_eq!(everything(&device).await.len(), 8);
    let status = device
        .ok(json!({"type":"partition_status","group":"list","values":values(LISTS[0])}))
        .await;
    let epochs: Vec<_> = status["epochs"]
        .as_array()
        .ok_or("epochs")?
        .iter()
        .map(|epoch| epoch["generation"].clone())
        .collect();
    assert_eq!(epochs, [json!(1), json!(2), json!(3)]);
    // It seals and enrolls devices like any trusted node; a joining
    // device imports the rows before the rotation from the sealed base.
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
        issuer
            .ok(json!({"type":"partition_create","group":"list","values":values(list)}))
            .await;
        write_item(&issuer, list, "before").await;
    }
    // A second trusted node, which holds every key, is later compromised.
    let trusted = root.path().join("trusted");
    let t = Device::new(Database::join(
        open(&trusted, &hub),
        &issuer.invite_as(json!({"kind": "issuer"})).await,
    )?)?;
    let t_identity = t.ok(json!({"type":"session"})).await["principal"].clone();
    let grant = issuer
        .ok(json!({"type":"tips","action":"trust","identity":t_identity,"root":issuer.root()}))
        .await;
    t.ok(json!({"type":"tips","action":"install_trust","grant":grant["grant"]}))
        .await;
    let t_records = escrow(&trusted)?;
    assert!(t_records.len() > LISTS.len(), "it escrows what it holds");
    let stolen = secrets(&trusted)?;
    assert!(
        stolen.len() >= 4,
        "node, identity, transport, naming and keys"
    );
    t.close().await;
    drop(t);
    issuer
        .ok(json!({"type":"revoke","identity":t_identity,"root":issuer.root()}))
        .await;
    write_item(&issuer, LISTS[0], "after").await;

    // No secret it held opens a record, its own or the creator's after the
    // rotation, and no record carries a list's name, group address or
    // naming secret.
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
            assert!(record.open(&RootKey::from_secret(*secret)).is_err());
        }
        let bytes = record.to_bytes()?;
        for name in &named {
            assert!(
                !bytes
                    .windows(name.len())
                    .any(|window| window == name.as_slice())
            );
        }
    }
    // The rotation's new keys are escrowed only where they are held.
    let fresh: Vec<_> = after
        .iter()
        .filter(|(tag, _)| t_records.iter().all(|(held, _)| held != tag))
        .collect();
    assert!(!fresh.is_empty(), "the rotation escrowed new epochs");

    // A stolen key is not the root: the revoked node cannot recover a
    // backup it made or forge one into a trusted node.
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
            "{refused}"
        );
    }
    // The root opens every record, the rotated ones too.
    let root_key = RootKey::from_phrase(&phrase)?;
    for (_, record) in &after {
        record.open(&root_key)?;
    }
    issuer.close().await;
    Ok(())
}

fn minting(directory: &Path) -> Result<(u64, u64), Failure> {
    let range = &identity(directory)?["minting"]["range"];
    Ok((
        range["first"].as_u64().ok_or("first")?,
        range["last"].as_u64().ok_or("last")?,
    ))
}

/// The backup predates an issuer its node enrolled: the recovered identity
/// still takes numbers no trusted node handed out.
#[tokio::test(flavor = "multi_thread")]
async fn a_recovery_takes_author_numbers_no_trusted_node_handed_out() -> Result<(), Failure> {
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
    let handed = minting(&later)?;
    let kept = minting(&creator)?;
    enrolled.close().await;
    issuer.close().await;
    std::fs::remove_dir_all(&creator)?;

    let recovered = root.path().join("recovered");
    let device = Device::new(Database::recover(
        open(&recovered, &hub),
        &archive,
        &RootKey::from_phrase(&phrase)?,
    )?)?;
    let taken = minting(&recovered)?;
    for other in [handed, kept] {
        assert!(
            taken.1 < other.0 || other.1 < taken.0,
            "{taken:?} overlaps {other:?}"
        );
    }
    assert!(taken.0 >= u64::from(rocky_credentials::RECOVERY_FIRST));
    assert!(handed.1 < u64::from(rocky_credentials::RECOVERY_FIRST));
    write_note(&device, "recovered").await;
    assert_eq!(
        column(&device, "SELECT key FROM notes ORDER BY key").await,
        ["before", "recovered"]
    );
    device.close().await;
    Ok(())
}

/// Every trusted identity the backup knows is lost with its devices: the
/// recovery revokes each with the root, and the app learns which.
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
    let grant = issuer
        .ok(json!({"type":"tips","action":"trust","identity":second,"root":issuer.root()}))
        .await;
    trusted
        .ok(json!({"type":"tips","action":"install_trust","grant":grant["grant"]}))
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
    // Both revocations are recorded and one more key epoch serves writes.
    let saved = identity(&recovered)?;
    assert!(generations(&saved, "notes").len() >= 2);
    write_note(&device, "recovered").await;
    assert_eq!(
        column(&device, "SELECT key FROM notes ORDER BY key").await,
        ["before", "recovered"]
    );
    // Revoking again is refused: nothing is left to revoke.
    for identity in &lost {
        let refused = device
            .database
            .session()
            .revoke_trusted(identity, Arc::new(RootKey::from_phrase(&phrase)?))
            .await
            .err()
            .ok_or("revoked twice")?;
        assert!(refused.to_string().contains("already revoked"), "{refused}");
    }
    device.close().await;
    Ok(())
}

/// Every device is lost but a keyless custodian of the notes and of one
/// list: its store alone, with the root, recovers every row it holds
/// ciphertext of, under each key epoch, the escrow records having
/// travelled to it with the frames.
#[tokio::test(flavor = "multi_thread")]
async fn a_custodians_store_and_the_root_recover_what_it_holds() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let creator = root.path().join("creator");
    let opened = Database::create(open(&creator, &hub), None)?;
    let phrase = opened.root.as_ref().ok_or("root")?.phrase();
    let issuer = Device::new(opened)?;
    let lost = principal(&issuer).await;
    let created = issuer
        .ok(json!({"type":"partition_create","group":"list","values":values(LISTS[0])}))
        .await;
    let custody = root.path().join("custody");
    let custodian = Device::new(Database::join(
        open(&custody, &hub),
        &issuer
            .invite_as(json!({"kind":"custody","scopes":["notes","lists","items"]}))
            .await,
    )?)?;
    for table in ["lists", "items"] {
        custodian
            .ok(json!({"type":"partition_register","table":table,"values":values(LISTS[0]),"name":created["name"]}))
            .await;
    }
    common::introduce(&issuer, &custodian).await;
    write_item(&issuer, LISTS[0], "before").await;
    write_note(&issuer, "before").await;
    issuer.sync().await.map_err(|error| error.to_string())?;
    // A rotation: later rows travel under the new epochs and their records.
    rotate(&issuer, &root.path().join("reader"), LISTS[0]).await?;
    write_item(&issuer, LISTS[0], "after").await;
    write_note(&issuer, "after").await;
    issuer.sync().await.map_err(|error| error.to_string())?;
    let expected = everything(&issuer).await;
    assert_eq!(expected.len(), 4, "{expected:?}");

    // The custodian holds no key, only records of the epochs it stores.
    let held = identity(&custody)?;
    assert_eq!(held["keys"], json!({}));
    let carried = escrow(&custody)?;
    assert!(!carried.is_empty());
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
    // The custodian's registry knows the lost creator.
    assert_eq!(names(&opened.revoked), [lost]);
    let device = Device::new(opened)?;
    assert_eq!(everything(&device).await, expected);
    // The recovered trusted node writes on.
    write_item(&device, LISTS[0], "recovered").await;
    write_note(&device, "recovered").await;
    assert_eq!(everything(&device).await.len(), 6);
    device.close().await;
    Ok(())
}
