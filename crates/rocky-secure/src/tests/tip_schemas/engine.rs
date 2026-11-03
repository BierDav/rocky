//! A tip's schema commits the replay engine: a build on another engine
//! follows read-only and replays nothing until a tip names its engine.
use super::super::compaction::{Replica, bookmark, follow_main, insert, sync};
use super::super::generations::{Lineage, select, session, text};
use super::*;
use crate::EngineStatus;
use rocky_core::{BUNDLED_SQLITE_VERSION, EngineIdentity, REPLAY_SEMANTICS, RowApplication, TipId};
use rocky_credentials::{SEAL_FORMAT, SealStatement};

fn upgraded() -> EngineIdentity {
    EngineIdentity::of(BUNDLED_SQLITE_VERSION, REPLAY_SEMANTICS + 1)
}
/// The owner seals the tip `replica` follows at what it applied and
/// continues it with the same tables on `engine`.
async fn open_on(
    lineage: &Lineage,
    replica: &Replica,
    engine: EngineIdentity,
) -> (TipId, Vec<TipStatement>) {
    let source = replica.followed_tip().await;
    let seal = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: lineage.domain,
            tip: source,
            frontier: replica.seal_frontier(lineage.domain, false).await.unwrap(),
            action: SealAction::Continue {
                schema: lineage.app(1).on_engine(engine).manifest().hash(),
            },
            recovery: None,
        },
        lineage.publisher(),
    )
    .unwrap();
    let target = seal.successor().unwrap().unwrap();
    (
        target,
        vec![
            TipStatement::Seal(seal),
            bookmark(lineage, Some(source), target),
        ],
    )
}
/// Installs `target`'s schema on `engine` and switches to it.
async fn switch(
    lineage: &Lineage,
    replica: &Replica,
    key: &PrivateKey,
    target: TipId,
    engine: EngineIdentity,
) -> Result<bool, Error> {
    let app = lineage.app(1).on_engine(engine);
    replica
        .install_tip_schema(target, app.manifest(), b"engine".to_vec())
        .await?;
    replica
        .switch_schema(
            target,
            app,
            crate::SchemaConfig {
                security: lineage.config(1, key),
                tables: Some(lineage.projection(1)),
            },
        )
        .await
}
async fn keys(replica: &Replica) -> Vec<Vec<rocky_core::SqlValue>> {
    select(replica, "SELECT key FROM items ORDER BY key").await
}
async fn engine(replica: &Replica) -> EngineStatus {
    replica.generations().await.unwrap().engine
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_build_on_another_engine_follows_read_only_until_a_tip_names_it() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let (alice_key, bob_key) = (PrivateKey::new(), PrivateKey::new());
    let alice_path = dir.path().join("alice.db");
    let alice = lineage
        .open(&alice_path, &alice_key, 1, None)
        .await
        .unwrap();
    let bob = lineage
        .open(&dir.path().join("bob.db"), &bob_key, 1, None)
        .await
        .unwrap();
    follow_main(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &bob, lineage.domain).await;
    // Identical builds name the same engine in the same manifest.
    assert_eq!(
        alice.generations().await.unwrap().manifest,
        bob.generations().await.unwrap().manifest
    );
    assert_eq!(engine(&bob).await, EngineStatus::Current);
    insert(&alice, "one", "replayed by both").await;
    sync(&lineage, &alice, &bob, &bob_key).await;
    assert_eq!(keys(&bob).await, vec![vec![text("one")]]);

    // Bob upgrades Rocky before the owner does.
    bob.replay_on(upgraded()).await;
    assert_eq!(engine(&bob).await, EngineStatus::AwaitingOwner);
    let refused = session(&bob)
        .await
        .run("UPDATE items SET title='refused'", vec![])
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("replay engine"), "{refused}");
    // Alice's write reaches Bob and waits, stored, instead of replaying.
    insert(&alice, "two", "waits on bob").await;
    sync(&lineage, &alice, &bob, &bob_key).await;
    assert_eq!(keys(&bob).await, vec![vec![text("one")]]);

    // An upgraded owner opens a tip on the new engine; Bob follows it,
    // replays what waited after the seal and writes again.
    let (target, statements) = open_on(&lineage, &bob, upgraded()).await;
    bob.receive_tip_statements(&statements).await.unwrap();
    assert!(
        switch(&lineage, &bob, &bob_key, target, upgraded())
            .await
            .unwrap()
    );
    assert_eq!(engine(&bob).await, EngineStatus::Current);
    insert(&bob, "three", "bob").await;
    assert_eq!(
        keys(&bob).await,
        vec![vec![text("one")], vec![text("three")], vec![text("two")]]
    );

    // Alice, not upgraded, waits read-only at the old tip, also after a restart.
    alice.receive_tip_statements(&statements).await.unwrap();
    assert_eq!(alice.pending_schema().await.unwrap().unwrap().tip, target);
    let refused = switch(&lineage, &alice, &alice_key, target, upgraded())
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("upgrade Rocky"), "{refused}");
    assert_eq!(engine(&alice).await, EngineStatus::UpgradeRequired);
    drop(alice);
    let alice = lineage
        .open(&alice_path, &alice_key, 1, None)
        .await
        .unwrap();
    assert_eq!(engine(&alice).await, EngineStatus::UpgradeRequired);
    assert!(
        session(&alice)
            .await
            .run("UPDATE items SET title='refused'", vec![])
            .await
            .is_err()
    );
    // Once upgraded, Alice follows the new tip and writes.
    alice.replay_on(upgraded()).await;
    assert!(
        switch(&lineage, &alice, &alice_key, target, upgraded())
            .await
            .unwrap()
    );
    insert(&alice, "four", "alice").await;
}
