use rocky_credentials::testing::MemoryKeys;
mod bootstrap_intent;
mod state_advice;
mod status;
use super::*;
use crate::{
    FileSecretStore,
    schema::Schema,
    transport::{Connector, memory::Hub},
};
use serde_json::{Value, json};

const SCHEMA: &str = r#"{"application":"sync","version":1,"format":10,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

async fn link_sessions(
    dialer: &Node,
    acceptor: &Node,
) -> Result<
    (
        rocky_secure::network::NativeSession,
        rocky_secure::network::NativeSession,
    ),
    Failure,
> {
    let (dialed, accepted) = opened_links(dialer, acceptor).await?;
    let handle = dialed.channels.handle.clone();
    let (served, mut incoming) = tokio::sync::mpsc::channel(1);
    tokio::task::spawn_local(dialed.drive(tokio::sync::mpsc::channel(1).0));
    tokio::task::spawn_local(accepted.drive(served));
    let session = handle.open().await?;
    let answering = incoming.recv().await.ok_or("link ended")?;
    Ok((session, answering))
}

async fn opened_links(
    dialer: &Node,
    acceptor: &Node,
) -> Result<
    (
        rocky_secure::network::link::Link,
        rocky_secure::network::link::Link,
    ),
    Failure,
> {
    use rocky_secure::network::link::Link;
    let (near, far) = tokio::io::duplex(64 * 1024);
    let accepting = PrivateKey::from_bytes(&acceptor.vault.get("node")?);
    let expected = accepting.public_key();
    let (near_credentials, far_credentials) = (
        dialer.settings.lock().await.credentials.clone(),
        acceptor.settings.lock().await.credentials.clone(),
    );
    let (dialed, accepted) = tokio::join!(
        Link::open(
            near,
            &dialer.replica,
            PrivateKey::from_bytes(&dialer.vault.get("node")?),
            near_credentials,
            Some(expected),
        ),
        Link::open(far, &acceptor.replica, accepting, far_credentials, None),
    );
    let (mut dialed, mut accepted) = (dialed?, accepted?);
    let (near, far) = tokio::join!(dialed.ready(false), accepted.ready(false));
    near?;
    far?;
    Ok((dialed, accepted))
}

async fn node(
    root: &std::path::Path,
    settings: Settings,
    vault: Vault,
    transport: impl Into<crate::transport::Connected>,
) -> Result<Arc<Node>, Failure> {
    let leases_held_until_a_test_advances_them = rocky_secure::ManualLeaseClock::default();
    node_on(
        root,
        settings,
        vault,
        transport,
        &leases_held_until_a_test_advances_them,
    )
    .await
}
async fn node_on(
    root: &std::path::Path,
    settings: Settings,
    vault: Vault,
    transport: impl Into<crate::transport::Connected>,
    lease_clock: &rocky_secure::ManualLeaseClock,
) -> Result<Arc<Node>, Failure> {
    let domain = settings.domain()?;
    let replica = EncryptedReplica::open(
        root.join("rocky.sqlite"),
        domain.app.clone(),
        settings
            .open_config(&vault, &domain)?
            .with_manual_lease_clock(lease_clock),
    )
    .await?;
    replica
        .register_credential_signer(PrivateKey::from_bytes(&vault.get("node")?))
        .await?;
    replica
        .install_readable_tables(domain.logical().clone(), domain.scope, domain.readers())
        .await?;
    let node = Arc::new(Node::new(
        replica,
        domain,
        vault,
        settings,
        root.join("rocky.sqlite"),
        BootstrapIntentFile::in_directory(root),
    ));
    node.link(transport, true).await?;
    Ok(node)
}
async fn enrolled(
    root: &std::path::Path,
    hub: &Arc<Hub>,
    name: &str,
    issuer: Option<(&Arc<Node>, crate::Access)>,
) -> Result<Arc<Node>, Failure> {
    enrolled_schema(root, hub, name, issuer, &Schema::parse(SCHEMA)?).await
}
async fn enrolled_schema(
    root: &std::path::Path,
    hub: &Arc<Hub>,
    name: &str,
    issuer: Option<(&Arc<Node>, crate::Access)>,
    schema: &Schema,
) -> Result<Arc<Node>, Failure> {
    std::fs::create_dir_all(root)?;
    let store: Arc<dyn crate::SecretStore> = Arc::new(MemoryKeys::default());
    let path = root.join("rocky.sqlite");
    if let Some((issuer, profile)) = issuer {
        let invitation = issuer.invite(&crate::Invite::new(profile)).await?;
        let prepared = Settings::prepare_invited(&path, &store, None, &invitation.code)?;
        let vault = prepared.vault.clone();
        let transport = hub
            .connector(name)
            .start(false, Settings::route_secret(&vault)?)
            .await?;
        let settings = Settings::enroll_invited(
            (&path, Some(schema)),
            (prepared.intent, prepared.vault),
            prepared.provider,
            &invitation.code,
            &*transport,
        )
        .await?;
        node(root, settings, vault, transport).await
    } else {
        let settings = Settings::create_owned(schema, &store, crate::enrollment::DEFAULT_TTL)?;
        settings.save(&path).await?;
        let vault = settings.vault(&store);
        let transport = hub
            .connector(name)
            .start(true, Settings::route_secret(&vault)?)
            .await?;
        node(root, settings, vault, transport).await
    }
}
async fn enable(node: &Node) -> Result<(), Failure> {
    let mut statuses = node.sync_status.subscribe();
    let before = statuses.borrow_and_update().token;
    node.automatic.store(true, Ordering::SeqCst);
    node.wake_sync();
    statuses
        .wait_for(|status| status.token != before && status.state == SyncState::Connected)
        .await?;
    assert_eq!(statuses.borrow().state, SyncState::Connected);
    Ok(())
}

