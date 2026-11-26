use super::*;
use crate::TipStatement;
use rocky_core::{RowApplication, TipId};
use rocky_credentials::{
    BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SEAL_FORMAT, SealAction, SealStatement,
    SignedBookmarkMove, SignedSeal,
};

fn bookmark(c: &Constrained, from: Option<TipId>, to: TipId) -> TipStatement {
    TipStatement::Bookmark(
        SignedBookmarkMove::sign(
            BookmarkMoveStatement {
                format: BOOKMARK_MOVE_FORMAT,
                scope: c.domain,
                name: "main".into(),
                from,
                to: Some(to),
            },
            &c.publisher,
        )
        .unwrap(),
    )
}

async fn follow_main(c: &Constrained, replica: &EncryptedReplica<TypedRows>) {
    let genesis = replica.followed_tip().await;
    replica
        .publish_tip_statements(&[bookmark(c, None, genesis)])
        .await
        .unwrap();
    replica.follow_bookmark(c.domain, "main").await.unwrap();
}

async fn foreign_key_violations(replica: &EncryptedReplica<TypedRows>) -> usize {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    crate::storage::rows(&inner.conn, "SELECT * FROM pragma_foreign_key_check")
        .await
        .unwrap()
        .len()
}

async fn raw_count(replica: &EncryptedReplica<TypedRows>, table: &str) -> i64 {
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    match crate::storage::rows(&inner.conn, &format!("SELECT COUNT(*) FROM {table}"))
        .await
        .unwrap()[0]
        .as_slice()
    {
        [SqlValue::Integer(count)] => *count,
        other => panic!("unexpected count row: {other:?}"),
    }
}

async fn switch_to_v2(
    v1: &Constrained,
    v2_json: serde_json::Value,
    device: &Device,
) -> Constrained {
    let mut json = v2_json;
    json["publisher"] = v1
        .publisher
        .public_key()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        .into();
    let schema = SchemaDefinition::parse(&serde_json::to_vec(&json).unwrap())
        .unwrap()
        .build()
        .unwrap();
    schema.supported_on(rocky_core::Profile::Secure).unwrap();
    let manifest = schema.manifest(&[v1.domain.domain]);
    let v2 = Constrained {
        publisher: v1.publisher.clone(),
        schema,
        manifest: SignedManifest::sign(&manifest, &v1.publisher).unwrap(),
        domain: v1.domain,
        tenant: None,
        extra_tenants: Vec::new(),
    };
    let current = device.replica.followed_tip().await;
    let seal = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: v1.domain,
            tip: current,
            frontier: device
                .replica
                .seal_frontier(v1.domain, false)
                .await
                .unwrap(),
            action: SealAction::Continue {
                schema: v2.app().manifest().hash(),
            },
        },
        &v1.publisher,
    )
    .unwrap();
    let target = seal.successor().unwrap().unwrap();
    device
        .replica
        .publish_tip_statements(&[
            TipStatement::Seal(seal),
            bookmark(v1, Some(current), target),
        ])
        .await
        .unwrap();
    device
        .replica
        .install_tip_schema(target, v2.app().manifest().clone(), b"v2".to_vec())
        .await
        .unwrap();
    let config = SecureConfig {
        publisher: v2.publisher.public_key(),
        manifest: v1.manifest.clone(),
        revocations: RevocationSet::new(),
        node: device.node.clone(),
        credentials: v2.credentials(&device.node),
        keys: v2
            .names()
            .into_iter()
            .map(|name| KeyMaterial::new(v2.scope(name), 1, v2.key(v2.scope(name))))
            .collect(),
    };
    let readers = v2
        .names()
        .into_iter()
        .map(|name| (name.to_string(), v2.scope(name).encryption))
        .collect();
    device
        .replica
        .switch_schema(
            target,
            v2.app(),
            crate::SchemaConfig {
                security: config,
                tables: Some((v2.schema.clone(), v2.domain, readers)),
            },
        )
        .await
        .unwrap();
    assert_eq!(device.replica.followed_tip().await, target);
    v2
}

