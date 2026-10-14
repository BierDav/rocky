//! Definition generations: projection plans and signed upgrades.
use super::*;
use crate::replica::migrate::PlanStage;
use crate::sql::SqlOptions;
use crate::storage::{collect_rows, rows};
use rocky_core::{
    Affinity, Collation, ColumnClass, DomainId, Generation, GenerationTransition, IndexColumn,
    LOGICAL_SCHEMA_FORMAT, LogicalColumn, LogicalIndex, LogicalSchema, LogicalTable,
    RowApplication, SchemaSource, Scope, SqlValue, TableClass, TypedRows, UpdatePolicy,
};
use rocky_credentials::SignedTransition;
use std::collections::BTreeMap;

const DOMAIN: DomainId = DomainId([1; 16]);

fn column(name: &str, affinity: Affinity, not_null: bool) -> LogicalColumn {
    LogicalColumn {
        sql_name: name.into(),
        affinity,
        not_null,
        default: None,
        collation: Collation::Binary,
        class: ColumnClass::Mergeable,
    }
}
fn table(name: &str, mut columns: Vec<LogicalColumn>) -> LogicalTable {
    columns.push(column("id", Affinity::Integer, true));
    let mut kind = column("row_kind", Affinity::Integer, true);
    kind.default = Some("0".into());
    columns.push(kind);
    let sql: Vec<String> = columns
        .iter()
        .map(|column| {
            let kind = if column.affinity == Affinity::Integer {
                "INTEGER"
            } else {
                "TEXT"
            };
            let null = if column.not_null { " NOT NULL" } else { "" };
            let key = if column.sql_name == "key" {
                " PRIMARY KEY"
            } else {
                ""
            };
            format!(
                "{} {kind}{null}{key}{}",
                column.sql_name,
                column
                    .default
                    .as_ref()
                    .map(|value| format!(" DEFAULT {value}"))
                    .unwrap_or_default()
            )
        })
        .collect();
    LogicalTable {
        sql_name: name.into(),
        class: TableClass::Mergeable,
        create_sql: format!("CREATE TABLE {name}({})", sql.join(",")),
        auto_generate: false,
        columns,
        primary_key: vec!["key".into()],
        partition_by: Vec::new(),
        partition_group: None,
        indexes: vec![LogicalIndex {
            name: format!("{name}_identity"),
            unique: true,
            columns: vec![IndexColumn {
                name: "id".into(),
                descending: false,
            }],
            create_sql: format!("CREATE UNIQUE INDEX {name}_identity ON {name}(id)"),
            on_conflict: None,
        }],
        foreign_keys: Vec::new(),

        existence_log: false,
    }
}
fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}

