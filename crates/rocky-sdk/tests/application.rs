use rocky_sdk::{Affinity, Column, Configuration, Failure, Schema, Table, params};

fn schema() -> Result<Schema, Failure> {
    Schema::builder("rust_application", 1)
        .table(
            Table::replayed("notes")
                .column(Column::new("body", Affinity::Text).required())
                .column(Column::new("optional", Affinity::Text)),
        )
        .build()
}

#[tokio::test]
async fn transactions_observe_commits_and_reopen_without_json() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let config = Configuration::new(directory.path(), &schema()?)?;
    let db = config.create().await?.database;
    assert!(config.create().await.is_err());
    let events = db.observe_tables(Some(vec!["notes".into()])).await?;
    let mut tx = db.transaction().await?;
    assert!(tx.execute("COMMIT", params![]).await.is_err());
    let id = tx.next_id("notes", vec![]).await?;
    let body = "quoted ' text".to_owned();
    tx.execute(
        "INSERT INTO notes(id,body,optional) VALUES(?,?,?)",
        params![id, &body, None::<String>],
    )
    .await?;
    tx.commit().await?;
    assert_eq!(events.next().await?.tables, ["notes"]);
    let mut tx = db.transaction().await?;
    tx.execute("UPDATE notes SET body='rolled back'", params![])
        .await?;
    tx.rollback().await?;
    db.close().await?;
    let db = config.open().await?.database;
    let rows = db
        .query("SELECT id,body,optional FROM notes", params![])
        .await?
        .collect()
        .await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i64>("id")?, id);
    assert_eq!(rows[0].get::<String>("body")?, "quoted ' text");
    assert_eq!(rows[0].get::<Option<String>>("optional")?, None);
    assert!(rows[0].get::<String>("optional").is_err());
    assert!(rows[0].get::<i64>("body").is_err());
    assert!(rows[0].get::<i64>("missing").is_err());
    db.close().await
}

#[tokio::test]
async fn dropped_and_cancelled_transactions_release_the_writer() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let db = Configuration::new(directory.path(), &schema()?)?
        .create()
        .await?
        .database;
    let mut tx = db.transaction().await?;
    let id = tx.next_id("notes", vec![]).await?;
    tx.execute(
        "INSERT INTO notes(id,body) VALUES(?,?)",
        params![id, "discarded"],
    )
    .await?;
    drop(tx);
    let (written, ready) = tokio::sync::oneshot::channel();
    let task_db = db.clone();
    let task = tokio::spawn(async move {
        let mut tx = task_db.transaction().await?;
        let id = tx.next_id("notes", vec![]).await?;
        tx.execute(
            "INSERT INTO notes(id,body) VALUES(?,?)",
            params![id, "cancelled"],
        )
        .await?;
        written.send(()).map_err(|_| "receiver closed")?;
        std::future::pending::<()>().await;
        tx.commit().await?;
        Ok::<_, Failure>(())
    });
    ready.await?;
    task.abort();
    assert!(task.await.expect_err("cancelled").is_cancelled());
    // The next write deterministically waits for rollback, without a sleep.
    let mut tx = db.transaction().await?;
    let id = tx.next_id("notes", vec![]).await?;
    tx.execute(
        "INSERT INTO notes(id,body) VALUES(?,?)",
        params![id, "kept"],
    )
    .await?;
    tx.commit().await?;
    let rows = db
        .query("SELECT body FROM notes", params![])
        .await?
        .collect()
        .await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<String>("body")?, "kept");
    db.close().await
}

