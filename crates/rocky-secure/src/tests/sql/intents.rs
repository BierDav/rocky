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

#[tokio::test]
async fn receivers_refuse_intent_frames_until_they_replay() {
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
    let [frame] = insert(&session, "INSERT INTO salaries VALUES('ann',10)", vec![])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames
        .try_into()
        .unwrap();
    assert_eq!(bob.receive(frame).await.unwrap(), ReceiveStatus::Rejected);
    assert!(bob.snapshot(t.salaries).await.unwrap().rows.is_empty());
}
