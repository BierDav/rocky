//! Signed backup archives restored into a fresh signing identity. See
//! `docs/runtime/backup-and-restore.md`.
mod common;
use common::identity as support;
use common::{Device, Failure, SCHEMA, introduce};
use rocky_sdk::{Database, Network, Open, transport::memory::Hub};
use serde_json::Value;
use std::{path::Path, sync::Arc};

fn open(directory: &Path, network: Option<Network>) -> Open<'_> {
    Open {
        directory,
        schema: SCHEMA,
        secrets: None,
        network,
        commands: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backup_restores_into_a_fresh_identity_without_forking_or_resending()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let a = Device::create(&root.path().join("a"), network("a"))?;
    let b = Device::join(
        &root.path().join("b"),
        &a.invite().await,
        Network::Owned(hub.connector("b")),
    )?;
    introduce(&a, &b).await;
    b.add(1, "apples").await;
    b.sync().await.expect("sync");
    // Bread never left the device that is about to be lost.
    b.add(2, "bread").await;
    let lost = b.node().await;
    let archive = root.path().join("b.backup");
    let (first, second) = tokio::join!(b.database.backup(&archive), b.database.backup(&archive));
    assert_ne!(first.is_ok(), second.is_ok(), "one archive writer wins");
    let inventory: Value = serde_json::from_str(&first.or(second)?)?;
    assert_eq!(inventory["node"].as_str(), Some(lost.as_str()));
    let names: Vec<&str> = inventory["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|file| file["name"].as_str())
        .collect();
    assert_eq!(names, ["identity.json", "rocky.sqlite"]);
    assert!(
        b.database.backup(&archive).await.is_err(),
        "never overwritten"
    );
    b.close().await;

    // A changed byte, or an invitation of another domain, is refused.
    let mut bytes = std::fs::read(&archive)?;
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let tampered = root.path().join("tampered.backup");
    std::fs::write(&tampered, bytes)?;
    let invitation = a.invite().await;
    assert!(
        Database::restore(
            open(&root.path().join("t"), network("t")),
            &tampered,
            &invitation
        )
        .is_err()
    );
    let other = Device::create(&root.path().join("other"), network("other"))?;
    let foreign = other.invite().await;
    let refused = Database::restore(
        open(&root.path().join("f"), network("f")),
        &archive,
        &foreign,
    );
    let error = refused.err().expect("another domain").to_string();
    assert!(error.contains("another domain"), "{error}");
    other.close().await;

    let c_dir = root.path().join("c");
    let c = Device::new(Database::restore(
        open(&c_dir, network("c")),
        &archive,
        &a.invite().await,
    )?)?;
    assert_ne!(c.node().await, lost, "a fresh signing identity");
    assert_eq!(c.titles().await, ["apples", "bread"]);
    // The lost device's handle still names its one operation.
    let replayed = c.ok(serde_json::json!({"type":"mutate","handle":vec![2;32],"fixture_author":lost,"payload":serde_json::json!({"type":"create","collection":"items","values":{"title":"bread","completed":false}}).to_string().into_bytes(),"entity":null,"baseline":null})).await;
    assert_eq!(replayed["replayed"], true);
    assert_eq!(c.titles().await, ["apples", "bread"]);
    // Its unsent edit is delivered as the lost device signed it; new edits
    // are the new identity's own.
    c.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread"]);
    c.add(3, "cheese").await;
    c.sync().await.expect("sync");
    assert_eq!(a.titles().await, ["apples", "bread", "cheese"]);
    c.close().await;
    // The directory now holds a domain: the archive is not restored again.
    assert!(Database::restore(open(&c_dir, network("c")), &archive, &a.invite().await).is_err());
    let c = Device::new(Database::open(open(&c_dir, None))?)?;
    assert_eq!(c.titles().await, ["apples", "bread", "cheese"]);
    c.close().await;
    a.close().await;
    Ok(())
}

const COUNTER: &str = r#"{"application":"counter","version":1,"commands":{"format":1,"handler":1,"commands":[{"name":"Add","version":1,"handler":"counts.add.v1","arguments":[{"name":"amount","type":"integer"}],"accepted":[{"name":"value","type":"integer"}],"rejected":[],"invocation":"counts","result":"counts","resources":["counts"]}]},"format":3,"tables":[{"name":"counts","class":"mergeable","columns":[{"name":"key","type":"text","not_null":true},{"name":"value","type":"integer","not_null":true}],"primary_key":["key"]}]}"#;

struct Counter;
impl rocky_sdk::CommandHandler for Counter {
    fn handle(
        &self,
        input: &rocky_core::HandlerInput,
    ) -> Result<rocky_core::HandlerOutput, String> {
        let rocky_core::SqlValue::Integer(amount) = input.arguments[0] else {
            return Err("amount".into());
        };
        let value = match input
            .reads
            .scan("counts", None, None, 1)?
            .first()
            .map(|row| &row[1])
        {
            Some(rocky_core::SqlValue::Integer(value)) => *value,
            _ => 0,
        } + amount;
        Ok(rocky_core::HandlerOutput {
            outcome: rocky_core::Outcome::Accepted(vec![rocky_core::SqlValue::Integer(value)]),
            writes: vec![rocky_core::TableWrite::Put {
                table: "counts".into(),
                row: support::identity_row(vec![
                    rocky_core::SqlValue::Text("total".into()),
                    rocky_core::SqlValue::Integer(value),
                ]),
            }],
            jobs: Vec::new(),
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restored_ticket_keeps_its_request_identity_and_confirms_once() -> Result<(), Failure> {
    use serde_json::json;
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let coordinator = Arc::new(rocky_coordinator::MemoryCoordinator::default());
    let commands = |handler: bool| {
        Some(rocky_sdk::Commands {
            coordinator: rocky_sdk::Coordination::Composed(coordinator.clone()),
            handler: handler.then(|| Arc::new(Counter) as Arc<dyn rocky_sdk::CommandHandler>),
            deadline: None,
        })
    };
    let directories = ["a", "b", "c"].map(|name| root.path().join(name));
    let open = |index: usize, handler: bool| Open {
        directory: &directories[index],
        schema: COUNTER,
        secrets: None,
        network: Some(Network::Owned(hub.connector(["a", "b", "c"][index]))),
        commands: commands(handler),
    };
    let a = Device::new(Database::create(open(0, true), None)?)?;
    let writer = json!({"kind": "writer", "scopes": ["counts"]});
    let b = Device::new(Database::join(
        open(1, false),
        &a.invite_as(writer.clone()).await,
    )?)?;
    introduce(&a, &b).await;
    let identity = |value: &Value| -> ([u8; 16], [u8; 16], rocky_core::Scope) {
        let id = |name: &str| -> [u8; 16] {
            let hex = value[name].as_str().unwrap_or_default();
            let bytes: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap_or_default())
                .collect();
            bytes.try_into().unwrap_or_default()
        };
        (
            id("node"),
            id("principal"),
            serde_json::from_value(value["scope"].clone()).expect("scope"),
        )
    };
    let (executor, owner, scope) = identity(&a.ok(json!({"type": "command_identity"})).await);
    let (caller, principal, _) = identity(&b.ok(json!({"type": "command_identity"})).await);
    let (executor, owner) = (rocky_core::NodeId(executor), rocky_core::PrincipalId(owner));
    coordinator.register(scope, owner, [1; 16], vec![executor], 1, 1)?;
    coordinator.set_invokers(
        scope,
        owner,
        vec![
            (executor, owner),
            (
                rocky_core::NodeId(caller),
                rocky_core::PrincipalId(principal),
            ),
        ],
    )?;
    coordinator.acquire(scope, owner, executor)?;
    let submitted = b
        .ok(
            json!({"type": "command_submit", "handle": vec![7; 32], "method": "Add", "version": 1,
                   "arguments": [{"type": "integer", "value": "5"}]}),
        )
        .await;
    assert_eq!(submitted["state"], "queued");
    let archive = root.path().join("b.backup");
    b.database.backup(&archive).await?;
    b.close().await;

    // The new device holds the same ticket under its original request.
    let c = Device::new(Database::restore(
        open(2, false),
        &archive,
        &a.invite_as(writer).await,
    )?)?;
    let restored = c
        .ok(json!({"type": "command_ticket", "handle": vec![7; 32]}))
        .await;
    assert_eq!(restored["request"], submitted["request"]);
    assert_eq!(restored["state"], "queued");
    let ran = c
        .ok(json!({"type": "command_run", "handle": vec![7; 32]}))
        .await["tickets"][0]
        .clone();
    assert_eq!(ran["request"], submitted["request"]);
    assert!(
        ["confirmed", "applied"].contains(&ran["state"].as_str().unwrap_or_default()),
        "{ran}"
    );
    assert_eq!(coordinator.head(scope)?.sequence, 1);
    // Running it again is the same request, never a second one.
    c.ok(json!({"type": "command_run", "handle": vec![7; 32]}))
        .await;
    assert_eq!(coordinator.head(scope)?.sequence, 1);
    c.close().await;
    a.close().await;
    Ok(())
}