async fn read_rows(node: &Node, sql: &str) -> Result<Vec<Vec<SqlValue>>, Failure> {
    let session = node.sql_session(SqlOptions::default()).await?;
    Ok(session.run(sql, Vec::new()).await?.rows)
}

async fn add(node: &Node, handle: u8) -> Result<(), Failure> {
    let sql = node.sql_session(SqlOptions::default()).await?;
    sql.run("BEGIN", Vec::new()).await?;
    sql.run(
        "INSERT INTO items(key,title) VALUES(?,'Milk')",
        vec![SqlValue::Text(handle.to_string())],
    )
    .await?;
    sql.commit(commitment("rocky.sdk.mutation.v1", &[handle; 32]))
        .await?;
    Ok(())
}

#[tokio::test]
async fn idle_has_no_poll_and_a_commit_reaches_the_peer_without_time_advancing()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((&a, crate::Access::Full)),
            )
            .await?;
            let contact = {
                let link = b.network.read().await;
                b.settings
                    .lock()
                    .await
                    .contact(&b.vault, &*link.as_ref().ok_or("link")?.transport)
                    .await?
                    .0
            };
            a.configure(|settings| settings.add_peer(&a.vault, &contact))
                .await?;
            enable(&a).await?;
            enable(&b).await?;
            assert!(!a.sync_status.borrow().retry);
            assert!(!b.sync_status.borrow().retry);
            while !a.current().is_some_and(|service| service.live.len() == 1)
                || !b.current().is_some_and(|service| service.live.len() == 1)
                || *a.worker_running.borrow()
                || *b.worker_running.borrow()
            {
                tokio::task::yield_now().await;
            }
            let dials = hub.counts()[1];
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(86400)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                hub.counts()[1],
                dials,
                "successful idle service must not dial on a timer"
            );
            tokio::time::resume();
            let mut revisions = b.replica.observe_revision();
            add(&a, 1).await?;
            tokio::time::timeout(Duration::from_secs(5), revisions.changed()).await??;
            let rows = read_rows(&b, "SELECT title FROM items").await?;
            assert_eq!(rows[0][0], SqlValue::Text("Milk".into()));
            a.detach().await;
            b.detach().await;
            Ok(())
        })
        .await
}

