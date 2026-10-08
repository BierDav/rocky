//! Command-controlled tables: confirmed command rows as read-only typed
//! tables. See `docs/runtime/confirmed-tables.md`.
use super::typed_commands::{Device, WRITE, World, identity_row, text};
use super::*;
use crate::observe::Cause;
use crate::sql::{SqlError, SqlOptions, SqlState};
use rocky_core::{
    RowApplication, RowChange, RowCommandApplication, SqlValue, TableClass, TypedCommands,
};
use std::collections::BTreeMap;

impl World {
    async fn install(&self, replica: &EncryptedReplica<TypedCommands>) {
        replica
            .install_tables(
                self.schema.clone(),
                self.domain,
                [
                    ("holds".to_string(), self.holds.encryption),
                    ("stock".to_string(), self.stock.encryption),
                ]
                .into(),
            )
            .await
            .unwrap();
    }
    async fn sql(&self, replica: &EncryptedReplica<TypedCommands>) -> SecureSql<TypedCommands> {
        replica
            .session([self.stock, self.holds].into())
            .await
            .unwrap()
            .sql(SqlOptions::default())
            .await
            .unwrap()
    }
    /// The typed `stock` table as SQL reads it.
    async fn stock_table(&self, replica: &EncryptedReplica<TypedCommands>) -> Vec<Vec<SqlValue>> {
        self.sql(replica)
            .await
            .run(
                "SELECT item,remaining,id,row_kind FROM stock ORDER BY item",
                vec![],
            )
            .await
            .unwrap()
            .rows
    }
    /// Registers a pseudonym that may write `holds` for `device`.
    async fn holds_author(&self, replica: &EncryptedReplica<TypedCommands>, device: &Device) {
        let key = PrivateKey::from_bytes(&rocky_core::commitment("holds", device.node.as_bytes()));
        let assigner = PrivateKey::new();
        let grants = self.grants(&[(self.holds, WRITE)]);
        let issued = crate::credentials::Authority {
            publisher: &self.publisher,
            manifest: self.manifest().hash(),
        }
        .issue_terms(
            assigner.public_key(),
            grants.clone(),
            (1, Some(crate::credentials::AuthorRange::ALL)),
            1,
            (0, u64::MAX, 0),
        )
        .unwrap();
        let chain = CredentialChain(vec![issued])
            .assign(
                &assigner,
                key.public_key(),
                7,
                device.node.public_key(),
                grants,
                u64::MAX,
                (0, u64::MAX, 0),
            )
            .unwrap();
        replica.register_row_author(key, chain).await.unwrap();
    }
}
fn stock(remaining: i64) -> Vec<SqlValue> {
    identity_row(vec![text("bolt"), SqlValue::Integer(remaining)])
}

#[tokio::test]
async fn confirmed_results_project_atomically_and_pending_commands_never_show() {
    let world = World::with_stock(TableClass::CommandControlled);
    let executor = world.executor().await;
    // Results confirmed before installation project when it happens.
    world.restock(&executor, 5).await;
    world.install(&executor).await;
    assert_eq!(world.stock_table(&executor).await, vec![stock(5)]);

    // A confirmed result changes the table with its application and
    // invalidates observers of it.
    let mut observer = executor.observe_tables(Some(&["stock"])).await.unwrap();
    world.restock(&executor, 3).await;
    assert_eq!(world.stock_table(&executor).await, vec![stock(8)]);
    let change = observer.try_next().unwrap().expect("a stock event");
    assert!(change.tables.contains("stock"));
    assert!(change.causes.contains(&Cause::Confirmed));

    // A failed application leaves the table as it was; the retry applies.
    let before = *executor.observe_revision().borrow();
    executor.inner.lock().await.fail_before_commit = 1;
    let handle = [9; 32];
    let arguments = world.arguments("Restock", 1, &[text("bolt"), SqlValue::Integer(2)]);
    executor
        .submit_command(handle, world.domain, "Restock", 1, &arguments)
        .await
        .unwrap();
    assert!(
        executor
            .run_ticket(handle, &*world.coordinator)
            .await
            .is_err()
    );
    assert_eq!(world.stock_table(&executor).await, vec![stock(8)]);
    assert_eq!(*executor.observe_revision().borrow(), before);
    assert!(observer.try_next().unwrap().is_none());
    let ticket = executor
        .run_ticket(handle, &*world.coordinator)
        .await
        .unwrap();
    assert_eq!(ticket.state, TicketState::Applied);
    assert_eq!(world.stock_table(&executor).await, vec![stock(10)]);
    assert!(observer.try_next().unwrap().is_some());

    // Another device shows nothing until results arrive, and never its own
    // pending command.
    let other = Device::new();
    let caller = world.caller("caller.db", &other).await;
    world.install(&caller).await;
    assert_eq!(
        world.stock_table(&caller).await,
        Vec::<Vec<SqlValue>>::new()
    );
    let mut seen = caller.observe_tables(Some(&["stock"])).await.unwrap();
    caller
        .submit_command(
            [2; 32],
            world.domain,
            "Reserve",
            2,
            &world.reserve(4, "theirs"),
        )
        .await
        .unwrap();
    assert_eq!(
        world.stock_table(&caller).await,
        Vec::<Vec<SqlValue>>::new()
    );
    let envelope = caller.export_ticket([2; 32]).await.unwrap();
    let answer = executor
        .deliver_command(&envelope.to_bytes().unwrap(), &*world.coordinator)
        .await;
    let answer = Delivery::from_bytes(&answer.to_bytes().unwrap()).unwrap();
    let settled = caller
        .settle_delivery([2; 32], answer, &*world.coordinator)
        .await
        .unwrap();
    // Confirmed, but its predecessors are not here: still nothing to show.
    assert_eq!(settled.state, TicketState::Confirmed);
    assert_eq!(
        world.stock_table(&caller).await,
        Vec::<Vec<SqlValue>>::new()
    );
    assert!(seen.try_next().unwrap().is_none());
    world
        .catch_up(&executor, &caller, &other, &[world.stock, world.holds])
        .await;
    assert_eq!(world.stock_table(&caller).await, vec![stock(6)]);
    assert_eq!(world.stock_table(&executor).await, vec![stock(6)]);
    assert!(seen.try_next().unwrap().unwrap().tables.contains("stock"));

    // The projection survives a reopen.
    drop(caller);
    let caller = world.caller("caller.db", &other).await;
    assert_eq!(world.stock_table(&caller).await, vec![stock(6)]);
}

