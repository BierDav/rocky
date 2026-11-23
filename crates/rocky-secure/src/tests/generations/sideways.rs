use super::super::compaction::{bookmark, follow_main, scalar, seal, sync};
use super::*;
use crate::TipStatement;
use rocky_core::TipId;
use rocky_credentials::{
    BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SealAction, SignedBookmarkMove, SignedSeal,
    SignedTipOpening, TIP_OPENING_FORMAT, TipOpeningStatement,
};

#[derive(Clone, Copy, Debug)]
enum Rebuild {
    Base,
    Replay,
}

async fn follow(
    lineage: &Lineage,
    replica: &EncryptedReplica<TypedRows>,
    (identity, key): (&PrivateKey, &PrivateKey),
    name: &str,
    version: u32,
) {
    replica.follow_bookmark(lineage.domain, name).await.unwrap();
    if replica.pending_schema().await.unwrap().is_none() {
        return;
    }
    let target = replica
        .tip_graph(lineage.domain)
        .await
        .unwrap()
        .bookmark(name)
        .unwrap()
        .unwrap();
    lineage.install_required(replica).await;
    let mut config = lineage.config(version, key);
    config.credentials = lineage.certified(version, identity, key);
    config.manifest = lineage.manifest(lineage.root(replica).await.unwrap());
    replica
        .switch_schema(
            target,
            lineage.app(version),
            crate::SchemaConfig {
                security: config,
                tables: Some(lineage.projection(version)),
            },
        )
        .await
        .unwrap();
    assert!(replica.pending_schema().await.unwrap().is_none());
}

pub(in crate::tests) fn open_dev(
    lineage: &Lineage,
    seal: Commitment,
    version: u32,
) -> Vec<TipStatement> {
    let opening = SignedTipOpening::sign(
        TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            schema: lineage.definition(version).hash(),
            scope: lineage.domain,
            seal,
        },
        lineage.publisher(),
    )
    .unwrap();
    let dev = opening.tip().unwrap();
    let movement = SignedBookmarkMove::sign(
        BookmarkMoveStatement {
            format: BOOKMARK_MOVE_FORMAT,
            scope: lineage.domain,
            name: "dev".into(),
            from: None,
            to: Some(dev),
        },
        lineage.publisher(),
    )
    .unwrap();
    vec![
        TipStatement::Opening(opening),
        TipStatement::Bookmark(movement),
    ]
}

const NOTES: &str = "SELECT key,title,note FROM items ORDER BY key";
const TITLES: &str = "SELECT key,title FROM items ORDER BY key";

fn note_rows(rows: &[(&str, &str, Option<&str>)]) -> Vec<Vec<SqlValue>> {
    rows.iter()
        .map(|(key, title, note)| vec![text(key), text(title), note.map_or(SqlValue::Null, text)])
        .collect()
}

