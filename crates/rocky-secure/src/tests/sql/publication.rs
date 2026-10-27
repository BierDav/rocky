//! A SQL transaction committed as a staged publication. See
//! `docs/decisions/0071-staged-migration-publication.md`.
use super::*;

/// Rows of one publication bundle here; one full-size run is the SDK's
/// `a_migration_of_100000_rows_converges_on_every_member`.
const BUNDLE: usize = 2 * ROWS_PER_FRAME;
/// Rows beyond one bundle.
const ROWS: usize = BUNDLE + 20;

async fn seed(session: &SecureSql<TypedRows>) -> Vec<EncryptedFrame> {
    let mut frames = Vec::new();
    for half in [0..ROWS / 2, ROWS / 2..ROWS] {
        session.run("BEGIN IMMEDIATE", vec![]).await.unwrap();
        let mut values = Vec::new();
        for row in half {
            let id = session.next_id("items").await.unwrap().integer();
            values.push(format!("('k{row:05}','item',0,{id})"));
        }
        let statement = format!(
            "INSERT INTO items(key,title,done,id) VALUES{}",
            values.join(",")
        );
        session.run(&statement, vec![]).await.unwrap();
        frames.extend(
            session
                .run("COMMIT", vec![])
                .await
                .unwrap()
                .committed
                .unwrap()
                .frames,
        );
    }
    frames
}
async fn done(session: &SecureSql<TypedRows>) -> Vec<Vec<SqlValue>> {
    select(
        session,
        "SELECT done, count(*) FROM items GROUP BY done ORDER BY done",
    )
    .await
}
async fn acknowledge(
    from: &EncryptedReplica<TypedRows>,
    to: &EncryptedReplica<TypedRows>,
    scope: ResourceScope,
) {
    let (node, credentials) = {
        let inner = from.inner.lock(crate::replica::Class::Background).await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    let frontier = from.frontier(scope).await.unwrap();
    to.acknowledge_frontier(node, &credentials, &frontier)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_transaction_of_any_size_leaves_as_consecutive_bundles() {
    let t = Typed::new(UpdatePolicy::Changed);
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
    let carol = t
        .open(
            &dir.path().join("c.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let (a, c) = (sql(&alice, &both).await, sql(&carol, &both).await);
    crate::replica::publication::BUNDLE_ROWS.set(BUNDLE);
    for status in carol.receive_all(seed(&a).await).await {
        status.unwrap();
    }

    // One transaction changes more rows than one bundle holds.
    let publication = [7; 32];
    a.run("BEGIN IMMEDIATE", vec![]).await.unwrap();
    a.run("UPDATE items SET done=1", vec![]).await.unwrap();
    let committed = a.commit_publication(publication).await.unwrap();
    let frames = committed.committed.unwrap().frames;
    assert_eq!(frames.len(), ROWS.div_ceil(ROWS_PER_FRAME));
    assert_eq!(
        alice.publications().await.unwrap(),
        [Publication {
            id: publication,
            bundles: 2,
            published: 0,
        }]
    );
    assert_eq!(
        done(&a).await,
        [vec![SqlValue::Integer(1), SqlValue::Integer(ROWS as i64)]]
    );

    // The same publication returns its frames again and changes nothing.
    a.run("BEGIN IMMEDIATE", vec![]).await.unwrap();
    a.run("UPDATE items SET done=0 WHERE key='k00000'", vec![])
        .await
        .unwrap();
    let again = a.commit_publication(publication).await.unwrap();
    assert_eq!(
        again
            .committed
            .unwrap()
            .frames
            .iter()
            .map(|frame| frame.id().unwrap())
            .collect::<Vec<_>>(),
        frames
            .iter()
            .map(|frame| frame.id().unwrap())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        done(&a).await,
        [vec![SqlValue::Integer(1), SqlValue::Integer(ROWS as i64)]]
    );

    // The first bundle is ordinary: it applies alone, rows not reached yet
    // keep their values, and a peer holding it publishes it.
    let first = BUNDLE / ROWS_PER_FRAME;
    for status in carol.receive_all(frames[..first].to_vec()).await {
        status.unwrap();
    }
    assert_eq!(
        done(&c).await,
        [
            vec![
                SqlValue::Integer(0),
                SqlValue::Integer((ROWS - BUNDLE) as i64)
            ],
            vec![SqlValue::Integer(1), SqlValue::Integer(BUNDLE as i64)]
        ]
    );
    acknowledge(&carol, &alice, t.items).await;
    assert_eq!(alice.publications().await.unwrap()[0].published, 1);

    // An edit made while the second bundle is still on its way is newer
    // and wins on both replicas.
    let waiting = select(&c, "SELECT key FROM items WHERE done=0 LIMIT 1").await[0][0].clone();
    let edit = c
        .run("UPDATE items SET done=2 WHERE key=?", vec![waiting.clone()])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    for status in carol.receive_all(frames[first..].to_vec()).await {
        status.unwrap();
    }
    for status in alice.receive_all(edit).await {
        status.unwrap();
    }
    let row = "SELECT done FROM items WHERE key=?";
    for session in [&a, &c] {
        assert_eq!(
            session.run(row, vec![waiting.clone()]).await.unwrap().rows,
            [vec![SqlValue::Integer(2)]]
        );
        assert_eq!(
            done(session).await,
            [
                vec![SqlValue::Integer(1), SqlValue::Integer(ROWS as i64 - 1)],
                vec![SqlValue::Integer(2), SqlValue::Integer(1)]
            ]
        );
    }
    let every = "SELECT key, title, done, id FROM items ORDER BY key";
    assert_eq!(select(&a, every).await, select(&c, every).await);
    acknowledge(&carol, &alice, t.items).await;
    assert_eq!(alice.publications().await.unwrap()[0].published, 2);

    // An empty publication is recorded too, so it is not run again.
    a.run("BEGIN IMMEDIATE", vec![]).await.unwrap();
    a.commit_publication([8; 32]).await.unwrap();
    assert!(alice.publications().await.unwrap().contains(&Publication {
        id: [8; 32],
        bundles: 0,
        published: 0,
    }));
}
