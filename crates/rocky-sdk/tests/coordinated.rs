//! Coordinated writes through the JSON contract: two trusted devices issue
//! invoices concurrently over live links and number them without gaps or
//! duplicates; without a coordinator the write fails typed. See
//! `docs/runtime/coordinated-writes.md`.
mod common;
use common::{Device, Failure, introduce};
use rocky_coordinator::coordinated::MemoryLog;
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::{Value, json};
use std::sync::Arc;

fn open<'a>(directory: &'a std::path::Path, network: Option<Network>) -> Open<'a> {
    Open {
        directory,
        schema: common::SCHEMA,
        secrets: None,
        network,
        coordination: None,
    }
}
fn domain(device: &Device) -> rocky_core::Scope {
    serde_json::from_str(&device.database.scope()).expect("scope")
}
async fn node_key(device: &Device) -> rocky_sdk::credentials::PublicKey {
    let hex = device.node().await;
    let bytes: [u8; 32] = data_encoding::HEXLOWER
        .decode(hex.as_bytes())
        .expect("node hex")
        .try_into()
        .expect("node key");
    rocky_sdk::credentials::PublicKey::from_bytes(&bytes).expect("node key")
}
/// Waits until `device` holds an open live link to `other`.
async fn linked(device: &Device, other: &Device) {
    let node = other.node().await;
    for _ in 0..200 {
        if device.peer(&node).await["link"]["state"] == "open" {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("no live link");
}
/// Issues the next invoice: reads the committed count inside a coordinated
/// transaction and inserts the following number. Retries lost races.
async fn invoice(device: &Device, handle: u8) -> Value {
    for attempt in 0..200u8 {
        let begun = device
            .call(json!({"type": "sql", "sql": "BEGIN", "coordinated": true}))
            .await;
        if let Err(error) = begun {
            assert_eq!(error["retryable"], true, "{error}");
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            continue;
        }
        let count = device
            .ok(json!({"type": "sql", "sql": "SELECT COUNT(*) FROM items"}))
            .await["rows"][0][0]["value"]
            .as_str()
            .expect("count")
            .parse::<u64>()
            .expect("count");
        let id = device
            .ok(json!({"type": "next_id", "table": "items"}))
            .await["id"]
            .clone();
        device
            .ok(json!({
                "type": "sql",
                "sql": "INSERT INTO items(key,completed,title,id) VALUES(?,0,?,?)",
                "values": [
                    {"type": "text", "value": format!("{:04}", count + 1)},
                    {"type": "text", "value": format!("invoice {}", count + 1)},
                    {"type": "integer", "value": id},
                ],
            }))
            .await;
        let mut key = [handle; 32];
        key[31] = attempt;
        match device
            .call(json!({"type": "sql", "sql": "COMMIT", "handle": key, "coordinated": true}))
            .await
        {
            Ok(committed) => return committed["committed"].clone(),
            Err(error) => {
                assert!(
                    ["conflict", "aborted", "undecided", "behind"]
                        .contains(&error["code"].as_str().unwrap_or("")),
                    "{error}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
    }
    panic!("no invoice committed")
}
/// The invoice numbers once every frame they depend on arrived by sync.
async fn keys(device: &Device) -> Vec<String> {
    let query = json!({"type": "read", "query": {"sql": "SELECT key FROM items ORDER BY key", "values": [], "max_rows": 1024}});
    let mut page = device.call(query.clone()).await;
    for _ in 0..200 {
        match &page {
            Err(error) if error["retryable"] == true => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                page = device.call(query.clone()).await;
            }
            _ => break,
        }
    }
    page.expect("keys")["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| row[0]["value"].as_str().expect("key").to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_devices_number_concurrent_invoices_without_gaps() -> Result<(), Failure> {
    let root = common::identity::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let (a_dir, b_dir) = (root.path().join("a"), root.path().join("b"));
    let a = Device::new(Database::create(open(&a_dir, network("a")), None)?)?;
    let b = Device::new(Database::join(
        open(&b_dir, network("b")),
        &a.invite_as(json!({"kind": "writer", "scopes": ["items"]}))
            .await,
    )?)?;
    // Without a composed log a coordinated write is refused, never queued.
    let refused = a
        .call(json!({"type": "sql", "sql": "BEGIN", "coordinated": true}))
        .await
        .expect_err("no log");
    assert_eq!(refused["code"], "rejected");
    let log = MemoryLog::new();
    log.configure(domain(&a), 2)?;
    for device in [&a, &b] {
        let client = log.client(node_key(device).await);
        device.database.coordinate(Some(Arc::new(client))).await?;
    }
    introduce(&a, &b).await;
    // Automatic sync keeps live links open between the trusted devices.
    for device in [&a, &b] {
        device.ok(json!({"type": "sync_start"})).await;
    }
    linked(&a, &b).await;
    linked(&b, &a).await;
    // Both join; the first alone is below the minimum.
    let below = a
        .call(json!({"type": "sql", "sql": "BEGIN", "coordinated": true}))
        .await
        .expect_err("below minimum");
    assert_eq!(below["code"], "below_minimum", "{below}");

    let issue = |device: &'static Device, base: u8| async move {
        let mut positions = Vec::new();
        for handle in base..base + 3 {
            let committed = invoice(device, handle).await;
            positions.push(committed["position"].as_str().expect("position").to_owned());
        }
        positions
    };
    let (a, b): (&'static Device, &'static Device) =
        (Box::leak(Box::new(a)), Box::leak(Box::new(b)));
    let (first, second) = tokio::join!(issue(a, 0), issue(b, 16));
    let mut positions: Vec<u64> = first
        .iter()
        .chain(&second)
        .map(|position| position.parse().expect("position"))
        .collect();
    positions.sort_unstable();
    assert_eq!(positions, (1..=6).collect::<Vec<_>>());
    let numbers: Vec<String> = (1..=6).map(|number| format!("{number:04}")).collect();
    for device in [a, b] {
        let status = device.ok(json!({"type": "coordinated"})).await;
        assert_eq!(status["undecided"], 0, "{status}");
        assert_eq!(status["position"], "6", "{status}");
        assert_eq!(keys(device).await, numbers);
    }
    a.close().await;
    b.close().await;
    Ok(())
}
