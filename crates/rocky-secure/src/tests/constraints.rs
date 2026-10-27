//! Unique indexes and foreign keys of mergeable tables converge through
//! hidden rows. See `docs/runtime/mergeable-constraints.md`.
use super::*;
use crate::sql::{SqlError, SqlOptions};
use crate::{Describe, ReceiveStatus, SecureSql};
use rocky_core::{DomainId, LogicalSchema, Scope, SqlValue, TypedRows};

const DEFINITION: &str = r#"{
    "format":6, "publisher": "PUBLISHER", "application": "shop",
    "tables": [
        {"name": "lists", "class": "mergeable",
         "columns": [{"name": "title", "type": "text", "not_null": true}],
         "primary_key": ["title"]},
        {"name": "items", "class": "mergeable",
         "columns": [
            {"name": "list", "type": "integer", "not_null": true},
            {"name": "code", "type": "text", "not_null": true},
            {"name": "title", "type": "text", "collation": "nocase"},
            {"name": "moved", "type": "integer"},
            {"name": "note", "type": "text"}],
         "primary_key": ["id"],
         "indexes": [
            {"name": "items_code", "columns": [{"name": "list", "descending": false}, {"name": "code", "descending": false}], "unique": true, "on_conflict": "latest_wins"},
            {"name": "items_title", "columns": [{"name": "title", "descending": false}], "unique": true, "on_conflict": "set_null"}],
         "foreign_keys": [
            {"columns": ["list"], "table": "lists", "on_delete": "cascade"},
            {"columns": ["moved"], "table": "lists", "on_delete": "set_null"}]}
    ]
}"#;

