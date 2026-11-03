//! Ordinary rows, seal finality, staged publication and rollback over real voters.
// The SDK's blocking constructors, native transports and paused clocks.
#![cfg(not(target_family = "wasm"))]
#![cfg(feature = "iroh")]
mod support;
use rocky_coordinator::{
    certified::*,
    service::{NetworkCoordinator, VoterService},
};
use rocky_core::{MeshId, net::Protocol};
use rocky_sdk::credentials::PrivateKey;
use rocky_sdk::{
    Coordination, CoordinationError, Database, Network, Open, counters, params,
    transport::TransportOptions,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc, time::Duration};

#[path = "service/claims.rs"]
mod claims;
#[path = "service/fixture.rs"]
mod fixture;
#[path = "service/inspection.rs"]
mod inspection;
#[path = "service/migration.rs"]
mod migration;
#[path = "service/prerequisites.rs"]
mod prerequisites;
#[path = "service/rollback.rs"]
mod rollback;
#[path = "service/voters.rs"]
mod voters;
use voters::{service, starter};

type Failure = Box<dyn std::error::Error + Send + Sync>;

const SCHEMA: &str = r#"{"application":"counter","version":1,"format":6,"tables":[{"name":"counts","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"value","type":"integer","not_null":true}],"primary_key":["key"]}]}"#;

async fn call(
    session: &rocky_sdk::Session,
    scope: &Value,
    request: Value,
) -> Result<Value, Failure> {
    let reply: Value = serde_json::from_str(
        &session
            .call(json!({"version": 1, "scope": scope, "request": request}).to_string())
            .await?,
    )?;
    if reply["status"] == "ok" {
        Ok(reply["value"].clone())
    } else {
        Err(format!("{} failed: {}", request["type"], reply["error"]).into())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registered_service_domain_commits_ordinary_rows_and_closes() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;
    let directory = root.path().join("a");
    let opened = tokio::task::spawn_blocking(move || {
        Database::create(
            Open {
                identity: None,
                directory: &directory,
                schema: SCHEMA,
                secrets: None,
                network: Some(Network::Owned(Arc::new(starter()))),
                coordination: Some(Coordination::Service(Box::new(bundle))),
            },
            None,
        )
    })
    .await??;
    let session = opened.database.session();
    let scope: Value = serde_json::from_str(&opened.database.scope())?;
    let identity = call(&session, &scope, json!({"type": "session"})).await?;
    // Controls trust identities; the node presents its certificate.
    let trusted = identity["principal"].clone();
    let tenant = PrivateKey::new();
    let register = json!({"type": "coordination_register", "tenant": tenant.to_hex(),
        "registration": vec![1u8; 32], "trusted": [trusted], "lifetime": 3600});
    let registered = call(&session, &scope, register.clone()).await?;
    assert_eq!(registered["tenant"], tenant.public_key().to_hex());
    assert_eq!(call(&session, &scope, register).await?, registered);
    call(&session, &scope, json!({"type": "sql", "sql": "BEGIN"})).await?;
    let id = call(
        &session,
        &scope,
        json!({"type":"next_id", "table":"counts"}),
    )
    .await?["id"]
        .clone();
    call(
        &session,
        &scope,
        json!({"type":"sql", "sql":"INSERT INTO counts(key,value,id) VALUES('total',5,?)",
        "values":[{"type":"integer", "value": id}]}),
    )
    .await?;
    let committed = call(
        &session,
        &scope,
        json!({"type":"sql", "sql":"COMMIT", "handle":vec![2u8;32]}),
    )
    .await?;
    assert!(!committed["committed"].is_null());
    // A claim is a coordinator decision outside any transaction: the
    // application keeps the claim, uses the value in an ordinary write and
    // retries with the same claim.
    let database = &opened.database;
    let invoices = counters::Name::new("company-1/invoices")?;
    let pending = counters::Claim::fresh()?;
    let number = database.claim(invoices.clone(), pending).await?;
    assert_eq!(number, 1);
    assert_eq!(database.claim(invoices.clone(), pending).await?, number);
    let next = counters::Claim::fresh()?;
    assert_eq!(database.claim(invoices.clone(), next).await?, 2);
    let reading = database.counter(invoices.clone(), Some(pending)).await?;
    assert_eq!(
        (reading.value, reading.claim),
        (2, Some(counters::Lookup::Claimed(1)))
    );
    let mut transaction = database.transaction().await?;
    let id = transaction.next_id("counts", params![]).await?;
    transaction
        .execute(
            "INSERT INTO counts(id,key,value) VALUES(?,?,?)",
            params![id, "invoice", i64::try_from(number)?],
        )
        .await?;
    transaction.commit().await?;
    let usage = call(
        &session,
        &scope,
        json!({"type":"coordination_usage", "tenant":tenant.to_hex()}),
    )
    .await?;
    assert_eq!(usage["open_domains"], 1);
    call(
        &session,
        &scope,
        json!({"type":"coordination_close", "tenant":tenant.to_hex(), "id":vec![3u8;32]}),
    )
    .await?;
    // A closed domain takes no claim.
    let refused = database
        .claim(invoices, counters::Claim::fresh()?)
        .await
        .unwrap_err();
    assert_eq!(
        refused.downcast_ref::<CoordinationError>(),
        Some(&CoordinationError::Denied)
    );
    session.close().await;
    if let Some(network) = opened.network {
        network.close().await;
    }
    opened.database.close().await?;
    Ok(())
}