#[tokio::test]
async fn secure_sql_refuses_every_write_to_a_command_controlled_table() {
    let world = World::with_stock(TableClass::CommandControlled);
    let executor = world.executor().await;
    world.install(&executor).await;
    world.holds_author(&executor, &world.executor).await;
    world.restock(&executor, 5).await;
    let sql = world.sql(&executor).await;
    let catalog = sql.catalog();
    let writable = |name: &str| {
        catalog
            .iter()
            .find(|table| table.name == name)
            .unwrap()
            .writable
    };
    // The session's grants include writes; the table's class does not.
    assert!(!writable("stock"));
    assert!(writable("holds"));
    let revision = *executor.observe_revision().borrow();
    let writes = [
        "INSERT INTO stock(item,remaining,id,row_kind) VALUES('nut',1,4294967297,0)",
        "UPDATE stock SET remaining=99",
        "DELETE FROM stock",
    ];
    for write in writes {
        // Inside a transaction that otherwise succeeds.
        sql.run("BEGIN", vec![]).await.unwrap();
        let id = sql.next_id("holds").await.unwrap();
        sql.run(
            "INSERT INTO holds(key,item,quantity,id) VALUES('h','bolt',1,?)",
            vec![SqlValue::Integer(id.integer())],
        )
        .await
        .unwrap();
        assert!(sql.next_id("stock").await.is_err());
        assert!(matches!(
            sql.run(write, vec![]).await,
            Err(Error::Sql(SqlError::Denied))
        ));
        // The refusal ended the transaction; its commit publishes nothing.
        assert!(matches!(
            sql.commit([3; 32]).await,
            Err(Error::Sql(SqlError::Aborted(_)))
        ));
        assert_eq!(sql.state().await, SqlState::Idle);
        // Alone, outside a transaction.
        assert!(matches!(
            sql.run(write, vec![]).await,
            Err(Error::Sql(SqlError::Denied))
        ));
    }
    // Nothing was published or changed.
    assert_eq!(*executor.observe_revision().borrow(), revision);
    assert_eq!(world.stock_table(&executor).await, vec![stock(5)]);
    let holds = sql
        .run("SELECT count(*) FROM holds", vec![])
        .await
        .unwrap()
        .rows;
    assert_eq!(holds, vec![vec![SqlValue::Integer(0)]]);

    // Row writes are refused at admission; only confirmed results apply.
    let app = world.app(None);
    let fields = app.rows().table(&world.stock.table).unwrap();
    let change = RowChange {
        scope: world.stock,
        row: app
            .rows()
            .row_id(world.domain.domain, fields, &stock(1))
            .unwrap(),
        before: None,
        after: Some(app.rows().fields(fields, &stock(1)).unwrap()),
    };
    let refused = app
        .validate_changes(std::slice::from_ref(&change))
        .unwrap_err();
    assert!(refused.contains("command-controlled"), "{refused}");
    app.validate_confirmed(&[change]).unwrap();
}

#[tokio::test]
async fn a_projection_plan_keeps_confirmed_tables_in_place() {
    let world = World::with_stock(TableClass::CommandControlled);
    let executor = world.executor().await;
    world.install(&executor).await;
    world.restock(&executor, 5).await;
    // New indexes on both tables replace the mergeable one through shadows;
    // the confirmed table keeps its rows and gains its index.
    let mut indexed = world.schema.clone();
    for (position, name) in [(0, "holds"), (1, "stock")] {
        let table = &mut indexed.tables[position];
        let column = if name == "holds" { "item" } else { "remaining" };
        table.indexes.insert(
            0,
            rocky_core::LogicalIndex {
                name: format!("{name}_by_{column}"),
                unique: false,
                columns: vec![rocky_core::IndexColumn {
                    name: column.into(),
                    descending: false,
                }],
                create_sql: format!("CREATE INDEX {name}_by_{column} ON {name}({column})"),
                on_conflict: None,
            },
        );
    }
    let readers: BTreeMap<String, Commitment> = [
        ("holds".to_string(), world.holds.encryption),
        ("stock".to_string(), world.stock.encryption),
    ]
    .into();
    executor
        .install_tables(indexed.clone(), world.domain, readers.clone())
        .await
        .unwrap();
    assert_eq!(world.stock_table(&executor).await, vec![stock(5)]);
    world.restock(&executor, 1).await;
    assert_eq!(world.stock_table(&executor).await, vec![stock(6)]);

    // Its statement cannot change: confirmed rows have no other source.
    let mut respelled = indexed;
    respelled.tables[1].create_sql = respelled.tables[1]
        .create_sql
        .replace("remaining INTEGER", "remaining  INTEGER");
    let refused = executor
        .install_tables(respelled, world.domain, readers)
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("command-controlled"), "{refused}");
}