/// Generation 1 has `items(id,title)`; 2 adds a nullable `note` and a
/// `tags` table; 3 adds a nullable `rank`; 4 removes `note` and `tags`;
/// 5 adds `note` again.
struct Lineage {
    publisher: PrivateKey,
    domain: Scope,
}
impl Lineage {
    fn new() -> Self {
        let publisher = PrivateKey::new();
        let domain = Scope {
            mesh: MeshId([1; 16]),
            app: rocky_core::ApplicationManifest::app_id(
                *publisher.public_key().as_bytes(),
                "lineage",
            ),
            domain: DOMAIN,
        };
        Self { publisher, domain }
    }
    fn schema(&self, generation: u32) -> LogicalSchema {
        let mut items = vec![
            column("key", Affinity::Text, true),
            column("title", Affinity::Text, true),
        ];
        if matches!(generation, 2 | 3 | 5) {
            items.push(column("note", Affinity::Text, false));
        }
        if generation >= 3 {
            items.push(column("rank", Affinity::Integer, false));
        }
        let mut tables = vec![table("items", items)];
        if matches!(generation, 2 | 3) {
            tables.push(table(
                "tags",
                vec![
                    column("key", Affinity::Text, true),
                    column("label", Affinity::Text, true),
                ],
            ));
        }
        LogicalSchema {
            format: LOGICAL_SCHEMA_FORMAT,
            publisher: *self.publisher.public_key().as_bytes(),
            application: "lineage".into(),
            source: SchemaSource {
                kind: "test".into(),
                version: generation,
                identity: "lineage".into(),
            },
            update_policy: UpdatePolicy::Changed,
            tables,
            views: Vec::new(),
        }
    }
    /// Generation 1 with an index on `title`: a local change.
    fn indexed(&self) -> LogicalSchema {
        let mut schema = self.schema(1);
        schema.tables[0].indexes.push(LogicalIndex {
            name: "items_title".into(),
            unique: false,
            columns: vec![IndexColumn {
                name: "title".into(),
                descending: false,
            }],
            create_sql: "CREATE INDEX items_title ON items(title)".into(),
            on_conflict: None,
        });
        schema
    }
    fn app(&self, generation: u32) -> TypedRows {
        let schema = self.schema(generation);
        let items = schema.table("items").unwrap();
        let added = items
            .columns
            .iter()
            .filter(|column| ["note", "rank"].contains(&column.sql_name.as_str()))
            .map(|column| schema.field_id(items, column))
            .collect();
        TypedRows::new(schema.clone(), vec![DOMAIN])
            .unwrap()
            .at_generation(generation, added)
            .unwrap()
    }
    fn definition(&self, generation: u32) -> rocky_core::ApplicationManifest {
        self.app(generation).manifest()
    }
    fn manifest(&self, generation: u32) -> SignedManifest {
        SignedManifest::sign(&self.definition(generation), &self.publisher).unwrap()
    }
    fn transition(&self, from: u32) -> SignedTransition {
        self.transition_at(from, 1_000)
    }
    /// The transition out of `from`, signed at `time`.
    fn transition_at(&self, from: u32, time: u64) -> SignedTransition {
        let at = |generation| Generation {
            schema: generation,
            handler: 1,
        };
        SignedTransition::sign(
            &GenerationTransition {
                app: self.domain.app,
                from: self.definition(from).hash(),
                to: self.definition(from + 1).hash(),
                before: at(from),
                after: at(from + 1),
                added: self.added(from),
                tables: rocky_core::ItemChanges::between(
                    &self.definition(from).entities,
                    &self.definition(from + 1).entities,
                ),
                fields: rocky_core::ItemChanges::between(
                    &self.definition(from).fields,
                    &self.definition(from + 1).fields,
                ),
                time,
            },
            &self.publisher,
        )
        .unwrap()
    }
    /// The field ids rows of generation `from` lack in the next.
    fn added(&self, from: u32) -> BTreeSet<Commitment> {
        let (old, new) = (self.schema(from), self.schema(from + 1));
        rocky_core::classify(&old, &new)
            .unwrap()
            .added()
            .iter()
            .map(|field| {
                let (table, column) = field.split_once('.').unwrap();
                let table = new.table(table).unwrap();
                new.field_id(table, table.column(column).unwrap())
            })
            .collect()
    }
    fn scope(&self, name: &str) -> ResourceScope {
        let schema = self.schema(3);
        ResourceScope {
            domain: self.domain,
            encryption: rocky_core::commitment("lineage.readers", name.as_bytes()),
            table: schema.table_id(schema.table(name).unwrap()),
        }
    }
    fn readers(&self, generation: u32) -> BTreeMap<String, Commitment> {
        self.schema(generation)
            .tables
            .iter()
            .map(|table| {
                (
                    table.sql_name.clone(),
                    self.scope(&table.sql_name).encryption,
                )
            })
            .collect()
    }
    fn grants(&self, generation: u32) -> Vec<ResourceGrant> {
        let schema = self.schema(generation);
        schema
            .tables
            .iter()
            .map(|table| {
                let fields: Vec<_> = table
                    .columns
                    .iter()
                    .map(|column| schema.field_id(table, column))
                    .collect();
                grant(
                    self.scope(&table.sql_name),
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
    fn credentials(&self, generation: u32, node: &PrivateKey) -> CredentialChain {
        let user = PrivateKey::new();
        let manifest = self.definition(generation).hash();
        let (not_before, expires) = crate::tests::lifetime();
        let capability = |subject, kind, delegations| Capability {
            manifest,
            subject,
            kind,
            grants: self.grants(generation),
            not_before,
            expires,
            delegations,
            extension: rocky_credentials::Extension::new(rocky_credentials::Extended {
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
    fn config(&self, generation: u32, node: &PrivateKey) -> SecureConfig {
        let keys = ["items", "tags"]
            .iter()
            .take(if matches!(generation, 2 | 3) { 2 } else { 1 })
            .enumerate()
            .map(|(index, name)| KeyMaterial::new(self.scope(name), 1, [40 + index as u8; 32]))
            .collect();
        SecureConfig {
            publisher: self.publisher.public_key(),
            manifest: self.manifest(generation),
            revocations: RevocationSet::new(),
            node: node.clone(),
            credentials: self.credentials(generation, node),
            keys,
        }
    }
    /// The upgrade from `from` to `to`, with every intermediate step.
    fn upgrade(&self, from: u32, to: u32) -> Upgrade {
        self.upgrade_at(from, to, 1_000)
    }
    /// `upgrade`, with the step into generation 5 signed at `readded`.
    fn upgrade_at(&self, from: u32, to: u32, readded: u64) -> Upgrade {
        Upgrade {
            steps: (from..to)
                .map(|generation| GenerationStep {
                    manifest: Some(self.manifest(generation)),
                    transition: match generation {
                        4 => self.transition_at(4, readded),
                        _ => self.transition(generation),
                    },
                })
                .collect(),
            tables: Some((self.schema(to), self.domain, self.readers(to))),
        }
    }
    /// The id of `items.note`.
    fn note(&self) -> Commitment {
        let schema = self.schema(3);
        let items = schema.table("items").unwrap();
        schema.field_id(items, items.column("note").unwrap())
    }
    async fn open(
        &self,
        path: &std::path::Path,
        node: &PrivateKey,
        generation: u32,
        upgrade: Option<Upgrade>,
    ) -> Result<EncryptedReplica<TypedRows>, Error> {
        let replica = EncryptedReplica::open_upgrading(
            path,
            self.app(generation),
            self.config(generation, node),
            upgrade,
        )
        .await?;
        replica
            .install_tables(
                self.schema(generation),
                self.domain,
                self.readers(generation),
            )
            .await?;
        self.register_authors(&replica, generation, node).await?;
        Ok(replica)
    }
    async fn register_authors(
        &self,
        replica: &EncryptedReplica<TypedRows>,
        generation: u32,
        node: &PrivateKey,
    ) -> Result<(), Error> {
        for table in self.schema(generation).tables {
            let material = rocky_core::commitment(&table.sql_name, node.as_bytes());
            let key = PrivateKey::from_bytes(&material);
            let number =
                (u32::from_le_bytes(material[..4].try_into().unwrap()) & 0x7fff_ffff).max(1);
            let grants: Vec<_> = self
                .grants(generation)
                .into_iter()
                .filter(|grant| grant.scope == self.scope(&table.sql_name))
                .collect();
            let assigner = PrivateKey::new();
            let root = crate::credentials::Authority {
                publisher: &self.publisher,
                manifest: self.definition(generation).hash(),
            }
            .issue_terms(
                assigner.public_key(),
                grants.clone(),
                (1, Some(crate::credentials::AuthorRange::ALL)),
                crate::tests::validity(),
            )
            .unwrap();
            let chain = CredentialChain(vec![root])
                .assign(
                    &assigner,
                    key.public_key(),
                    number,
                    node.public_key(),
                    grants,
                    u64::MAX,
                    (0, u64::MAX, 0),
                )
                .unwrap();
            replica.register_row_author(key, chain).await?;
        }
        Ok(())
    }
}
async fn session(replica: &EncryptedReplica<TypedRows>) -> SecureSql<TypedRows> {
    let scopes = {
        let inner = replica.inner.lock().await;
        inner.config.keys.iter().map(|key| key.scope).collect()
    };
    replica
        .session(scopes)
        .await
        .unwrap()
        .sql(SqlOptions::default())
        .await
        .unwrap()
}
async fn select(replica: &EncryptedReplica<TypedRows>, sql: &str) -> Vec<Vec<SqlValue>> {
    session(replica).await.run(sql, vec![]).await.unwrap().rows
}
async fn identity_insert(
    session: &SecureSql<TypedRows>,
    sql: &str,
    mut values: Vec<SqlValue>,
) -> Result<SecureRows, Error> {
    let implicit = session.state().await == crate::sql::SqlState::Idle;
    if implicit {
        session.run("BEGIN", vec![]).await?;
    }
    let result = async {
        let (head, tail) = sql.split_once(" VALUES").unwrap();
        let name = head
            .trim_start_matches("INSERT INTO ")
            .split('(')
            .next()
            .unwrap();
        let columns = match head.split_once('(') {
            Some((_, columns)) => columns.trim_end_matches(')').to_string(),
            None => session
                .catalog()
                .into_iter()
                .find(|table| table.name == name)
                .unwrap()
                .columns
                .iter()
                .filter(|column| !["id", "row_kind"].contains(&column.name.as_str()))
                .map(|column| column.name.clone())
                .collect::<Vec<_>>()
                .join(","),
        };
        let id = session.next_id(name).await?;
        let tail = tail.trim().trim_end_matches(')');
        // The allocator value is a checked integer; embed it without changing positional bindings.
        let rewritten = format!(
            "INSERT INTO {name}({columns},id) VALUES{tail},{})",
            id.integer()
        );
        session.run(&rewritten, std::mem::take(&mut values)).await
    }
    .await;
    if implicit {
        match result {
            Ok(mut result) => {
                result.committed = session.run("COMMIT", vec![]).await?.committed;
                Ok(result)
            }
            Err(error) => {
                let _ = session.run("ROLLBACK", vec![]).await;
                Err(error)
            }
        }
    } else {
        result
    }
}
async fn insert(replica: &EncryptedReplica<TypedRows>, ids: std::ops::Range<usize>) {
    let session = session(replica).await;
    session.run("BEGIN IMMEDIATE", vec![]).await.unwrap();
    for id in ids.clone() {
        identity_insert(
            &session,
            "INSERT INTO items(key,title) VALUES(?,?)",
            vec![text(&format!("{id:04}")), text(&format!("title {id}"))],
        )
        .await
        .unwrap();
    }
    session
        .commit(rocky_core::commitment(
            "generations.insert",
            format!("{ids:?}").as_bytes(),
        ))
        .await
        .unwrap();
}
/// Every frame `target` may receive from `source`, page by page.
async fn frames(
    source: &EncryptedReplica<TypedRows>,
    target: &EncryptedReplica<TypedRows>,
    scope: ResourceScope,
) -> Vec<EncryptedFrame> {
    let (node, credentials) = {
        let inner = target.inner.lock().await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    frames_as(source, node, &credentials, scope).await
}
/// Every frame a node presenting `credentials` may receive from `source`.
async fn frames_as(
    source: &EncryptedReplica<TypedRows>,
    node: PublicKey,
    credentials: &CredentialChain,
    scope: ResourceScope,
) -> Vec<EncryptedFrame> {
    let mut frames = Vec::new();
    let mut after = None;
    loop {
        let page = source
            .transfer_page(node, credentials, &[scope], after, MAX_SECURE_FRAME)
            .await
            .unwrap();
        frames.extend(page.frames);
        match page.next {
            Some(next) => after = Some(next),
            None => return frames,
        }
    }
}

#[tokio::test]
async fn a_local_plan_survives_an_interruption_at_every_step() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::new();
    for (round, stage) in [PlanStage::Create, PlanStage::Fill, PlanStage::Switch]
        .into_iter()
        .enumerate()
    {
        let path = dir.path().join(format!("{round}.db"));
        let replica = lineage.open(&path, &node, 1, None).await.unwrap();
        insert(&replica, 0..150).await;
        insert(&replica, 150..300).await;
        // A second fill step is interrupted: the first one committed.
        replica
            .inject_plan_fault(stage, usize::from(stage == PlanStage::Fill))
            .await;
        let failed = replica
            .install_tables(lineage.indexed(), lineage.domain, lineage.readers(1))
            .await;
        assert!(matches!(failed, Err(Error::InjectedFailure)), "{failed:?}");
        let pending = replica.projection_plan().await.unwrap().unwrap();
        assert_eq!(pending.tables, 1);
        assert_eq!(pending.filled, usize::from(stage == PlanStage::Switch));
        // Nothing half-built is visible; the installed table still answers.
        assert_eq!(
            select(&replica, "SELECT count(*) FROM items").await,
            vec![vec![SqlValue::Integer(300)]]
        );
        // Another plan waits for this one.
        let mut other = lineage.indexed();
        other.tables[0].indexes[1].name = "items_by_title".into();
        other.tables[0].indexes[1].create_sql =
            "CREATE INDEX items_by_title ON items(title)".into();
        let other = replica
            .install_tables(other, lineage.domain, lineage.readers(1))
            .await;
        assert!(matches!(other, Err(Error::Incompatible(_))), "{other:?}");
        // Writes between steps reach the shadow as well.
        let writer = session(&replica).await;
        writer
            .run("UPDATE items SET title='changed' WHERE key='0001'", vec![])
            .await
            .unwrap();
        writer
            .run("DELETE FROM items WHERE key='0299'", vec![])
            .await
            .unwrap();
        identity_insert(&writer, "INSERT INTO items VALUES('9999','late')", vec![])
            .await
            .unwrap();
        drop(writer);
        drop(replica);
        // Reopening finishes the same plan before anything else runs.
        let replica = EncryptedReplica::open(&path, lineage.app(1), lineage.config(1, &node))
            .await
            .unwrap();
        assert_eq!(replica.projection_plan().await.unwrap(), None);
        replica
            .install_tables(lineage.indexed(), lineage.domain, lineage.readers(1))
            .await
            .unwrap();
        assert_eq!(
            select(&replica, "SELECT count(*), max(key) FROM items").await,
            vec![vec![SqlValue::Integer(300), text("9999")]]
        );
        assert_eq!(
            select(&replica, "SELECT title FROM items WHERE key='0001'").await,
            vec![vec![text("changed")]]
        );
        let indexes = {
            let inner = replica.inner.lock().await;
            rows(
                &inner.conn,
                "SELECT name FROM sqlite_schema WHERE type='index' AND tbl_name='items' AND name NOT LIKE 'sqlite_%'",
            )
            .await
            .unwrap()
        };
        assert_eq!(
            indexes,
            vec![vec![text("items_identity")], vec![text("items_title")]]
        );
        let status = replica.generations().await.unwrap();
        assert_eq!(
            status.current,
            Generation {
                schema: 1,
                handler: 1
            }
        );
        assert!(status.previous.is_empty());
    }
}

#[tokio::test]
async fn an_uninterrupted_local_plan_switches_while_reads_continue() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let replica = lineage
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 1, None)
        .await
        .unwrap();
    insert(&replica, 0..10).await;
    let mut events = replica.observe_revision();
    let before = *events.borrow_and_update();
    replica
        .install_tables(lineage.indexed(), lineage.domain, lineage.readers(1))
        .await
        .unwrap();
    assert!(*events.borrow() > before, "the switch is announced");
    assert_eq!(
        select(&replica, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(10)]]
    );
    let plan = {
        let inner = replica.inner.lock().await;
        rows(
            &inner.conn,
            "EXPLAIN QUERY PLAN SELECT key FROM items WHERE title='title 3'",
        )
        .await
        .unwrap()
    };
    assert!(format!("{plan:?}").contains("items_title"), "{plan:?}");
    // A renamed field is another field: refused before a plan.
    let mut renamed = lineage.schema(1);
    renamed.tables[0].columns[1].sql_name = "name".into();
    assert!(matches!(
        replica
            .install_tables(renamed, lineage.domain, lineage.readers(1))
            .await,
        Err(Error::Incompatible(_))
    ));
    assert_eq!(replica.projection_plan().await.unwrap(), None);
}

#[tokio::test]
async fn a_signed_upgrade_keeps_identities_pending_work_and_every_older_peer() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
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
    let items = lineage.scope("items");
    insert(&alice, 0..2).await;
    // Alice's second row has not left her device when she upgrades.
    let sent = frames(&alice, &bob, items).await;
    assert_eq!(sent.len(), 1);
    for frame in sent {
        assert_eq!(bob.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    }
    insert(&alice, 2..3).await;
    drop(alice);

    // An unsupported change activates nothing.
    let mut required = lineage.upgrade(1, 2);
    if let Some((schema, _, _)) = &mut required.tables {
        schema.tables[0].columns[2].not_null = true;
    }
    assert!(
        EncryptedReplica::open_upgrading(
            &alice_path,
            lineage.app(2),
            lineage.config(2, &alice_key),
            Some(required),
        )
        .await
        .is_err()
    );
    // An activation interrupted before its commit leaves generation 1.
    EncryptedReplica::<TypedRows>::fail_next_activation();
    assert!(matches!(
        lineage
            .open(&alice_path, &alice_key, 2, Some(lineage.upgrade(1, 2)))
            .await,
        Err(Error::InjectedFailure)
    ));
    let alice = lineage
        .open(&alice_path, &alice_key, 1, None)
        .await
        .unwrap();
    assert_eq!(alice.generations().await.unwrap().current.schema, 1);
    drop(alice);

    let alice = lineage
        .open(&alice_path, &alice_key, 2, Some(lineage.upgrade(1, 2)))
        .await
        .unwrap();
    let status = alice.generations().await.unwrap();
    assert_eq!(
        status.current,
        Generation {
            schema: 2,
            handler: 1
        }
    );
    assert_eq!(status.previous.len(), 1);
    // Existing rows gained the new column, empty; the new table exists.
    assert_eq!(
        select(&alice, "SELECT key,title,note FROM items ORDER BY key").await,
        (0..3)
            .map(|id| vec![
                text(&format!("{id:04}")),
                text(&format!("title {id}")),
                SqlValue::Null
            ])
            .collect::<Vec<_>>()
    );
    let writer = session(&alice).await;
    identity_insert(
        &writer,
        "INSERT INTO items VALUES('0100','new','with a note')",
        vec![],
    )
    .await
    .unwrap();
    identity_insert(&writer, "INSERT INTO tags VALUES('t','label')", vec![])
        .await
        .unwrap();
    drop(writer);

    // Bob, still at generation 1, receives Alice's pending generation 1
    // row with its original bytes, and nothing of generation 2.
    let sent = frames(&alice, &bob, items).await;
    assert_eq!(sent.len(), 2);
    let mut statuses = Vec::new();
    for frame in sent {
        statuses.push(bob.receive(frame).await.unwrap());
    }
    assert!(statuses.contains(&ReceiveStatus::Applied));
    assert_eq!(
        select(&bob, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(3)]]
    );
    // Bob's own generation 1 write is admitted by Alice. Bob cannot verify
    // generation 2 credentials; Alice pulls with the ones she had before.
    identity_insert(
        &session(&bob).await,
        "INSERT INTO items VALUES('0200','from bob')",
        vec![],
    )
    .await
    .unwrap();
    let earlier = lineage.credentials(1, &alice_key);
    for frame in frames_as(&bob, alice_key.public_key(), &earlier, items).await {
        alice.receive(frame).await.unwrap();
    }
    assert_eq!(
        select(&alice, "SELECT title,note FROM items WHERE key='0200'").await,
        vec![vec![text("from bob"), SqlValue::Null]]
    );
    // A generation 2 frame forced on Bob is refused, and admitted once he
    // upgrades.
    let newer: Vec<_> = {
        let inner = alice.inner.lock().await;
        let (node, credentials) = (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        );
        drop(inner);
        alice
            .transfer_page(node, &credentials, &[items], None, MAX_SECURE_FRAME)
            .await
            .unwrap()
            .frames
    };
    let mut refused = 0;
    for frame in newer {
        if bob.receive(frame).await.is_err() {
            refused += 1;
        }
    }
    assert_eq!(refused, 1, "only the generation 2 frame is refused");
    assert_eq!(
        select(&bob, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(4)]]
    );
    bob.audit(items).await.unwrap();
    let bob_path = dir.path().join("bob.db");
    drop(bob);
    let bob = lineage
        .open(&bob_path, &bob_key, 2, Some(lineage.upgrade(1, 2)))
        .await
        .unwrap();
    bob.audit(items).await.unwrap();
    for frame in frames(&alice, &bob, items).await {
        bob.receive(frame).await.unwrap();
    }
    bob.audit(items).await.unwrap();
    assert_eq!(
        select(&bob, "SELECT note FROM items WHERE key='0100'").await,
        vec![vec![text("with a note")]]
    );
    // Reopening at generation 2 needs no upgrade; the audit replays the
    // mixed history.
    drop(bob);
    let bob = lineage.open(&bob_path, &bob_key, 2, None).await.unwrap();
    bob.audit(items).await.unwrap();

    // Generation 3 has no admission horizon: a device two generations
    // behind keeps syncing with its generation 1 credentials.
    let carol_key = PrivateKey::new();
    let carol = lineage
        .open(&dir.path().join("carol.db"), &carol_key, 1, None)
        .await
        .unwrap();
    identity_insert(
        &session(&carol).await,
        "INSERT INTO items VALUES('0300','older')",
        vec![],
    )
    .await
    .unwrap();
    drop(alice);
    let alice = lineage
        .open(&alice_path, &alice_key, 3, Some(lineage.upgrade(2, 3)))
        .await
        .unwrap();
    assert_eq!(alice.generations().await.unwrap().previous.len(), 2);
    let older = frames_as(&carol, alice_key.public_key(), &earlier, items).await;
    assert_eq!(older.len(), 1);
    for frame in older {
        assert_eq!(alice.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    }
    let (node, credentials) = {
        let inner = carol.inner.lock().await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    alice
        .transfer_page(node, &credentials, &[items], None, MAX_SECURE_FRAME)
        .await
        .unwrap();
    let second = lineage.credentials(2, &alice_key);
    for frame in frames_as(&bob, alice_key.public_key(), &second, items).await {
        alice.receive(frame).await.unwrap();
    }
    alice.audit(items).await.unwrap();
    assert_eq!(
        select(&alice, "SELECT count(*), max(rank) FROM items").await,
        vec![vec![SqlValue::Integer(6), SqlValue::Null]]
    );
}

#[tokio::test]
async fn a_checkpoint_of_the_previous_generation_rebootstraps_typed_rows_and_tombstones() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let (alice_key, bob_key, carol_key) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let alice_path = dir.path().join("alice.db");
    let alice = lineage
        .open(&alice_path, &alice_key, 1, None)
        .await
        .unwrap();
    let bob = lineage
        .open(&dir.path().join("bob.db"), &bob_key, 1, None)
        .await
        .unwrap();
    let items = lineage.scope("items");
    insert(&alice, 0..40).await;
    let writer = session(&alice).await;
    writer
        .run("DELETE FROM items WHERE key='0003'", vec![])
        .await
        .unwrap();
    writer
        .run("UPDATE items SET title='kept' WHERE key='0004'", vec![])
        .await
        .unwrap();
    drop(writer);
    for frame in frames(&alice, &bob, items).await {
        bob.receive(frame).await.unwrap();
    }
    let bob_credentials = lineage.credentials(1, &bob_key);
    alice
        .acknowledge_frontier(
            bob_key.public_key(),
            &bob_credentials,
            &bob.frontier(items).await.unwrap(),
        )
        .await
        .unwrap();
    alice
        .set_retention_policy(
            items,
            RetentionPolicy {
                required: [rocky_credentials::PeerId::of(bob_key.public_key())].into(),
                behind: [rocky_credentials::PeerId::of(carol_key.public_key())].into(),
                copies: 1,
                window: 0,
            },
        )
        .await
        .unwrap();
    let signed = alice.checkpoint(items).await.unwrap();
    assert!(alice.prune(items).await.unwrap().pruned > 0);
    drop(alice);

    // Alice moves to generation 2; her checkpoint is of generation 1.
    let alice = lineage
        .open(&alice_path, &alice_key, 2, Some(lineage.upgrade(1, 2)))
        .await
        .unwrap();
    identity_insert(
        &session(&alice).await,
        "INSERT INTO items VALUES('0100','after','noted')",
        vec![],
    )
    .await
    .unwrap();
    // Carol starts at generation 2 with the lineage, and is behind Alice's
    // horizon: she imports the generation 1 checkpoint, then catches up.
    let carol_credentials = lineage.credentials(2, &carol_key);
    let carol = EncryptedReplica::open_upgrading(
        dir.path().join("carol.db"),
        lineage.app(2),
        lineage.config(2, &carol_key),
        Some(lineage.upgrade(1, 2)),
    )
    .await
    .unwrap();
    carol
        .install_tables(lineage.schema(2), lineage.domain, lineage.readers(2))
        .await
        .unwrap();
    let offered = alice
        .checkpoint_offer(carol_key.public_key(), &carol_credentials, items)
        .await
        .unwrap()
        .expect("a checkpoint");
    assert_eq!(offered.id().unwrap(), signed.id().unwrap());
    let checkpoint = offered.id().unwrap();
    carol.begin_rebootstrap(offered).await.unwrap();
    let mut after = None;
    loop {
        let page = alice
            .checkpoint_page(
                carol_key.public_key(),
                &carol_credentials,
                items,
                checkpoint,
                after,
                64 * 1024,
            )
            .await
            .unwrap();
        after = page.next;
        carol.stage_checkpoint_page(page).await.unwrap();
        if after.is_none() {
            break;
        }
    }
    carol.finish_rebootstrap(items).await.unwrap();
    for frame in frames(&alice, &carol, items).await {
        carol.receive(frame).await.unwrap();
    }
    carol.audit(items).await.unwrap();
    assert_eq!(
        select(&carol, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(40)]]
    );
    assert_eq!(
        select(&carol, "SELECT title FROM items WHERE key='0004'").await,
        vec![vec![text("kept")]]
    );
    assert_eq!(
        select(&carol, "SELECT note FROM items WHERE key='0100'").await,
        vec![vec![text("noted")]]
    );
    // The checkpoint retains the old numeric identity and its sequence mark.
    let number = (u32::from_le_bytes(
        rocky_core::commitment("items", alice_key.as_bytes())[..4]
            .try_into()
            .unwrap(),
    ) & 0x7fff_ffff)
        .max(1);
    let old_id = rocky_core::RowIdentity::new(number, 4).unwrap();
    assert!(
        session(&carol)
            .await
            .run(
                "INSERT INTO items(key,title,note,id) VALUES('0003','again',NULL,?)",
                vec![SqlValue::Integer(old_id.integer())]
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_backup_copy_opens_only_under_a_fresh_identity_and_keeps_its_history() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let (old_key, new_key) = (PrivateKey::new(), PrivateKey::new());
    let alice = lineage
        .open(&dir.path().join("alice.db"), &old_key, 1, None)
        .await
        .unwrap();
    insert(&alice, 0..3).await;
    let copy = dir.path().join("copy.db");
    alice.backup_into(&copy).await.unwrap();
    assert!(alice.backup_into(&copy).await.is_err(), "never overwritten");
    let restore = |node: &PrivateKey, predecessor: PublicKey| {
        EncryptedReplica::open_restored(
            &copy,
            lineage.app(1),
            lineage.config(1, node),
            None,
            predecessor,
        )
    };
    assert!(restore(&old_key, old_key.public_key()).await.is_err());
    assert!(
        restore(&new_key, PrivateKey::new().public_key())
            .await
            .is_err()
    );
    // Copying a file is not opening it: its identity is the old node's.
    assert!(
        EncryptedReplica::open(&copy, lineage.app(1), lineage.config(1, &new_key))
            .await
            .is_err()
    );
    let restored = restore(&new_key, old_key.public_key()).await.unwrap();
    restored
        .install_tables(lineage.schema(1), lineage.domain, lineage.readers(1))
        .await
        .unwrap();
    assert_eq!(
        restored.predecessors().await.unwrap(),
        [old_key.public_key()]
    );
    assert_eq!(
        select(&restored, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(3)]]
    );
    lineage
        .register_authors(&restored, 1, &new_key)
        .await
        .unwrap();
    insert(&restored, 3..4).await;
    let items = lineage.scope("items");
    let authors: BTreeSet<_> = frames(&restored, &alice, items)
        .await
        .iter()
        .map(|frame| frame.operation().unwrap().header.public_key)
        .collect();
    assert_eq!(
        authors,
        [
            PrivateKey::from_bytes(&rocky_core::commitment("items", old_key.as_bytes()))
                .public_key(),
            PrivateKey::from_bytes(&rocky_core::commitment("items", new_key.as_bytes()))
                .public_key()
        ]
        .into()
    );
    restored.audit(items).await.unwrap();
    drop(restored);
    // Rebinding happened once; reopening is ordinary.
    let reopened = restore(&new_key, old_key.public_key()).await.unwrap();
    assert_eq!(
        reopened.predecessors().await.unwrap(),
        [old_key.public_key()]
    );
}

#[tokio::test]
async fn an_open_replica_adopts_a_newer_generation_in_place() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let (alice_key, bob_key) = (PrivateKey::new(), PrivateKey::new());
    let alice_path = dir.path().join("alice.db");
    let alice = lineage
        .open(&alice_path, &alice_key, 1, None)
        .await
        .unwrap();
    insert(&alice, 0..3).await;
    let bob = lineage
        .open(
            &dir.path().join("bob.db"),
            &bob_key,
            3,
            Some(lineage.upgrade(1, 3)),
        )
        .await
        .unwrap();
    let items = lineage.scope("items");
    identity_insert(
        &session(&bob).await,
        "INSERT INTO items VALUES('0100','ranked',NULL,7)",
        vec![],
    )
    .await
    .unwrap();
    // Generation 3 frames forced on Alice are refused before the switch.
    let third = lineage.credentials(3, &alice_key);
    let newer = frames_as(&bob, alice_key.public_key(), &third, items).await;
    assert!(!newer.is_empty());
    for frame in newer {
        assert!(matches!(
            alice.receive(frame).await,
            Err(Error::Incompatible(_))
        ));
    }
    let mut observer = alice.observe_tables(None).await.unwrap();
    let before = session(&alice).await;

    // A transition another key signed changes nothing.
    let mut forged = lineage.upgrade(1, 3);
    forged.steps[1].transition = Lineage::new().transition(2);
    assert!(
        alice
            .adopt_generation(lineage.app(3), lineage.config(3, &alice_key), forged)
            .await
            .is_err()
    );
    assert_eq!(alice.generations().await.unwrap().current.schema, 1);
    assert_eq!(
        before
            .run("SELECT count(*) FROM items", vec![])
            .await
            .unwrap()
            .rows,
        vec![vec![SqlValue::Integer(3)]]
    );

    assert!(
        alice
            .adopt_generation(
                lineage.app(3),
                lineage.config(3, &alice_key),
                lineage.upgrade(1, 3)
            )
            .await
            .unwrap()
    );
    let status = alice.generations().await.unwrap();
    assert_eq!(status.current.schema, 3);
    assert_eq!(status.previous.len(), 2);
    assert_eq!(status.plan, None);
    // Sessions planned against the old tables end; observers stay.
    assert!(before.run("SELECT 1", vec![]).await.is_err());
    assert!(observer.try_next().unwrap().is_some());
    assert_eq!(
        select(&alice, "SELECT key,note,rank FROM items ORDER BY key").await,
        (0..3)
            .map(|id| vec![text(&format!("{id:04}")), SqlValue::Null, SqlValue::Null])
            .collect::<Vec<_>>()
    );
    for frame in frames(&bob, &alice, items).await {
        alice.receive(frame).await.unwrap();
    }
    assert_eq!(
        select(&alice, "SELECT title,rank FROM items WHERE key='0100'").await,
        vec![vec![text("ranked"), SqlValue::Integer(7)]]
    );
    // Adopting the current generation again changes nothing.
    assert!(
        !alice
            .adopt_generation(
                lineage.app(3),
                lineage.config(3, &alice_key),
                lineage.upgrade(1, 3)
            )
            .await
            .unwrap()
    );
    // Its writes reach the generation 3 peer, and the file reopens at 3.
    lineage
        .register_authors(&alice, 3, &alice_key)
        .await
        .unwrap();
    identity_insert(
        &session(&alice).await,
        "INSERT INTO tags VALUES('t','label')",
        vec![],
    )
    .await
    .unwrap();
    for frame in frames(&alice, &bob, lineage.scope("tags")).await {
        bob.receive(frame).await.unwrap();
    }
    assert_eq!(
        select(&bob, "SELECT label FROM tags").await,
        vec![vec![text("label")]]
    );
    drop(before);
    drop(observer);
    drop(alice);
    let alice = lineage
        .open(&alice_path, &alice_key, 3, Some(lineage.upgrade(1, 3)))
        .await
        .unwrap();
    alice.audit(items).await.unwrap();
    assert_eq!(
        select(&alice, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(4)]]
    );
}

#[tokio::test]
async fn one_schema_signed_apart_is_one_generation_with_the_earliest_baseline() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let path = dir.path().join("alice.db");
    let alice = lineage
        .open(&path, &key, 2, Some(lineage.upgrade(1, 2)))
        .await
        .unwrap();
    assert_eq!(alice.generations().await.unwrap().baseline, Some(1_000));
    // Another issuer signed the same step later, then one earlier.
    assert!(
        !alice
            .learn_baseline(&lineage.transition_at(1, 1_500))
            .await
            .unwrap()
    );
    assert!(
        alice
            .learn_baseline(&lineage.transition_at(1, 400))
            .await
            .unwrap()
    );
    let status = alice.generations().await.unwrap();
    assert_eq!(status.baseline, Some(400));
    assert_eq!(status.previous.len(), 1);
    // Another step or another publisher's transition is refused.
    assert!(
        !alice
            .learn_baseline(&lineage.transition_at(2, 100))
            .await
            .unwrap()
    );
    assert!(
        alice
            .learn_baseline(&Lineage::new().transition_at(1, 100))
            .await
            .is_err()
    );
    drop(alice);
    let alice = lineage
        .open(&path, &key, 2, Some(lineage.upgrade(1, 2)))
        .await
        .unwrap();
    assert_eq!(alice.generations().await.unwrap().baseline, Some(400));
}

#[tokio::test]
async fn an_older_generation_the_replica_lacks_is_learned_before_its_frames_apply() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let (alice_key, carol_key) = (PrivateKey::new(), PrivateKey::new());
    // Alice knows generations 2 and 3 only, as a device that dropped 1.
    let alice = lineage
        .open(
            &dir.path().join("alice.db"),
            &alice_key,
            3,
            Some(lineage.upgrade(2, 3)),
        )
        .await
        .unwrap();
    assert_eq!(alice.generations().await.unwrap().previous.len(), 1);
    let carol = lineage
        .open(&dir.path().join("carol.db"), &carol_key, 1, None)
        .await
        .unwrap();
    identity_insert(
        &session(&carol).await,
        "INSERT INTO items VALUES('0001','from generation 1')",
        vec![],
    )
    .await
    .unwrap();
    let items = lineage.scope("items");
    let earlier = lineage.credentials(1, &alice_key);
    for frame in frames_as(&carol, alice_key.public_key(), &earlier, items).await {
        assert!(matches!(
            alice.receive(frame).await,
            Err(Error::Incompatible(_))
        ));
    }
    // Another publisher's steps, or none that lead to a known generation,
    // record nothing.
    assert!(
        alice
            .learn_generations(&Lineage::new().upgrade(1, 3).steps)
            .await
            .is_err()
    );
    assert_eq!(
        alice
            .learn_generations(&lineage.upgrade(1, 2).steps[..0])
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        alice
            .learn_generations(&lineage.upgrade(1, 3).steps)
            .await
            .unwrap(),
        1
    );
    let status = alice.generations().await.unwrap();
    assert_eq!(status.current.schema, 3);
    assert_eq!(status.previous.len(), 2);
    for frame in frames_as(&carol, alice_key.public_key(), &earlier, items).await {
        alice.receive(frame).await.unwrap();
    }
    assert_eq!(
        select(&alice, "SELECT title, note, rank FROM items").await,
        vec![vec![
            text("from generation 1"),
            SqlValue::Null,
            SqlValue::Null
        ]]
    );
}