struct Shop {
    publisher: PrivateKey,
    schema: LogicalSchema,
    manifest: SignedManifest,
    policy: RevocationSet,
    domain: Scope,
    lists: ResourceScope,
    items: ResourceScope,
}
impl Shop {
    fn new() -> Self {
        Self::with_sql_case(false)
    }
    fn with_sql_case(mixed: bool) -> Self {
        let publisher = PrivateKey::new();
        let definition = DEFINITION.replace("PUBLISHER", &publisher.public_key().to_string());
        let definition = if mixed {
            definition.replace("\"table\": \"lists\"", "\"table\": \"LISTS\"")
        } else {
            definition
        };
        let mut schema = rocky_core::SchemaDefinition::parse(definition.as_bytes())
            .unwrap()
            .build()
            .unwrap();
        if mixed {
            for table in &mut schema.tables {
                for name in &mut table.primary_key {
                    *name = name.to_ascii_uppercase();
                }
                for index in &mut table.indexes {
                    for column in &mut index.columns {
                        column.name = column.name.to_ascii_uppercase();
                    }
                    if index.on_conflict.is_some() {
                        index.create_sql = rocky_core::unique_index_sql(
                            &table.sql_name,
                            &index.name,
                            &index.columns,
                        );
                    }
                }
                for key in &mut table.foreign_keys {
                    let before = rocky_core::foreign_key_clause(key);
                    key.table = key.table.to_ascii_uppercase();
                    for name in key.columns.iter_mut().chain(&mut key.references) {
                        *name = name.to_ascii_uppercase();
                    }
                    table.create_sql = table
                        .create_sql
                        .replace(&before, &rocky_core::foreign_key_clause(key));
                }
            }
        }
        let manifest = schema.manifest(&[DomainId([1; 16])]);
        let domain = Scope {
            mesh: MeshId([1; 16]),
            app: schema.app_id(),
            domain: DomainId([1; 16]),
        };
        // One reader scope holds both tables.
        let scope = |name: &str| ResourceScope {
            domain,
            encryption: rocky_core::commitment("shop.readers", b"list"),
            table: schema.table_id(schema.table(name).unwrap()),
        };
        Self {
            manifest: SignedManifest::sign(&manifest, &publisher).unwrap(),
            policy: RevocationSet::new(),
            lists: scope("lists"),
            items: scope("items"),
            domain,
            schema,
            publisher,
        }
    }
    fn grants(&self) -> Vec<ResourceGrant> {
        [("lists", self.lists), ("items", self.items)]
            .into_iter()
            .map(|(name, scope)| {
                let table = self.schema.table(name).unwrap();
                let fields: Vec<_> = table
                    .columns
                    .iter()
                    .map(|column| self.schema.field_id(table, column))
                    .collect();
                grant(
                    scope,
                    &[
                        Right::Read,
                        Right::Insert,
                        Right::Update,
                        Right::Delete,
                        Right::Custody,
                    ],
                    &fields,
                )
            })
            .collect()
    }
    fn credentials(&self, node: &PrivateKey, number: u32) -> CredentialChain {
        let user = PrivateKey::new();
        let (not_before, expires) = crate::tests::lifetime();
        let capability = |subject, kind, delegations| Capability {
            subject,
            kind,
            grants: self.grants(),
            not_before,
            expires,
            delegations,
            extension: rocky_credentials::Extension::new(rocky_credentials::Extended {
                authors: Some(if kind == PrincipalKind::Node {
                    rocky_credentials::AuthorRange::new(number, number).unwrap()
                } else {
                    rocky_credentials::AuthorRange::ALL
                }),
                node: (kind == PrincipalKind::Node).then(|| rocky_credentials::PeerId::of(subject)),
                ..Default::default()
            })
            .unwrap(),
        };
        CredentialChain(vec![
            SignedCapability::sign(
                capability(user.public_key(), PrincipalKind::User, 1),
                &self.publisher,
            )
            .unwrap(),
            SignedCapability::sign(capability(node.public_key(), PrincipalKind::Node, 0), &user)
                .unwrap(),
        ])
    }
    async fn open(
        &self,
        path: &std::path::Path,
        node: &PrivateKey,
        number: u32,
    ) -> EncryptedReplica<TypedRows> {
        let replica = EncryptedReplica::open(
            path,
            TypedRows::new(self.schema.clone(), vec![DomainId([1; 16])]).unwrap(),
            SecureConfig {
                publisher: self.publisher.public_key(),
                manifest: self.manifest.clone(),
                revocations: self.policy.clone(),
                node: node.clone(),
                credentials: self.credentials(node, number),
                // Keys are per table; both tables share one reader scope.
                keys: [(self.lists, 44), (self.items, 45)]
                    .into_iter()
                    .map(|(scope, byte)| KeyMaterial::new(scope, 1, [byte; 32]))
                    .collect(),
            },
        )
        .await
        .unwrap();
        replica
            .install_tables(
                self.schema.clone(),
                self.domain,
                [
                    ("lists".to_string(), self.lists.encryption),
                    ("items".to_string(), self.items.encryption),
                ]
                .into(),
            )
            .await
            .unwrap();
        replica
    }
    async fn sql(&self, replica: &EncryptedReplica<TypedRows>) -> SecureSql<TypedRows> {
        replica
            .session([self.lists, self.items].into())
            .await
            .unwrap()
            .sql(SqlOptions::default())
            .await
            .unwrap()
    }
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}
fn integer(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}
/// Runs `statements` in one transaction; `?id` in a statement binds a new id
/// of the table it inserts into. Returns the ids and the frames.
async fn commit(
    sql: &SecureSql<TypedRows>,
    statements: &[(&str, Vec<SqlValue>)],
) -> (Vec<i64>, Vec<EncryptedFrame>) {
    sql.run("BEGIN", vec![]).await.unwrap();
    let mut ids = Vec::new();
    for (statement, values) in statements {
        let mut values = values.clone();
        if statement.contains("?id") {
            let table = statement
                .trim_start_matches("INSERT INTO ")
                .split('(')
                .next()
                .unwrap();
            let id = sql.next_id(table).await.unwrap().integer();
            ids.push(id);
            values.push(integer(id));
        }
        sql.run(&statement.replace("?id", "?"), values)
            .await
            .unwrap();
    }
    let mut handle = [0; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut handle);
    let frames = sql.commit(handle).await.unwrap().committed.unwrap().frames;
    (ids, frames)
}
async fn deliver(replica: &EncryptedReplica<TypedRows>, frames: &[EncryptedFrame]) {
    for frame in frames {
        let status = replica.receive(frame.clone()).await.unwrap();
        assert!(
            matches!(
                status,
                ReceiveStatus::Applied
                    | ReceiveStatus::Duplicate
                    | ReceiveStatus::PendingDependencies
            ),
            "a remote row never raises: {status:?}"
        );
    }
}
async fn rows(sql: &SecureSql<TypedRows>, statement: &str) -> Vec<Vec<SqlValue>> {
    sql.run(statement, vec![]).await.unwrap().rows
}
const ITEMS: &str = "SELECT id,list,code,title,moved FROM items ORDER BY id";

