//! Existence logs folded at seals: a holder of every record compacts them
//! into a base; a device holding only the child imports the signed base.
use super::*;
use crate::{ExistenceBasis, TipStatement};
use rocky_core::TipId;
use rocky_credentials::{
    BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SEAL_FORMAT, SealAction, SealStatement,
    SignedBookmarkMove, SignedExistenceBase, SignedSeal,
};

fn bookmark(erp: &Erp, from: Option<TipId>, to: TipId) -> TipStatement {
    TipStatement::Bookmark(
        SignedBookmarkMove::sign(
            BookmarkMoveStatement {
                format: BOOKMARK_MOVE_FORMAT,
                scope: erp.domain,
                name: "main".into(),
                from,
                to: Some(to),
            },
            &erp.publisher,
        )
        .unwrap(),
    )
}
/// The owner seals `device`'s followed tip at what it holds and moves
/// `main` on; returns the seal and the statements for other devices.
pub(super) async fn seal(erp: &Erp, device: &Device) -> (SignedSeal, Vec<TipStatement>) {
    let replica = &device.replica;
    let tip = replica.followed_tip().await;
    let schema = replica
        .tip_graph(erp.domain)
        .await
        .unwrap()
        .schema(tip)
        .unwrap();
    let seal = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: erp.domain,
            tip,
            frontier: replica.seal_frontier(erp.domain, false).await.unwrap(),
            action: SealAction::Continue { schema },
            recovery: None,
        },
        &erp.publisher,
    )
    .unwrap();
    let statements = vec![
        TipStatement::Seal(seal.clone()),
        bookmark(erp, Some(tip), seal.successor().unwrap().unwrap()),
    ];
    replica.publish_tip_statements(&statements).await.unwrap();
    (seal, statements)
}
async fn follow(erp: &Erp, device: &Device, statements: &[TipStatement]) {
    device
        .replica
        .receive_tip_statements(statements)
        .await
        .unwrap();
    device
        .replica
        .follow_bookmark(erp.domain, "main")
        .await
        .unwrap();
}
/// Records the tip `to` follows on `from`, as a link hello does.
async fn admit(from: &Device, to: &Device) {
    from.replica
        .admit_peer(
            to.node.public_key(),
            &to.credentials,
            to.replica.followed_tip().await,
        )
        .await
        .unwrap();
}
/// Copies the records of `table` that `to` lacks from `from`.
async fn existence(erp: &Erp, from: &Device, to: &Device, table: &str) {
    admit(from, to).await;
    erp.existence(from, to, table).await;
}
async fn count(device: &Device, sql: &str) -> i64 {
    match stored(device, sql).await.as_slice() {
        [row] => match row.as_slice() {
            [SqlValue::Integer(count)] => *count,
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}
/// Imports `from`'s offered base of `table` into `to`, page by page.
async fn import_base(erp: &Erp, from: &Device, to: &Device, table: &str) {
    let scope = erp.scope(table);
    let peer = to.node.public_key();
    admit(from, to).await;
    let offered = from
        .replica
        .existence_base_offer(peer, &to.credentials, scope)
        .await
        .unwrap()
        .expect("a signed base is offered");
    let id = offered.id().unwrap();
    let (mut after, mut complete) = to.replica.begin_existence_base(offered).await.unwrap();
    while !complete {
        let page = from
            .replica
            .existence_base_page(peer, &to.credentials, scope, id, after, 2)
            .await
            .unwrap();
        after = page.next;
        complete = after.is_none();
        to.replica.stage_existence_base(scope, page).await.unwrap();
    }
    to.replica.finish_existence_base(scope).await.unwrap();
}

#[tokio::test]
async fn a_holder_folds_the_existence_log_at_a_seal_and_a_child_holder_imports_its_signed_base() {
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    // A holds everything, B items only and follows the log from the start.
    let a = erp
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 11, &TABLES)
        .await;
    let b = erp
        .open(&dir.path().join("b.db"), &PrivateKey::new(), 12, &["items"])
        .await;
    let genesis = a.replica.followed_tip().await;
    let created = vec![bookmark(&erp, None, genesis)];
    follow(&erp, &a, &created).await;
    follow(&erp, &b, &created).await;
    let (sa, sb) = (erp.sql(&a).await, erp.sql(&b).await);
    let lists = erp.scope("lists");

    let (ids, _) = commit(
        &sa,
        &[
            (
                "INSERT INTO lists(title,id) VALUES('groceries',?id)",
                vec![],
            ),
            ("INSERT INTO lists(title,id) VALUES('hardware',?id)", vec![]),
        ],
    )
    .await;
    let (groceries, hardware) = (ids[0], ids[1]);
    let mut items = Vec::new();
    for (list, code) in [(groceries, "milk"), (hardware, "nails")] {
        let (_, frames) = commit(
            &sa,
            &[(
                "INSERT INTO items(list,code,id) VALUES(?,?,?id)",
                vec![integer(list), text(code)],
            )],
        )
        .await;
        items.extend(frames);
    }
    for cycle in 0..64 {
        let (id, _) = commit(
            &sa,
            &[(
                &format!("INSERT INTO lists(title,id) VALUES('churn{cycle}',?id)"),
                vec![],
            )],
        )
        .await;
        commit(
            &sa,
            &[("DELETE FROM lists WHERE id=?", vec![integer(id[0])])],
        )
        .await;
    }
    commit(
        &sa,
        &[("DELETE FROM lists WHERE id=?", vec![integer(hardware)])],
    )
    .await;
    deliver(&b, &items).await;
    existence(&erp, &a, &b, "lists").await;
    let milk = rows(&sa, ITEMS).await;
    assert_eq!(milk.len(), 1, "the hardware item is orphaned");
    assert_eq!(rows(&sb, ITEMS).await, milk);
    let logged = "SELECT count(*) FROM secure_existence_log";
    assert_eq!(count(&a, logged).await, 131);

    let (sealed, statements) = seal(&erp, &a).await;
    follow(&erp, &b, &[created.clone(), statements.clone()].concat()).await;
    let report = a.replica.compact(erp.domain, 0).await.unwrap();
    assert_eq!(report.waiting, None);
    assert_eq!(report.compacted, Some(sealed.id().unwrap()));
    // Following the successor ends sessions of the sealed tip.
    let sa = erp.sql(&a).await;
    // Only the live list's creation is still evidenced; dead rows go,
    // and their ids stay retired by the base's identity marks.
    assert_eq!(count(&a, logged).await, 0);
    assert_eq!(
        stored(&a, "SELECT id FROM secure_existence").await,
        vec![vec![integer(groceries)]]
    );
    assert_eq!(
        a.replica.existence(lists, hardware).await.unwrap(),
        Some(false)
    );
    assert_eq!(
        a.replica.existence(lists, groceries).await.unwrap(),
        Some(true)
    );
    assert_eq!(rows(&sa, ITEMS).await, milk);
    // B holds no list frames: it folds nothing and keeps the records.
    b.replica.compact(erp.domain, 0).await.unwrap();
    assert_eq!(count(&b, logged).await, 131);

    // An unsigned fold is never offered; the owner signs it for others.
    admit(&a, &b).await;
    assert!(
        a.replica
            .existence_base_offer(b.node.public_key(), &b.credentials, lists)
            .await
            .unwrap()
            .is_none()
    );
    let unsigned = a
        .replica
        .unsigned_existence_bases(erp.domain)
        .await
        .unwrap();
    assert_eq!(unsigned.len(), 2);
    let empty = unsigned
        .iter()
        .find(|base| base.base.scope == erp.scope("customers"))
        .unwrap();
    assert_eq!(empty.seal, sealed.id().unwrap());
    assert_eq!(empty.base.rows, 0);
    assert!(empty.base.identities.is_empty());
    assert!(empty.cut.logs.is_empty());
    for statement in unsigned {
        let signed = SignedExistenceBase::sign(statement, &erp.publisher).unwrap();
        a.replica
            .install_signed_existence_base(signed)
            .await
            .unwrap();
    }

    // C joins after the records were released: it holds items only and
    // imports the signed base instead of the log.
    let c = erp
        .open(&dir.path().join("c.db"), &PrivateKey::new(), 13, &["items"])
        .await;
    follow(&erp, &c, &[created, statements].concat()).await;
    import_base(&erp, &a, &c, "lists").await;
    let d = erp
        .open(
            &dir.path().join("d.db"),
            &PrivateKey::new(),
            14,
            &["orders"],
        )
        .await;
    follow(&erp, &d, &a.replica.tip_statements(None, 32).await.unwrap()).await;
    import_base(&erp, &a, &d, "customers").await;
    let empty = d
        .replica
        .installed_existence_base(erp.scope("customers"))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(empty.basis, ExistenceBasis::Signed(_)));
    assert_eq!(empty.descriptor().unwrap().rows, 0);
    assert!(empty.cut().unwrap().logs.is_empty());
    let base = c
        .replica
        .installed_existence_base(lists)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(base.basis, ExistenceBasis::Signed(_)));
    deliver(&c, &items).await;
    let sc = erp.sql(&c).await;
    assert_eq!(rows(&sc, ITEMS).await, milk);
    assert_eq!(
        c.replica.existence(lists, hardware).await.unwrap(),
        Some(false)
    );

    // Later records continue every log past the base.
    let (later, _) = commit(
        &sa,
        &[("INSERT INTO lists(title,id) VALUES('later',?id)", vec![])],
    )
    .await;
    existence(&erp, &a, &b, "lists").await;
    existence(&erp, &a, &c, "lists").await;
    for device in [&b, &c] {
        assert_eq!(
            device.replica.existence(lists, later[0]).await.unwrap(),
            Some(true)
        );
    }
}