/// Whether any stored row, or checkpoint row, holds `field`.
async fn holds(replica: &EncryptedReplica<TypedRows>, table: &str, field: Commitment) -> bool {
    let inner = replica.inner.lock().await;
    rows(&inner.conn, &format!("SELECT fields FROM {table}"))
        .await
        .unwrap()
        .iter()
        .any(|row| match row.as_slice() {
            [SqlValue::Blob(bytes)] => bytes.windows(32).any(|window| window == field),
            _ => false,
        })
}
/// Delivers every frame of `scope` from `source` at generation
/// `generation` to `target`, applied or not.
async fn deliver(
    source: &EncryptedReplica<TypedRows>,
    target: &EncryptedReplica<TypedRows>,
    target_key: &PrivateKey,
    lineage: &Lineage,
    generation: u32,
    scope: ResourceScope,
) {
    let credentials = lineage.credentials(generation, target_key);
    for frame in frames_as(source, target_key.public_key(), &credentials, scope).await {
        let _ = target.receive(frame).await;
    }
}
async fn write(replica: &EncryptedReplica<TypedRows>, sql: &str) {
    session(replica).await.run(sql, vec![]).await.unwrap();
}

#[tokio::test]
async fn a_generation_removes_a_field_and_a_table_on_every_replica_that_adopts_it() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let (alice_key, carol_key) = (PrivateKey::new(), PrivateKey::new());
    let alice_path = dir.path().join("alice.db");
    let alice = lineage
        .open(&alice_path, &alice_key, 3, Some(lineage.upgrade(1, 3)))
        .await
        .unwrap();
    let carol = lineage
        .open(
            &dir.path().join("carol.db"),
            &carol_key,
            3,
            Some(lineage.upgrade(1, 3)),
        )
        .await
        .unwrap();
    let (items, tags, note) = (
        lineage.scope("items"),
        lineage.scope("tags"),
        lineage.note(),
    );
    for (key, title) in [("0001", "one"), ("0002", "two")] {
        identity_insert(
            &session(&alice).await,
            &format!("INSERT INTO items VALUES('{key}','{title}','noted',NULL)"),
            vec![],
        )
        .await
        .unwrap();
    }
    identity_insert(
        &session(&alice).await,
        "INSERT INTO tags VALUES('t','label')",
        vec![],
    )
    .await
    .unwrap();
    deliver(&alice, &carol, &carol_key, &lineage, 3, items).await;
    // A checkpoint sealed at generation 3 holds the field.
    alice.checkpoint(items).await.unwrap();
    assert!(holds(&alice, "secure_checkpoint_rows", note).await);

    assert!(
        alice
            .adopt_generation(
                lineage.app(4),
                lineage.config(4, &alice_key),
                lineage.upgrade(1, 4)
            )
            .await
            .unwrap()
    );
    assert_eq!(alice.generations().await.unwrap().current.schema, 4);
    // Exactly the new schema, every remaining row visible.
    let reader = session(&alice).await;
    assert!(reader.run("SELECT note FROM items", vec![]).await.is_err());
    assert!(reader.run("SELECT * FROM tags", vec![]).await.is_err());
    drop(reader);
    assert_eq!(
        select(&alice, "SELECT key,title FROM items ORDER BY key").await,
        vec![
            vec![text("0001"), text("one")],
            vec![text("0002"), text("two")]
        ]
    );
    // No stored row or checkpoint keeps the removed field or table.
    assert!(!holds(&alice, "secure_rows", note).await);
    assert!(!holds(&alice, "secure_checkpoint_rows", note).await);
    {
        let inner = alice.inner.lock().await;
        let stored = |scope: ResourceScope| {
            let conn = inner.conn.clone();
            async move {
                collect_rows(
                    conn.query(
                        "SELECT count(*) FROM secure_rows WHERE scope=?",
                        [crate::encode(&scope).unwrap()],
                    )
                    .await
                    .unwrap(),
                )
                .await
                .unwrap()
            }
        };
        assert_eq!(stored(tags).await, vec![vec![SqlValue::Integer(0)]]);
        assert_eq!(stored(items).await, vec![vec![SqlValue::Integer(2)]]);
    }
    alice.checkpoint(items).await.unwrap();
    assert!(!holds(&alice, "secure_checkpoint_rows", note).await);

    // A device still at generation 3 writes a kept and a removed field:
    // the kept one applies, the removed one is ignored.
    write(
        &carol,
        "UPDATE items SET title='uno', note='carol' WHERE key='0001'",
    )
    .await;
    write(&carol, "UPDATE tags SET label='other'").await;
    deliver(&carol, &alice, &alice_key, &lineage, 3, items).await;
    deliver(&carol, &alice, &alice_key, &lineage, 3, tags).await;
    assert_eq!(
        select(&alice, "SELECT title FROM items WHERE key='0001'").await,
        vec![vec![text("uno")]]
    );
    assert!(!holds(&alice, "secure_rows", note).await);
    alice.audit(items).await.unwrap();
    // The frame stays as signed; reopening replays it the same way.
    drop(alice);
    let alice = lineage
        .open(&alice_path, &alice_key, 4, Some(lineage.upgrade(1, 4)))
        .await
        .unwrap();
    alice.audit(items).await.unwrap();
    assert_eq!(
        select(&alice, "SELECT key,title FROM items ORDER BY key").await,
        vec![
            vec![text("0001"), text("uno")],
            vec![text("0002"), text("two")]
        ]
    );
    // The removal is read from the transitions: dropping the earlier
    // manifests nothing names keeps it.
    assert!(
        !alice
            .drop_unreferenced_generations()
            .await
            .unwrap()
            .is_empty()
    );
    drop(alice);
    let alice = lineage
        .open(&alice_path, &alice_key, 4, Some(lineage.upgrade(1, 4)))
        .await
        .unwrap();
    alice.audit(items).await.unwrap();
    assert!(!holds(&alice, "secure_rows", note).await);
    write(&carol, "UPDATE items SET note='again' WHERE key='0002'").await;
    deliver(&carol, &alice, &alice_key, &lineage, 3, items).await;
    assert!(!holds(&alice, "secure_rows", note).await);
}