#[tokio::test]
async fn unique_values_converge_in_any_order_and_the_loser_returns() {
    let shop = Shop::new();
    let dir = TempDir::new().unwrap();
    let nodes = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let mut replicas = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        replicas.push(
            shop.open(
                &dir.path().join(format!("{index}.db")),
                node,
                11 + index as u32,
            )
            .await,
        );
    }
    let mut sessions = Vec::new();
    for replica in &replicas {
        sessions.push(shop.sql(replica).await);
    }
    let (list, created) = commit(
        &sessions[0],
        &[(
            "INSERT INTO lists(title,id) VALUES('groceries',?id)",
            vec![],
        )],
    )
    .await;
    for replica in &replicas[1..] {
        deliver(replica, &created).await;
    }
    let list = list[0];
    // Offline, both devices take the code milk and one title each.
    let insert = "INSERT INTO items(list,code,title,id) VALUES(?,?,?,?id)";
    let (a, from_a) = commit(
        &sessions[0],
        &[
            (insert, vec![integer(list), text("milk"), SqlValue::Null]),
            (insert, vec![integer(list), text("tea"), text("Green")]),
        ],
    )
    .await;
    let (_, from_b) = commit(
        &sessions[1],
        &[
            (insert, vec![integer(list), text("milk"), SqlValue::Null]),
            (insert, vec![integer(list), text("coffee"), text("GREEN")]),
        ],
    )
    .await;
    deliver(&replicas[0], &from_b).await;
    deliver(&replicas[1], &from_a).await;
    // The third device receives everything the other way round.
    for frame in from_b.iter().chain(&from_a).rev() {
        deliver(&replicas[2], std::slice::from_ref(frame)).await;
    }
    let first = rows(&sessions[0], ITEMS).await;
    for session in &sessions[1..] {
        assert_eq!(rows(session, ITEMS).await, first);
    }
    // One milk row survives; the NOCASE title clash shows NULL on the loser.
    let codes = |rows: &[Vec<SqlValue>]| {
        let mut codes: Vec<_> = rows.iter().map(|row| row[2].clone()).collect();
        codes.sort_by_key(|code| format!("{code:?}"));
        codes
    };
    assert_eq!(codes(&first), [text("coffee"), text("milk"), text("tea")]);
    let titled: Vec<_> = first
        .iter()
        .filter(|row| row[3] != SqlValue::Null)
        .collect();
    assert_eq!(titled.len(), 1, "{first:?}");
    let owner = |id: &SqlValue| match id {
        SqlValue::Integer(id) if a.contains(id) => 0,
        _ => 1,
    };

    // A local duplicate fails at its statement; the transaction goes on.
    let session = &sessions[0];
    session.run("BEGIN", vec![]).await.unwrap();
    let id = session.next_id("items").await.unwrap().integer();
    let duplicate = session
        .run(
            "INSERT INTO items(list,code,id) VALUES(?,'milk',?)",
            vec![integer(list), integer(id)],
        )
        .await;
    assert!(matches!(
        duplicate,
        Err(Error::Sql(SqlError::Statement { code: 2067, .. }))
    ));
    session.run("ROLLBACK", vec![]).await.unwrap();

    // The milk winner changes its code; the hidden row returns everywhere.
    let winner = first.iter().find(|row| row[2] == text("milk")).unwrap()[0].clone();
    let (_, renamed) = commit(
        &sessions[owner(&winner)],
        &[(
            "UPDATE items SET code='bread' WHERE id=?",
            vec![winner.clone()],
        )],
    )
    .await;
    // The title loser edits another column without publishing its NULL;
    // then the title winner takes another title.
    let titled = titled[0][0].clone();
    let nulled = first
        .iter()
        .find(|row| row[2] != text("milk") && row[3] == SqlValue::Null)
        .unwrap()[0]
        .clone();
    let (_, noted) = commit(
        &sessions[owner(&nulled)],
        &[(
            "UPDATE items SET note='seen' WHERE id=?",
            vec![nulled.clone()],
        )],
    )
    .await;
    let (_, retitled) = commit(
        &sessions[owner(&titled)],
        &[(
            "UPDATE items SET title='other' WHERE id=?",
            vec![titled.clone()],
        )],
    )
    .await;
    for replica in &replicas[..2] {
        for frames in [&renamed, &noted, &retitled] {
            deliver(replica, frames).await;
        }
    }
    for frames in [&retitled, &noted, &renamed] {
        deliver(&replicas[2], frames).await;
    }
    const SETTLED: &str = "SELECT code,title,note FROM items ORDER BY code";
    let settled = rows(&sessions[0], SETTLED).await;
    for session in &sessions[1..] {
        assert_eq!(rows(session, SETTLED).await, settled);
    }
    let lower = |value: &SqlValue| match value {
        SqlValue::Text(text) => SqlValue::Text(text.to_lowercase()),
        other => other.clone(),
    };
    assert_eq!(
        settled
            .iter()
            .map(|row| row.iter().map(lower).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        [
            [text("bread"), SqlValue::Null, SqlValue::Null],
            [
                text("coffee"),
                text(if owner(&titled) == 1 {
                    "other"
                } else {
                    "green"
                }),
                if owner(&nulled) == 1 {
                    text("seen")
                } else {
                    SqlValue::Null
                }
            ],
            [text("milk"), SqlValue::Null, SqlValue::Null],
            [
                text("tea"),
                text(if owner(&titled) == 0 {
                    "other"
                } else {
                    "green"
                }),
                if owner(&nulled) == 0 {
                    text("seen")
                } else {
                    SqlValue::Null
                }
            ],
        ]
    );

    // Rebuilding and reopening reproduce the same visible rows.
    replicas[2].rebuild_projection(shop.items).await.unwrap();
    assert_eq!(rows(&sessions[2], SETTLED).await, settled);
    drop(sessions);
    let path = dir.path().join("2.db");
    let replica = replicas.pop().unwrap();
    drop(replica);
    let reopened = shop.open(&path, &nodes[2], 13).await;
    assert_eq!(rows(&shop.sql(&reopened).await, SETTLED).await, settled);
}

