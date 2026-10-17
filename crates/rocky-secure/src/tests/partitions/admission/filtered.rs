//! A removed partitioned table resolves its original admission only when
//! its own source path and descriptor return.
use super::*;
use rocky_core::{LogicalSchema, RowApplication, TipId};
use rocky_credentials::{
    AuthorMark, BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SEAL_FORMAT, SealAction,
    SealStatement, SignedBookmarkMove, SignedSeal, SignedTipOpening, TIP_OPENING_FORMAT,
    TipOpeningStatement,
};

fn bookmark(f: &Fixture, name: &str, from: Option<TipId>, to: TipId) -> TipStatement {
    TipStatement::Bookmark(
        SignedBookmarkMove::sign(
            BookmarkMoveStatement {
                format: BOOKMARK_MOVE_FORMAT,
                scope: f.domain,
                name: name.into(),
                from,
                to: Some(to),
            },
            &f.publisher,
        )
        .unwrap(),
    )
}

fn removed(f: &Fixture) -> LogicalSchema {
    let mut schema = f.schema.clone();
    let table = &mut schema.tables[0];
    table.sql_name = "elsewhere".into();
    table.create_sql = table.create_sql.replace("items", "elsewhere");
    for index in &mut table.indexes {
        index.name = index.name.replace("items", "elsewhere");
        index.create_sql = index.create_sql.replace("items", "elsewhere");
    }
    schema.views.clear();
    schema
}

pub(super) async fn security(f: &Fixture, replica: &EncryptedReplica<TypedRows>) -> SecureConfig {
    let inner = replica.inner.lock().await;
    SecureConfig {
        publisher: f.publisher.public_key(),
        manifest: f.manifest.clone(),
        revocations: inner.config.revocations.clone(),
        node: inner.config.node.clone(),
        credentials: inner.config.credentials.clone(),
        keys: vec![
            KeyMaterial::new(f.scope("A"), 1, [1; 32]),
            KeyMaterial::new(f.scope("B"), 1, [2; 32]),
        ],
    }
}

async fn switch(
    f: &Fixture,
    replica: &EncryptedReplica<TypedRows>,
    name: &str,
    schema: &LogicalSchema,
) {
    replica.follow_bookmark(f.domain, name).await.unwrap();
    let tip = replica
        .tip_graph(f.domain)
        .await
        .unwrap()
        .bookmark(name)
        .unwrap()
        .unwrap();
    let app = TypedRows::new(schema.clone(), vec![f.domain.domain]).unwrap();
    if replica.pending_schema().await.unwrap().is_some() {
        replica
            .install_tip_schema(tip, app.manifest(), vec![])
            .await
            .unwrap();
        let readers = [(schema.tables[0].sql_name.clone(), [9; 32])].into();
        assert!(
            replica
                .switch_schema(
                    tip,
                    app,
                    crate::SchemaConfig {
                        security: security(f, replica).await,
                        tables: Some((schema.clone(), f.domain, readers)),
                    }
                )
                .await
                .unwrap()
        );
    }
}

fn removed_path(
    f: &Fixture,
    source: TipId,
    frontier: Vec<AuthorMark>,
) -> (LogicalSchema, Vec<TipStatement>) {
    let sealed = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: f.domain,
            tip: source,
            frontier,
            action: SealAction::Continue {
                schema: f.app().manifest().hash(),
            },
            recovery: None,
        },
        &f.publisher,
    )
    .unwrap();
    let next = sealed.successor().unwrap().unwrap();
    let absent = removed(f);
    let absent_app = TypedRows::new(absent.clone(), vec![f.domain.domain]).unwrap();
    let opening = SignedTipOpening::sign(
        TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            schema: absent_app.manifest().hash(),
            scope: f.domain,
            seal: sealed.id().unwrap(),
            minimum_in_sync: 1,
        },
        &f.publisher,
    )
    .unwrap();
    let dev = opening.tip().unwrap();
    let statements = [
        bookmark(f, "main", None, source),
        TipStatement::Seal(sealed),
        bookmark(f, "main", Some(source), next),
        TipStatement::Opening(opening),
        bookmark(f, "dev", None, dev),
    ];
    (absent, statements.into())
}

mod audit;
mod carry;
mod delete;
mod held;
mod ordinary;
mod restoration;