#[tokio::test]
async fn a_holder_waits_for_every_record_its_frames_count() {
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let a = erp
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 11, &TABLES)
        .await;
    let d = erp
        .open(&dir.path().join("d.db"), &PrivateKey::new(), 14, &["lists"])
        .await;
    let genesis = a.replica.followed_tip().await;
    let created = vec![bookmark(&erp, None, genesis)];
    follow(&erp, &a, &created).await;
    follow(&erp, &d, &created).await;
    let sa = erp.sql(&a).await;
    let mut frames = Vec::new();
    for title in ["first", "second"] {
        let (_, committed) = commit(
            &sa,
            &[(
                &format!("INSERT INTO lists(title,id) VALUES('{title}',?id)"),
                vec![],
            )],
        )
        .await;
        frames.extend(committed);
    }
    let (_, statements) = seal(&erp, &a).await;
    deliver(&d, &frames).await;
    follow(&erp, &d, &[created, statements].concat()).await;
    // D holds every list frame but no record they count.
    let report = d.replica.compact(erp.domain, 0).await.unwrap();
    assert!(
        report
            .waiting
            .as_deref()
            .is_some_and(|waiting| waiting.contains("missing existence records")),
        "{report:?}"
    );
    assert_eq!(report.compacted, None);
    existence(&erp, &a, &d, "lists").await;
    let report = d.replica.compact(erp.domain, 0).await.unwrap();
    assert_eq!(report.waiting, None);
    assert!(report.compacted.is_some());
    assert_eq!(
        count(&d, "SELECT count(*) FROM secure_existence_log").await,
        0
    );
}

