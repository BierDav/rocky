use crate::common::Failure;
use rocky_platform::fs::Scratch;
use rocky_sdk::{Affinity, Column, Configuration, Schema, Table, params};
use wasm_bindgen_test::wasm_bindgen_test;

fn schema() -> Result<Schema, Failure> {
    Schema::builder("rust_application", 1)
        .table(
            Table::replayed("notes")
                .column(Column::new("body", Affinity::Text).required())
                .column(Column::new("optional", Affinity::Text)),
        )
        .build()
        .map_err(Into::into)
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn transactions_observe_commits_and_reopen_without_json() -> Result<(), Failure> {
    let directory = Scratch::new().await;
    let config = Configuration::new(directory.path(), &schema()?)?;
    let db = config.create().await?.database;
    assert!(config.create().await.is_err());
    let mut events = db.observe(&["notes"]).await?;
    let id = db
        .write(async |tx| {
            assert!(tx.execute("COMMIT", params![]).await.is_err());
            let id = tx.next_id("notes").await?;
            let body = "quoted ' text".to_owned();
            tx.execute(
                "INSERT INTO notes(id,body,optional) VALUES(?,?,?)",
                params![id, &body, None::<String>],
            )
            .await?;
            Ok::<_, Failure>(id)
        })
        .await?;
    assert_eq!(events.next().await?.tables, ["notes"]);
    let rolled_back = db
        .write(async |tx| {
            tx.execute("UPDATE notes SET body='rolled back'", params![])
                .await?;
            Err::<(), Failure>("roll it back".into())
        })
        .await;
    assert!(rolled_back.is_err());
    db.close().await?;
    let db = config.open().await?;
    let rows = db
        .query("SELECT id,body,optional FROM notes", params![])
        .await?
        .collect()
        .await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<rocky_sdk::RowIdentity>("id")?, id);
    assert_eq!(rows[0].get::<String>("body")?, "quoted ' text");
    assert_eq!(rows[0].get::<Option<String>>("optional")?, None);
    assert!(rows[0].get::<String>("optional").is_err());
    assert!(rows[0].get::<i64>("body").is_err());
    assert!(rows[0].get::<i64>("missing").is_err());
    Ok(db.close().await?)
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn dropped_and_cancelled_transactions_release_the_writer() -> Result<(), Failure> {
    let directory = Scratch::new().await;
    let db = Configuration::new(directory.path(), &schema()?)?
        .create()
        .await?
        .database;
    let (inserted, discarding) = tokio::sync::oneshot::channel();
    let dropped = db.write(async |tx| {
        let id = tx.next_id("notes").await?;
        tx.execute(
            "INSERT INTO notes(id,body) VALUES(?,?)",
            params![id, "discarded"],
        )
        .await?;
        inserted.send(()).map_err(|_| "receiver closed")?;
        std::future::pending::<Result<(), Failure>>().await
    });
    tokio::select! {
        _ = dropped => return Err("the pending write finished".into()),
        ready = discarding => ready?,
    }
    let (written, ready) = tokio::sync::oneshot::channel();
    let task_db = db.clone();
    let task = rocky_platform::task::spawn(async move {
        task_db
            .write(async |tx| {
                let id = tx.next_id("notes").await?;
                tx.execute(
                    "INSERT INTO notes(id,body) VALUES(?,?)",
                    params![id, "cancelled"],
                )
                .await?;
                written.send(()).map_err(|_| "receiver closed")?;
                std::future::pending::<Result<(), Failure>>().await
            })
            .await
    });
    ready.await?;
    task.abort();
    assert!(task.await.expect_err("cancelled").is_cancelled());
    db.write(async |tx| {
        let id = tx.next_id("notes").await?;
        tx.execute(
            "INSERT INTO notes(id,body) VALUES(?,?)",
            params![id, "kept"],
        )
        .await
    })
    .await?;
    let rows = db
        .query("SELECT body FROM notes", params![])
        .await?
        .collect()
        .await?;
    assert_eq!(
        rows.len(),
        1,
        "the next write waits for the cancelled transaction's rollback"
    );
    assert_eq!(rows[0].get::<String>("body")?, "kept");
    Ok(db.close().await?)
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn paged_reads_hold_a_snapshot_and_decode_aliases() -> Result<(), Failure> {
    let directory = Scratch::new().await;
    let schema = Schema::builder("paging", 1)
        .table(Table::replayed("cache").column(Column::new("n", Affinity::Integer).required()))
        .build()?;
    let db = Configuration::new(directory.path(), &schema)?
        .create()
        .await?
        .database;
    let batches_under_intent_statement_quota = [1..=150_i64, 151..=300];
    for batch in batches_under_intent_statement_quota {
        db.write(async |tx| {
            for n in batch {
                let id = tx.next_id("cache").await?;
                tx.execute("INSERT INTO cache(id,n) VALUES(?,?)", params![id, n])
                    .await?;
            }
            Ok::<_, Failure>(())
        })
        .await?;
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
    db.write(async |tx| tx.execute("DELETE FROM cache", params![]).await)
        .await?;
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
    Ok(db.close().await?)
}

#[wasm_bindgen_test(unsupported = test)]
fn invalid_schema_is_refused_before_installation() {
    assert!(
        Schema::builder("bad", 0)
            .table(Table::replayed("notes").column(Column::new("body", Affinity::Text)))
            .build()
            .is_err()
    );
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn configured_peers_enroll_and_sync_through_the_rust_api() -> Result<(), Failure> {
    use rocky_sdk::{Network, transport::memory::Hub};
    use std::sync::Arc;
    let hub = Arc::new(Hub::default());
    let issuer_directory = Scratch::new().await;
    let member_directory = Scratch::new().await;
    let issuer = Configuration::new(issuer_directory.path(), &schema()?)?
        .network(Network::Owned(hub.connector("issuer")))
        .create()
        .await?
        .database;
    let invitation = issuer
        .invite(rocky_sdk::Invite::new(rocky_sdk::Access::Full))
        .await?;
    assert!(invitation.code.to_string().starts_with("rocky://invite/"));
    let member_configuration = Configuration::new(member_directory.path(), &schema()?)?
        .network(Network::Owned(hub.connector("member")))
        .identity(Arc::new(rocky_sdk::IdentityKey::generate()));
    let member = member_configuration.join(&invitation.code).await?;
    member.set_sync(rocky_sdk::SyncMode::Manual).await?;
    issuer.set_sync(rocky_sdk::SyncMode::Manual).await?;
    member.sync_now().await?;
    issuer
        .write(async |tx| {
            let id = tx.next_id("notes").await?;
            tx.execute(
                "INSERT INTO notes(id,body) VALUES(?,?)",
                params![id, "from issuer"],
            )
            .await
        })
        .await?;
    member.sync_now().await?;
    let rows = member
        .query("SELECT body FROM notes", params![])
        .await?
        .collect()
        .await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<String>("body")?, "from issuer");
    let contact = member.contact().await?.uri;
    let peer = issuer.add_peer(&contact).await?;
    assert_eq!(peer, member.me().await?.device);
    member.close().await?;
    let reopened = member_configuration.open().await?;
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
    Ok(issuer.close().await?)
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn a_created_database_follows_main_and_seals_routinely() -> Result<(), Failure> {
    use rocky_sdk::{Network, transport::memory::Hub};
    use std::sync::Arc;
    let directory = Scratch::new().await;
    let hub = Arc::new(Hub::default());
    let configuration = Configuration::new(directory.path(), &schema()?)?
        .network(Network::Owned(hub.connector("owner")));
    let db = configuration.create().await?.database;
    db.set_sync(rocky_sdk::SyncMode::Manual).await?;
    let status = db.tip_status().await?;
    assert_eq!(status.bookmark.as_deref(), Some("main"));
    let genesis = status.tip;
    let transactions_for_routine_seal = 128;
    for index in 0..transactions_for_routine_seal {
        db.write(async |tx| {
            let id = tx.next_id("notes").await?;
            tx.execute(
                "INSERT INTO notes(id,body) VALUES(?,?)",
                params![id, format!("note {index}")],
            )
            .await
        })
        .await?;
    }
    assert!(
        db.sync_now().await.err().is_some_and(|error| matches!(
            error.kind(),
            rocky_sdk::ErrorKind::Service(rocky_sdk::ServiceError::NoPeerReached(report))
                if report.peers().is_empty()
        )),
        "a pass that knows no peer still seals, and says it reconciled with none"
    );
    let sealed = db.tip_status().await?;
    assert_eq!(sealed.bookmark.as_deref(), Some("main"));
    assert_ne!(
        sealed.tip, genesis,
        "the routine seal moved the bookmark on"
    );
    db.close().await?;
    let db = configuration.open().await?;
    assert_eq!(db.tip_status().await?.bookmark.as_deref(), Some("main"));
    Ok(db.close().await?)
}

fn outbox_schema() -> Result<Schema, Failure> {
    Schema::builder("outbox_application", 1)
        .table(Table::replayed("orders").column(Column::new("item", Affinity::Text).required()))
        .table(
            Table::replayed("outbox")
                .column(Column::new("order_id", Affinity::Blob).required())
                .column(Column::new("txn", Affinity::Blob).required()),
        )
        .build()
        .map_err(Into::into)
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn the_token_is_known_inside_the_transaction_and_its_outcome_survives_restart()
-> Result<(), Failure> {
    use rocky_sdk::{TransactionOutcome, TransactionToken};
    let directory = Scratch::new().await;
    let configuration = Configuration::new(directory.path(), &outbox_schema()?)?;
    let db = configuration.create().await?.database;
    let token = db
        .write(async |tx| {
            let token = tx.token();
            let id = tx.next_id("orders").await?;
            tx.execute(
                "INSERT INTO orders(id,item) VALUES(?,?)",
                params![id, "book"],
            )
            .await?;
            let outbox = tx.next_id("outbox").await?;
            tx.execute(
                "INSERT INTO outbox(id,order_id,txn) VALUES(?,?,?)",
                params![outbox, id, token],
            )
            .await?;
            Ok::<_, Failure>(token)
        })
        .await?;

    let mut abandoned = None;
    let rolled_back = db
        .write(async |tx| {
            abandoned = Some(tx.token());
            let id = tx.next_id("orders").await?;
            tx.execute(
                "INSERT INTO orders(id,item) VALUES(?,?)",
                params![id, "pen"],
            )
            .await?;
            Err::<(), Failure>("abandon the order".into())
        })
        .await;
    assert!(rolled_back.is_err());
    let abandoned = abandoned.ok_or("the abandoned write never ran")?;
    assert_ne!(abandoned, token);
    db.close().await?;

    let db = configuration.open().await?;
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
        Some(TransactionOutcome::Applied { sealed: false })
    );
    assert_eq!(
        db.transaction_outcome(abandoned).await?,
        None,
        "a rolled-back transaction's token never gets an outcome"
    );
    Ok(db.close().await?)
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn flush_persists_every_commit_returned_before_it() -> Result<(), Failure> {
    use rocky_sdk::TransactionOutcome;
    let directory = Scratch::new().await;
    let configuration = Configuration::new(directory.path(), &schema()?)?;
    let db = configuration.create().await?.database;
    let mut tokens = Vec::new();
    for body in ["first", "second"] {
        let token = db
            .write(async |tx| {
                let id = tx.next_id("notes").await?;
                tx.execute("INSERT INTO notes(id,body) VALUES(?,?)", params![id, body])
                    .await?;
                Ok::<_, Failure>(tx.token())
            })
            .await?;
        tokens.push(token);
    }
    db.flush().await?;
    db.close().await?;
    let db = configuration.open().await?;
    for token in tokens {
        assert_eq!(
            db.transaction_outcome(token).await?,
            Some(TransactionOutcome::Applied { sealed: false }),
            "flush persists every commit returned before it"
        );
    }
    assert!(db.flush().await.is_ok());
    Ok(db.close().await?)
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn the_crate_quickstart_writes_reads_and_closes_a_note() -> Result<(), Failure> {
    let directory = Scratch::new().await;
    let schema = Schema::builder("notes", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()?;
    let configuration = Configuration::new(directory.path(), &schema)?;
    let database = configuration.create().await?.database;
    let id = database
        .write(async |tx| {
            let id = tx.next_id("notes").await?;
            tx.execute(
                "INSERT INTO notes(id,body) VALUES(?,?)",
                params![id, "Hello Rocky"],
            )
            .await?;
            Ok::<_, Failure>(id)
        })
        .await?;
    let mut rows = database
        .query("SELECT body FROM notes WHERE id=?", params![id])
        .await?;
    let row = rows.next().await?.ok_or("the note was written")?;
    assert_eq!(row.get::<String>("body")?, "Hello Rocky");
    drop(rows);
    Ok(database.close().await?)
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn invite_refuses_a_label_over_256_bytes() -> Result<(), Failure> {
    use rocky_sdk::{Network, transport::memory::Hub};
    use std::sync::Arc;
    let hub = Arc::new(Hub::default());
    let issuer_directory = Scratch::new().await;
    let member_directory = Scratch::new().await;
    let issuer = Configuration::new(issuer_directory.path(), &schema()?)?
        .network(Network::Owned(hub.connector("issuer")))
        .create()
        .await?
        .database;
    let profile = rocky_sdk::Access::Full;
    let refused = issuer
        .invite(rocky_sdk::Invite::new(profile.clone()).label("x".repeat(257)))
        .await
        .err()
        .ok_or("a 257-byte device label is refused")?;
    assert!(refused.to_string().contains("256 bytes"), "{refused}");
    let invitation = issuer
        .invite(rocky_sdk::Invite::new(profile).label("x".repeat(256)))
        .await?;
    let joined = Configuration::new(member_directory.path(), &schema()?)?
        .network(Network::Owned(hub.connector("member")))
        .fixture_person()
        .join(&invitation.code)
        .await?;
    joined.close().await?;
    Ok(issuer.close().await?)
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn trusted_staleness_is_set_between_a_minute_and_ten_years() -> Result<(), Failure> {
    let directory = Scratch::new().await;
    let configuration = Configuration::new(directory.path(), &schema()?)?;
    let ten_years = 10 * 366 * 24 * 60 * 60;
    for refused in [0, 59, ten_years + 1] {
        assert!(
            configuration
                .clone()
                .admin_stale_after(std::time::Duration::from_secs(refused))
                .is_err(),
            "a staleness threshold of {refused} s is refused"
        );
    }
    configuration
        .clone()
        .admin_stale_after(std::time::Duration::from_secs(60))?;
    let database = configuration
        .admin_stale_after(std::time::Duration::from_secs(ten_years))?
        .create()
        .await?
        .database;
    Ok(database.close().await?)
}