#[tokio::test]
async fn a_short_link_stays_open_until_its_last_request_ends() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let profile = crate::Access::Full;
            let b = enrolled(&root.path().join("b"), &hub, "b", Some((&a, profile))).await?;
            let contact = {
                let link = b.network.read().await;
                let transport = &*link.as_ref().ok_or("link")?.transport;
                b.settings
                    .lock()
                    .await
                    .contact(&b.vault, transport)
                    .await?
                    .0
            };
            a.configure(|settings| settings.add_peer(&a.vault, &contact))
                .await?;
            let (peer, contact) = a
                .settings
                .lock()
                .await
                .targets()
                .into_iter()
                .next()
                .ok_or("peer")?;
            let service = a.current().ok_or("service")?;
            a.request_link(&*service.transport, peer, &contact).await?;
            let (first, second) = (service.live.short(peer), service.live.short(peer));
            drop(first);
            assert!(service.live.handle(peer).is_some());
            drop(second);
            assert!(service.live.handle(peer).is_none());
            a.detach().await;
            b.detach().await;
            Ok(())
        })
        .await
}

#[tokio::test]
async fn incoming_keyless_custody_wakes_forwarding_and_connectivity_wakes_catch_up()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let relay = enrolled(
                &root.path().join("relay"),
                &hub,
                "relay",
                Some((&a, crate::Access::Store)),
            )
            .await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((&a, crate::Access::Full)),
            )
            .await?;
            let contact = {
                let link = b.network.read().await;
                b.settings
                    .lock()
                    .await
                    .contact(&b.vault, &*link.as_ref().ok_or("link")?.transport)
                    .await?
                    .0
            };
            relay
                .configure(|settings| settings.add_peer(&relay.vault, &contact))
                .await?;
            let contact = {
                let link = relay.network.read().await;
                relay
                    .settings
                    .lock()
                    .await
                    .contact(&relay.vault, &*link.as_ref().ok_or("link")?.transport)
                    .await?
                    .0
            };
            b.configure(|settings| settings.add_peer(&b.vault, &contact))
                .await?;
            enable(&relay).await?;
            let mut revisions = b.replica.observe_revision();
            add(&a, 1).await?;
            let contact = {
                let settings = relay.settings.lock().await;
                let link = relay.network.read().await;
                settings
                    .contact(&relay.vault, &*link.as_ref().ok_or("link")?.transport)
                    .await?
                    .0
            };
            a.configure(|settings| settings.add_peer(&a.vault, &contact))
                .await?;
            let transport = a
                .network
                .read()
                .await
                .as_ref()
                .ok_or("link")?
                .transport
                .clone();
            a.sync(&*transport).await?;
            tokio::time::timeout(Duration::from_secs(5), revisions.changed()).await??;
            assert_eq!(
                *relay.replica.observe_revision().borrow(),
                0,
                "custody cannot apply rows"
            );
            let mut statuses = relay.sync_status.subscribe();
            let before = statuses.borrow_and_update().token;
            hub.network_changed();
            tokio::time::timeout(
                Duration::from_secs(5),
                statuses.wait_for(|status| {
                    status.token != before && status.state == SyncState::Connected
                }),
            )
            .await??;
            assert!(
                !statuses.borrow().retry,
                "keyless custody owes no key retry"
            );
            for node in [&a, &relay, &b] {
                node.detach().await;
            }
            Ok(())
        })
        .await
}

#[tokio::test]
async fn failed_work_retries_on_clock_and_connectivity_interrupts_backoff() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((&a, crate::Access::Full)),
            )
            .await?;
            let contact = {
                let link = b.network.read().await;
                b.settings
                    .lock()
                    .await
                    .contact(&b.vault, &*link.as_ref().ok_or("link")?.transport)
                    .await?
                    .0
            };
            a.configure(|settings| settings.add_peer(&a.vault, &contact))
                .await?;
            add(&a, 2).await?;
            b.detach().await;
            let mut status = a.sync_status.subscribe();
            a.automatic.store(true, Ordering::SeqCst);
            a.wake_sync();
            status.wait_for(|status| status.retry).await?;
            assert!(status.borrow().retry);
            let dials = hub.counts()[1];
            tokio::time::pause();
            tokio::time::advance(Duration::from_millis(500)).await;
            tokio::task::yield_now().await;
            assert_eq!(hub.counts()[1], dials);
            tokio::time::advance(Duration::from_millis(500)).await;
            status.changed().await?;
            assert!(hub.counts()[1] > dials);
            tokio::time::resume();
            b.attach(Network::Owned(hub.connector("b"))).await?;
            let before = status.borrow_and_update().token;
            hub.network_changed();
            tokio::time::timeout(
                Duration::from_secs(5),
                status.wait_for(|status| {
                    status.token != before && status.state == SyncState::Connected
                }),
            )
            .await??;
            assert_eq!(status.borrow().state, SyncState::Connected);
            assert!(!status.borrow().retry);
            a.detach().await;
            b.detach().await;
            Ok(())
        })
        .await
}

