use rocky_sdk::{
    Database, Network, ServiceError,
    transport::{
        Connector,
        memory::{Hub, Memory},
    },
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, atomic::Ordering},
};
use support::snapshot as snap;
type Failure = Box<dyn std::error::Error + Send + Sync>;

use crate::support;

fn schema() -> rocky_sdk::Schema {
    crate::common::schema()
}

fn open(directory: &Path, network: Option<Network>) -> rocky_sdk::Configuration {
    crate::common::configuration(directory, &schema(), None, None, network)
}
fn service(error: &rocky_sdk::Error) -> Option<&ServiceError> {
    match &error.kind() {
        rocky_sdk::ErrorKind::Service(service) => Some(service),
        _ => None,
    }
}

async fn add(database: &Database, handle: u8, title: &str) {
    snap::ok(snap::mutate(
        database,
        vec![handle; 32],
        snap::Action::Create {
            table: "items".to_owned(),
            values: vec![
                ("title".to_owned(), snap::field(json!(title))),
                ("completed".to_owned(), snap::field(json!(false))),
            ],
        },
        None,
        None,
    ))
    .await;
}
async fn titles(database: &Database) -> Vec<String> {
    let page = snap::ok(snap::read(
        database,
        "SELECT title FROM items ORDER BY title",
        Vec::new(),
    ))
    .await;
    page["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row[0]["value"].as_str().map(String::from))
        .collect()
}
async fn sync(database: &Database) -> Result<Value, Value> {
    snap::observe(database.sync_now()).await
}
async fn transport(database: &Database) -> Option<String> {
    database.status().await.expect("status").network.kind
}

fn memory(hub: &Arc<Hub>, name: &'static str) -> Arc<Memory> {
    hub.connector(name)
}
fn dials(hub: &Hub) -> usize {
    let [_, dials, _] = hub.counts();
    dials
}
fn closed_transports(hub: &Hub) -> usize {
    let [.., closes] = hub.counts();
    closes
}

#[tokio::test]
async fn status_observation_does_not_block_commits_or_shutdown() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database = open(root.path(), None).create().await?.database;
    let observer = database.clone();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let writes = async {
            for id in 1..=4 {
                add(&database, id, "Milk").await;
            }
        };
        let observations = async {
            for _ in 0..8 {
                snap::ok(observer.status()).await;
            }
        };
        tokio::join!(writes, observations);
        assert_eq!(titles(&database).await.len(), 4);
        database.close().await
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn a_database_opens_reads_writes_and_reopens_without_a_network() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("a");
    let database = open(&directory, None).create().await?.database;
    assert!(database.status().await?.authority.is_issuer);
    assert_eq!(transport(&database).await, None);
    add(&database, 1, "Milk").await;
    assert_eq!(titles(&database).await, ["Milk"]);
    snap::ok(async {
        database.renew().await?;
        snap::device(&database).await
    })
    .await;
    let error = sync(&database).await.err().ok_or("needs a network")?;
    assert_eq!(
        error["code"], "network_required",
        "sync requires a network: {error}"
    );
    assert_eq!(error["retryable"], json!(false));
    let offline = database
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await
        .err()
        .ok_or("an invitation needs a route to its admin")?;
    assert_eq!(
        offline.kind(),
        &rocky_sdk::ErrorKind::Invite(rocky_sdk::InviteError::NetworkRequired)
    );
    database.close().await?;
    database.close().await?;

    let database = open(&directory, None).open().await?;
    add(&database, 2, "Eggs").await;
    assert_eq!(titles(&database).await, ["Eggs", "Milk"]);
    database.close().await?;

    assert_eq!(
        "rocky://provision/x"
            .parse::<rocky_sdk::InvitationCode>()
            .err(),
        Some(rocky_sdk::JoinError::Invalid),
        "an invalid code is refused before anything is stored"
    );
    assert!(!root.path().join("c").exists());
    assert!(open(&root.path().join("c"), None).open().await.is_err());
    assert!(open(&directory, None).create().await.is_err());
    Ok(())
}