#[tokio::test]
async fn a_readded_field_ignores_earlier_writes_and_pulls_again_the_later_ones() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let keys: Vec<_> = (0..4).map(|_| PrivateKey::new()).collect();
    let mut replicas = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        replicas.push(
            lineage
                .open(
                    &dir.path().join(format!("{index}.db")),
                    key,
                    3,
                    Some(lineage.upgrade(1, 3)),
                )
                .await
                .unwrap(),
        );
    }
    let [alice, bob, carol, dave] = &replicas[..] else {
        unreachable!()
    };
    let [alice_key, bob_key, carol_key, dave_key] = &keys[..] else {
        unreachable!()
    };
    let items = lineage.scope("items");
    for (key, note) in [("0001", "old"), ("0002", "stale")] {
        identity_insert(
            &session(alice).await,
            &format!("INSERT INTO items VALUES('{key}','title','{note}',NULL)"),
            vec![],
        )
        .await
        .unwrap();
    }
    for (target, key) in [(bob, bob_key), (carol, carol_key), (dave, dave_key)] {
        deliver(alice, target, key, &lineage, 3, items).await;
    }
    // Dave writes before the field is added again; his frame travels late.
    write(dave, "UPDATE items SET note='early' WHERE key='0002'").await;
    let started = crate::micros().unwrap();
    let readded = started + 100_000_000;

    // Alice goes 3 -> 4 -> 5; Bob goes 3 -> 5.
    alice
        .adopt_generation(
            lineage.app(4),
            lineage.config(4, alice_key),
            lineage.upgrade(1, 4),
        )
        .await
        .unwrap();
    assert!(!holds(alice, "secure_rows", lineage.note()).await);
    // Carol, still at 3, writes after the baseline.
    crate::CLOCK_SKEW.with(|skew| skew.set(200));
    write(carol, "UPDATE items SET note='late' WHERE key='0001'").await;
    crate::CLOCK_SKEW.with(|skew| skew.set(0));
    // Alice, at 4, ignores the field and relays the frame as signed.
    deliver(carol, alice, alice_key, &lineage, 3, items).await;
    deliver(alice, bob, bob_key, &lineage, 3, items).await;
    for (replica, key) in [(alice, alice_key), (bob, bob_key)] {
        replica
            .adopt_generation(
                lineage.app(5),
                lineage.config(5, key),
                lineage.upgrade_at(1, 5, readded),
            )
            .await
            .unwrap();
        assert_eq!(replica.generations().await.unwrap().baseline, Some(readded));
    }
    // Both read the write stamped after the baseline and none before it.
    let notes = "SELECT key,note FROM items ORDER BY key";
    let expected = vec![
        vec![text("0001"), text("late")],
        vec![text("0002"), SqlValue::Null],
    ];
    assert_eq!(select(alice, notes).await, expected);
    assert_eq!(select(bob, notes).await, expected);
    // Dave's earlier write arrives now and is ignored everywhere.
    deliver(dave, alice, alice_key, &lineage, 3, items).await;
    deliver(dave, bob, bob_key, &lineage, 3, items).await;
    assert_eq!(select(alice, notes).await, expected);
    assert_eq!(select(bob, notes).await, expected);

    // An issuer signed the same step earlier: every device that learns that
    // baseline pulls again the frames stamped after it.
    let earlier = lineage.transition_at(4, started - 100_000_000);
    for replica in [alice, bob] {
        assert!(replica.learn_baseline(&earlier).await.unwrap());
        assert_eq!(
            replica.generations().await.unwrap().baseline,
            Some(started - 100_000_000)
        );
        assert_eq!(
            select(replica, notes).await,
            vec![
                vec![text("0001"), text("late")],
                vec![text("0002"), text("early")]
            ]
        );
        replica.audit(items).await.unwrap();
    }

    // Alice finalizes a horizon on her own; a replica that missed every
    // frame imports the finalized checkpoint and agrees.
    lineage.register_authors(alice, 5, alice_key).await.unwrap();
    let cut = rocky_credentials::SignedHorizon::sign(
        alice.horizon_statement(items, &[]).await.unwrap(),
        &lineage.publisher,
    )
    .unwrap();
    alice.receive_horizon(cut).await.unwrap();
    alice.finalize_horizon(items).await.unwrap();
    let erin_key = PrivateKey::new();
    let erin = lineage
        .open(
            &dir.path().join("erin.db"),
            &erin_key,
            5,
            Some(lineage.upgrade_at(1, 5, started - 100_000_000)),
        )
        .await
        .unwrap();
    let credentials = lineage.credentials(5, &erin_key);
    let offered = alice
        .checkpoint_offer(erin_key.public_key(), &credentials, items)
        .await
        .unwrap()
        .expect("a finalized checkpoint");
    assert!(offered.statement.finalized.is_some());
    let checkpoint = offered.id().unwrap();
    erin.begin_rebootstrap(offered).await.unwrap();
    let mut after = None;
    loop {
        let page = alice
            .checkpoint_page(
                erin_key.public_key(),
                &credentials,
                items,
                checkpoint,
                after,
                64 * 1024,
            )
            .await
            .unwrap();
        after = page.next;
        erin.stage_checkpoint_page(page).await.unwrap();
        if after.is_none() {
            break;
        }
    }
    erin.finish_rebootstrap(items).await.unwrap();
    assert_eq!(select(&erin, notes).await, select(alice, notes).await);
    erin.audit(items).await.unwrap();
}