#[tokio::test]
async fn consumer_sql_allocates_text_named_ids_of_its_writer() -> Result<(), Failure> {
    tokio::task::LocalSet::new().run_until(async {
    let root = tempfile::tempdir()?;
    let schema = Schema::parse(
        r#"{"format":10,"application":"numeric_consumer","version":1,"tables":[{"name":"items","class":"replayed","columns":[{"name":"title","type":"text","not_null":true}],"primary_key":["id"]}]}"#,
    )?;
    let store: Arc<dyn crate::SecretStore> =
        Arc::new(FileSecretStore::new(root.path().join("secrets")));
    let settings = Settings::create_owned(&schema, &store, crate::enrollment::DEFAULT_TTL)?;
    let vault = settings.vault(&store);
    let hub = Arc::new(Hub::default());
    let transport = hub
        .connector("numeric")
        .start(true, Settings::route_secret(&vault)?)
        .await?;
    let node = node(root.path(), settings, vault, transport).await?;
    let sql = node.sql_session(SqlOptions::default()).await?;
    sql.run("BEGIN", Vec::new()).await?;
    let id = sql.next_id("items").await?;
    let rows = sql
        .run(
            "INSERT INTO items(title,id) VALUES('first',?) RETURNING id",
            vec![id.sql()],
        )
        .await?;
    assert_eq!(rows.rows[0][0], id.sql());
    sql.run("COMMIT", Vec::new()).await?;
    let rows = read_rows(&node, "SELECT id FROM items").await?;
    assert_eq!(rows[0][0], id.sql());
    node.detach().await;
    Ok(())
    }).await
}

mod admission;
mod change_driven;
mod discovery;
mod mesh;

#[tokio::test]
async fn a_connection_write_commits_while_a_renewal_waits_for_the_replica() -> Result<(), Failure> {
    use crate::connection::{Call, ConnectionOptions, Registry, Reply, serve};
    tokio::task::LocalSet::new().run_until(async {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let node = enrolled(&root.path().join("a"), &hub, "a", None).await?;

        let connections = Registry::default();
        let options = ConnectionOptions {
            read_only: false,
            transaction_timeout: Some(Duration::from_secs(1)),
        };
        let writer = connections.get(connections.connect(node.clone(), options).await?)?;
        let execute = |sql: &str, params: Vec<SqlValue>| {
            let call = Call::Execute { statement: 1, sql: sql.into(), params };
            serve(writer.clone(), call)
        };
        execute("BEGIN IMMEDIATE TRANSACTION", Vec::new()).await?;
        let Reply::Page(_) = execute("SELECT count(*) FROM items", Vec::new()).await? else {
            return Err("no count".into());
        };
        let renewal = node.renew(None, false, None, false);
        tokio::pin!(renewal);
        tokio::select! {
            biased;
            renewed = &mut renewal => return Err(format!("renewal did not wait: {renewed:?}").into()),
            () = std::future::ready(()) => {}
        }
        assert!(node.settings.try_lock().is_err(), "the renewal holds the settings");
        let write = async {
            execute("INSERT INTO items(key,title) VALUES('k','Milk')", Vec::new()).await?;
            execute("COMMIT", Vec::new()).await
        };
        let (renewed, written) = tokio::join!(renewal, write);
        written?;
        renewed?;
        node.detach().await;
        Ok(())
    }).await
}