#[tokio::test]
async fn a_domain_enrolled_over_a_network_reopens_without_dialing() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let creator = open(
        &root.path().join("a"),
        Some(Network::Owned(memory(&hub, "a"))),
    )
    .create()
    .await?
    .database;
    let invitation = creator
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;
    let member = open(
        &root.path().join("b"),
        Some(Network::Owned(memory(&hub, "b"))),
    )
    .join(&invitation)
    .await?
    .database;
    member.sync_now().await?;
    member.close().await?;
    creator.close().await?;
    let enrolled = hub.counts();
    assert_eq!(
        enrolled,
        [2, 3, 2],
        "starts, dials, closes: one dial redeems the invitation and one bootstraps"
    );
    assert!(!hub.listens("a"));

    for (directory, handle) in [("a", 1), ("b", 2)] {
        let directory = root.path().join(directory);
        for round in 0..2 {
            let database = open(&directory, None).open().await?;
            assert_eq!(
                transport(&database).await,
                None,
                "a domain opened without a network is bound to none"
            );
            add(&database, handle + 10 * round, &format!("{handle}.{round}")).await;
            assert_eq!(titles(&database).await.len(), usize::from(round) + 1);
            assert_eq!(
                sync(&database).await.err().ok_or("offline")?["code"],
                "network_required"
            );
            database.close().await?;
        }
    }
    assert_eq!(
        hub.counts(),
        enrolled,
        "both devices open, write and reopen without dialing"
    );

    let creator = open(&root.path().join("a"), None).open().await?;
    let member = open(&root.path().join("b"), None).open().await?;
    creator.attach(Network::Owned(memory(&hub, "a"))).await?;
    member.attach(Network::Owned(memory(&hub, "b"))).await?;
    assert!(
        member.sync_now().await?.complete(),
        "the same encrypted sync path serves databases opened locally"
    );
    assert_eq!(titles(&member).await, ["1.0", "1.1", "2.0", "2.1"]);
    assert_eq!(titles(&creator).await, titles(&member).await);
    member.detach().await?;
    creator.detach().await?;
    member.close().await?;
    creator.close().await?;
    Ok(())
}

#[tokio::test]
async fn clones_share_one_database_and_any_close_ends_every_wait() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let database = open(root.path(), None).create().await?.database;
    let (first, second) = (database.clone(), database.clone());
    let mut first_changes = first.status_changes();
    first_changes.next().await?;
    let mut second_changes = second.status_changes();
    second_changes.next().await?;

    add(&second, 1, "Milk").await;
    assert_ne!(second_changes.next().await?.token, "");
    assert_eq!(titles(&first).await, ["Milk"]);

    let waiting = tokio::spawn(async move {
        first_changes.next().await?;
        first_changes.next().await
    });
    tokio::task::yield_now().await;
    second.close().await?;
    first.close().await?;
    let ended = waiting.await?.err().ok_or("the wait ends")?;
    assert_eq!(
        service(&ended),
        Some(&ServiceError::Closed("database")),
        "closing any clone ends a wait held by another: {ended}"
    );
    let error = first.status().await.err().ok_or("closed database")?;
    assert_eq!(service(&error), Some(&ServiceError::Closed("database")));
    Ok(())
}