#[tokio::test]
async fn deleted_parents_hide_children_until_a_live_parent_is_named() {
    let shop = Shop::new();
    let dir = TempDir::new().unwrap();
    let nodes = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let mut replicas = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        replicas.push(
            shop.open(
                &dir.path().join(format!("{index}.db")),
                node,
                21 + index as u32,
            )
            .await,
        );
    }
    let mut sessions = Vec::new();
    for replica in &replicas {
        sessions.push(shop.sql(replica).await);
    }
    let (lists, created) = commit(
        &sessions[0],
        &[
            (
                "INSERT INTO lists(title,id) VALUES('groceries',?id)",
                vec![],
            ),
            ("INSERT INTO lists(title,id) VALUES('hardware',?id)", vec![]),
        ],
    )
    .await;
    for replica in &replicas[1..] {
        deliver(replica, &created).await;
    }
    let (groceries, hardware) = (lists[0], lists[1]);
    // A deletes groceries while B adds an item to it and moves another from it.
    let (_, deleted) = commit(
        &sessions[0],
        &[("DELETE FROM lists WHERE title='groceries'", vec![])],
    )
    .await;
    assert_eq!(
        deleted.len(),
        1,
        "a parent delete publishes no child change"
    );
    let (items, added) = commit(
        &sessions[1],
        &[
            (
                "INSERT INTO items(list,code,id) VALUES(?,'milk',?id)",
                vec![integer(groceries)],
            ),
            (
                "INSERT INTO items(list,code,moved,id) VALUES(?,'nails',?,?id)",
                vec![integer(hardware), integer(groceries)],
            ),
        ],
    )
    .await;
    let (milk, nails) = (items[0], items[1]);
    // C sees the insert first and points the item at a live list meanwhile.
    deliver(&replicas[2], &added).await;
    let (_, repointed) = commit(
        &sessions[2],
        &[(
            "UPDATE items SET list=? WHERE id=?",
            vec![integer(hardware), integer(milk)],
        )],
    )
    .await;
    deliver(&replicas[2], &deleted).await;
    deliver(&replicas[0], &added).await;
    deliver(&replicas[1], &deleted).await;
    for session in &sessions[..2] {
        // Hidden on every replica that has not seen the repoint.
        assert_eq!(
            rows(session, ITEMS).await,
            vec![vec![
                integer(nails),
                integer(hardware),
                text("nails"),
                SqlValue::Null,
                SqlValue::Null
            ]]
        );
    }
    deliver(&replicas[0], &repointed).await;
    deliver(&replicas[1], &repointed).await;
    let expected = vec![
        vec![
            integer(milk),
            integer(hardware),
            text("milk"),
            SqlValue::Null,
            SqlValue::Null,
        ],
        vec![
            integer(nails),
            integer(hardware),
            text("nails"),
            SqlValue::Null,
            SqlValue::Null,
        ],
    ];
    for session in &sessions {
        assert_eq!(rows(session, ITEMS).await, expected);
    }

    // A re-created parent has a new id; the old children stay orphans.
    let (orphan, child) = commit(
        &sessions[1],
        &[(
            "INSERT INTO items(list,code,id) VALUES(?,'tea',?id)",
            vec![integer(hardware)],
        )],
    )
    .await;
    let (_, recreated) = commit(
        &sessions[0],
        &[
            ("DELETE FROM lists WHERE title='hardware'", vec![]),
            ("INSERT INTO lists(title,id) VALUES('hardware',?id)", vec![]),
        ],
    )
    .await;
    deliver(&replicas[0], &child).await;
    deliver(&replicas[1], &recreated).await;
    deliver(&replicas[2], &recreated).await;
    deliver(&replicas[2], &child).await;
    for session in &sessions {
        assert!(rows(session, ITEMS).await.is_empty());
        assert_eq!(
            rows(session, "SELECT title FROM lists ORDER BY title").await,
            vec![vec![text("hardware")]]
        );
    }
    assert_ne!(orphan[0], hardware);

    // A dangling reference fails at commit; nothing is published.
    sessions[0].run("BEGIN", vec![]).await.unwrap();
    let id = sessions[0].next_id("items").await.unwrap().integer();
    sessions[0]
        .run(
            "INSERT INTO items(list,code,id) VALUES(?,'x',?)",
            vec![integer(hardware), integer(id)],
        )
        .await
        .unwrap();
    assert!(matches!(
        sessions[0].commit([7; 32]).await,
        Err(Error::Sql(SqlError::Statement { code: 787, .. }))
    ));

    // A child that arrives before its parent stays hidden until it does.
    let (fresh, parent) = commit(
        &sessions[1],
        &[("INSERT INTO lists(title,id) VALUES('garden',?id)", vec![])],
    )
    .await;
    let (_, early) = commit(
        &sessions[1],
        &[(
            "INSERT INTO items(list,code,id) VALUES(?,'seeds',?id)",
            vec![integer(fresh[0])],
        )],
    )
    .await;
    deliver(&replicas[0], &early).await;
    assert!(rows(&sessions[0], ITEMS).await.is_empty());
    deliver(&replicas[0], &parent).await;
    assert_eq!(rows(&sessions[0], ITEMS).await.len(), 1);

    // ORMs read the declared actions.
    let described = sessions[0]
        .describe(Describe::ForeignKeyList, "items")
        .await
        .unwrap();
    let mut actions: Vec<_> = described
        .rows
        .iter()
        .map(|row| (row[3].clone(), row[6].clone()))
        .collect();
    actions.sort_by_key(|(column, _)| format!("{column:?}"));
    assert_eq!(
        actions,
        vec![
            (text("list"), text("CASCADE")),
            (text("moved"), text("SET NULL"))
        ]
    );
}