#[tokio::test]
async fn the_write_lease_times_out_and_aborts_until_acknowledged() -> Result<(), Failure> {
    use crate::connection::{Call, ConnectionOptions, Registry, Reply, serve};
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let store: Arc<dyn crate::SecretStore> = Arc::new(MemoryKeys::default());
            let settings = Settings::create_owned(
                &Schema::parse(SCHEMA)?,
                &store,
                crate::enrollment::DEFAULT_TTL,
            )?;
            let vault = settings.vault(&store);
            let transport = Arc::new(Hub::default())
                .connector("a")
                .start(true, Settings::route_secret(&vault)?)
                .await?;
            let clock = rocky_secure::ManualLeaseClock::default();
            let node = node_on(root.path(), settings, vault, transport, &clock).await?;
            let timeout = Duration::from_millis(200);
            let connections = Registry::default();
            let writer = connections.get(
                connections
                    .connect(
                        node.clone(),
                        ConnectionOptions {
                            read_only: false,
                            transaction_timeout: Some(timeout),
                        },
                    )
                    .await?,
            )?;
            let execute_with = |sql: &str, params: Vec<SqlValue>| {
                let call = Call::Execute {
                    statement: 1,
                    sql: sql.into(),
                    params,
                };
                serve(writer.clone(), call)
            };
            let execute = |sql: &str| execute_with(sql, Vec::new());
            let in_transaction = || async {
                match serve(writer.clone(), Call::InTransaction).await? {
                    Reply::Flag(flag) => Ok::<_, Failure>(flag),
                    _ => Err("in-transaction answers with a flag".into()),
                }
            };
            let count = || async {
                match execute("SELECT count(*) FROM items").await? {
                    Reply::Page(page) => Ok::<_, Failure>(page.rows[0][0].clone()),
                    _ => Err("a count answers with a page".into()),
                }
            };
            execute("BEGIN IMMEDIATE TRANSACTION").await?;
            let Reply::Page(_) = execute("SELECT count(*) FROM items").await? else {
                return Err("no count".into());
            };
            execute_with(
                "INSERT INTO items(key,title) VALUES('k','held')",
                Vec::new(),
            )
            .await?;
            clock.advance(timeout - Duration::from_millis(1));
            assert_eq!(
                count().await?,
                SqlValue::Integer(1),
                "the lease holds until its clock reaches the timeout"
            );
            clock.advance(Duration::from_millis(1));
            let Err(error) = execute("SELECT count(*) FROM items").await else {
                return Err("a statement after the lease clock passed the timeout ran".into());
            };
            assert_eq!(error.kind(), &crate::ErrorKind::Busy, "{error}");
            assert!(
                error.to_string().contains("Timeout"),
                "the lease ends on its own: {error}"
            );
            assert!(
                in_transaction().await?,
                "a timed-out lease stays a transaction for the driver until it rolls back"
            );
            execute("ROLLBACK TRANSACTION").await?;
            assert!(!in_transaction().await?);
            assert_eq!(count().await?, SqlValue::Integer(0));
            node.detach().await;
            Ok(())
        })
        .await
}

#[tokio::test]
async fn a_read_preempted_before_its_first_page_reopens_but_a_read_transaction_does_not()
-> Result<(), Failure> {
    use crate::connection::{
        Call, ConnectionOptions, REOPENS_OF_A_PREEMPTED_READ, Registry, Reply, serve,
    };
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let node = enrolled(root.path(), &Arc::new(Hub::default()), "a", None).await?;
            add(&node, 1).await?;
            let connections = Registry::default();
            let query =
                || connections.query_once(node.clone(), "SELECT key FROM items".into(), Vec::new());
            let busy = |error: Failure| {
                let error = crate::Error::from_failure(&error);
                assert_eq!(error.kind(), &crate::ErrorKind::Busy, "{error}");
                assert!(error.retryable(), "{error}");
                assert!(error.to_string().contains("Preempted"), "{error}");
            };

            node.replica
                .preempt_reader_opens(REOPENS_OF_A_PREEMPTED_READ)
                .await;
            let (page, _) = query().await?;
            assert_eq!(
                page.rows,
                vec![vec![SqlValue::Text("1".into())]],
                "a read that delivered no row yet reopens on a fresh snapshot"
            );

            node.replica
                .preempt_reader_opens(REOPENS_OF_A_PREEMPTED_READ + 1)
                .await;
            busy(
                query()
                    .await
                    .err()
                    .ok_or("a read preempted on every reopen succeeded")?,
            );
            query().await?;

            let reader = connections.get(
                connections
                    .connect(
                        node.clone(),
                        ConnectionOptions {
                            read_only: true,
                            transaction_timeout: None,
                        },
                    )
                    .await?,
            )?;
            let execute = |sql: &str| {
                serve(
                    reader.clone(),
                    Call::Execute {
                        statement: 1,
                        sql: sql.into(),
                        params: Vec::new(),
                    },
                )
            };
            execute("BEGIN").await?;
            node.replica.preempt_reader_opens(1).await;
            let Err(error) = execute("SELECT key FROM items").await else {
                return Err("a read transaction replaced its preempted snapshot".into());
            };
            assert_eq!(error.kind(), &crate::ErrorKind::Busy, "{error}");
            assert!(error.retryable(), "{error}");
            let Reply::Page(_) = execute("ROLLBACK").await? else {
                return Err("a rollback answers with a page".into());
            };
            node.detach().await;
            Ok(())
        })
        .await
}

