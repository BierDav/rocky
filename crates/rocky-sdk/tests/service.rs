//! Ordinary rows, seal finality, staged publication and rollback over real voters.
// The SDK's blocking constructors, native transports and paused clocks.
#![cfg(not(target_family = "wasm"))]
use support::snapshot as snap;
mod support;
use rocky_coordinator::{certified::*, service::NetworkCoordinator};
use rocky_core::{MeshId, net::Protocol};
use rocky_sdk::credentials::PrivateKey;
use rocky_sdk::{CoordinationError, Database, Network, Open, counters, params};
use rocky_voter::VoterService;
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
use voters::{gated_service, service, starter};

type Failure = Box<dyn std::error::Error + Send + Sync>;

const SCHEMA: &str = r#"{"application":"counter","version":1,"format":6,"tables":[{"name":"counts","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"value","type":"integer","not_null":true}],"primary_key":["key"]}]}"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registered_service_domain_commits_ordinary_rows_and_closes() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;
    let peer =
        fixture::Peer::start(&root.path().join("a"), SCHEMA, Some(&bundle), None, true).await?;
    let opened = &peer.opened;
    let session = &peer.session;
    let identity = snap::value(session.info()).await?;
    // Controls trust identities; the node presents its certificate.
    let trusted = identity["principal"].clone();
    let tenant = PrivateKey::new();
    let register = || {
        peer.publish_coordination(
            rocky_coordinator::control::ControlRequest {
                tenant: rocky_sdk::credentials::PrivateKey::from_bytes(&snap::digest(
                    &(tenant.to_hex()),
                )),
                trusted: serde_json::from_value::<Vec<rocky_sdk::credentials::PublicKey>>(json!([
                    trusted
                ]))
                .expect("typed fixture"),
                lifetime: 3600,
                id: serde_json::from_value::<[u8; 32]>(json!(vec![1u8; 32]))
                    .expect("typed fixture"),
                revision: rocky_coordinator::control::ControlRevision::Register {
                    grant: serde_json::from_value::<
                        Option<Box<rocky_sdk::credentials::SignedCoordinationGrant>>,
                    >(json!(null))
                    .expect("typed fixture"),
                    admission: serde_json::from_value::<
                        Option<Box<rocky_sdk::credentials::SignedTenantAdmission>>,
                    >(json!(null))
                    .expect("typed fixture"),
                },
            },
            Some(std::sync::Arc::new(
                rocky_sdk::RootKey::from_phrase(&snap::text(
                    &(opened.root.as_ref().expect("created with a root").phrase()),
                ))
                .expect("root"),
            )),
        )
    };
    let registered = snap::value(register()).await?;
    assert_eq!(registered["tenant"], tenant.public_key().to_hex());
    assert_eq!(snap::value(register()).await?, registered);
    snap::value(session.execute("BEGIN", Vec::new())).await?;
    let id = snap::value(session.next_id("counts")).await?["id"].clone();
    snap::value(session.execute(
        "INSERT INTO counts(key,value,id) VALUES('total',5,?)",
        vec![snap::row_id(&(id))],
    ))
    .await?;
    let committed = snap::value(session.commit([2u8; 32])).await?;
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
    let usage = snap::value(
        peer.control()
            .usage(rocky_sdk::credentials::PrivateKey::from_bytes(
                &snap::digest(&(tenant.to_hex())),
            )),
    )
    .await?;
    assert_eq!(usage["open_domains"], 1);
    snap::value(peer.control().close(
        peer.scope,
        rocky_sdk::credentials::PrivateKey::from_bytes(&snap::digest(&(tenant.to_hex()))),
        [3u8; 32],
    ))
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
    peer.close().await?;
    Ok(())
}