async fn import_checkpoint(
    c: &Constrained,
    source: &Device,
    target: &Device,
    scope: ResourceScope,
) {
    let credentials = c.credentials(&target.node);
    source
        .replica
        .admit_peer(
            target.node.public_key(),
            &credentials,
            target.replica.followed_tip().await,
        )
        .await
        .unwrap();
    let offered = source
        .replica
        .checkpoint_offer(target.node.public_key(), &credentials, scope)
        .await
        .unwrap()
        .expect("offered checkpoint");
    let checkpoint = offered.id().unwrap();
    target.replica.begin_rebootstrap(offered).await.unwrap();
    let mut after = None;
    loop {
        let page = source
            .replica
            .checkpoint_page(
                target.node.public_key(),
                &credentials,
                scope,
                checkpoint,
                after,
                64 * 1024,
            )
            .await
            .unwrap();
        after = page.next;
        target.replica.stage_checkpoint_page(page).await.unwrap();
        if after.is_none() {
            break;
        }
    }
    target.replica.finish_rebootstrap(scope).await.unwrap();
}

async fn seal_and_compact(c: &Constrained, device: &Device) {
    let tip = device.replica.followed_tip().await;
    let seal = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: c.domain,
            tip,
            frontier: device.replica.seal_frontier(c.domain, false).await.unwrap(),
            action: SealAction::Continue {
                schema: c.app().manifest().hash(),
            },
        },
        &c.publisher,
    )
    .unwrap();
    let next = seal.successor().unwrap().unwrap();
    device
        .replica
        .publish_tip_statements(&[TipStatement::Seal(seal), bookmark(c, Some(tip), next)])
        .await
        .unwrap();
    device.replica.compact(c.domain, 0).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn self_referencing_restrict_table_handles_insert_rewind_rebuild_clear_and_v2_drop() {
    let c = Constrained::with(
        serde_json::json!({
            "format": 10,
            "application": "self-ref-restrict",
            "tables": [
                replayed(
                    "nodes",
                    serde_json::json!([
                        {"name": "name", "type": "text", "not_null": true},
                        {"name": "parent", "type": "blob"}
                    ]),
                    serde_json::json!({
                        "foreign_keys": [
                            {"columns": ["parent"], "table": "nodes", "on_delete": "restrict"}
                        ]
                    }),
                ),
                replayed(
                    "notes",
                    serde_json::json!([{"name": "body", "type": "text", "not_null": true}]),
                    serde_json::json!({}),
                ),
            ],
        }),
        |_| {},
    );
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut peers, _, _) = devices(&c, root.path(), 3, &[]).await;
    follow_main(&c, &peers[0].replica).await;
    crate::tests::exchange_graphs(&peers[0].replica, &peers[1].replica, c.domain).await;
    crate::tests::exchange_graphs(&peers[0].replica, &peers[2].replica, c.domain).await;
    peers[1]
        .replica
        .follow_bookmark(c.domain, "main")
        .await
        .unwrap();
    peers[2]
        .replica
        .follow_bookmark(c.domain, "main")
        .await
        .unwrap();

    let first_tree = peers[0]
        .commit(
            &c,
            &[
                "INSERT INTO nodes(name,parent,id) VALUES('root',NULL,#id)",
                "INSERT INTO nodes(name,parent,id) VALUES('child',NULL,#id)",
            ],
        )
        .await
        .unwrap();
    let cycle = peers[0]
        .commit(
            &c,
            &[
                &format!(
                    "UPDATE nodes SET parent={} WHERE id={}",
                    first_tree[0], first_tree[1]
                ),
                "INSERT INTO nodes(name,parent,id) VALUES('cycle-a',NULL,#id)",
                "INSERT INTO nodes(name,parent,id) VALUES('cycle-b',NULL,#id)",
            ],
        )
        .await
        .unwrap();
    let (cycle_a, cycle_b) = (cycle[0], cycle[1]);
    peers[0]
        .commit(
            &c,
            &[
                &format!("UPDATE nodes SET parent={cycle_b} WHERE id={cycle_a}"),
                &format!("UPDATE nodes SET parent={cycle_a} WHERE id={cycle_b}"),
                "INSERT INTO notes(body,id) VALUES('kept',#id)",
            ],
        )
        .await
        .unwrap();
    let frames0 = peers[0].fresh().await;

    let peer1_tree = peers[1]
        .commit(
            &c,
            &["INSERT INTO nodes(name,parent,id) VALUES('b-root',NULL,#id)"],
        )
        .await
        .unwrap();
    peers[1]
        .commit(
            &c,
            &[&format!(
                "INSERT INTO nodes(name,parent,id) VALUES('b-child',{},#id)",
                peer1_tree[0]
            )],
        )
        .await
        .unwrap();
    let frames1 = peers[1].fresh().await;

    peers[1].receive(&frames0).await;
    peers[0].receive(&frames1).await;
    assert_eq!(peers[0].rows(&c).await, peers[1].rows(&c).await);
    assert_eq!(peers[0].rows(&c).await["nodes"].len(), 6);
    assert_eq!(foreign_key_violations(&peers[0].replica).await, 0);
    assert_eq!(foreign_key_violations(&peers[1].replica).await, 0);

    peers[0]
        .replica
        .rebuild_projection(c.scope("nodes"))
        .await
        .unwrap();
    assert_eq!(peers[0].rows(&c).await, peers[1].rows(&c).await);
    assert_eq!(foreign_key_violations(&peers[0].replica).await, 0);

    let obsolete = peers[2]
        .commit(
            &c,
            &["INSERT INTO nodes(name,parent,id) VALUES('obs-root',NULL,#id)"],
        )
        .await
        .unwrap();
    peers[2]
        .commit(
            &c,
            &[&format!(
                "INSERT INTO nodes(name,parent,id) VALUES('obs-child',{},#id)",
                obsolete[0]
            )],
        )
        .await
        .unwrap();
    seal_and_compact(&c, &peers[0]).await;
    crate::tests::exchange_graphs(&peers[0].replica, &peers[2].replica, c.domain).await;
    import_checkpoint(&c, &peers[0], &peers[2], c.scope("nodes")).await;
    import_checkpoint(&c, &peers[0], &peers[2], c.scope("notes")).await;
    assert_eq!(peers[2].rows(&c).await["nodes"].len(), 8);
    assert_eq!(foreign_key_violations(&peers[2].replica).await, 0);

    peers[2]
        .replica
        .drop_local_history_for_test(c.scope("nodes"))
        .await;
    assert_eq!(raw_count(&peers[2].replica, "nodes").await, 0);
    assert_eq!(foreign_key_violations(&peers[2].replica).await, 0);

    let v2 = switch_to_v2(
        &c,
        serde_json::json!({
            "format": 10,
            "application": "self-ref-restrict",
            "tables": [
                replayed(
                    "notes",
                    serde_json::json!([{"name": "body", "type": "text", "not_null": true}]),
                    serde_json::json!({}),
                ),
            ],
        }),
        &peers[0],
    )
    .await;
    assert_eq!(peers[0].rows(&v2).await["notes"].len(), 1);
    assert_eq!(foreign_key_violations(&peers[0].replica).await, 0);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn mutual_restrict_cycle_handles_insert_rewind_rebuild_clear_and_v2_drop() {
    let c = Constrained::with(
        serde_json::json!({
            "format": 10,
            "application": "mutual-restrict-cycle",
            "tables": [
                replayed(
                    "hens",
                    serde_json::json!([
                        {"name": "name", "type": "text", "not_null": true},
                        {"name": "egg", "type": "blob"}
                    ]),
                    serde_json::json!({
                        "foreign_keys": [
                            {"columns": ["egg"], "table": "eggs", "on_delete": "restrict"}
                        ]
                    }),
                ),
                replayed(
                    "eggs",
                    serde_json::json!([
                        {"name": "name", "type": "text", "not_null": true},
                        {"name": "hen", "type": "blob"}
                    ]),
                    serde_json::json!({
                        "foreign_keys": [
                            {"columns": ["hen"], "table": "hens", "on_delete": "restrict"}
                        ]
                    }),
                ),
                replayed(
                    "barns",
                    serde_json::json!([{"name": "name", "type": "text", "not_null": true}]),
                    serde_json::json!({}),
                ),
            ],
        }),
        |_| {},
    );
    let root = rocky_platform::fs::Scratch::new().await;
    let (mut peers, _, _) = devices(&c, root.path(), 3, &[]).await;
    follow_main(&c, &peers[0].replica).await;
    crate::tests::exchange_graphs(&peers[0].replica, &peers[1].replica, c.domain).await;
    crate::tests::exchange_graphs(&peers[0].replica, &peers[2].replica, c.domain).await;
    peers[1]
        .replica
        .follow_bookmark(c.domain, "main")
        .await
        .unwrap();
    peers[2]
        .replica
        .follow_bookmark(c.domain, "main")
        .await
        .unwrap();

    let pair0 = peers[0]
        .commit(
            &c,
            &[
                "INSERT INTO hens(name,egg,id) VALUES('hen-0',NULL,#id)",
                "INSERT INTO eggs(name,hen,id) VALUES('egg-0',NULL,#id)",
                "INSERT INTO barns(name,id) VALUES('red',#id)",
            ],
        )
        .await
        .unwrap();
    peers[0]
        .commit(
            &c,
            &[
                &format!("UPDATE eggs SET hen={} WHERE id={}", pair0[0], pair0[1]),
                &format!("UPDATE hens SET egg={} WHERE id={}", pair0[1], pair0[0]),
            ],
        )
        .await
        .unwrap();
    let frames0 = peers[0].fresh().await;

    let pair1 = peers[1]
        .commit(
            &c,
            &[
                "INSERT INTO hens(name,egg,id) VALUES('hen-1',NULL,#id)",
                "INSERT INTO eggs(name,hen,id) VALUES('egg-1',NULL,#id)",
            ],
        )
        .await
        .unwrap();
    peers[1]
        .commit(
            &c,
            &[
                &format!("UPDATE eggs SET hen={} WHERE id={}", pair1[0], pair1[1]),
                &format!("UPDATE hens SET egg={} WHERE id={}", pair1[1], pair1[0]),
            ],
        )
        .await
        .unwrap();
    let frames1 = peers[1].fresh().await;

    peers[1].receive(&frames0).await;
    peers[0].receive(&frames1).await;
    assert_eq!(peers[0].rows(&c).await, peers[1].rows(&c).await);
    assert_eq!(peers[0].rows(&c).await["hens"].len(), 2);
    assert_eq!(peers[0].rows(&c).await["eggs"].len(), 2);
    assert_eq!(foreign_key_violations(&peers[0].replica).await, 0);
    assert_eq!(foreign_key_violations(&peers[1].replica).await, 0);

    peers[0]
        .replica
        .rebuild_projection(c.scope("hens"))
        .await
        .unwrap();
    peers[0]
        .replica
        .rebuild_projection(c.scope("eggs"))
        .await
        .unwrap();
    assert_eq!(peers[0].rows(&c).await, peers[1].rows(&c).await);
    assert_eq!(foreign_key_violations(&peers[0].replica).await, 0);

    let obs = peers[2]
        .commit(
            &c,
            &[
                "INSERT INTO hens(name,egg,id) VALUES('obs-hen',NULL,#id)",
                "INSERT INTO eggs(name,hen,id) VALUES('obs-egg',NULL,#id)",
            ],
        )
        .await
        .unwrap();
    peers[2]
        .commit(
            &c,
            &[
                &format!("UPDATE eggs SET hen={} WHERE id={}", obs[0], obs[1]),
                &format!("UPDATE hens SET egg={} WHERE id={}", obs[1], obs[0]),
            ],
        )
        .await
        .unwrap();
    seal_and_compact(&c, &peers[0]).await;
    crate::tests::exchange_graphs(&peers[0].replica, &peers[2].replica, c.domain).await;
    import_checkpoint(&c, &peers[0], &peers[2], c.scope("hens")).await;
    import_checkpoint(&c, &peers[0], &peers[2], c.scope("eggs")).await;
    import_checkpoint(&c, &peers[0], &peers[2], c.scope("barns")).await;
    peers[2].replica.rederive().await.unwrap();
    assert_eq!(peers[2].rows(&c).await["hens"].len(), 3);
    assert_eq!(peers[2].rows(&c).await["eggs"].len(), 3);
    assert_eq!(foreign_key_violations(&peers[2].replica).await, 0);

    peers[2]
        .replica
        .drop_local_history_for_test(c.scope("hens"))
        .await;
    assert_eq!(raw_count(&peers[2].replica, "hens").await, 0);
    assert_eq!(raw_count(&peers[2].replica, "eggs").await, 0);
    assert_eq!(foreign_key_violations(&peers[2].replica).await, 0);

    let v2 = switch_to_v2(
        &c,
        serde_json::json!({
            "format": 10,
            "application": "mutual-restrict-cycle",
            "tables": [
                replayed(
                    "barns",
                    serde_json::json!([{"name": "name", "type": "text", "not_null": true}]),
                    serde_json::json!({}),
                ),
            ],
        }),
        &peers[0],
    )
    .await;
    assert_eq!(peers[0].rows(&v2).await["barns"].len(), 1);
    assert_eq!(foreign_key_violations(&peers[0].replica).await, 0);
}
