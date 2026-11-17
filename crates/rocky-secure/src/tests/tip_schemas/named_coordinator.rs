use super::*;
use rocky_coordinator::coordinated::MemoryLog;
use rocky_credentials::{
    Coordination, DATABASE_SETTINGS_FORMAT, DatabaseSettingsStatement, SignedDatabaseSettings,
    SignedTipOpening, TipOpeningStatement,
};
use std::sync::Arc;

fn refusal_code(error: &Error) -> Option<&'static str> {
    match error {
        Error::Application(inner) => inner.downcast_ref::<crate::Refusal>().map(|r| r.code),
        _ => None,
    }
}

struct Staged {
    lineage: Lineage,
    dir: Scratch,
    source: EncryptedReplica<rocky_core::TypedRows>,
    publication: Commitment,
    close: SignedSeal,
    public: Vec<TipStatement>,
}
async fn committed_stage(seed: u8, log: Option<&MemoryLog>) -> Staged {
    let lineage = Lineage::pinned(seed);
    let dir = Scratch::new().await;
    let key = PrivateKey::from_bytes(&[seed + 1; 32]);
    let source = lineage
        .open_certified(
            &dir.path().join("source.db"),
            (lineage.publisher(), &key),
            2,
            None,
        )
        .await
        .unwrap();
    let root = source.followed_tip().await;
    let close = SignedSeal::sign(
        rocky_credentials::SealStatement {
            format: rocky_credentials::SEAL_FORMAT,
            scope: lineage.domain,
            tip: root,
            frontier: vec![],
            action: SealAction::Close,
            recovery: None,
        },
        lineage.publisher(),
    )
    .unwrap();
    let opening = SignedTipOpening::sign(
        TipOpeningStatement {
            format: rocky_credentials::TIP_OPENING_FORMAT,
            scope: lineage.domain,
            seal: close.id().unwrap(),
            schema: lineage.definition(3).hash(),
            minimum_in_sync: 1,
        },
        lineage.publisher(),
    )
    .unwrap();
    let target = opening.tip().unwrap();
    let movement = |from, to| {
        TipStatement::Bookmark(
            rocky_credentials::SignedBookmarkMove::sign(
                rocky_credentials::BookmarkMoveStatement {
                    format: rocky_credentials::BOOKMARK_MOVE_FORMAT,
                    scope: lineage.domain,
                    name: "dev".into(),
                    from,
                    to: Some(to),
                },
                lineage.publisher(),
            )
            .unwrap(),
        )
    };
    let minimum = TipStatement::GenesisLine(
        rocky_credentials::SignedGenesisLine::sign(
            rocky_credentials::GenesisLineStatement {
                format: rocky_credentials::GENESIS_LINE_FORMAT,
                scope: lineage.domain,
                minimum_in_sync: 1,
            },
            lineage.publisher(),
        )
        .unwrap(),
    );
    let public = vec![minimum, movement(None, root)];
    source.publish_tip_statements(&public).await.unwrap();
    source.follow_bookmark(lineage.domain, "dev").await.unwrap();
    if let Some(log) = log {
        source.set_domain_log(Some(Arc::new(log.client(key.public_key()))));
    }
    let publication = rocky_core::commitment("rocky.sdk.migration.v3", &target.0);
    let stage = crate::SchemaStage {
        publication,
        scope: lineage.domain,
        bookmark: "dev".into(),
        source: root,
        target,
        from_version: 2,
        coordinated: log.is_some(),
        statements: vec![
            TipStatement::Seal(close.clone()),
            TipStatement::Opening(opening),
            movement(Some(root), target),
        ],
        program: vec![],
        parents: vec![],
    };
    source.stage_schema(stage.clone()).await.unwrap();
    source
        .install_tip_schema(target, lineage.definition(3), b"verified".to_vec())
        .await
        .unwrap();
    let mut config = lineage.config(3, &key);
    config.credentials = lineage.certified(3, lineage.publisher(), &key);
    config.manifest = lineage.config(2, &key).manifest;
    source
        .switch_schema(
            target,
            lineage.app(3),
            crate::SchemaConfig {
                security: config,
                tables: Some(lineage.projection(3)),
            },
        )
        .await
        .unwrap();
    let writer = PrivateKey::from_bytes(&[seed + 2; 32]);
    source
        .register_line_node(crate::LineNode {
            credentials: lineage.certified(3, lineage.publisher(), &writer),
            key: writer,
        })
        .await
        .unwrap();
    let migration = source
        .session([lineage.scope("items")].into())
        .await
        .unwrap()
        .migration_sql(publication, crate::sql::SqlOptions::default())
        .await
        .unwrap();
    migration.run("BEGIN", vec![]).await.unwrap();
    migration.commit_publication(publication).await.unwrap();
    drop(migration);
    let (intent, writer) = source.migration_intent(publication).await.unwrap().unwrap();
    let descriptor = rocky_credentials::SignedMigrationPublication::endorse(
        intent,
        lineage.publisher(),
        &CredentialChain::default(),
        writer,
    )
    .unwrap();
    source
        .admit_migration_publication(descriptor)
        .await
        .unwrap();
    Staged {
        lineage,
        dir,
        source,
        publication,
        close,
        public,
    }
}
fn named(lineage: &Lineage, log: &MemoryLog) -> TipStatement {
    TipStatement::Settings(
        SignedDatabaseSettings::sign(
            DatabaseSettingsStatement {
                format: DATABASE_SETTINGS_FORMAT,
                scope: lineage.domain,
                revision: 1,
                coordination: Coordination::Coordinated {
                    bundle: log.bundle(),
                },
            },
            lineage.publisher(),
        )
        .unwrap(),
    )
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_stage_made_before_the_settings_named_a_coordinator_is_never_released_unended() {
    let Staged {
        lineage,
        source,
        publication,
        ..
    } = committed_stage(193, None).await;
    source
        .receive_tip_statements(&[named(&lineage, &MemoryLog::new())])
        .await
        .unwrap();
    let refused = source
        .release_uncoordinated_schema(publication)
        .await
        .unwrap_err();
    assert_eq!(
        refusal_code(&refused),
        Some("coordinator_required"),
        "a stage made before the settings arrived is not released without finality: {refused}"
    );
    assert!(
        source.private_schema().await.unwrap(),
        "the stage stays unreleased"
    );
    assert!(
        source.tip_statements(None, 32).await.is_err(),
        "nothing is offered while the unended stage stays private"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_node_without_the_named_log_stages_no_schema() {
    let lineage = Lineage::pinned(196);
    let dir = Scratch::new().await;
    let key = PrivateKey::from_bytes(&[197; 32]);
    let source = lineage
        .open_certified(
            &dir.path().join("source.db"),
            (lineage.publisher(), &key),
            2,
            None,
        )
        .await
        .unwrap();
    let log = MemoryLog::new();
    let named = TipStatement::Settings(
        SignedDatabaseSettings::sign(
            DatabaseSettingsStatement {
                format: DATABASE_SETTINGS_FORMAT,
                scope: lineage.domain,
                revision: 1,
                coordination: Coordination::Coordinated {
                    bundle: log.bundle(),
                },
            },
            lineage.publisher(),
        )
        .unwrap(),
    );
    source.receive_tip_statements(&[named]).await.unwrap();
    let root = source.followed_tip().await;
    let other = MemoryLog::new();
    for (composed, code) in [
        (None, "coordinator_required"),
        (
            Some(Arc::new(other.client(key.public_key())) as Arc<dyn rocky_core::DomainLog>),
            "coordinator_mismatch",
        ),
    ] {
        source.set_domain_log(composed);
        let refused = source
            .stage_schema(crate::SchemaStage {
                publication: [7; 32],
                scope: lineage.domain,
                bookmark: "main".into(),
                source: root,
                target: root,
                from_version: 2,
                coordinated: true,
                statements: Vec::new(),
                program: vec![],
                parents: vec![],
            })
            .await
            .unwrap_err();
        assert_eq!(refusal_code(&refused), Some(code), "{refused}");
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_staged_close_final_only_at_another_coordinator_is_never_released() {
    let other = MemoryLog::new();
    let Staged {
        lineage,
        dir,
        source,
        publication,
        close,
        public,
    } = committed_stage(198, Some(&other)).await;
    other.configure(lineage.domain, 1).unwrap();
    let (identity, node) = (PrivateKey::new(), PrivateKey::new());
    let relay = lineage
        .open_trusted(&dir.path().join("relay.db"), (&identity, &node), 2, None)
        .await
        .unwrap();
    relay.receive_tip_statements(&public).await.unwrap();
    relay.follow_bookmark(lineage.domain, "dev").await.unwrap();
    relay.set_domain_log(Some(Arc::new(other.client(node.public_key()))));
    let proof = match relay
        .relay_staged_close(&close.to_bytes().unwrap())
        .await
        .unwrap()
    {
        crate::StagedCloseStatus::Finality(proof) => proof,
        status => panic!("the relay ends the other log while no settings name one: {status:?}"),
    };

    source
        .receive_tip_statements(&[named(&lineage, &MemoryLog::new())])
        .await
        .unwrap();
    let graph = source.tip_graph(lineage.domain).await.unwrap();
    assert!(!graph.counts_finality(&proof).unwrap());
    let refused = source
        .release_staged_schema(publication, proof)
        .await
        .unwrap_err();
    assert_eq!(
        refusal_code(&refused),
        Some("coordinator_mismatch"),
        "{refused}"
    );
    assert!(
        source.private_schema().await.unwrap(),
        "the stage stays unreleased"
    );
}