#[tokio::test]
async fn a_peer_importing_a_scope_is_not_fed_its_history() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((&a, crate::Access::Full)),
            )
            .await?;
            add(&a, 1).await?;
            let scopes = a.app_scopes();
            b.replica
                .set_importing(scopes.iter().copied().collect())
                .await;
            let (mut session, mut answering) = link_sessions(&a, &b).await?;
            let server = b.clone();
            let serving = async move {
                let deliveries = delivery::Port {
                    node: &server,
                    serving: true,
                };
                answering
                    .serve_all(&server.replica, &deliveries, None)
                    .await
            };
            let exchanging = async {
                let exchange = session.changed_scopes(&a.replica, &scopes).await?;
                session.close().await?;
                Ok::<_, Failure>(exchange)
            };
            let (served, exchange) = tokio::join!(serving, exchanging);
            served?;
            let exchange = exchange?;
            assert_eq!(exchange.importing, scopes);
            assert!(exchange.changed.is_empty());
            b.replica
                .set_importing(std::collections::BTreeSet::new())
                .await;
            let (mut session, mut answering) = link_sessions(&a, &b).await?;
            let server = b.clone();
            let serving = async move {
                let deliveries = delivery::Port {
                    node: &server,
                    serving: true,
                };
                answering
                    .serve_all(&server.replica, &deliveries, None)
                    .await
            };
            let exchanging = async {
                let exchange = session.changed_scopes(&a.replica, &scopes).await?;
                session.close().await?;
                Ok::<_, Failure>(exchange)
            };
            let (served, exchange) = tokio::join!(serving, exchanging);
            served?;
            assert_eq!(exchange?.changed, scopes);
            a.detach().await;
            b.detach().await;
            Ok(())
        })
        .await
}

#[tokio::test]
async fn an_issuer_whose_registry_waits_on_a_key_still_admits_the_link_that_brings_it()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let issuer = enrolled(&root.path().join("issuer"), &hub, "issuer", None).await?;
            let member = enrolled(
                &root.path().join("member"),
                &hub,
                "member",
                Some((&issuer, crate::Access::Full)),
            )
            .await?;
            assert!(
                !issuer.settings.lock().await.renews_from_issuer(),
                "the only issuer rotates on its own, so it reads its registry at every link"
            );
            issuer.registry_awaits_key.store(true, Ordering::SeqCst);
            let report = member.sync_once().await?;
            assert_eq!(
                report.reconciled(),
                1,
                "the issuer admits the member while its registry waits on a key: {report}"
            );
            issuer.registry_awaits_key.store(false, Ordering::SeqCst);
            issuer.detach().await;
            member.detach().await;
            Ok(())
        })
        .await
}

fn completed_peers(report: &Value) -> usize {
    serde_json::from_value::<crate::SyncReport>(report.clone())
        .expect("a native sync report")
        .reconciled()
}

pub(super) fn small_directory() -> std::io::Result<crate::test_directory::Directory> {
    crate::test_directory::small()
}
