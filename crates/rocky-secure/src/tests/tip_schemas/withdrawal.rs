use super::super::compaction::{Replica, seal};
use super::*;
use crate::{SchemaConfig, SchemaStage};
use rocky_core::{SqlValue, TipId};

#[derive(Clone, Copy)]
struct Generations {
    source: u32,
    staged: u32,
}
const NARROWING: Generations = Generations {
    source: 3,
    staged: 2,
};
const ADDING_TAGS: Generations = Generations {
    source: 1,
    staged: 2,
};

fn linked() -> Lineage {
    let mut lineage = Lineage::new();
    lineage.linked = true;
    lineage
}
fn schema(
    lineage: &Lineage,
    generations: Generations,
    version: u32,
    key: &PrivateKey,
) -> SchemaConfig {
    let mut security = lineage.config(version, key);
    security.manifest = lineage.config(generations.source, key).manifest;
    SchemaConfig {
        security,
        tables: Some(lineage.projection(version)),
    }
}
async fn opened(
    lineage: &Lineage,
    generations: Generations,
    dir: &Scratch,
    key: &PrivateKey,
) -> Replica {
    let replica = lineage
        .open(
            &dir.path().join("a.db"),
            key,
            generations.source,
            Some(lineage.projection(generations.source)),
        )
        .await
        .unwrap();
    follow_main(lineage, &replica).await;
    replica
}
async fn stage(
    lineage: &Lineage,
    generations: Generations,
    replica: &Replica,
) -> (Commitment, TipId) {
    let source = replica.followed_tip().await;
    let mut statement = sign_seal(
        lineage,
        source,
        replica.seal_frontier(lineage.domain, false).await.unwrap(),
    )
    .statement;
    statement.action = SealAction::Continue {
        schema: lineage.definition(generations.staged).hash(),
    };
    let seal = SignedSeal::sign(statement, lineage.publisher()).unwrap();
    let target = seal.successor().unwrap().unwrap();
    let publication = rocky_core::commitment("withdrawal.stage", &target.0);
    replica
        .stage_schema(SchemaStage {
            publication,
            scope: lineage.domain,
            bookmark: "main".into(),
            source,
            target,
            from_version: generations.source,
            statements: vec![
                TipStatement::Seal(seal),
                bookmark(lineage, Some(source), target),
            ],
            program: vec![],
            parents: vec![],
        })
        .await
        .unwrap();
    replica
        .install_tip_schema(
            target,
            lineage.definition(generations.staged),
            b"verified".to_vec(),
        )
        .await
        .unwrap();
    (publication, target)
}
async fn switched_to_stage(
    lineage: &Lineage,
    generations: Generations,
    replica: &Replica,
    key: &PrivateKey,
) -> Commitment {
    let (publication, target) = stage(lineage, generations, replica).await;
    assert!(
        replica
            .switch_schema(
                target,
                lineage.app(generations.staged),
                schema(lineage, generations, generations.staged, key),
            )
            .await
            .unwrap()
    );
    assert_eq!(replica.followed_tip().await, target);
    publication
}
async fn withdraw(
    lineage: &Lineage,
    generations: Generations,
    replica: &Replica,
    key: &PrivateKey,
    publication: Commitment,
) -> Result<(), Error> {
    replica
        .withdraw_staged_schema(
            publication,
            lineage.app(generations.source),
            schema(lineage, generations, generations.source, key),
        )
        .await
}
async fn stored(replica: &Replica, sql: &str) -> Vec<Vec<SqlValue>> {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    crate::storage::rows(&inner.conn, sql).await.unwrap()
}
async fn item_id(replica: &Replica, key: &str) -> SqlValue {
    super::generations::select(replica, &format!("SELECT id FROM items WHERE key='{key}'"))
        .await
        .remove(0)
        .remove(0)
}
async fn tag(replica: &Replica, key: &str, item: &str) {
    let id = item_id(replica, item).await;
    super::generations::identity_insert(
        &session(replica).await,
        "INSERT INTO tags(key,label,item,also) VALUES(?,'l',?,?)",
        vec![text(key), id.clone(), id],
    )
    .await
    .unwrap();
}
fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}
async fn titles(replica: &Replica) -> Vec<Vec<SqlValue>> {
    super::generations::select(replica, "SELECT key,title FROM items ORDER BY key").await
}
async fn tags(replica: &Replica) -> Vec<Vec<SqlValue>> {
    super::generations::select(
        replica,
        "SELECT t.key,i.key FROM tags t LEFT JOIN items i ON i.id=t.item ORDER BY t.key",
    )
    .await
}
async fn waits_to_rederive(replica: &Replica) -> bool {
    !stored(replica, "SELECT 1 FROM secure_replayed_rederive")
        .await
        .is_empty()
}
async fn back_at_the_source(lineage: &Lineage, replica: &Replica, source: TipId) {
    assert_eq!(replica.followed_tip().await, source);
    assert!(replica.staged_schema().await.unwrap().is_none());
    assert!(replica.pending_schema().await.unwrap().is_none());
    assert!(replica.projection_plan().await.unwrap().is_none());
    assert_eq!(
        replica
            .tip_graph(lineage.domain)
            .await
            .unwrap()
            .bookmark("main")
            .unwrap(),
        Some(source)
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn withdrawing_a_stage_discards_the_rows_written_under_it() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let replica = opened(&lineage, NARROWING, &dir, &key).await;
    insert(&replica, "a", "written at the source").await;
    let source = replica.followed_tip().await;
    let frames = stored(&replica, "SELECT hash FROM secure_operations ORDER BY hash").await;
    let publication = switched_to_stage(&lineage, NARROWING, &replica, &key).await;
    insert(&replica, "b", "written under the stage").await;
    withdraw(&lineage, NARROWING, &replica, &key, publication)
        .await
        .unwrap();
    back_at_the_source(&lineage, &replica, source).await;
    assert_eq!(
        titles(&replica).await,
        vec![vec![text("a"), text("written at the source")]],
        "the source's tables come from its own history, not from the staged schema's rows"
    );
    assert_eq!(
        stored(&replica, "SELECT hash FROM secure_operations ORDER BY hash").await,
        frames,
        "a write made under a withdrawn stage never left this replica and is discarded with it"
    );
    let items = lineage.scope("items");
    replica.audit(items).await.unwrap();
    assert!(replica.held_changes().await.unwrap().is_empty());
    assert!(!waits_to_rederive(&replica).await);
    assert_eq!(
        replica
            .compaction_status(lineage.domain)
            .await
            .unwrap()
            .waiting,
        None
    );
    insert(&replica, "c", "written after the withdrawal").await;
    drop(replica);
    let replica = lineage
        .reopen(
            &dir.path().join("a.db"),
            &key,
            NARROWING.source,
            NARROWING.source,
        )
        .await
        .unwrap();
    replica.audit(items).await.unwrap();
    assert_eq!(
        titles(&replica).await,
        vec![
            vec![text("a"), text("written at the source")],
            vec![text("c"), text("written after the withdrawal")],
        ],
        "the source's log continues after the discarded write"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn withdrawing_a_stage_whose_rows_break_a_restored_reference_restores_the_source() {
    let mut lineage = linked();
    lineage.unlinked_at = Some(NARROWING.staged);
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let replica = opened(&lineage, NARROWING, &dir, &key).await;
    insert(&replica, "p", "parent").await;
    tag(&replica, "t", "p").await;
    let source = replica.followed_tip().await;
    let publication = switched_to_stage(&lineage, NARROWING, &replica, &key).await;
    super::generations::write(&replica, "DELETE FROM items WHERE key='p'").await;
    assert_eq!(
        tags(&replica).await,
        vec![vec![text("t"), SqlValue::Null]],
        "the staged schema drops the reference, so its rows orphan the tag"
    );
    withdraw(&lineage, NARROWING, &replica, &key, publication)
        .await
        .unwrap();
    back_at_the_source(&lineage, &replica, source).await;
    assert_eq!(
        titles(&replica).await,
        vec![vec![text("p"), text("parent")]]
    );
    assert_eq!(tags(&replica).await, vec![vec![text("t"), text("p")]]);
    replica.audit(lineage.scope("items")).await.unwrap();
    replica.audit(lineage.scope("tags")).await.unwrap();
    assert!(!waits_to_rederive(&replica).await);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_withdrawal_whose_replay_orphans_a_settled_parent_waits_to_rederive_instead_of_refusing()
{
    for switched in [true, false] {
        let lineage = linked();
        let dir = Scratch::new().await;
        let key = PrivateKey::new();
        let replica = opened(&lineage, NARROWING, &dir, &key).await;
        insert(&replica, "p", "parent").await;
        tag(&replica, "t", "p").await;
        seal(&lineage, &replica).await;
        replica.compact(lineage.domain, 0).await.unwrap();
        let source = replica.followed_tip().await;
        let publication = if switched {
            switched_to_stage(&lineage, NARROWING, &replica, &key).await
        } else {
            let (publication, target) = stage(&lineage, NARROWING, &replica).await;
            replica.fail_next_plan_switch().await;
            assert!(matches!(
                replica
                    .switch_schema(
                        target,
                        lineage.app(NARROWING.staged),
                        schema(&lineage, NARROWING, NARROWING.staged, &key),
                    )
                    .await,
                Err(Error::InjectedFailure)
            ));
            assert!(replica.pending_schema().await.unwrap().is_some());
            publication
        };
        let inner = replica.inner.lock(crate::replica::Class::Background).await;
        inner
            .conn
            .execute(
                "DELETE FROM secure_replayed_base WHERE scope=(SELECT id FROM secure_scope_state WHERE scope=?)",
                [encode(&lineage.scope("items")).unwrap()],
            )
            .await
            .unwrap();
        drop(inner);
        withdraw(&lineage, NARROWING, &replica, &key, publication)
            .await
            .unwrap_or_else(|refused| panic!("switched={switched}: {refused}"));
        back_at_the_source(&lineage, &replica, source).await;
        let blocked = session(&replica)
            .await
            .run("SELECT key FROM tags ORDER BY key", vec![])
            .await
            .unwrap_err();
        assert!(
            matches!(
                blocked,
                Error::Blocked(ref blockers)
                    if blockers.iter().any(|blocker| matches!(
                        &blocker.kind,
                        crate::BlockerKind::Rederiving { reason: Some(reason) }
                            if reason == "a row of tags references a missing row of items"
                    ))
            ),
            "switched={switched}: {blocked:?}"
        );
        assert!(waits_to_rederive(&replica).await, "switched={switched}");
        let waiting = replica
            .compaction_status(lineage.domain)
            .await
            .unwrap()
            .waiting;
        assert_eq!(
            waiting.as_deref(),
            Some(
                "the replayed rows cannot be derived again: a row of tags references a missing row of items"
            ),
            "switched={switched}"
        );
    }
}

async fn insert_handled(replica: &Replica, handle: Commitment, key: &str) -> Result<(), Error> {
    let session = session(replica).await;
    session.run("BEGIN IMMEDIATE", vec![]).await?;
    super::generations::identity_insert(
        &session,
        "INSERT INTO items(key,title) VALUES(?,'retried')",
        vec![text(key)],
    )
    .await?;
    super::generations::identity_insert(
        &session,
        "INSERT INTO tags(key,label) VALUES(?,'retried')",
        vec![text(key)],
    )
    .await?;
    session.commit(handle).await?;
    Ok(())
}
async fn outcomes(replica: &Replica) -> i64 {
    let counted = stored(
        replica,
        "SELECT (SELECT COUNT(*) FROM secure_outcomes)+(SELECT COUNT(DISTINCT txn) FROM secure_outcome_members)",
    )
    .await;
    match counted.as_slice() {
        [row] => match row.as_slice() {
            [SqlValue::Integer(count)] => *count / 2,
            other => panic!("outcome count {other:?}"),
        },
        other => panic!("outcome count {other:?}"),
    }
}
async fn traces(replica: &Replica, scope: &[SqlValue]) -> Vec<String> {
    let [id, blob] = scope else {
        panic!("scope state {scope:?}");
    };
    let mut found = Vec::new();
    for table in stored(
        replica,
        "SELECT m.name,c.type FROM sqlite_schema m JOIN pragma_table_info(m.name) c WHERE m.type='table' AND m.name GLOB 'secure_*' AND m.name<>'secure_keys' AND c.name='scope' ORDER BY m.name",
    )
    .await
    {
        let [SqlValue::Text(name), SqlValue::Text(kind)] = table.as_slice() else {
            panic!("scope keyed table {table:?}");
        };
        let key = if kind == "INTEGER" { id } else { blob };
        let inner = replica.inner.lock(crate::replica::Class::Background).await;
        let held = crate::storage::collect_rows(
            inner
                .conn
                .query(
                    format!("SELECT 1 FROM {name} WHERE scope=? LIMIT 1"),
                    crate::storage::bindings(vec![key.clone()]),
                )
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        if !held.is_empty() {
            found.push(name.clone());
        }
    }
    found
}
async fn retained_handles_agree(replica: &Replica) -> bool {
    stored(
        replica,
        "SELECT (SELECT COUNT(*) FROM secure_handles)=handles FROM secure_retained",
    )
    .await
        == vec![vec![SqlValue::Integer(1)]]
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_mutation_written_under_a_withdrawn_stage_is_uncommitted_and_its_retry_applies_at_the_source()
 {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let replica = opened(&lineage, NARROWING, &dir, &key).await;
    let source = replica.followed_tip().await;
    let publication = switched_to_stage(&lineage, NARROWING, &replica, &key).await;
    let handle = rocky_core::commitment("withdrawal.retry", b"b");
    insert_handled(&replica, handle, "b").await.unwrap();
    assert!(replica.handle_committed(handle).await.unwrap());
    assert_eq!(outcomes(&replica).await, 1);
    withdraw(&lineage, NARROWING, &replica, &key, publication)
        .await
        .unwrap();
    back_at_the_source(&lineage, &replica, source).await;
    assert!(
        !replica.handle_committed(handle).await.unwrap(),
        "a mutation whose frames the withdrawal discarded is no longer committed"
    );
    assert_eq!(replica.lookup_mutation(handle).await.unwrap(), None);
    assert!(retained_handles_agree(&replica).await);
    assert_eq!(
        outcomes(&replica).await,
        0,
        "the outcome signed for the discarded bundle goes with it, so its retry signs a new one"
    );
    assert!(titles(&replica).await.is_empty());
    insert_handled(&replica, handle, "b").await.unwrap();
    assert_eq!(
        titles(&replica).await,
        vec![vec![text("b"), text("retried")]],
        "the retried mutation is written on the source"
    );
    assert!(replica.handle_committed(handle).await.unwrap());
    assert!(retained_handles_agree(&replica).await);
    assert_eq!(outcomes(&replica).await, 1);
    replica.audit(lineage.scope("items")).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn withdrawing_a_stage_that_added_a_written_table_forgets_the_table() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let replica = opened(&lineage, ADDING_TAGS, &dir, &key).await;
    insert(&replica, "a", "written at the source").await;
    let source = replica.followed_tip().await;
    let scopes = stored(
        &replica,
        "SELECT id,scope FROM secure_scope_state ORDER BY id",
    )
    .await;
    let publication = switched_to_stage(&lineage, ADDING_TAGS, &replica, &key).await;
    super::generations::identity_insert(
        &session(&replica).await,
        "INSERT INTO tags(key,label) VALUES(?,?)",
        vec![text("t"), text("written under the stage")],
    )
    .await
    .unwrap();
    assert_eq!(
        super::generations::select(&replica, "SELECT key FROM tags").await,
        vec![vec![text("t")]]
    );
    let added = stored(
        &replica,
        "SELECT id,scope FROM secure_scope_state ORDER BY id",
    )
    .await
    .into_iter()
    .find(|scope| !scopes.contains(scope))
    .unwrap();
    assert!(!traces(&replica, &added).await.is_empty());
    withdraw(&lineage, ADDING_TAGS, &replica, &key, publication)
        .await
        .unwrap();
    back_at_the_source(&lineage, &replica, source).await;
    assert_eq!(
        stored(
            &replica,
            "SELECT id,scope FROM secure_scope_state ORDER BY id"
        )
        .await,
        scopes,
        "the table the stage added leaves no scope behind"
    );
    assert_eq!(traces(&replica, &added).await, Vec::<String>::new());
    assert!(
        stored(
            &replica,
            "SELECT name FROM sqlite_schema WHERE name GLOB '*tags*' OR tbl_name GLOB '*tags*'"
        )
        .await
        .is_empty()
    );
    assert_eq!(
        titles(&replica).await,
        vec![vec![text("a"), text("written at the source")]]
    );
    let items = lineage.scope("items");
    replica.audit(items).await.unwrap();
    assert!(!waits_to_rederive(&replica).await);
    insert(&replica, "c", "written after the withdrawal").await;
    drop(replica);
    let replica = lineage
        .reopen(
            &dir.path().join("a.db"),
            &key,
            ADDING_TAGS.source,
            ADDING_TAGS.source,
        )
        .await
        .unwrap();
    replica.audit(items).await.unwrap();
    assert_eq!(
        titles(&replica).await,
        vec![
            vec![text("a"), text("written at the source")],
            vec![text("c"), text("written after the withdrawal")],
        ]
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_failure_after_a_withdrawal_commits_leaves_the_replica_on_the_source() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let replica = opened(&lineage, NARROWING, &dir, &key).await;
    insert(&replica, "a", "written at the source").await;
    let source = replica.followed_tip().await;
    let publication = switched_to_stage(&lineage, NARROWING, &replica, &key).await;
    insert(&replica, "b", "written under the stage").await;
    replica.fail_next_withdrawal_reload().await;
    assert!(matches!(
        withdraw(&lineage, NARROWING, &replica, &key, publication).await,
        Err(Error::InjectedFailure)
    ));
    back_at_the_source(&lineage, &replica, source).await;
    insert(&replica, "c", "written after the withdrawal").await;
    assert_eq!(
        titles(&replica).await,
        vec![
            vec![text("a"), text("written at the source")],
            vec![text("c"), text("written after the withdrawal")],
        ],
        "a committed withdrawal never puts the stage back in memory"
    );
    replica.audit(lineage.scope("items")).await.unwrap();
    drop(replica);
    let replica = lineage
        .reopen(
            &dir.path().join("a.db"),
            &key,
            NARROWING.source,
            NARROWING.source,
        )
        .await
        .unwrap();
    assert_eq!(replica.followed_tip().await, source);
    assert_eq!(titles(&replica).await.len(), 2);
}