#[tokio::test]
async fn checkpoints_and_evictions_rebuild_the_same_hidden_rows() {
    let shop = Shop::new();
    let dir = TempDir::new().unwrap();
    let (a_node, b_node) = (PrivateKey::new(), PrivateKey::new());
    let a = shop.open(&dir.path().join("a.db"), &a_node, 31).await;
    let b = shop.open(&dir.path().join("b.db"), &b_node, 32).await;
    let (sa, sb) = (shop.sql(&a).await, shop.sql(&b).await);
    let (lists, created) = commit(
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
    deliver(&b, &created).await;
    let (_, deleted) = commit(&sa, &[("DELETE FROM lists WHERE title='hardware'", vec![])]).await;
    let (_, added) = commit(
        &sb,
        &[
            (
                "INSERT INTO items(list,code,id) VALUES(?,'nails',?id)",
                vec![integer(lists[1])],
            ),
            (
                "INSERT INTO items(list,code,moved,id) VALUES(?,'milk',?,?id)",
                vec![integer(lists[0]), integer(lists[1])],
            ),
        ],
    )
    .await;
    deliver(&a, &added).await;
    deliver(&b, &deleted).await;
    let visible = rows(&sa, ITEMS).await;
    assert_eq!(rows(&sb, ITEMS).await, visible);
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0][4], SqlValue::Null);

    // A fresh replica bootstraps both tables from checkpoints.
    let fresh_node = PrivateKey::new();
    let fresh = shop
        .open(&dir.path().join("fresh.db"), &fresh_node, 33)
        .await;
    let credentials = shop.credentials(
        &fresh_node,
        crate::tests::node_number(fresh_node.public_key()),
    );
    for scope in [shop.items, shop.lists] {
        let signed = compacted_base(&a, &shop.publisher, scope).await;
        let checkpoint = signed.id().unwrap();
        link_up(&a, &fresh, scope.domain).await;
        fresh.begin_rebootstrap(signed).await.unwrap();
        let mut after = None;
        loop {
            let page = a
                .checkpoint_page(
                    fresh_node.public_key(),
                    &credentials,
                    scope,
                    checkpoint,
                    after,
                    crate::MAX_SECURE_FRAME,
                )
                .await
                .unwrap();
            let next = page.next;
            fresh.stage_checkpoint_page(page).await.unwrap();
            if next.is_none() {
                break;
            }
            after = next;
        }
        fresh.finish_rebootstrap(scope).await.unwrap();
    }
    let session = shop.sql(&fresh).await;
    assert_eq!(rows(&session, ITEMS).await, visible);

    // Clearing the parents' table as eviction does hides every child with it.
    {
        let mut inner = fresh.inner.lock(crate::replica::Class::Background).await;
        let lists = shop.lists;
        transaction!(inner, |inner| inner.clear_typed(lists).await).unwrap();
    }
    assert!(rows(&session, "SELECT id FROM lists").await.is_empty());
    assert!(rows(&session, ITEMS).await.is_empty());
    fresh.rebuild_projection(shop.lists).await.unwrap();
    assert_eq!(rows(&session, ITEMS).await, visible);
}