#[tokio::test]
async fn borrowed_networks_outlive_a_detach_and_owned_ones_are_released() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let listening = memory(&hub, "a").start(true, [0; 32]).await?;
    let dialing = memory(&hub, "b").start(false, [0; 32]).await?;

    let creator = open(&root.path().join("a"), None).create().await?.database;
    creator.attach(Network::Attached(listening.clone())).await?;
    let error = creator
        .attach(Network::Attached(listening.clone()))
        .await
        .err()
        .ok_or("one network at a time")?;
    assert_eq!(service(&error), Some(&ServiceError::NetworkAttached));
    assert_eq!(
        transport(&creator).await.as_deref(),
        Some("memory"),
        "the status names the attached network's kind"
    );
    let invitation = creator
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;

    let member = open(
        &root.path().join("b"),
        Some(Network::Attached(dialing.clone())),
    )
    .join(&invitation)
    .await?
    .database;
    assert_eq!(
        transport(&member).await.as_deref(),
        Some("memory"),
        "the joining network stays attached"
    );
    add(&creator, 1, "Milk").await;
    sync(&member).await.map_err(|e| e.to_string())?;
    assert_eq!(titles(&member).await, ["Milk"]);

    member.detach().await?;
    member.detach().await?;
    creator.detach().await?;
    assert_eq!(
        closed_transports(&hub),
        0,
        "detaching leaves both borrowed transports open"
    );
    assert!(hub.listens("a"));
    assert_eq!(
        sync(&member).await.err().ok_or("detached")?["code"],
        "network_required"
    );
    add(&member, 2, "Eggs").await;
    add(&creator, 3, "Jam").await;

    creator.attach(Network::Attached(listening.clone())).await?;
    member.attach(Network::Attached(dialing.clone())).await?;
    sync(&member).await.map_err(|e| e.to_string())?;
    assert_eq!(
        titles(&member).await,
        ["Eggs", "Jam", "Milk"],
        "the same transports serve again once attached again"
    );
    assert_eq!(titles(&creator).await, titles(&member).await);

    member.close().await?;
    creator.close().await?;
    assert_eq!(
        closed_transports(&hub),
        0,
        "closing a database detaches but leaves a borrowed transport"
    );
    assert!(hub.listens("a"));
    listening.close().await;
    dialing.close().await;
    assert_eq!(closed_transports(&hub), 2);

    let creator = open(
        &root.path().join("a"),
        Some(Network::Owned(memory(&hub, "a"))),
    )
    .open()
    .await?;
    assert!(hub.listens("a"));
    creator.detach().await?;
    assert!(
        !hub.listens("a"),
        "a detach closes a transport the database started"
    );
    assert_eq!(closed_transports(&hub), 3);
    assert_eq!(titles(&creator).await.len(), 3);
    creator.close().await?;
    assert_eq!(
        closed_transports(&hub),
        3,
        "a started transport is closed once"
    );
    Ok(())
}

#[tokio::test]
async fn a_detach_cancels_work_that_outlasts_its_drain() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let creator = open(
        &root.path().join("a"),
        Some(Network::Owned(memory(&hub, "a"))),
    )
    .create()
    .await?
    .database;
    let invitation = creator
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?
        .code;
    let member = open(
        &root.path().join("b"),
        Some(Network::Owned(memory(&hub, "b"))),
    )
    .join(&invitation)
    .await?
    .database;
    add(&member, 1, "Milk").await;

    hub.stalled.store(true, Ordering::SeqCst);
    let dialed_before_the_stall = dials(&hub);
    let stuck = {
        let member = member.clone();
        tokio::spawn(async move { sync(&member).await })
    };
    while dials(&hub) == dialed_before_the_stall {
        tokio::task::yield_now().await;
    }
    let started = std::time::Instant::now();
    member.detach().await?;
    assert!(
        started.elapsed().as_secs() >= 4,
        "a detach waits out the drain bound before it cancels the stalled dial"
    );
    assert_eq!(
        stuck.await?.err().ok_or("cancelled")?["code"],
        "closed",
        "a dial that never completes holds the sync until the network detaches"
    );
    assert_eq!(closed_transports(&hub), 1);

    add(&member, 2, "Eggs").await;
    hub.stalled.store(false, Ordering::SeqCst);
    member.attach(Network::Owned(memory(&hub, "b"))).await?;
    sync(&member).await.map_err(|e| e.to_string())?;
    assert_eq!(
        titles(&creator).await,
        ["Eggs", "Milk"],
        "the database stays intact and the interrupted sync is safe to repeat"
    );
    member.close().await?;
    creator.close().await?;
    assert_eq!(closed_transports(&hub), 3);
    Ok(())
}