#[tokio::test]
async fn paged_reads_hold_a_snapshot_and_decode_aliases() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let schema = Schema::builder("paging", 1)
        .table(Table::replayed("cache").column(Column::new("n", Affinity::Integer).required()))
        .build()?;
    let db = Configuration::new(directory.path(), &schema)?
        .create()
        .await?
        .database;
    // Two transactions, each under the intent statement quota.
    for batch in [1..=150_i64, 151..=300] {
        let mut tx = db.transaction().await?;
        for n in batch {
            let id = tx.next_id("cache", params![]).await?;
            tx.execute("INSERT INTO cache(id,n) VALUES(?,?)", params![id, n])
                .await?;
        }
        tx.commit().await?;
    }
    let mut snapshot = db
        .query("SELECT n FROM cache ORDER BY n", params![])
        .await?;
    assert_eq!(
        snapshot
            .next()
            .await?
            .ok_or("missing first")?
            .get::<i64>("n")?,
        1
    );
    let mut tx = db.transaction().await?;
    tx.execute("DELETE FROM cache", params![]).await?;
    tx.commit().await?;
    let rows = snapshot.collect().await?;
    assert_eq!(rows.len(), 299);
    assert_eq!(rows[298].get_at::<i64>(0)?, 300);
    let mut rows = db
        .query(
            "SELECT 1 AS duplicate, 2 AS duplicate, NULL AS optional, 2 AS bad_boolean",
            params![],
        )
        .await?;
    let row = rows.next().await?.ok_or("missing row")?;
    assert!(row.get::<i64>("duplicate").is_err());
    assert_eq!(row.get_at::<i64>(1)?, 2);
    assert_eq!(row.get::<Option<i64>>("optional")?, None);
    assert!(row.get::<bool>("bad_boolean").is_err());
    drop(rows);
    db.close().await
}

