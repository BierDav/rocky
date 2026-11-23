use rocky_sdk::{
    Database, Network, NetworkService, Open, Opened, ServiceError, Session,
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

const SCHEMA: &str = r#"{"application":"shopping","version":1,"format":6,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"completed","type":"integer","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

fn open(directory: &Path, network: Option<Network>) -> Open<'_> {
    Open {
        identity: None,
        directory,
        schema: crate::support::snapshot::schema(SCHEMA),
        secrets: None,
        network,
    }
}
fn service(error: &Failure) -> Option<&ServiceError> {
    error.downcast_ref()
}

struct Client {
    database: Arc<Database>,
    session: Session,
}
impl Client {
    fn new(database: &Arc<Database>) -> Result<Self, Failure> {
        Ok(Self {
            database: database.clone(),
            session: database.session(),
        })
    }

    async fn add(&self, handle: u8, title: &str) {
        snap::ok(snap::mutate(
            &self.database,
            &self.session,
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
    async fn titles(&self) -> Vec<String> {
        let page = snap::ok(snap::read(
            &self.database,
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
    async fn sync(&self) -> Result<Value, Value> {
        snap::observe(self.session.sync_once()).await
    }
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
    let Opened { database, .. } = Database::create(open(root.path(), None), None)?;
    let writer = Client::new(&database)?;
    let observer = Client::new(&database)?;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let writes = async {
            for id in 1..=4 {
                writer.add(id, "Milk").await;
            }
        };
        let observations = async {
            for _ in 0..8 {
                snap::ok(observer.session.status()).await;
            }
        };
        tokio::join!(writes, observations);
        assert_eq!(writer.titles().await.len(), 4);
        database.close().await
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn a_database_opens_reads_writes_and_reopens_without_a_network() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("a");
    let Opened {
        database, network, ..
    } = Database::create(open(&directory, None), None)?;
    assert!(network.is_none());
    let client = Client::new(&database)?;
    let domain = snap::ok(client.session.domain()).await;
    assert_eq!(domain["issuer"], json!(true));
    assert_eq!(domain["transport"], "");
    client.add(1, "Milk").await;
    assert_eq!(client.titles().await, ["Milk"]);
    snap::ok(async {
        client.session.renew().await?;
        client.session.info().await
    })
    .await;
    let error = snap::observe(client.session.sync_once())
        .await
        .err()
        .ok_or("needs a network")?;
    assert_eq!(
        error["code"], "network_required",
        "sync requires a network: {error}"
    );
    assert_eq!(error["retryable"], json!(false));
    let provisioning = client
        .session
        .provision(
            rocky_sdk::GrantProfile::Writer {
                scopes: ["items".into()].into(),
            },
            None,
        )
        .await?;
    database.close().await?;
    database.close().await?;

    let Opened { database, .. } = Database::open(open(&directory, None))?;
    let client = Client::new(&database)?;
    client.add(2, "Eggs").await;
    assert_eq!(client.titles().await, ["Eggs", "Milk"]);
    database.close().await?;

    let joined = Database::join(open(&root.path().join("b"), None), &provisioning.uri)?;
    assert!(joined.network.is_none());
    joined.database.close().await?;
    assert!(Database::join(open(&root.path().join("c"), None), "rocky://provision/x").is_err());
    assert!(
        !root.path().join("c").exists(),
        "an invalid provisioning bundle is refused before anything is stored"
    );
    assert!(Database::open(open(&root.path().join("c"), None)).is_err());
    assert!(Database::create(open(&directory, None), None).is_err());
    Ok(())
}

#[tokio::test]
async fn a_domain_enrolled_over_a_network_reopens_without_dialing() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let creator = Database::create(
        open(
            &root.path().join("a"),
            Some(Network::Owned(memory(&hub, "a"))),
        ),
        None,
    )?;
    let invitation = creator
        .database
        .session()
        .provision(
            rocky_sdk::GrantProfile::Writer {
                scopes: ["items".into()].into(),
            },
            None,
        )
        .await?
        .uri;
    let member = Database::join(
        open(
            &root.path().join("b"),
            Some(Network::Owned(memory(&hub, "b"))),
        ),
        &invitation,
    )?;
    member.database.session().sync_once().await?;
    if let Some(network) = member.network {
        network.close().await;
    }
    member.database.close().await?;
    if let Some(network) = creator.network {
        network.close().await;
    }
    creator.database.close().await?;
    let enrolled = hub.counts();
    assert_eq!(
        enrolled,
        [2, 2, 2],
        "starts, dials, closes: 0-RTT provisioning needs no enrollment dial and bootstrap shares a link"
    );
    assert!(!hub.listens("a"));

    for (directory, handle) in [("a", 1), ("b", 2)] {
        let directory = root.path().join(directory);
        for round in 0..2 {
            let Opened {
                database, network, ..
            } = Database::open(open(&directory, None))?;
            assert!(network.is_none());
            let client = Client::new(&database)?;
            assert_eq!(
                snap::ok(client.session.domain()).await["transport"],
                "",
                "a domain opened without a network is bound to none"
            );
            client
                .add(handle + 10 * round, &format!("{handle}.{round}"))
                .await;
            assert_eq!(client.titles().await.len(), usize::from(round) + 1);
            assert_eq!(
                client.sync().await.err().ok_or("offline")?["code"],
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

    let Opened {
        database: creator, ..
    } = Database::open(open(&root.path().join("a"), None))?;
    let Opened {
        database: member, ..
    } = Database::open(open(&root.path().join("b"), None))?;
    let serving = creator.attach(Network::Owned(memory(&hub, "a"))).await?;
    let syncing = member.attach(Network::Owned(memory(&hub, "b"))).await?;
    let (creator_client, member_client) = (Client::new(&creator)?, Client::new(&member)?);
    assert!(
        support::reconciled(&member_client.sync().await.map_err(|e| e.to_string())?),
        "the same encrypted sync path serves databases opened locally"
    );
    assert_eq!(member_client.titles().await, ["1.0", "1.1", "2.0", "2.1"]);
    assert_eq!(creator_client.titles().await, member_client.titles().await);
    syncing.close().await;
    serving.close().await;
    member.close().await?;
    creator.close().await?;
    Ok(())
}

#[tokio::test]
async fn sessions_share_a_database_and_close_independently() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let Opened { database, .. } = Database::create(open(root.path(), None), None)?;
    let (first, second) = (Client::new(&database)?, Client::new(&database)?);
    let first_token = first.session.status().await?.token;
    let second_token = second.session.status().await?.token;
    let first = Arc::new(first);
    let waiting = {
        let first = first.clone();
        tokio::spawn(async move { first.session.status_after(first_token).await })
    };
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());

    first.session.close().await;
    first.session.close().await;
    let ended = waiting.await?.err().ok_or("the wait ends")?;
    assert_eq!(
        service(&ended),
        Some(&ServiceError::Closed("session")),
        "closing a session ends its wait: {ended}"
    );
    let error = first.session.domain().await.err().ok_or("closed session")?;
    assert_eq!(service(&error), Some(&ServiceError::Closed("session")));

    second.add(1, "Milk").await;
    let next = second.session.status_after(second_token).await?;
    assert_ne!(next.token, "");
    let third = Client::new(&database)?;
    assert_eq!(third.titles().await, ["Milk"]);

    database.close().await?;
    let error = second
        .session
        .domain()
        .await
        .err()
        .ok_or("closed database")?;
    assert_eq!(
        service(&error),
        Some(&ServiceError::Closed("database")),
        "closing the database closes the sessions that remain"
    );
    Ok(())
}

#[tokio::test]
async fn borrowed_networks_outlive_their_service_and_owned_ones_are_released() -> Result<(), Failure>
{
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let listening = memory(&hub, "a").start(true, [0; 32]).await?;
    let dialing = memory(&hub, "b").start(false, [0; 32]).await?;

    let Opened {
        database: creator, ..
    } = Database::create(open(&root.path().join("a"), None), None)?;
    let creator_client = Client::new(&creator)?;
    let serving = creator.attach(Network::Attached(listening.clone())).await?;
    let error = creator
        .attach(Network::Attached(listening.clone()))
        .await
        .err()
        .ok_or("one network at a time")?;
    assert_eq!(service(&error), Some(&ServiceError::NetworkAttached));
    assert_eq!(
        snap::ok(creator_client.session.domain()).await["transport"],
        "memory",
        "the status names the attached network's kind"
    );
    let invitation = snap::ok(creator_client.session.provision(
        rocky_sdk::GrantProfile::Writer {
            scopes: ["items".to_owned()].into_iter().collect(),
        },
        None,
    ))
    .await["uri"]
        .as_str()
        .ok_or("invitation")?
        .to_owned();

    let member_directory = root.path().join("b");
    let Opened {
        database: member,
        network,
        ..
    } = Database::join(
        open(&member_directory, Some(Network::Attached(dialing.clone()))),
        &invitation,
    )?;
    let syncing: NetworkService = network.ok_or("the joining network stays attached")?;
    let member_client = Client::new(&member)?;
    creator_client.add(1, "Milk").await;
    member_client.sync().await.map_err(|e| e.to_string())?;
    assert_eq!(member_client.titles().await, ["Milk"]);

    syncing.close().await;
    syncing.close().await;
    serving.close().await;
    assert_eq!(
        closed_transports(&hub),
        0,
        "closing the services leaves both borrowed transports open"
    );
    assert!(hub.listens("a"));
    assert_eq!(
        member_client.sync().await.err().ok_or("detached")?["code"],
        "network_required"
    );
    member_client.add(2, "Eggs").await;
    creator_client.add(3, "Jam").await;

    let serving = creator.attach(Network::Attached(listening.clone())).await?;
    let syncing = member.attach(Network::Attached(dialing.clone())).await?;
    member_client.sync().await.map_err(|e| e.to_string())?;
    assert_eq!(
        member_client.titles().await,
        ["Eggs", "Jam", "Milk"],
        "the same transports serve again once attached again"
    );
    assert_eq!(creator_client.titles().await, member_client.titles().await);
    drop((serving, syncing));

    member.close().await?;
    creator.close().await?;
    assert_eq!(
        closed_transports(&hub),
        0,
        "closing a database detaches its service but leaves a borrowed transport"
    );
    assert!(hub.listens("a"));
    listening.close().await;
    dialing.close().await;
    assert_eq!(closed_transports(&hub), 2);

    let Opened {
        database: creator,
        network,
        ..
    } = Database::open(open(
        &root.path().join("a"),
        Some(Network::Owned(memory(&hub, "a"))),
    ))?;
    let serving = network.ok_or("attached at open")?;
    assert!(hub.listens("a"));
    serving.close().await;
    assert!(
        !hub.listens("a"),
        "a service that started its transport closes it"
    );
    assert_eq!(closed_transports(&hub), 3);
    assert_eq!(Client::new(&creator)?.titles().await.len(), 3);
    creator.close().await?;
    assert_eq!(
        closed_transports(&hub),
        3,
        "a started transport is closed once"
    );
    Ok(())
}

#[tokio::test]
async fn a_closing_service_cancels_work_that_outlasts_its_drain() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let Opened {
        database: creator, ..
    } = Database::create(
        open(
            &root.path().join("a"),
            Some(Network::Owned(memory(&hub, "a"))),
        ),
        None,
    )?;
    let creator_client = Client::new(&creator)?;
    let invitation = snap::ok(creator_client.session.provision(
        rocky_sdk::GrantProfile::Writer {
            scopes: ["items".to_owned()].into_iter().collect(),
        },
        None,
    ))
    .await["uri"]
        .as_str()
        .ok_or("invitation")?
        .to_owned();
    let member_directory = root.path().join("b");
    let Opened {
        database: member,
        network,
        ..
    } = Database::join(
        open(&member_directory, Some(Network::Owned(memory(&hub, "b")))),
        &invitation,
    )?;
    let syncing = network.ok_or("attached")?;
    let member_client = Arc::new(Client::new(&member)?);
    member_client.add(1, "Milk").await;

    hub.stalled.store(true, Ordering::SeqCst);
    let stuck = {
        let client = member_client.clone();
        tokio::spawn(async move { client.sync().await })
    };
    while dials(&hub) < 2 {
        tokio::task::yield_now().await;
    }
    let started = std::time::Instant::now();
    syncing.close().await;
    assert!(
        started.elapsed().as_secs() >= 4,
        "close waits out the drain bound before it cancels the stalled dial"
    );
    assert_eq!(
        stuck.await?.err().ok_or("cancelled")?["code"],
        "closed",
        "a dial that never completes holds the sync until the service closes"
    );
    assert_eq!(closed_transports(&hub), 1);

    member_client.add(2, "Eggs").await;
    hub.stalled.store(false, Ordering::SeqCst);
    member.attach(Network::Owned(memory(&hub, "b"))).await?;
    member_client.sync().await.map_err(|e| e.to_string())?;
    assert_eq!(
        creator_client.titles().await,
        ["Eggs", "Milk"],
        "the database stays intact and the interrupted sync is safe to repeat"
    );
    member.close().await?;
    creator.close().await?;
    assert_eq!(closed_transports(&hub), 3);
    Ok(())
}
