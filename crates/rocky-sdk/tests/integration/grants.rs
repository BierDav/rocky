//! Credential primitives through the call contract: raw grants to a device
//! that is not an issuer.
use crate::support::snapshot as snap;
use common::{Device, Failure};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

use crate::common;

const SCHEMA: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]},{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"text","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

fn open<'a>(directory: &'a Path, hub: &Arc<Hub>, name: &str) -> Open<'a> {
    Open {
        identity: common::prepared_identity(directory),
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
/// The secret of `role` in a device's file key store, if any.
fn secret(root: &Path, name: &str, role: &str) -> Result<bool, Failure> {
    let identity: Value =
        serde_json::from_slice(&std::fs::read(root.join(name).join("identity.json"))?)?;
    let installation = identity["installation"].as_str().ok_or("installation")?;
    Ok(
        std::fs::read_dir(root.join(name).join("secrets"))?.any(|entry| {
            entry.is_ok_and(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&format!("rocky.{installation}.{role}."))
            })
        }),
    )
}
async fn mutate(
    device: &Device,
    handle: u8,
    payload: snap::Action,
    entity: Option<String>,
) -> Result<Value, Value> {
    snap::observe(snap::mutate(
        &device.session,
        vec![handle; 32],
        payload,
        entity,
        None,
    ))
    .await
}
async fn insert(device: &Device, handle: u8, title: &str) -> Result<Value, Value> {
    let values = json!({"title": title, "completed": false});
    let payload = snap::Action::Create {
        table: "items".to_owned(),
        values: snap::fields(values),
    };
    mutate(device, handle, payload, None).await
}
async fn set(
    device: &Device,
    handle: u8,
    id: &Value,
    field: &str,
    value: Value,
) -> Result<Value, Value> {
    let payload = snap::Action::Set {
        table: "items".to_owned(),
        field: snap::text(&(field)),
        value: snap::field(json!(value)),
    };
    mutate(device, handle, payload, Some(snap::text(id))).await
}
async fn rows(device: &Device, sql: &str) -> Vec<Vec<String>> {
    let page = snap::observe(device.session.read(rocky_sdk::ReadQuery {
        sql: snap::text(&(sql)),
        values: vec![],
        max_rows: 64,
        max_bytes: None,
        cursor: None,
    }))
    .await
    .unwrap_or_default();
    page["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            row.as_array()
                .into_iter()
                .flatten()
                .map(|cell| cell["value"].as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .collect()
}

#[tokio::test]
async fn raw_grants_reach_a_device_that_is_not_an_issuer() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a")?;
    let directory = root.join("s");
    let keys = common::node_keys(&directory, None)?;

    // Command rights where no command is, unknown names and empty sets.
    for refused in [
        json!([{"scope": "items", "rights": ["execute"]}]),
        json!([{"scope": "items", "rights": ["invoke"]}]),
        json!([{"scope": "items", "rights": ["read"], "columns": ["missing"]}]),
        json!([{"scope": "missing", "rights": ["read"]}]),
        json!([{"scope": "items", "rights": ["publish_schema"]}]),
        json!([{"scope": "items", "rights": ["fly"]}]),
        json!([{"scope": "items", "rights": []}]),
        json!([]),
    ] {
        assert!(
            snap::observe(issue(
                &a,
                keys,
                refused.clone(),
                rocky_sdk::GrantTerms::default(),
                false
            ))
            .await
            .is_err(),
            "{refused}"
        );
    }
    let granted = json!([
        {"scope": "items", "rights": ["read", "update", "custody"], "columns": ["completed"]},
        {"scope": "notes", "rights": ["custody"]},
    ]);
    let supplied = snap::ok(issue(
        &a,
        keys,
        granted.clone(),
        rocky_sdk::GrantTerms::default(),
        false,
    ))
    .await["supplied"]
        .clone();
    let s = Device::new(Database::install(
        open(&directory, &hub, "s"),
        serde_json::from_value(supplied)?,
    )?)?;
    let status = snap::ok(s.session.info()).await;
    assert_eq!(status["issuer"], json!(false));
    assert_eq!(
        status["profile"],
        json!({"kind": "grants", "grants": granted})
    );
    // Custody alone holds no reader key.
    assert!(secret(root, "s", "reader.items")?);
    assert!(!secret(root, "s", "reader.notes")?);

    let milk = insert(&a, 1, "Milk").await.map_err(|e| e.to_string())?["id"].clone();
    s.sync().await.map_err(|e| e.to_string())?;
    let items = "SELECT title, completed FROM items ORDER BY title";
    assert_eq!(rows(&s, items).await, [["Milk", "0"]]);
    // Exactly the granted rights: no insert, no other column.
    assert!(insert(&s, 2, "Eggs").await.is_err());
    assert!(set(&s, 3, &milk, "title", json!("Bread")).await.is_err());
    set(&s, 4, &milk, "completed", json!(true))
        .await
        .map_err(|e| e.to_string())?;
    s.sync().await.map_err(|e| e.to_string())?;
    assert_eq!(rows(&a, items).await, [["Milk", "1"]]);
    assert!(rows(&s, "SELECT text FROM notes").await.is_empty());

    // The issuer lists the identity with its raw grants and can revoke it.
    let identities = snap::ok(a.session.identities()).await;
    let granted_identity = identities["identities"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|record| record["identity"] == json!(keys.identity))
        .ok_or("the granted identity")?;
    assert_eq!(granted_identity["profile"]["kind"], "grants");
    snap::ok(
        a.session
            .revoke_identity(&(snap::text(&(keys.identity.to_hex())))),
    )
    .await;
    assert!(
        snap::observe(issue(
            &a,
            keys,
            granted,
            rocky_sdk::GrantTerms::default(),
            false
        ))
        .await
        .is_err()
    );
    for device in [&a, &s] {
        device.close().await;
    }
    Ok(())
}

#[tokio::test]
async fn an_invitation_carries_raw_grants() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a")?;
    let profile = json!({"kind": "grants", "grants": [{"scope": "notes", "rights": ["read", "insert", "custody"]}]});
    let invitation = snap::ok(
        a.session.invite(
            Some(
                serde_json::from_value::<rocky_sdk::GrantProfile>(json!(profile))
                    .expect("typed fixture"),
            ),
            None,
        ),
    )
    .await["uri"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let j = Device::new(Database::join(
        open(&root.join("j"), &hub, "j"),
        &invitation,
    )?)?;
    assert_eq!(snap::ok(j.session.info()).await["profile"], profile);
    let note = snap::Action::Create {
        table: "notes".to_owned(),
        values: vec![("text".to_owned(), snap::field(json!("Hello")))],
    };
    mutate(&j, 1, note, None).await.map_err(|e| e.to_string())?;
    assert!(insert(&j, 2, "Milk").await.is_err());
    j.sync().await.map_err(|e| e.to_string())?;
    assert_eq!(rows(&a, "SELECT text FROM notes").await, [["Hello"]]);
    // A renewal keeps exactly the raw grants.
    snap::ok(async {
        j.session.renew().await?;
        j.session.info().await
    })
    .await;
    assert_eq!(snap::ok(j.session.info()).await["profile"], profile);
    for device in [&a, &j] {
        device.close().await;
    }
    Ok(())
}

/// Installs what `supplied` carries in a fresh directory `name`.
fn install(root: &Path, hub: &Arc<Hub>, name: &str, supplied: &Value) -> Result<Device, Failure> {
    Device::new(Database::install(
        open(&root.join(name), hub, name),
        serde_json::from_value(supplied.clone())?,
    )?)
}

#[tokio::test]
async fn a_delegate_never_holds_more_than_its_delegator() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let root = root.path();
    let hub = Arc::new(Hub::default());
    let a = create(root, &hub, "a")?;
    let s_keys = common::node_keys(&root.join("s"), None)?;
    let held = json!([
        {"scope": "items", "rights": ["read", "insert", "update", "delete", "custody"]},
        {"scope": "notes", "rights": ["read", "custody"]},
    ]);
    let supplied = snap::ok(
        a.session.grant(
            s_keys,
            rocky_sdk::GrantProfile::Grants {
                grants: serde_json::from_value::<Vec<rocky_sdk::credentials::Grant>>(json!(held))
                    .expect("typed fixture"),
            },
            rocky_sdk::GrantTerms {
                ttl: None,
                delegations: Some(2),
            },
        ),
    )
    .await["supplied"]
        .clone();
    let s = install(root, &hub, "s", &supplied)?;
    let d_keys = common::node_keys(&root.join("d"), None)?;
    // No right, scope, column or depth the delegator lacks.
    let insert_only = json!([{"scope": "items", "rights": ["read", "insert", "custody"]}]);
    for (grants, extra) in [
        (
            json!([{"scope": "notes", "rights": ["read", "insert"]}]),
            json!({}),
        ),
        (
            json!([{"scope": "items", "rights": ["read", "invoke"]}]),
            json!({}),
        ),
        (insert_only.clone(), json!({"delegations": 2})),
        (insert_only.clone(), json!({"delegations": 0})),
    ] {
        assert!(
            snap::observe(issue(
                &s,
                d_keys,
                grants.clone(),
                serde_json::from_value(extra).expect("grant terms"),
                true
            ))
            .await
            .is_err(),
            "{grants}"
        );
    }
    // A device whose capability allows no further link delegates nothing;
    // an issuer grants instead.
    assert!(
        snap::observe(issue(
            &a,
            d_keys,
            insert_only.clone(),
            serde_json::from_value(json!({})).expect("grant terms"),
            true
        ))
        .await
        .is_err()
    );
    let plain_keys = common::node_keys(&root.join("p"), None)?;
    let plain = snap::ok(
        a.session.grant(
            serde_json::from_value::<rocky_sdk::NodeKeys>(json!(plain_keys))
                .expect("typed fixture"),
            rocky_sdk::GrantProfile::Grants {
                grants: serde_json::from_value::<Vec<rocky_sdk::credentials::Grant>>(json!(
                    insert_only
                ))
                .expect("typed fixture"),
            },
            rocky_sdk::GrantTerms {
                ttl: None,
                delegations: None,
            },
        ),
    )
    .await["supplied"]
        .clone();
    let p = install(root, &hub, "p", &plain)?;
    assert!(
        snap::observe(issue(
            &p,
            d_keys,
            insert_only.clone(),
            serde_json::from_value(json!({})).expect("grant terms"),
            true
        ))
        .await
        .is_err()
    );

    let supplied = snap::ok(issue(
        &s,
        d_keys,
        insert_only.clone(),
        serde_json::from_value(json!({})).expect("grant terms"),
        true,
    ))
    .await["supplied"]
        .clone();
    let d = install(root, &hub, "d", &supplied)?;
    let status = snap::ok(d.session.info()).await;
    assert_eq!(
        status["profile"],
        json!({"kind": "grants", "grants": insert_only})
    );
    // The delegate's chain is a link longer, valid no longer than its delegator's.
    assert!(status["expires"].as_str() <= snap::ok(s.session.info()).await["expires"].as_str());
    assert!(!secret(root, "d", "reader.notes")?);
    // Anyone who configured it serves it; its frames verify on arrival.
    for (left, right) in [(&a, &d)] {
        left.add_peer(&right.contact().await)
            .await
            .map_err(|e| e.to_string())?;
        right
            .add_peer(&left.contact().await)
            .await
            .map_err(|e| e.to_string())?;
    }
    let eggs = insert(&d, 1, "Eggs").await.map_err(|e| e.to_string())?["id"].clone();
    let delete = snap::Action::Delete {
        table: "items".to_owned(),
    };
    assert!(
        mutate(&d, 2, delete, Some(snap::text(&eggs)))
            .await
            .is_err()
    );
    d.sync().await.map_err(|e| e.to_string())?;
    assert_eq!(rows(&a, "SELECT title FROM items").await, [["Eggs"]]);
    // Reopening the delegator preserves its recipient range for later grants.
    s.close().await;
    let s = Device::new(Database::open(open(&root.join("s"), &hub, "s"))?)?;
    // An issuer renews no delegated chain; the delegator delegates again.
    assert!(
        snap::observe(async {
            d.session.renew().await?;
            d.session.info().await
        })
        .await
        .is_err()
    );
    let narrower = json!([{"scope": "items", "rights": ["read", "custody"]}]);
    let renewed = snap::ok(issue(
        &s,
        d_keys,
        narrower.clone(),
        serde_json::from_value(json!({})).expect("grant terms"),
        true,
    ))
    .await["supplied"]
        .clone();
    let renewed = snap::ok(async {
        d.session
            .install_credentials(
                serde_json::from_value::<rocky_sdk::Supplied>(json!(renewed))
                    .expect("typed fixture"),
            )
            .await?;
        d.session.info().await
    })
    .await;
    assert_eq!(renewed["writer"], status["writer"]);
    let status = renewed;
    // Raw grants that are exactly a starter profile are shown as one.
    assert_eq!(
        status["profile"],
        json!({"kind": "reader", "scopes": ["items"]})
    );
    assert!(insert(&d, 3, "Jam").await.is_err());
    // Credentials of another device or wider than the delegator's are refused.
    assert!(
        snap::observe(async {
            d.session
                .install_credentials(
                    serde_json::from_value::<rocky_sdk::Supplied>(json!(plain))
                        .expect("typed fixture"),
                )
                .await?;
            d.session.info().await
        })
        .await
        .is_err()
    );
    for device in [&a, &s, &d, &p] {
        device.close().await;
    }
    Ok(())
}

async fn issue(
    device: &Device,
    keys: rocky_sdk::NodeKeys,
    grants: Value,
    terms: rocky_sdk::GrantTerms,
    delegate: bool,
) -> Result<rocky_sdk::Supplied, Failure> {
    let profile = rocky_sdk::GrantProfile::Grants {
        grants: serde_json::from_value(grants)?,
    };
    if delegate {
        device.session.delegate(keys, profile, terms).await
    } else {
        device.session.grant(keys, profile, terms).await
    }
}