/// A row record names its author's bindings by digest. Each set is stored
/// once, and a checkpoint page carries exactly the sets its rows name: an
/// importer refuses any other page and ends up holding the same sets.
#[tokio::test]
async fn author_bindings_are_stored_once_and_travel_with_checkpoint_pages() {
    async fn sets(replica: &EncryptedReplica<TypedRows>) -> Vec<Vec<SqlValue>> {
        let inner = replica.inner.lock(crate::replica::Class::Background).await;
        crate::storage::rows(
            &inner.conn,
            "SELECT digest,bindings FROM secure_author_bindings ORDER BY digest",
        )
        .await
        .unwrap()
    }
    let shop = Shop::new();
    let dir = TempDir::new().unwrap();
    let a_node = PrivateKey::new();
    let a = shop.open(&dir.path().join("a.db"), &a_node, 31).await;
    let sa = shop.sql(&a).await;
    for title in ["hardware", "garden", "kitchen"] {
        commit(
            &sa,
            &[(
                "INSERT INTO lists(title,id) VALUES(?,?id)",
                vec![text(title)],
            )],
        )
        .await;
    }
    let stored = sets(&a).await;
    assert_eq!(stored.len(), 1, "three rows of one author name one set");

    let fresh_node = PrivateKey::new();
    let fresh = shop
        .open(&dir.path().join("fresh.db"), &fresh_node, 33)
        .await;
    let signed = compacted_base(&a, &shop.publisher, shop.lists).await;
    link_up(&a, &fresh, shop.lists.domain).await;
    fresh.begin_rebootstrap(signed.clone()).await.unwrap();
    let page = a
        .checkpoint_page(
            fresh_node.public_key(),
            &shop.credentials(
                &fresh_node,
                crate::tests::node_number(fresh_node.public_key()),
            ),
            shop.lists,
            signed.id().unwrap(),
            None,
            crate::MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    assert!(page.next.is_none());
    let key = [44; 32];
    let forge = |edit: fn(&mut Vec<Vec<u8>>)| {
        let mut forged = page.clone();
        crate::replica::checkpoint::forge_page_bindings(&mut forged, &key, edit);
        forged
    };
    for forged in [
        forge(|sets| sets.clear()),
        forge(|sets| {
            sets.push(crate::encode(&Vec::<rocky_credentials::AuthorBinding>::new()).unwrap())
        }),
        forge(|sets| sets.push(sets[0].clone())),
        forge(|sets| {
            sets[0] = crate::encode(&Vec::<rocky_credentials::AuthorBinding>::new()).unwrap()
        }),
    ] {
        assert!(fresh.stage_checkpoint_page(forged).await.is_err());
        assert!(sets(&fresh).await.is_empty());
    }
    fresh.stage_checkpoint_page(page).await.unwrap();
    fresh.finish_rebootstrap(shop.lists).await.unwrap();
    // The shop's never-written tables are signed empty bases at the seal.
    let empty = crate::tests::import_needed_bases(
        &a,
        &fresh,
        &fresh_node,
        &shop.credentials(
            &fresh_node,
            crate::tests::node_number(fresh_node.public_key()),
        ),
        shop.lists.domain,
    )
    .await;
    assert!(!empty.is_empty());
    for signed in &empty {
        assert_ne!(signed.statement.scope, shop.lists);
        crate::tests::assert_signed_empty(signed);
    }
    assert_eq!(sets(&fresh).await, stored);
    assert_eq!(
        rows(&shop.sql(&fresh).await, "SELECT title FROM lists")
            .await
            .len(),
        3
    );
}

#[tokio::test]
async fn equal_unique_stamps_from_different_columns_choose_the_larger_row_id() {
    let shop = Shop::new();
    let dir = TempDir::new().unwrap();
    let nodes = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let mut replicas = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        replicas.push(
            shop.open(
                &dir.path().join(format!("tie{index}.db")),
                node,
                41 + index as u32,
            )
            .await,
        );
    }
    let mut sessions = Vec::new();
    for replica in &replicas {
        sessions.push(shop.sql(replica).await);
    }
    let (lists, parents) = commit(
        &sessions[0],
        &[
            ("INSERT INTO lists(title,id) VALUES('a',?id)", vec![]),
            ("INSERT INTO lists(title,id) VALUES('b',?id)", vec![]),
        ],
    )
    .await;
    let (ids, initial) = commit(
        &sessions[0],
        &[
            (
                "INSERT INTO items(list,code,id) VALUES(?,'z',?id)",
                vec![integer(lists[0])],
            ),
            (
                "INSERT INTO items(list,code,id) VALUES(?,'y',?id)",
                vec![integer(lists[1])],
            ),
        ],
    )
    .await;
    for replica in &replicas[1..] {
        deliver(replica, &parents).await;
        deliver(replica, &initial).await;
    }
    let (_, offline) = commit(
        &sessions[0],
        &[(
            "UPDATE items SET code='x' WHERE id=?",
            vec![integer(ids[0])],
        )],
    )
    .await;
    let (_, advance) = commit(
        &sessions[1],
        &[(
            "UPDATE items SET note='advance' WHERE id=?",
            vec![integer(ids[1])],
        )],
    )
    .await;
    let (_, tied) = commit(
        &sessions[1],
        &[
            (
                "UPDATE items SET list=? WHERE id=?",
                vec![integer(lists[1]), integer(ids[0])],
            ),
            (
                "UPDATE items SET code='x' WHERE id=?",
                vec![integer(ids[1])],
            ),
        ],
    )
    .await;
    assert_eq!(
        tied.len(),
        1,
        "one frame writes the two unique-index columns"
    );
    deliver(&replicas[0], &advance).await;
    deliver(&replicas[0], &tied).await;
    deliver(&replicas[1], &offline).await;
    deliver(&replicas[2], &advance).await;
    deliver(&replicas[2], &tied).await;
    deliver(&replicas[2], &offline).await;
    let expected = vec![vec![
        integer(*ids.iter().max().unwrap()),
        integer(lists[1]),
        text("x"),
    ]];
    const QUERY: &str = "SELECT id,list,code FROM items ORDER BY id";
    for session in &sessions {
        assert_eq!(rows(session, QUERY).await, expected);
    }
    replicas[2].rebuild_projection(shop.items).await.unwrap();
    assert_eq!(rows(&sessions[2], QUERY).await, expected);
    drop(sessions);
    drop(replicas.pop());
    let reopened = shop.open(&dir.path().join("tie2.db"), &nodes[2], 43).await;
    assert_eq!(rows(&shop.sql(&reopened).await, QUERY).await, expected);
}