async fn there_and_back(rebuild: Rebuild, dev_version: u32) {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let (identity, alice_key, dave_key) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let alice = lineage
        .open_certified(
            &dir.path().join("alice.db"),
            (&identity, &alice_key),
            3,
            None,
        )
        .await
        .unwrap();
    let dave = lineage
        .open(&dir.path().join("dave.db"), &dave_key, 3, None)
        .await
        .unwrap();
    follow_main(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &dave, lineage.domain).await;
    for (key, note) in [("0001", "'old'"), ("0002", "NULL")] {
        identity_insert(
            &session(&alice).await,
            &format!("INSERT INTO items VALUES('{key}','title',{note},NULL)"),
            vec![],
        )
        .await
        .unwrap();
    }
    sync(&lineage, &alice, &dave, &dave_key).await;
    write(&dave, "UPDATE items SET title='late' WHERE key='0002'").await;
    let first = seal(&lineage, &alice).await.id().unwrap();
    if matches!(rebuild, Rebuild::Base) {
        assert_eq!(
            alice.compact(lineage.domain, 0).await.unwrap().compacted,
            Some(first)
        );
    }
    alice
        .publish_tip_statements(&open_dev(&lineage, first, dev_version))
        .await
        .unwrap();
    for frame in super::switches::authored(&dave, &dave_key).await {
        alice.receive(frame).await.unwrap();
    }
    write(&alice, "UPDATE items SET note='x' WHERE key='0002'").await;
    let main_state = note_rows(&[("0001", "title", Some("old")), ("0002", "late", Some("x"))]);
    assert_eq!(
        select(&alice, NOTES).await,
        main_state,
        "a late write from the first tip walks the `continue` seal onto `main`"
    );
    let at_first = vec![
        vec![text("0001"), text("title")],
        vec![text("0002"), text("title")],
    ];

    follow(
        &lineage,
        &alice,
        (&identity, &alice_key),
        "dev",
        dev_version,
    )
    .await;
    assert_eq!(select(&alice, TITLES).await, at_first);
    assert!(alice.held_changes().await.unwrap().is_empty());
    assert_eq!(scalar(&alice, "SELECT count(*) FROM secure_voids").await, 0);
    let branch_node = PrivateKey::new();
    alice
        .register_line_node(crate::LineNode {
            credentials: lineage.certified(dev_version, &identity, &branch_node),
            key: branch_node,
        })
        .await
        .unwrap();
    identity_insert(
        &session(&alice).await,
        "INSERT INTO items(key,title) VALUES('0009','dev test')",
        vec![],
    )
    .await
    .unwrap();

    assert_eq!(
        scalar(&alice, "SELECT count(*) FROM secure_identity_ledger").await,
        2
    );
    let journal = crate::storage::rows(&alice.inner.lock(crate::replica::Class::Background).await.conn,"SELECT 'O',scope,author,sequence FROM secure_identity_own UNION ALL SELECT 'R',entry,bytes,chain FROM secure_admission_entries UNION ALL SELECT 'C',identity,entry,provenance FROM secure_admission_claims").await.unwrap();
    follow(&lineage, &alice, (&identity, &alice_key), "main", 3).await;
    assert_eq!(select(&alice, NOTES).await, main_state);
    assert_eq!(
        scalar(&alice, "SELECT count(*) FROM secure_identity_ledger").await,
        1,
        "a write on `dev`, by its own line node, stays there"
    );
    assert_eq!(crate::storage::rows(&alice.inner.lock(crate::replica::Class::Background).await.conn,"SELECT 'O',scope,author,sequence FROM secure_identity_own UNION ALL SELECT 'R',entry,bytes,chain FROM secure_admission_entries UNION ALL SELECT 'C',identity,entry,provenance FROM secure_admission_claims").await.unwrap(), journal);
    assert!(alice.held_changes().await.unwrap().is_empty());
    assert_eq!(scalar(&alice, "SELECT count(*) FROM secure_voids").await, 0);
    alice.audit(lineage.scope("items")).await.unwrap();
    dave.follow_bookmark(lineage.domain, "main")
        .await
        .unwrap_or_else(|_| panic!("dave follows main"));
    sync(&lineage, &alice, &dave, &dave_key).await;
    assert_eq!(
        select(&dave, NOTES).await,
        main_state,
        "the unsent `main` write was kept for `main` and is sent now"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn following_back_restores_main_from_a_base_at_the_seal() {
    there_and_back(Rebuild::Base, 4).await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn following_back_restores_main_by_replaying_every_frame() {
    there_and_back(Rebuild::Replay, 4).await;
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn following_a_branch_of_the_same_schema_and_back_behaves_the_same() {
    for rebuild in [Rebuild::Base, Rebuild::Replay] {
        there_and_back(rebuild, 3).await;
    }
}

async fn rolled_back(rebuild: Rebuild, removing: bool) {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let (identity, key) = (PrivateKey::new(), PrivateKey::new());
    let alice = lineage
        .open_certified(&dir.path().join("alice.db"), (&identity, &key), 3, None)
        .await
        .unwrap();
    follow_main(&lineage, &alice).await;
    identity_insert(
        &session(&alice).await,
        "INSERT INTO items VALUES('0001','title','old',NULL)",
        vec![],
    )
    .await
    .unwrap();
    let bob_key = PrivateKey::new();
    let bob = lineage
        .open(&dir.path().join("bob.db"), &bob_key, 3, None)
        .await
        .unwrap();
    crate::tests::exchange_graphs(&alice, &bob, lineage.domain).await;
    sync(&lineage, &alice, &bob, &bob_key).await;
    let a = alice.followed_tip().await;
    let version = if removing { 4 } else { 3 };
    let statements = lineage.publish(&alice, version).await.unwrap();
    if removing {
        let mut config = lineage.config(4, &key);
        config.credentials = lineage.certified(4, &identity, &key);
        assert!(
            lineage
                .follow(&alice, config, 4, &statements)
                .await
                .unwrap()
        );
    }
    let b = alice.followed_tip().await;
    if matches!(rebuild, Rebuild::Base) {
        alice.compact(lineage.domain, 0).await.unwrap();
    }
    write(&alice, "UPDATE items SET title='at B' WHERE key='0001'").await;
    let close = SignedSeal::sign(
        rocky_credentials::SealStatement {
            format: rocky_credentials::SEAL_FORMAT,
            scope: lineage.domain,
            tip: b,
            frontier: alice.seal_frontier(lineage.domain, false).await.unwrap(),
            action: SealAction::Close,
        },
        lineage.publisher(),
    )
    .unwrap();
    alice
        .receive_tip_statements(&[TipStatement::Seal(close), bookmark_move(&lineage, b, a)])
        .await
        .unwrap();
    if removing {
        assert!(alice.pending_schema().await.unwrap().is_some());
        follow(&lineage, &alice, (&identity, &key), "main", 3).await;
    }
    assert_eq!(alice.followed_tip().await, a);
    assert_eq!(
        select(&alice, NOTES).await,
        note_rows(&[("0001", "title", Some("old"))]),
        "{rebuild:?}"
    );
    assert!(
        alice.held_changes().await.unwrap().is_empty(),
        "intents have no rows to re-author, so the unsent write goes with its line: {rebuild:?}"
    );
    alice.audit(lineage.scope("items")).await.unwrap();
}

fn bookmark_move(lineage: &Lineage, from: TipId, to: TipId) -> TipStatement {
    match bookmark(lineage, Some(from), to) {
        TipStatement::Bookmark(movement) => TipStatement::Bookmark(movement),
        _ => unreachable!(),
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_owner_rollback_past_a_removal_restores_the_field_and_withdraws_unseen_writes() {
    for rebuild in [Rebuild::Replay, Rebuild::Base] {
        rolled_back(rebuild, true).await;
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_owner_rollback_within_one_schema_withdraws_the_same_writes() {
    for rebuild in [Rebuild::Base, Rebuild::Replay] {
        rolled_back(rebuild, false).await;
    }
}

async fn identical_revert(rebuild: Rebuild) {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let (identity, key, bob_key) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let alice = lineage
        .open_certified(&dir.path().join("alice.db"), (&identity, &key), 3, None)
        .await
        .unwrap();
    let bob = lineage
        .open(&dir.path().join("bob.db"), &bob_key, 3, None)
        .await
        .unwrap();
    follow_main(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &bob, lineage.domain).await;
    identity_insert(
        &session(&alice).await,
        "INSERT INTO items VALUES('0001','title','old',NULL)",
        vec![],
    )
    .await
    .unwrap();
    sync(&lineage, &alice, &bob, &bob_key).await;
    let config = |version| {
        let mut config = lineage.config(version, &key);
        config.credentials = lineage.certified(version, &identity, &key);
        config
    };
    let removed = lineage.publish(&alice, 4).await.unwrap();
    assert!(
        lineage
            .follow(&alice, config(4), 4, &removed)
            .await
            .unwrap()
    );
    let b = alice.followed_tip().await;
    let reverted = lineage.publish(&alice, 3).await.unwrap();
    assert!(
        lineage
            .follow(&alice, config(3), 3, &reverted)
            .await
            .unwrap()
    );
    let erased = note_rows(&[("0001", "title", None)]);
    assert_eq!(select(&alice, NOTES).await, erased, "{rebuild:?}");
    if !matches!(rebuild, Rebuild::Replay) {
        alice.compact(lineage.domain, 0).await.unwrap();
    }
    let b_seal = alice
        .tip_graph(lineage.domain)
        .await
        .unwrap()
        .seal(b)
        .unwrap()
        .unwrap()
        .id()
        .unwrap();
    alice
        .publish_tip_statements(&open_dev(&lineage, b_seal, 4))
        .await
        .unwrap();
    follow(&lineage, &alice, (&identity, &key), "dev", 4).await;
    follow(&lineage, &alice, (&identity, &key), "main", 3).await;
    assert_eq!(select(&alice, NOTES).await, erased, "{rebuild:?}");
    alice.audit(lineage.scope("items")).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_schema_identical_to_one_before_a_removal_never_restores_its_values() {
    for rebuild in [Rebuild::Base, Rebuild::Replay] {
        identical_revert(rebuild).await;
    }
}

mod admission;

fn linked_children_first() -> Lineage {
    let mut lineage = Lineage::new();
    lineage.linked = true;
    lineage.children_first = true;
    lineage
}
async fn tagged_pair(
    lineage: &Lineage,
    dir: &Scratch,
) -> (
    EncryptedReplica<TypedRows>,
    (PrivateKey, PrivateKey),
    Commitment,
) {
    let (identity, key) = (PrivateKey::new(), PrivateKey::new());
    let replica = lineage
        .open_certified(&dir.path().join("alice.db"), (&identity, &key), 3, None)
        .await
        .unwrap();
    follow_main(lineage, &replica).await;
    for key in ["0001", "0002"] {
        identity_insert(
            &session(&replica).await,
            &format!("INSERT INTO items VALUES('{key}','title',NULL,NULL)"),
            vec![],
        )
        .await
        .unwrap();
    }
    let first = seal(lineage, &replica).await.id().unwrap();
    (replica, (identity, key), first)
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_rebuild_renames_every_old_table_before_creating_a_new_one_so_a_child_declared_first_keeps_its_new_parent()
 {
    let lineage = linked_children_first();
    let dir = Scratch::new().await;
    let (alice, (identity, key), first) = tagged_pair(&lineage, &dir).await;
    alice
        .publish_tip_statements(&open_dev(&lineage, first, 2))
        .await
        .unwrap();
    follow(&lineage, &alice, (&identity, &key), "dev", 2).await;
    let parents = crate::storage::rows(
        &alice
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .conn,
        "SELECT \"table\" FROM pragma_foreign_key_list('badges')",
    )
    .await
    .unwrap();
    assert_eq!(parents, vec![vec![text("items")]; 2]);
}