#[tokio::test]
async fn an_excluded_parent_delete_replays_the_same_children_puppets_and_virtual_rows() {
    let erp = Erp::new();
    let dir = TempDir::new().unwrap();
    let device = erp
        .open(
            &dir.path().join("parent.db"),
            &PrivateKey::new(),
            51,
            &TABLES,
        )
        .await;
    let tip = device.replica.followed_tip().await;
    follow(&erp, &device, &[bookmark(&erp, None, tip)]).await;
    let sql = erp.sql(&device).await;
    let (region, _) = commit(
        &sql,
        &[("INSERT INTO regions(name,id) VALUES('west',?id)", vec![])],
    )
    .await;
    let (customer, _) = commit(
        &sql,
        &[(
            "INSERT INTO customers(name,region,score,photo,id) VALUES('customer',?,1.0,x'01',?id)",
            vec![integer(region[0])],
        )],
    )
    .await;
    commit(
        &sql,
        &[(
            "INSERT INTO orders(customer,label,id) VALUES(?,'order',?id)",
            vec![integer(customer[0])],
        )],
    )
    .await;
    let frontier = device
        .replica
        .seal_frontier(erp.domain, false)
        .await
        .unwrap();
    let regions = erp.scope("regions");
    let (row, before) = device
        .replica
        .snapshot(regions)
        .await
        .unwrap()
        .rows
        .into_iter()
        .next()
        .unwrap();
    device
        .replica
        .author(
            [204; 32],
            vec![RowChange {
                scope: regions,
                row,
                before: Some(before),
                after: None,
            }],
            vec![],
        )
        .await
        .unwrap();
    let children = stored(&device, "SELECT * FROM orders ORDER BY id").await;
    let puppets = stored(&device, "SELECT * FROM customers ORDER BY id").await;
    let virtual_rows = stored(&device, "SELECT * FROM regions ORDER BY id").await;
    assert_eq!(
        count(&device, "SELECT count(*) FROM customers WHERE row_kind=1").await,
        1
    );
    assert_eq!(
        count(&device, "SELECT count(*) FROM regions WHERE row_kind=2").await,
        1
    );
    let schema = device
        .replica
        .tip_graph(erp.domain)
        .await
        .unwrap()
        .schema(tip)
        .unwrap();
    let signed = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: erp.domain,
            tip,
            frontier,
            action: SealAction::Continue { schema },
            recovery: None,
        },
        &erp.publisher,
    )
    .unwrap();
    device
        .replica
        .publish_tip_statements(&[
            TipStatement::Seal(signed.clone()),
            bookmark(&erp, Some(tip), signed.successor().unwrap().unwrap()),
        ])
        .await
        .unwrap();
    device.replica.finalize_path(erp.domain).await.unwrap();
    assert!(device.replica.inner.lock().await.finalization_oracle_time > std::time::Duration::ZERO);
    assert_eq!(
        stored(&device, "SELECT * FROM orders ORDER BY id").await,
        children
    );
    assert_eq!(
        stored(&device, "SELECT * FROM customers ORDER BY id").await,
        puppets
    );
    assert_eq!(
        stored(&device, "SELECT * FROM regions ORDER BY id").await,
        virtual_rows
    );
    for table in ["regions", "customers", "orders"] {
        device.replica.audit(erp.scope(table)).await.unwrap();
    }
}
