use super::*;
use crate::TipStatement;
use rocky_credentials::{
    BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SEAL_FORMAT, SealAction, SealStatement,
    SignedBookmarkMove, SignedSeal,
};

#[tokio::test]
async fn excluding_a_parent_delete_restores_dependents_and_matches_the_rebuild() {
    let shop = Shop::new();
    let dir = TempDir::new().unwrap();
    let replica = shop
        .open(&dir.path().join("scope.db"), &PrivateKey::new(), 31)
        .await;
    let sql = shop.sql(&replica).await;
    let tip = replica.followed_tip().await;
    let movement = |from, to| {
        TipStatement::Bookmark(
            SignedBookmarkMove::sign(
                BookmarkMoveStatement {
                    format: BOOKMARK_MOVE_FORMAT,
                    scope: shop.domain,
                    name: "main".into(),
                    from,
                    to: Some(to),
                },
                &shop.publisher,
            )
            .unwrap(),
        )
    };
    replica
        .publish_tip_statements(&[movement(None, tip)])
        .await
        .unwrap();
    replica.follow_bookmark(shop.domain, "main").await.unwrap();
    let (parents, _) = commit(
        &sql,
        &[
            ("INSERT INTO lists(title,id) VALUES('kept',?id)", vec![]),
            (
                "INSERT INTO lists(title,id) VALUES('excluded delete',?id)",
                vec![],
            ),
        ],
    )
    .await;
    commit(
        &sql,
        &[
            (
                "INSERT INTO items(list,code,id) VALUES(?,'cascade',?id)",
                vec![integer(parents[1])],
            ),
            (
                "INSERT INTO items(list,code,moved,id) VALUES(?,'nullable',?,?id)",
                vec![integer(parents[0]), integer(parents[1])],
            ),
        ],
    )
    .await;
    let frontier = replica.seal_frontier(shop.domain, false).await.unwrap();
    let (row, before) = replica
        .snapshot(shop.lists)
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|(_, fields)| {
            let table = shop.schema.table("lists").unwrap();
            let title = shop.schema.field_id(table, table.column("title").unwrap());
            fields.get(&title).is_some_and(|bytes| {
                rocky_core::SqlValue::decode(&mut bytes.as_slice())
                    .is_ok_and(|value| value == text("excluded delete"))
            })
        })
        .unwrap();
    replica
        .author(
            [203; 32],
            vec![RowChange {
                scope: shop.lists,
                row,
                before: Some(before),
                after: None,
            }],
            vec![],
        )
        .await
        .unwrap();
    let live = rows(&sql, ITEMS).await;
    assert_eq!(live.len(), 1);
    assert_eq!(live[0][4], SqlValue::Null);
    let seal = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: shop.domain,
            tip,
            frontier,
            action: SealAction::Continue {
                schema: shop.schema.manifest(&[DomainId([1; 16])]).hash(),
            },
            recovery: None,
        },
        &shop.publisher,
    )
    .unwrap();
    replica
        .publish_tip_statements(&[
            TipStatement::Seal(seal.clone()),
            movement(Some(tip), seal.successor().unwrap().unwrap()),
        ])
        .await
        .unwrap();
    replica.finalize_path(shop.domain).await.unwrap();
    assert!(
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .compare_finalization
    );
    assert_eq!(rows(&sql, ITEMS).await, live);
    assert!(
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .finalization_oracle_time
            > std::time::Duration::ZERO
    );
    for scope in [shop.lists, shop.items] {
        replica.audit(scope).await.unwrap();
    }
}
