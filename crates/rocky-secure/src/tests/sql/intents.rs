//! Writes to replayed tables commit as intent frames.
use super::*;

/// `salaries` replicates as intents; `items` stays mergeable.
fn replayed() -> Typed {
    let mut schema = Typed::new(UpdatePolicy::Changed).schema;
    let publisher = PrivateKey::new();
    schema.publisher = *publisher.public_key().as_bytes();
    let salaries = schema
        .tables
        .iter_mut()
        .find(|table| table.sql_name == "salaries")
        .unwrap();
    salaries.class = TableClass::Replayed;
    Typed::with_schema(publisher, schema)
}

fn opened(
    t: &Typed,
    frame: &EncryptedFrame,
) -> (Vec<RowWrite>, Option<rocky_core::replay::Intent>) {
    let operation = frame.operation_ref().unwrap();
    let plaintext = crate::wire::decrypt(
        &t.key(frame.scope().unwrap()).key,
        &encode(&operation.header.extensions).unwrap(),
        frame.body_bytes(),
    )
    .unwrap();
    let payload = crate::wire::RowPayload::decode_test(&plaintext, frame, &t.app()).unwrap();
    (payload.writes().to_vec(), payload.intent().cloned())
}

#[tokio::test]
async fn replayed_writes_commit_as_intents_beside_mergeable_rows() {
    let t = replayed();
    let dir = TempDir::new().unwrap();
    let both = [t.items, t.salaries];
    let node = PrivateKey::new();
    let alice = t
        .open(&dir.path().join("a.db"), &node, t.full(), &both)
        .await;
    let session = sql(&alice, &both).await;
    session.run("BEGIN IMMEDIATE", vec![]).await.unwrap();
    insert(&session, "INSERT INTO salaries VALUES('ann',10)", vec![])
        .await
        .unwrap();
    session
        .run(
            "UPDATE salaries SET amount=amount+? WHERE key=?",
            vec![SqlValue::Integer(5), text("ann")],
        )
        .await
        .unwrap();
    // A savepoint rolled back takes its statements out of the intent.
    session.run("SAVEPOINT undo", vec![]).await.unwrap();
    session
        .run("UPDATE salaries SET amount=0", vec![])
        .await
        .unwrap();
    session.run("ROLLBACK TO undo", vec![]).await.unwrap();
    session.run("RELEASE undo", vec![]).await.unwrap();
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    let frames = session
        .commit([1; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    assert_eq!(frames.len(), 2);

    // The author's execution is its local result.
    assert_eq!(
        select(&session, "SELECT key,amount FROM salaries").await,
        vec![vec![text("ann"), SqlValue::Integer(15)]]
    );
    let mut intents = Vec::new();
    for frame in &frames {
        let (writes, carried) = opened(&t, frame);
        match frame.scope().unwrap() {
            scope if scope == t.items => assert!(carried.is_none() && writes.len() == 1),
            scope if scope == t.salaries => {
                assert!(writes.is_empty());
                intents.push(carried.unwrap());
            }
            scope => panic!("unexpected scope {scope:?}"),
        }
    }
    let [intent] = intents.as_slice() else {
        panic!("one intent frame");
    };
    assert!(intent.base.is_empty());
    let statements: Vec<_> = intent
        .statements
        .iter()
        .map(|statement| (statement.sql.as_str(), statement.params.clone()))
        .collect();
    assert_eq!(statements.len(), 2);
    assert!(statements[0].0.starts_with("INSERT INTO salaries"));
    assert!(
        statements[1]
            .0
            .starts_with("UPDATE salaries SET amount = amount + ?")
    );
    assert_eq!(statements[1].1, vec![SqlValue::Integer(5), text("ann")]);

    // The next intent reads the first: its base names the author's frame.
    session.run("BEGIN", vec![]).await.unwrap();
    session
        .run(
            "UPDATE salaries SET amount=amount+1 WHERE key='ann'",
            vec![],
        )
        .await
        .unwrap();
    let latest = session
        .commit([2; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    let [latest] = latest.as_slice() else {
        panic!("one intent frame");
    };
    let header = |frame: &EncryptedFrame| frame.operation_ref().unwrap().header.clone();
    let first = header(
        frames
            .iter()
            .find(|frame| frame.scope().unwrap() == t.salaries)
            .unwrap(),
    );
    let (_, next) = opened(&t, latest);
    assert_eq!(
        next.unwrap().base,
        vec![rocky_core::replay::BaseMark {
            author: first.extensions.author.unwrap(),
            sequence: first.extensions.author_sequence,
        }]
    );
    assert!(header(latest).timestamp > first.timestamp);
}

#[tokio::test]
async fn nondeterministic_replayed_writes_are_refused_before_they_run() {
    let t = replayed();
    let dir = TempDir::new().unwrap();
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let session = sql(&alice, &both).await;
    insert(&session, "INSERT INTO salaries VALUES('ann',10)", vec![])
        .await
        .unwrap();
    for refused in [
        "UPDATE salaries SET amount=random()",
        "UPDATE salaries SET amount=amount WHERE rowid=1",
        "WITH raise AS (SELECT 1 AS x) UPDATE salaries SET amount=amount+(SELECT x FROM raise)",
    ] {
        assert!(
            matches!(
                sql_error(session.run(refused, vec![]).await),
                SqlError::Unsupported(_)
            ),
            "{refused}"
        );
    }
    assert_eq!(
        select(&session, "SELECT amount FROM salaries").await,
        vec![vec![SqlValue::Integer(10)]]
    );
}

#[tokio::test]
async fn a_single_row_intent_authors_within_its_statement_budget() {
    let t = replayed();
    let dir = TempDir::new().unwrap();
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let session = sql(&alice, &both).await;
    insert(&session, "INSERT INTO salaries VALUES('ann',10)", vec![])
        .await
        .unwrap();
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    let count = |sql: &'static str| {
        let session = &session;
        async move {
            let before = crate::sqlite::statements();
            crate::sqlite::trace_statements(true);
            session.run(sql, vec![]).await.unwrap();
            let trace = crate::sqlite::trace_statements(false);
            if std::env::var_os("ROCKY_TRACE_AUTHOR").is_some() {
                for statement in &trace {
                    println!("SQL {statement}");
                }
            }
            let total = crate::sqlite::statements() - before;
            // The transaction itself; session admission runs around it.
            let begin = trace
                .iter()
                .position(|sql| sql.starts_with("BEGIN"))
                .unwrap();
            let commit = trace.iter().position(|sql| sql == "COMMIT").unwrap();
            (total, commit - begin + 1)
        }
    };
    let mergeable = count("UPDATE items SET done=1 WHERE key='a'").await;
    let replayed = count("UPDATE salaries SET amount=amount+1 WHERE key='ann'").await;
    println!(
        "P4 author_statements total/transaction mergeable={mergeable:?} replayed={replayed:?}"
    );
    assert!(replayed.1 <= 43, "{replayed:?} statements");
}

/// One update of `ann`'s amount, committed alone.
async fn raise(session: &SecureSql<TypedRows>, by: i64) -> EncryptedFrame {
    let [frame] = session
        .run(
            "UPDATE salaries SET amount=amount+? WHERE key='ann'",
            vec![SqlValue::Integer(by)],
        )
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames
        .try_into()
        .unwrap();
    frame
}

async fn amounts(
    replica: &EncryptedReplica<TypedRows>,
    both: &[ResourceScope],
) -> Vec<Vec<SqlValue>> {
    select(
        &sql(replica, both).await,
        "SELECT key,amount FROM salaries ORDER BY key",
    )
    .await
}

/// Each replayed member's partition order and drop reason, in that order.
async fn outcomes(replica: &EncryptedReplica<TypedRows>) -> Vec<Option<String>> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    collect_rows(
        inner
            .conn
            .query(
                "SELECT dropped FROM secure_replayed ORDER BY clock,author,sequence",
                (),
            )
            .await
            .unwrap(),
    )
    .await
    .unwrap()
    .into_iter()
    .map(|row| match &row[0] {
        SqlValue::Text(reason) => Some(reason.clone()),
        _ => None,
    })
    .collect()
}

#[tokio::test]
async fn concurrent_increments_replay_in_order_and_both_apply() {
    let t = replayed();
    let dir = TempDir::new().unwrap();
    let both = [t.items, t.salaries];
    let open = |name: &'static str| {
        let (t, dir) = (&t, &dir);
        async move {
            t.open(&dir.path().join(name), &PrivateKey::new(), t.full(), &both)
                .await
        }
    };
    let (alice, bob, carol) = (open("a.db").await, open("b.db").await, open("c.db").await);
    let [created] = insert(
        &sql(&alice, &both).await,
        "INSERT INTO salaries VALUES('ann',10)",
        vec![],
    )
    .await
    .unwrap()
    .committed
    .unwrap()
    .frames
    .try_into()
    .unwrap();
    assert_eq!(
        bob.receive(created.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    // Both raise the amount they read; neither has seen the other.
    let first = raise(&sql(&alice, &both).await, 1).await;
    let second = raise(&sql(&bob, &both).await, 1).await;
    assert!(replay_stamp(&first) < replay_stamp(&second));
    let ann = |amount| vec![vec![text("ann"), SqlValue::Integer(amount)]];
    for frame in [created, first.clone(), second.clone()] {
        assert_eq!(carol.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    }
    assert_eq!(amounts(&carol, &both).await, ann(12));
    assert_eq!(alice.receive(second).await.unwrap(), ReceiveStatus::Applied);
    assert_eq!(amounts(&alice, &both).await, ann(12));
    // Bob already applied a later transaction: the earlier one waits to rewind.
    assert_eq!(
        bob.receive(first).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    // Its partition is incomplete until then, never read as Bob's alone.
    assert!(matches!(
        sql_error(
            sql(&bob, &both)
                .await
                .run("SELECT amount FROM salaries", vec![])
                .await
        ),
        SqlError::Unavailable { .. }
    ));
    assert_eq!(outcomes(&carol).await, vec![None, None, None]);
}

fn replay_stamp(frame: &EncryptedFrame) -> (u64, u32, u64) {
    let header = &frame.operation_ref().unwrap().header;
    (
        header.timestamp,
        header.extensions.author.unwrap(),
        header.extensions.author_sequence,
    )
}

#[tokio::test]
async fn a_failing_replay_drops_its_transaction_and_the_log_continues() {
    let t = replayed();
    let dir = TempDir::new().unwrap();
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let bob = t
        .open(
            &dir.path().join("b.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let carol = t
        .open(
            &dir.path().join("c.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    // Both create the same key; the later one fails its primary key.
    let create = |replica, amount| {
        let both = &both;
        async move {
            let frames = insert(
                &sql(replica, both).await,
                &format!("INSERT INTO salaries VALUES('ann',{amount})"),
                vec![],
            )
            .await
            .unwrap()
            .committed
            .unwrap()
            .frames;
            frames.into_iter().next().unwrap()
        }
    };
    let earlier = create(&alice, 10).await;
    let later = create(&bob, 20).await;
    let after = raise(&sql(&bob, &both).await, 5).await;
    for frame in [earlier.clone(), later.clone(), after] {
        assert_eq!(carol.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    }
    // Bob's raise replays on Alice's row: a drop is not a tombstone.
    assert_eq!(
        amounts(&carol, &both).await,
        vec![vec![text("ann"), SqlValue::Integer(15)]]
    );
    assert_eq!(
        outcomes(&carol).await,
        vec![None, Some("constraint".into()), None]
    );
    let transaction = |frame: &EncryptedFrame| {
        crate::wire::RowPayload::decode_test(
            &crate::wire::decrypt(
                &t.key(t.salaries).key,
                &encode(&frame.operation_ref().unwrap().header.extensions).unwrap(),
                frame.body_bytes(),
            )
            .unwrap(),
            frame,
            &t.app(),
        )
        .unwrap()
        .membership()
        .unwrap()
        .transaction
    };
    use rocky_core::replay::{DropReason, TransactionOutcome};
    assert_eq!(
        carol
            .transaction_outcome(transaction(&earlier))
            .await
            .unwrap(),
        Some(TransactionOutcome::Applied)
    );
    assert_eq!(
        carol
            .transaction_outcome(transaction(&later))
            .await
            .unwrap(),
        Some(TransactionOutcome::Dropped {
            reason: DropReason::Constraint,
            sealed: false,
        })
    );
    // Bob's own replay of his transaction applied; Carol's ordering dropped it.
    assert_eq!(
        bob.transaction_outcome(transaction(&later)).await.unwrap(),
        Some(TransactionOutcome::Applied)
    );
}

#[tokio::test]
async fn replayed_effects_outside_the_authors_grants_drop_as_admission() {
    let t = replayed();
    let dir = TempDir::new().unwrap();
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    // Bob may insert salaries but not change them.
    let inserter = grant(
        t.salaries,
        &[Right::Read, Right::Insert, Right::Custody],
        &t.every_field("salaries"),
    );
    let bob = t
        .open(
            &dir.path().join("b.db"),
            &PrivateKey::new(),
            vec![t.writer(t.items, "items"), inserter],
            &both,
        )
        .await;
    let [created] = insert(
        &sql(&alice, &both).await,
        "INSERT INTO salaries VALUES('ann',10)",
        vec![],
    )
    .await
    .unwrap()
    .committed
    .unwrap()
    .frames
    .try_into()
    .unwrap();
    assert_eq!(bob.receive(created).await.unwrap(), ReceiveStatus::Applied);
    let raise = "UPDATE salaries SET amount=amount+1 WHERE key='ann'";
    // The author's own commit admits its effects as every replica will.
    assert!(matches!(
        sql(&bob, &both).await.run(raise, vec![]).await,
        Err(Error::Rejected(_))
    ));
    // An author that skips that check still cannot change other replicas.
    bob.inner
        .lock(crate::replica::Class::Background)
        .await
        .unchecked_replayed_admission = true;
    let rows = sql(&bob, &both).await.run(raise, vec![]).await.unwrap();
    let [frame] = rows.committed.unwrap().frames.try_into().unwrap();
    assert_eq!(alice.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    assert_eq!(
        amounts(&alice, &both).await,
        vec![vec![text("ann"), SqlValue::Integer(10)]]
    );
    assert_eq!(outcomes(&alice).await, vec![None, Some("admission".into())]);
}

#[tokio::test]
async fn a_single_row_intent_receives_within_its_statement_budget() {
    let t = replayed();
    let dir = TempDir::new().unwrap();
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let bob = t
        .open(
            &dir.path().join("b.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let session = sql(&alice, &both).await;
    let [created] = insert(&session, "INSERT INTO salaries VALUES('ann',10)", vec![])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames
        .try_into()
        .unwrap();
    bob.receive(created).await.unwrap();
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    for frame in transfer(&alice, &bob, t.items).await.frames {
        bob.receive(frame).await.unwrap();
    }
    let [edit] = session
        .run("UPDATE items SET done=1 WHERE key='a'", vec![])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames
        .try_into()
        .unwrap();
    // The whole call, and the received frame's own application in it.
    let count = |frame: EncryptedFrame| {
        let bob = &bob;
        async move {
            let before = crate::sqlite::statements();
            crate::sqlite::trace_statements(true);
            assert_eq!(bob.receive(frame).await.unwrap(), ReceiveStatus::Applied);
            let trace = crate::sqlite::trace_statements(false);
            if std::env::var_os("ROCKY_TRACE_RECEIVE").is_some() {
                for statement in &trace {
                    println!("SQL {statement}");
                }
            }
            let at = |sql: &str| trace.iter().position(|traced| traced == sql).unwrap();
            (
                crate::sqlite::statements() - before,
                at("RELEASE received_application") - at("SAVEPOINT received_application") + 1,
            )
        }
    };
    let mergeable = count(edit).await;
    let replayed = count(raise(&session, 1).await).await;
    println!(
        "P4 receive_statements total/application mergeable={mergeable:?} replayed={replayed:?}"
    );
    assert!(replayed.1 <= 45, "{replayed:?} statements");
}