#[tokio::test]
async fn case_insensitive_schema_references_install_capture_and_resolve() {
    let shop = Shop::with_sql_case(true);
    let dir = TempDir::new().unwrap();
    let nodes = [PrivateKey::new(), PrivateKey::new()];
    let a = shop
        .open(&dir.path().join("case-a.db"), &nodes[0], 51)
        .await;
    let b = shop
        .open(&dir.path().join("case-b.db"), &nodes[1], 52)
        .await;
    let (sa, sb) = (shop.sql(&a).await, shop.sql(&b).await);
    let (lists, parents) = commit(
        &sa,
        &[("INSERT INTO lists(title,id) VALUES('list',?id)", vec![])],
    )
    .await;
    deliver(&b, &parents).await;
    let (_, inserted) = commit(
        &sa,
        &[(
            "INSERT INTO items(list,code,id) VALUES(?,'milk',?id)",
            vec![integer(lists[0])],
        )],
    )
    .await;
    deliver(&b, &inserted).await;
    assert_eq!(rows(&sa, ITEMS).await, rows(&sb, ITEMS).await);
    assert_eq!(rows(&sb, ITEMS).await.len(), 1);
    let (_, deleted) = commit(&sa, &[("DELETE FROM lists WHERE title='list'", vec![])]).await;
    deliver(&b, &deleted).await;
    assert!(rows(&sa, ITEMS).await.is_empty());
    assert!(rows(&sb, ITEMS).await.is_empty());
}

mod finalization;