#[tokio::test]
async fn an_unreferenced_manifest_is_dropped_and_learned_again_from_a_frame() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let (alice_key, carol_key) = (PrivateKey::new(), PrivateKey::new());
    // Alice starts at generation 3 with the lineage: nothing she holds
    // names generations 1 and 2.
    let alice = lineage
        .open(
            &dir.path().join("alice.db"),
            &alice_key,
            3,
            Some(lineage.upgrade(1, 3)),
        )
        .await
        .unwrap();
    assert_eq!(alice.generations().await.unwrap().previous.len(), 2);
    let dropped = alice.drop_unreferenced_generations().await.unwrap();
    assert_eq!(
        dropped,
        vec![lineage.definition(1).hash(), lineage.definition(2).hash()]
    );
    let status = alice.generations().await.unwrap();
    assert!(status.previous.is_empty());
    // The transition into generation 3 stays, and with it the baseline.
    assert_eq!(status.baseline, Some(1_000));
    assert!(
        alice
            .drop_unreferenced_generations()
            .await
            .unwrap()
            .is_empty()
    );

    // A frame of generation 1 is refused until its manifest is learned
    // again: the writer's manifest, with the transitions Alice kept.
    let carol = lineage
        .open(&dir.path().join("carol.db"), &carol_key, 1, None)
        .await
        .unwrap();
    identity_insert(
        &session(&carol).await,
        "INSERT INTO items VALUES('0001','from generation 1')",
        vec![],
    )
    .await
    .unwrap();
    let items = lineage.scope("items");
    let earlier = lineage.credentials(1, &alice_key);
    for frame in frames_as(&carol, alice_key.public_key(), &earlier, items).await {
        assert!(matches!(
            alice.receive(frame).await,
            Err(Error::Incompatible(_))
        ));
    }
    let carols = GenerationStep {
        manifest: Some(lineage.manifest(1)),
        transition: lineage.transition(1),
    };
    // A forged manifest is refused before anything is recorded.
    let forged = GenerationStep {
        manifest: Some(Lineage::new().manifest(1)),
        transition: lineage.transition(1),
    };
    assert!(alice.learn_generations(&[forged]).await.is_err());
    assert_eq!(alice.learn_generations(&[carols]).await.unwrap(), 1);
    let status = alice.generations().await.unwrap();
    assert_eq!(status.previous.len(), 1);
    for frame in frames_as(&carol, alice_key.public_key(), &earlier, items).await {
        alice.receive(frame).await.unwrap();
    }
    assert_eq!(
        select(&alice, "SELECT title, note, rank FROM items").await,
        vec![vec![
            text("from generation 1"),
            SqlValue::Null,
            SqlValue::Null
        ]]
    );
    // A retained frame names generation 1 now: its manifest stays.
    assert!(
        alice
            .drop_unreferenced_generations()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(alice.generations().await.unwrap().previous.len(), 1);
}

#[tokio::test]
async fn a_transition_that_hides_a_removal_is_refused() {
    let lineage = Lineage::new();
    let dir = TempDir::new().unwrap();
    let key = PrivateKey::new();
    let mut upgrade = lineage.upgrade(1, 4);
    let honest = upgrade.steps[2]
        .transition
        .verify(lineage.publisher.public_key())
        .unwrap();
    assert!(honest.fields.removed.contains(&lineage.note()));
    let hidden = rocky_core::GenerationTransition {
        fields: rocky_core::ItemChanges {
            removed: BTreeSet::new(),
            ..honest.fields.clone()
        },
        tables: rocky_core::ItemChanges::default(),
        ..honest
    };
    upgrade.steps[2].transition = SignedTransition::sign(&hidden, &lineage.publisher).unwrap();
    assert!(matches!(
        lineage
            .open(&dir.path().join("a.db"), &key, 4, Some(upgrade))
            .await,
        Err(Error::Incompatible(_))
    ));
}