#[test]
fn invalid_schema_is_refused_before_installation() {
    assert!(
        Schema::builder("bad", 0)
            .table(Table::replayed("notes").column(Column::new("body", Affinity::Text)))
            .build()
            .is_err()
    );
    assert!(Schema::parse(r#"{"format":0}"#).is_err());
}

#[tokio::test]
async fn configured_peers_enroll_and_sync_through_the_rust_api() -> Result<(), Failure> {
    use rocky_sdk::{GrantProfile, Network, transport::memory::Hub};
    use std::sync::Arc;
    let hub = Arc::new(Hub::default());
    let issuer_directory = tempfile::tempdir()?;
    let member_directory = tempfile::tempdir()?;
    let issuer = Configuration::new(issuer_directory.path(), &schema()?)?
        .network(Network::Owned(hub.connector("issuer")))
        .create()
        .await?
        .database;
    let session = issuer.session();
    let invitation = session
        .pairing(GrantProfile::Writer {
            scopes: ["notes".into()].into(),
        })
        .await?;
    assert!(invitation.expires_at_epoch_seconds > 0);
    let member_configuration = Configuration::new(member_directory.path(), &schema()?)?
        .network(Network::Owned(hub.connector("member")));
    let member = member_configuration.join(&invitation.uri).await?.database;
    let member_session = member.session();
    member_session.set_automatic_sync(false).await?;
    session.set_automatic_sync(false).await?;
    member_session.sync_once().await?;
    let mut tx = issuer.transaction().await?;
    let id = tx.next_id("notes", vec![]).await?;
    tx.execute(
        "INSERT INTO notes(id,body) VALUES(?,?)",
        params![id, "from issuer"],
    )
    .await?;
    tx.commit().await?;
    member_session.sync_once().await?;
    let rows = member
        .query("SELECT body FROM notes", params![])
        .await?
        .collect()
        .await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<String>("body")?, "from issuer");
    let contact = member_session.contact().await?;
    let peer = session.add_peer(&contact).await?;
    assert!(!peer.is_empty());
    member.close().await?;
    let reopened = member_configuration.open().await?.database;
    assert_eq!(
        reopened
            .query("SELECT body FROM notes", params![])
            .await?
            .collect()
            .await?
            .len(),
        1
    );
    reopened.close().await?;
    issuer.close().await
}

#[test]
fn configuration_without_a_tokio_runtime_returns_an_error() -> Result<(), Failure> {
    let configuration = Configuration::new("unused", &schema()?)?;
    let mut creating = std::pin::pin!(configuration.create());
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(matches!(
        std::future::Future::poll(creating.as_mut(), &mut context),
        std::task::Poll::Ready(Err(_))
    ));
    Ok(())
}

#[tokio::test]
async fn a_created_database_follows_a_bookmark_and_seals_routinely() -> Result<(), Failure> {
    use rocky_sdk::{Network, transport::memory::Hub};
    use std::sync::Arc;
    let directory = tempfile::tempdir()?;
    let hub = Arc::new(Hub::default());
    let configuration = Configuration::new(directory.path(), &schema()?)?
        .network(Network::Owned(hub.connector("owner")));
    let db = configuration.create().await?.database;
    let session = db.session();
    session.set_automatic_sync(false).await?;
    let status = session.tip_status().await?;
    assert_eq!(status.bookmark.as_deref(), Some(Configuration::BOOKMARK));
    let genesis = status.tip;
    // Enough replayed transactions for the owner's routine seal.
    for index in 0..128 {
        let mut tx = db.transaction().await?;
        let id = tx.next_id("notes", vec![]).await?;
        tx.execute(
            "INSERT INTO notes(id,body) VALUES(?,?)",
            params![id, format!("note {index}")],
        )
        .await?;
        tx.commit().await?;
    }
    session.sync_once().await?;
    let sealed = session.tip_status().await?;
    assert_eq!(sealed.bookmark.as_deref(), Some(Configuration::BOOKMARK));
    assert_ne!(
        sealed.tip, genesis,
        "the routine seal moved the bookmark on"
    );
    db.close().await?;
    let db = configuration.open().await?.database;
    assert_eq!(
        db.session().tip_status().await?.bookmark.as_deref(),
        Some(Configuration::BOOKMARK)
    );
    db.close().await
}

fn outbox_schema() -> Result<Schema, Failure> {
    Schema::builder("outbox_application", 1)
        .table(Table::replayed("orders").column(Column::new("item", Affinity::Text).required()))
        .table(
            Table::replayed("outbox")
                .column(Column::new("order_id", Affinity::Integer).required())
                .column(Column::new("txn", Affinity::Blob).required()),
        )
        .build()
}

#[tokio::test]
async fn the_token_is_known_inside_the_transaction_and_its_outcome_survives_restart()
-> Result<(), Failure> {
    use rocky_sdk::{TransactionOutcome, TransactionToken};
    let directory = tempfile::tempdir()?;
    let configuration = Configuration::new(directory.path(), &outbox_schema()?)?;
    let db = configuration.create().await?.database;
    let mut tx = db.transaction().await?;
    let token = tx.token();
    let id = tx.next_id("orders", vec![]).await?;
    tx.execute(
        "INSERT INTO orders(id,item) VALUES(?,?)",
        params![id, "book"],
    )
    .await?;
    let outbox = tx.next_id("outbox", vec![]).await?;
    tx.execute(
        "INSERT INTO outbox(id,order_id,txn) VALUES(?,?,?)",
        params![outbox, id, token],
    )
    .await?;
    assert_eq!(tx.commit().await?, Some(token));

    // A rolled-back transaction's token never gets an outcome.
    let mut tx = db.transaction().await?;
    let abandoned = tx.token();
    assert_ne!(abandoned, token);
    let id = tx.next_id("orders", vec![]).await?;
    tx.execute(
        "INSERT INTO orders(id,item) VALUES(?,?)",
        params![id, "pen"],
    )
    .await?;
    tx.rollback().await?;
    db.close().await?;

    let db = configuration.open().await?.database;
    let rows = db
        .query("SELECT txn FROM outbox", params![])
        .await?
        .collect()
        .await?;
    assert_eq!(rows.len(), 1);
    let stored = rows[0].get::<TransactionToken>("txn")?;
    assert_eq!(stored, token);
    assert_eq!(
        db.transaction_outcome(stored).await?,
        Some(TransactionOutcome::Applied)
    );
    assert_eq!(db.transaction_outcome(abandoned).await?, None);
    db.close().await
}

#[tokio::test]
async fn transactions_choose_durability_and_buffered_commits_can_be_flushed() -> Result<(), Failure>
{
    use rocky_sdk::{Durability, TransactionOutcome};
    let directory = tempfile::tempdir()?;
    let configuration = Configuration::new(directory.path(), &schema()?)?;
    let db = configuration.create().await?.database;
    let mut tokens = Vec::new();
    for durability in [Durability::Flushed, Durability::Buffered] {
        let mut tx = db.transaction_with(durability).await?;
        let id = tx.next_id("notes", vec![]).await?;
        tx.execute(
            "INSERT INTO notes(id,body) VALUES(?,?)",
            params![id, format!("{durability:?}")],
        )
        .await?;
        let token = tx.token();
        assert_eq!(tx.commit().await?, Some(token));
        tokens.push(token);
    }
    // Every commit that returned before the call, buffered ones included.
    db.flush().await?;
    db.close().await?;
    let db = configuration.open().await?.database;
    for token in tokens {
        assert_eq!(
            db.transaction_outcome(token).await?,
            Some(TransactionOutcome::Applied)
        );
    }
    assert!(db.flush().await.is_ok());
    db.close().await
}
