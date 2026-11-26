use super::*;
#[cfg(not(target_family = "wasm"))]
mod catch_up;
mod intents;
mod live_publication;
mod maintenance;
use crate::sql::{AbortReason, SqlError, SqlOptions, SqlState};
use crate::storage::collect_rows;
use rocky_core::{
    Affinity, Collation, ColumnClass, DomainId, LOGICAL_SCHEMA_FORMAT, LogicalColumn,
    LogicalSchema, LogicalTable, RowApplication, SchemaSource, Scope, SqlValue, TableClass,
    TypedRows,
};

const ITEMS: &str =
    "CREATE TABLE items(key TEXT NOT NULL PRIMARY KEY,title TEXT NOT NULL,done INTEGER NOT NULL)";
const SALARIES: &str =
    "CREATE TABLE salaries(key TEXT NOT NULL PRIMARY KEY,amount INTEGER NOT NULL)";
const DRAFTS: &str = "CREATE TABLE drafts(key INTEGER PRIMARY KEY,body TEXT)";

struct Typed {
    publisher: PrivateKey,
    schema: LogicalSchema,
    manifest: SignedManifest,
    policy: RevocationSet,
    domain: Scope,
    items: ResourceScope,
    salaries: ResourceScope,
}
fn column(name: &str, affinity: Affinity, not_null: bool) -> LogicalColumn {
    LogicalColumn {
        earlier_names: Vec::new(),
        sql_name: name.into(),
        affinity,
        not_null,
        default: None,
        collation: Collation::Binary,
        class: ColumnClass::Replicated,
    }
}
fn table(name: &str, sql: &str, columns: Vec<LogicalColumn>) -> LogicalTable {
    LogicalTable {
        earlier_names: Vec::new(),
        sql_name: name.into(),
        explicit_access_only: false,
        class: TableClass::Replayed,
        create_sql: sql.into(),
        auto_generate: false,
        columns,
        primary_key: vec!["key".into()],
        partition_by: Vec::new(),
        partition_group: None,
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    }
}
fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}
impl Typed {
    fn new() -> Self {
        let publisher = PrivateKey::new();
        let mut drafts = table(
            "drafts",
            DRAFTS,
            vec![
                column("key", Affinity::Integer, false),
                column("body", Affinity::Text, false),
            ],
        );
        drafts.class = TableClass::LocalOnly;
        let schema = LogicalSchema {
            format: LOGICAL_SCHEMA_FORMAT,
            publisher: *publisher.public_key().as_bytes(),
            application: "secure-sql".into(),
            source: SchemaSource {
                kind: "test".into(),
                version: 1,
                identity: "secure-sql".into(),
            },
            tables: vec![
                drafts,
                table(
                    "items",
                    ITEMS,
                    vec![
                        column("key", Affinity::Text, true),
                        column("title", Affinity::Text, true),
                        column("done", Affinity::Integer, true),
                    ],
                ),
                table(
                    "salaries",
                    SALARIES,
                    vec![
                        column("key", Affinity::Text, true),
                        column("amount", Affinity::Integer, true),
                    ],
                ),
            ],
            views: Vec::new(),
            assertions: Vec::new(),
            restrictions: Vec::new(),
            roles: Vec::new(),
        };
        Self::with_schema(publisher, schema)
    }
    fn with_schema(publisher: PrivateKey, schema: LogicalSchema) -> Self {
        let manifest = schema.manifest(&[DomainId([1; 16])]);
        let domain = Scope {
            mesh: MeshId([1; 16]),
            app: schema.app_id(),
            domain: DomainId([1; 16]),
        };
        let scope = |name: &str| ResourceScope {
            domain,
            encryption: rocky_core::commitment("secure-sql.readers", name.as_bytes()),
            table: schema.table_id(schema.table(name).unwrap()),
        };
        Self {
            manifest: SignedManifest::sign(&manifest, &publisher).unwrap(),
            policy: RevocationSet::new(),
            items: scope("items"),
            salaries: scope("salaries"),
            domain,
            schema,
            publisher,
        }
    }
    fn app(&self) -> TypedRows {
        TypedRows::new(self.schema.clone(), vec![DomainId([1; 16])]).unwrap()
    }
    fn fields(&self, name: &str, columns: &[&str]) -> Vec<Commitment> {
        let table = self.schema.table(name).unwrap();
        columns
            .iter()
            .map(|column| self.schema.field_id(table, table.column(column).unwrap()))
            .collect()
    }
    fn every_field(&self, name: &str) -> Vec<Commitment> {
        let table = self.schema.table(name).unwrap();
        table
            .columns
            .iter()
            .map(|column| self.schema.field_id(table, column))
            .collect()
    }
    fn writer(&self, scope: ResourceScope, name: &str) -> ResourceGrant {
        grant(
            scope,
            &[
                Right::Read,
                Right::Insert,
                Right::Update,
                Right::Delete,
                Right::Custody,
            ],
            &self.every_field(name),
        )
    }
    fn full(&self) -> Vec<ResourceGrant> {
        vec![
            self.writer(self.items, "items"),
            self.writer(self.salaries, "salaries"),
        ]
    }
    fn credentials(&self, node: &PrivateKey, grants: Vec<ResourceGrant>) -> CredentialChain {
        self.credentials_until(node, grants, crate::tests::lifetime().1)
    }
    fn credentials_until(
        &self,
        node: &PrivateKey,
        grants: Vec<ResourceGrant>,
        expires: u64,
    ) -> CredentialChain {
        let user = crate::tests::identity_of(node);
        let capability = |subject, kind, delegations| Capability {
            subject,
            kind,
            grants: if kind == PrincipalKind::Node {
                Vec::new()
            } else {
                grants.clone()
            },
            not_before: crate::tests::lifetime().0,
            expires: if kind == PrincipalKind::Node {
                rocky_credentials::NEVER
            } else {
                expires
            },
            delegations,
            extension: rocky_credentials::Extension::new(rocky_credentials::Extended {
                node: (kind == PrincipalKind::Node).then(|| rocky_credentials::PeerId::of(subject)),
                database: Some(rocky_credentials::DatabaseContext {
                    publisher: self.publisher.public_key(),
                    scope: self.items.domain,
                }),
                sequence: (kind == PrincipalKind::Node).then_some(0),
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
            SignedCapability::sign(
                crate::tests::proven(
                    capability(node.public_key(), PrincipalKind::Node, 0),
                    node,
                    &user,
                ),
                &user,
            )
            .unwrap(),
        ])
    }
    fn key(&self, scope: ResourceScope) -> KeyMaterial {
        KeyMaterial::new(
            scope,
            1,
            if scope == self.items {
                [31; 32]
            } else {
                [32; 32]
            },
        )
    }
    fn config(
        &self,
        node: &PrivateKey,
        grants: Vec<ResourceGrant>,
        keys: &[ResourceScope],
    ) -> SecureConfig {
        SecureConfig {
            publisher: self.publisher.public_key(),
            manifest: self.manifest.clone(),
            revocations: self.policy.clone(),
            node: node.clone(),
            credentials: self.credentials(node, grants),
            keys: keys.iter().map(|scope| self.key(*scope)).collect(),
        }
    }
    async fn open(
        &self,
        path: &std::path::Path,
        node: &PrivateKey,
        grants: Vec<ResourceGrant>,
        keys: &[ResourceScope],
    ) -> EncryptedReplica<TypedRows> {
        self.open_configured(path, self.config(node, grants, keys).into())
            .await
    }
    async fn open_on_manual_lease_clock(
        &self,
        path: &std::path::Path,
        clock: &crate::ManualLeaseClock,
    ) -> EncryptedReplica<TypedRows> {
        let both = [self.items, self.salaries];
        let config = OpenConfig::from(self.config(&PrivateKey::new(), self.full(), &both));
        self.open_configured(path, config.with_manual_lease_clock(clock))
            .await
    }
    async fn open_configured(
        &self,
        path: &std::path::Path,
        config: OpenConfig,
    ) -> EncryptedReplica<TypedRows> {
        let replica = EncryptedReplica::open(path, self.app(), config)
            .await
            .unwrap();
        replica
            .install_tables(
                self.schema.clone(),
                self.domain,
                [
                    ("items".to_string(), self.items.encryption),
                    ("salaries".to_string(), self.salaries.encryption),
                ]
                .into(),
            )
            .await
            .unwrap();
        replica
    }
}
async fn sql(
    replica: &EncryptedReplica<TypedRows>,
    scopes: &[ResourceScope],
) -> SecureSql<TypedRows> {
    replica
        .session(scopes.iter().copied().collect())
        .await
        .unwrap()
        .sql(SqlOptions::default())
        .await
        .unwrap()
}
async fn select(session: &SecureSql<TypedRows>, sql: &str) -> Vec<Vec<SqlValue>> {
    session.run(sql, vec![]).await.unwrap().rows
}
fn is_outcome_record(t: &Typed, frame: &EncryptedFrame) -> bool {
    let plaintext = crate::wire::Opened::open(
        &t.key(frame.scope().unwrap()).key,
        &frame.operation_ref().unwrap().header.extensions,
        frame.body_bytes(),
    )
    .unwrap()
    .payload;
    crate::wire::RowPayload::decode_test(&plaintext, frame, &t.app())
        .unwrap()
        .outcome()
        .is_some()
}
async fn transfer(
    source: &EncryptedReplica<TypedRows>,
    target: &EncryptedReplica<TypedRows>,
    scope: ResourceScope,
) -> CiphertextPage {
    let (node, credentials) = {
        let inner = target.inner.lock(crate::replica::Class::Background).await;
        (
            inner.config.node.public_key(),
            inner.config.credentials.clone(),
        )
    };
    source
        .transfer_page(node, &credentials, &[scope], None, MAX_SECURE_FRAME)
        .await
        .unwrap()
}
fn sql_error(result: Result<SecureRows, Error>) -> SqlError {
    match result {
        Err(Error::Sql(error)) => error,
        other => panic!("expected a SQL error, found {other:?}"),
    }
}

fn incomplete(error: SqlError, rule: &str) -> Vec<rocky_core::Blocker> {
    match error {
        SqlError::Unavailable {
            table,
            reason: crate::sql::Unavailable::Incomplete { blockers },
        } if table == "items" => blockers,
        other => panic!("{rule}: expected items to be incomplete, found {other:?}"),
    }
}

fn assert_pending_in(blockers: &[rocky_core::Blocker], scope: ResourceScope) {
    let label = rocky_core::scope_label(&scope);
    assert!(
        blockers.iter().any(|blocker| blocker.scope == label
            && matches!(blocker.kind, rocky_core::BlockerKind::PendingFrames { .. })),
        "an incomplete read names the pending frames of its scope: {blockers:?}"
    );
}

async fn insert(
    session: &SecureSql<TypedRows>,
    sql: &str,
    values: Vec<SqlValue>,
) -> Result<SecureRows, Error> {
    let implicit = session.state().await == SqlState::Idle;
    if implicit {
        session.run("BEGIN", vec![]).await?;
    }
    let result = session.run(sql, values).await;
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_sql_transaction_over_two_reader_scopes_commits_one_bundle() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let session = sql(&alice, &both).await;
    session.run("BEGIN IMMEDIATE", vec![]).await.unwrap();
    for (id, title) in [("a", "apples"), ("b", "bread"), ("c", "cheese")] {
        let inserted = insert(
            &session,
            "INSERT INTO items(key,title,done) VALUES(?,?,0)",
            vec![text(id), text(title)],
        )
        .await
        .unwrap();
        assert_eq!(inserted.changes, 1);
    }
    insert(&session, "INSERT INTO salaries VALUES('a',1000)", vec![])
        .await
        .unwrap();
    insert(
        &session,
        "INSERT INTO drafts(body) VALUES('local only')",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(
        select(&session, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(3)]],
        "the transaction reads its own writes"
    );
    let committed = session.commit([1; 32]).await.unwrap();
    assert_eq!(committed.state, SqlState::Idle);
    let frames = committed.committed.unwrap().frames;
    assert_eq!(frames.len(), 2);
    for (table, count) in [("items", 3), ("salaries", 1)] {
        assert_eq!(
            select(&session, &format!("SELECT count(*) FROM {table}")).await,
            vec![vec![SqlValue::Integer(count)]]
        );
    }

    let bob = t
        .open(
            &dir.path().join("b.db"),
            &PrivateKey::new(),
            vec![grant(
                t.items,
                &[Right::Read, Right::Custody],
                &t.every_field("items"),
            )],
            &[t.items],
        )
        .await;
    let public = frames
        .iter()
        .find(|frame| frame.scope().unwrap() == t.items)
        .unwrap()
        .clone();
    assert_eq!(bob.receive(public).await.unwrap(), ReceiveStatus::Applied);
    let reader = sql(&bob, &[t.items]).await;
    assert_eq!(
        select(&reader, "SELECT key,title FROM items ORDER BY key").await,
        vec![
            vec![text("a"), text("apples")],
            vec![text("b"), text("bread")],
            vec![text("c"), text("cheese")],
        ],
        "a public reader projects the public partition into its typed table"
    );
    assert_eq!(
        sql_error(reader.run("SELECT * FROM salaries", vec![]).await),
        SqlError::Denied,
        "a public reader cannot read the private partition"
    );
    assert_eq!(
        select(&reader, "SELECT count(*) FROM drafts").await,
        vec![vec![SqlValue::Integer(0)]]
    );

    let carol = t
        .open(
            &dir.path().join("c.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let full = sql(&carol, &both).await;
    let mut ordered = frames.clone();
    ordered.sort_by_key(|frame| frame.scope().unwrap() != t.items);
    carol.receive(ordered[0].clone()).await.unwrap();
    let blockers = incomplete(
        sql_error(full.run("SELECT count(*) FROM items", vec![]).await),
        "a full reader refuses the read as incomplete, not empty, until the bundle is whole",
    );
    assert_pending_in(&blockers, t.items);
    carol.receive(ordered[1].clone()).await.unwrap();
    assert_eq!(
        select(
            &full,
            "SELECT i.title,s.amount FROM items i JOIN salaries s USING(key)"
        )
        .await,
        vec![vec![text("apples"), SqlValue::Integer(1000)]]
    );

    for statement in [
        "UPDATE items SET done=1 WHERE key='b'",
        "DELETE FROM items WHERE key='c'",
    ] {
        session.run(statement, vec![]).await.unwrap();
    }
    let page = transfer(&alice, &bob, t.items).await;
    assert!(
        page.frames.iter().any(|frame| is_outcome_record(&t, frame)),
        "the items log also carries the author's outcome record of the bundle"
    );
    for status in bob.receive_all(page.frames).await {
        status.unwrap();
    }
    assert_eq!(
        select(&reader, "SELECT key,done FROM items ORDER BY key").await,
        vec![
            vec![text("a"), SqlValue::Integer(0)],
            vec![text("b"), SqlValue::Integer(1)],
        ],
        "single statements commit by themselves and peers follow updates and deletes"
    );
    insert(&session, "INSERT INTO items VALUES('c','again',0)", vec![])
        .await
        .unwrap();
    assert_eq!(
        select(&session, "SELECT count(*) FROM items WHERE key='c'").await,
        vec![vec![SqlValue::Integer(1)]],
        "a deleted business key can be inserted with a fresh identity"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn applied_rows_stay_readable_while_later_frames_await_their_predecessors() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            vec![t.writer(t.items, "items")],
            &[t.items],
        )
        .await;
    let session = sql(&alice, &[t.items]).await;
    let mut frames = Vec::new();
    for (key, title) in [("a", "apples"), ("b", "bread"), ("c", "cheese")] {
        let committed = insert(
            &session,
            "INSERT INTO items(key,title,done) VALUES(?,?,0)",
            vec![text(key), text(title)],
        )
        .await
        .unwrap();
        frames.extend(committed.committed.unwrap().frames);
    }
    let [first, second, third] = frames.try_into().unwrap();
    let reader_grant = || {
        vec![grant(
            t.items,
            &[Right::Read, Right::Custody],
            &t.every_field("items"),
        )]
    };

    let fresh = t
        .open(
            &dir.path().join("c.db"),
            &PrivateKey::new(),
            reader_grant(),
            &[t.items],
        )
        .await;
    assert_eq!(
        fresh.receive(third.clone()).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    let blockers = incomplete(
        sql_error(
            sql(&fresh, &[t.items])
                .await
                .run("SELECT count(*) FROM items", vec![])
                .await,
        ),
        "a reader that applied nothing yet refuses as incomplete, not empty",
    );
    assert_pending_in(&blockers, t.items);

    let bob = t
        .open(
            &dir.path().join("b.db"),
            &PrivateKey::new(),
            reader_grant(),
            &[t.items],
        )
        .await;
    let reader = sql(&bob, &[t.items]).await;
    assert_eq!(bob.receive(first).await.unwrap(), ReceiveStatus::Applied);
    assert_eq!(
        bob.receive(third).await.unwrap(),
        ReceiveStatus::PendingDependencies
    );
    assert_eq!(
        select(&reader, "SELECT key FROM items ORDER BY key").await,
        vec![vec![text("a")]],
        "a statement reads the applied rows while a later frame awaits its predecessor"
    );
    let mut cursor = reader
        .cursor(
            "SELECT key FROM items ORDER BY key",
            vec![],
            crate::cursor::CursorLimits::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        rows_of(&mut cursor).await,
        vec![vec![text("a")]],
        "a cursor reads the applied rows while a later frame awaits its predecessor"
    );
    assert_eq!(bob.receive(second).await.unwrap(), ReceiveStatus::Applied);
    assert_eq!(
        select(&reader, "SELECT key FROM items ORDER BY key").await,
        vec![vec![text("a")], vec![text("b")], vec![text("c")]]
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_import_of_another_scope_does_not_make_an_unwritable_table_retry_allocation() {
    let t = identity_fixture();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let replica = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            vec![
                grant(
                    t.items,
                    &[Right::Read, Right::Custody],
                    &t.every_field("items"),
                ),
                t.writer(t.salaries, "salaries"),
            ],
            &both,
        )
        .await;
    replica.set_importing([t.salaries].into()).await;
    let session = sql(&replica, &both).await;
    session.run("BEGIN", vec![]).await.unwrap();
    assert!(
        matches!(
            session.next_id("items").await,
            Err(Error::Sql(SqlError::Denied))
        ),
        "only an import of the table's own scope may make its allocation retry"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn statements_outside_the_session_and_unpermitted_fields_are_refused() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let session = sql(&alice, &both).await;
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    for refused in [
        "SELECT * FROM secure_row_records",
        "SELECT * FROM sqlite_schema",
        "PRAGMA table_info(items)",
        "CREATE TABLE extra(a)",
        "DROP TABLE items",
        "ATTACH DATABASE ':memory:' AS other",
    ] {
        assert!(session.run(refused, vec![]).await.is_err(), "{refused}");
    }

    let narrow = sql(&alice, &[t.items]).await;
    assert_eq!(
        sql_error(
            narrow
                .run(
                    "INSERT INTO salaries(key,amount,id) VALUES('b',1,4294967297)",
                    vec![]
                )
                .await
        ),
        SqlError::Denied,
        "a narrower session cannot name the other scope's table"
    );

    let bob = t
        .open(
            &dir.path().join("b.db"),
            &PrivateKey::new(),
            vec![grant(
                t.items,
                &[Right::Read, Right::Custody],
                &t.every_field("items"),
            )],
            &[t.items],
        )
        .await;
    let page = transfer(&alice, &bob, t.items).await;
    for status in bob.receive_all(page.frames).await {
        status.unwrap();
    }
    let reader = sql(&bob, &[t.items]).await;
    assert_eq!(
        select(&reader, "SELECT title FROM items").await,
        vec![vec![text("apples")]]
    );
    assert_eq!(
        sql_error(reader.run("UPDATE items SET title='x'", vec![]).await),
        SqlError::Denied,
        "a read-only principal reads but cannot write"
    );
    reader.run("BEGIN", vec![]).await.unwrap();
    assert!(reader.run("DELETE FROM items", vec![]).await.is_err());
    assert!(
        matches!(
            sql_error(reader.run("SELECT 1", vec![]).await),
            SqlError::Aborted(AbortReason::Poisoned(_))
        ),
        "a transaction that tried a refused write reports it until rolled back"
    );
    reader.run("ROLLBACK", vec![]).await.unwrap();
    assert_eq!(
        select(&reader, "SELECT title FROM items").await,
        vec![vec![text("apples")]]
    );

    let title_only_editor = t
        .open(
            &dir.path().join("c.db"),
            &PrivateKey::new(),
            vec![grant(
                t.items,
                &[Right::Read, Right::Update, Right::Custody],
                &t.fields("items", &["title"]),
            )],
            &[t.items],
        )
        .await;
    let page = transfer(&alice, &title_only_editor, t.items).await;
    for status in title_only_editor.receive_all(page.frames).await {
        status.unwrap();
    }
    let editor = sql(&title_only_editor, &[t.items]).await;
    let titled = editor
        .run("UPDATE items SET title='pears' WHERE key='a'", vec![])
        .await
        .unwrap();
    assert_eq!(titled.committed.unwrap().frames.len(), 1);
    assert!(
        editor
            .run("UPDATE items SET done=1 WHERE key='a'", vec![])
            .await
            .is_err(),
        "the commit admits every changed field against the grant"
    );
    assert_eq!(
        select(&editor, "SELECT title,done FROM items").await,
        vec![vec![text("pears"), SqlValue::Integer(0)]]
    );
    assert!(
        matches!(
            insert(&editor, "INSERT INTO items VALUES('z','new',0)", vec![]).await,
            Err(Error::Rejected(_))
        ),
        "the table is writable, but the commit refuses an insert"
    );
    assert_eq!(
        select(&editor, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(1)]],
        "a refused commit leaves no row behind"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn authority_changes_wait_for_the_lease_and_end_the_session() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let node = PrivateKey::new();
    let alice = t
        .open(&dir.path().join("a.db"), &node, t.full(), &both)
        .await;
    let root = alice
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .credentials
        .0[0]
        .id()
        .unwrap();
    let session = sql(&alice, &both).await;
    session.run("BEGIN", vec![]).await.unwrap();
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    let learning = {
        let alice = alice.clone();
        let policy = crate::tests::revoking(&t.publisher, t.app().manifest().app, &[], &[root]);
        rocky_platform::task::spawn(async move { alice.learn_revocations(&policy).await })
    };
    rocky_platform::task::yield_now().await;
    assert!(
        !learning.is_finished(),
        "a revocation waits for the open transaction"
    );
    let committed = session.commit([1; 32]).await.unwrap();
    assert_eq!(
        committed.committed.unwrap().frames.len(),
        1,
        "the open transaction commits under the authority it began with"
    );
    learning.await.unwrap().unwrap();
    assert!(
        session.run("SELECT * FROM items", vec![]).await.is_err(),
        "after the revocation every statement of the old session is refused"
    );
    assert!(alice.session(both.into()).await.is_err());

    alice
        .renew_credentials(t.credentials_until(
            &node,
            t.full(),
            crate::tests::lifetime().1 - rocky_credentials::DAY,
        ))
        .await
        .unwrap();
    let renewed = sql(&alice, &both).await;
    assert_eq!(
        select(&renewed, "SELECT title FROM items").await,
        vec![vec![text("apples")]],
        "credentials no statement ends open a new session"
    );
    alice.logout().await;
    assert!(
        renewed.run("SELECT * FROM items", vec![]).await.is_err(),
        "logout ends the session"
    );
    let after = alice.session(both.into()).await.unwrap();
    let after = after.sql(SqlOptions::default()).await.unwrap();
    assert_eq!(
        sql_error(after.run("SELECT * FROM items", vec![]).await),
        SqlError::Unavailable {
            table: "items".into(),
            reason: crate::sql::Unavailable::KeyMissing
        },
        "after logout a new session finds the rows unavailable, not empty"
    );
}

const PAST_CREDENTIAL_EXPIRY_SECONDS: u64 = 7200;

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn credentials_expiring_inside_an_open_transaction_refuse_it() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let node = PrivateKey::new();
    let alice = t
        .open(&dir.path().join("a.db"), &node, t.full(), &both)
        .await;
    let expires = now().unwrap() + 3600;
    alice
        .renew_credentials(t.credentials_until(&node, t.full(), expires))
        .await
        .unwrap();
    assert_eq!(
        alice
            .session(both.into())
            .await
            .unwrap()
            .status()
            .await
            .unwrap()
            .expires,
        expires
    );

    let session = sql(&alice, &both).await;
    session.run("BEGIN", vec![]).await.unwrap();
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    CLOCK_SKEW.with(|skew| skew.set(PAST_CREDENTIAL_EXPIRY_SECONDS));
    assert!(
        insert(&session, "INSERT INTO items VALUES('b','bread',0)", vec![])
            .await
            .is_err(),
        "the next statement after the chain expires inside the transaction is refused"
    );
    assert!(session.commit([1; 32]).await.is_err());
    assert_eq!(
        session.state().await,
        SqlState::Idle,
        "the refused commit rolls the transaction back and frees the replica"
    );
    assert!(session.run("SELECT * FROM items", vec![]).await.is_err());

    CLOCK_SKEW.with(|skew| skew.set(0));
    let staged = alice.session(both.into()).await.unwrap();
    let table = t.schema.table("items").unwrap();
    let typed = t.app();
    let row = [text("c"), text("cheese"), SqlValue::Integer(0)];
    let (id, fields) = {
        let table = typed
            .tables()
            .find(|typed| typed.sql_name == table.sql_name)
            .unwrap();
        (
            typed.row_id(t.domain.domain, table, &row).unwrap(),
            typed.fields(table, &row).unwrap(),
        )
    };
    let mut transaction = staged.transaction();
    transaction.insert(t.items, id, fields).await.unwrap();
    CLOCK_SKEW.with(|skew| skew.set(PAST_CREDENTIAL_EXPIRY_SECONDS));
    assert!(
        transaction.commit([2; 32]).await.is_err(),
        "a staged secure transaction is admitted again at commit"
    );
    assert!(alice.session(both.into()).await.is_err());

    CLOCK_SKEW.with(|skew| skew.set(0));
    assert!(alice.snapshot(t.items).await.unwrap().rows.is_empty());
    assert!(!alice.handle_committed([1; 32]).await.unwrap());
    assert!(
        !alice.handle_committed([2; 32]).await.unwrap(),
        "nothing committed under expired credentials"
    );
    alice
        .renew_credentials(t.credentials(&node, t.full()))
        .await
        .unwrap();
    assert_eq!(
        select(&sql(&alice, &both).await, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(0)]],
        "renewed credentials open a session again"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn cancel_and_timeout_roll_back_and_free_the_replica() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let clock = crate::ManualLeaseClock::default();
    let alice = t
        .open_on_manual_lease_clock(&dir.path().join("a.db"), &clock)
        .await;
    let session = sql(&alice, &both).await;
    session.run("BEGIN", vec![]).await.unwrap();
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    session.cancel().await;
    assert_eq!(
        sql_error(session.run("SELECT 1", vec![]).await),
        SqlError::Aborted(AbortReason::Cancelled)
    );
    session.run("ROLLBACK", vec![]).await.unwrap();
    assert!(select(&session, "SELECT * FROM items").await.is_empty());

    let short_timeout = std::time::Duration::from_millis(50);
    let short = alice
        .session(both.into())
        .await
        .unwrap()
        .sql(SqlOptions {
            transaction_timeout: short_timeout,
            ..SqlOptions::default()
        })
        .await
        .unwrap();
    short.run("BEGIN", vec![]).await.unwrap();
    insert(&short, "INSERT INTO items VALUES('b','bread',0)", vec![])
        .await
        .unwrap();
    let written = insert(&session, "INSERT INTO items VALUES('c','cheese',0)", vec![]);
    tokio::pin!(written);
    tokio::select! {
        biased;
        () = alice.inner.interactive_queued() => {}
        written = &mut written => panic!("another writer ran inside a held lease: {:?}", written.err()),
    }
    clock.advance(short_timeout);
    let written = written.await.unwrap();
    assert_eq!(
        written.committed.unwrap().frames.len(),
        1,
        "another writer waits for the lease until the timeout releases it"
    );
    assert_eq!(
        sql_error(short.run("COMMIT", vec![]).await),
        SqlError::Aborted(AbortReason::Timeout)
    );
    assert_eq!(
        select(&session, "SELECT key FROM items").await,
        vec![vec![text("c")]],
        "the expired transaction is rolled back"
    );

    let dropped = sql(&alice, &both).await;
    dropped.run("BEGIN", vec![]).await.unwrap();
    insert(&dropped, "INSERT INTO items VALUES('d','dates',0)", vec![])
        .await
        .unwrap();
    drop(dropped);
    assert_eq!(
        select(&session, "SELECT key FROM items").await,
        vec![vec![text("c")]],
        "a dropped session rolls its transaction back"
    );
}

#[cfg(not(target_family = "wasm"))]
#[tokio::test(start_paused = true)]
async fn a_commit_past_the_lease_deadline_aborts_before_the_lease_watcher_runs() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let timeout = std::time::Duration::from_secs(1);
    let held = alice
        .session(both.into())
        .await
        .unwrap()
        .sql(SqlOptions {
            transaction_timeout: timeout,
            ..SqlOptions::default()
        })
        .await
        .unwrap();
    held.run("BEGIN", vec![]).await.unwrap();
    insert(&held, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    assert_eq!(
        held.state().await,
        SqlState::Active,
        "the paused clock has not reached the lease deadline yet"
    );
    let without_running_other_tasks = &mut std::task::Context::from_waker(std::task::Waker::noop());
    let advanced = std::pin::pin!(tokio::time::advance(timeout)).poll(without_running_other_tasks);
    assert!(
        advanced.is_pending(),
        "the clock moves past the deadline but the lease watcher gets no turn"
    );
    let mut commit = std::pin::pin!(held.run("COMMIT", vec![]));
    let committed = match commit.as_mut().poll(without_running_other_tasks) {
        std::task::Poll::Ready(committed) => committed,
        std::task::Poll::Pending => commit.await,
    };
    assert_eq!(
        sql_error(committed),
        SqlError::Aborted(AbortReason::Timeout),
        "a COMMIT after the lease deadline is aborted by the call itself, not by the watcher"
    );
    assert!(
        select(&sql(&alice, &both).await, "SELECT key FROM items")
            .await
            .is_empty(),
        "the expired transaction is rolled back"
    );
}

#[cfg(not(target_family = "wasm"))]
#[tokio::test(start_paused = true)]
async fn a_lease_on_a_manual_clock_expires_only_when_that_clock_passes_its_timeout() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let clock = crate::ManualLeaseClock::default();
    let alice = t
        .open_on_manual_lease_clock(&dir.path().join("a.db"), &clock)
        .await;
    let timeout = std::time::Duration::from_millis(50);
    let held = alice
        .session(both.into())
        .await
        .unwrap()
        .sql(SqlOptions {
            transaction_timeout: timeout,
            ..SqlOptions::default()
        })
        .await
        .unwrap();
    held.run("BEGIN", vec![]).await.unwrap();
    insert(&held, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    tokio::time::advance(crate::sql::MAX_TRANSACTION_TIMEOUT).await;
    insert(&held, "INSERT INTO items VALUES('b','bread',0)", vec![])
        .await
        .expect(
            "a transaction slowed by a loaded host keeps its lease while the lease clock stands",
        );
    clock.advance(timeout - std::time::Duration::from_millis(1));
    insert(&held, "INSERT INTO items VALUES('c','cheese',0)", vec![])
        .await
        .expect("the lease holds until its clock reaches the timeout");
    clock.advance(std::time::Duration::from_millis(1));
    assert_eq!(
        sql_error(held.run("COMMIT", vec![]).await),
        SqlError::Aborted(AbortReason::Timeout),
        "the next call after the lease clock passes the timeout is aborted"
    );
    assert!(
        select(&sql(&alice, &both).await, "SELECT key FROM items")
            .await
            .is_empty(),
        "the expired transaction is rolled back"
    );
}

#[cfg(not(target_family = "wasm"))]
#[tokio::test]
async fn a_lease_watcher_ends_with_its_transaction_instead_of_waiting_for_the_deadline() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let clock = crate::ManualLeaseClock::default();
    let alice = t
        .open_on_manual_lease_clock(&dir.path().join("a.db"), &clock)
        .await;
    let held = sql(&alice, &both).await;
    let idle = clock.followers();
    held.run("BEGIN", vec![]).await.unwrap();
    insert(&held, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    assert_eq!(
        clock.followers(),
        idle + 2,
        "the held transaction and its lease watcher follow the clock"
    );
    held.run("COMMIT", vec![]).await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(
        clock.followers(),
        idle,
        "the lease watcher ends once its transaction commits"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn savepoint_rollback_and_cancellation_do_not_cache_uncommitted_frontiers() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let scopes = [t.items, t.salaries];
    let replica = t
        .open(
            &dir.path().join("frontiers.db"),
            &PrivateKey::new(),
            t.full(),
            &scopes,
        )
        .await;
    let session = sql(&replica, &scopes).await;
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    let committed = replica.frontier(t.items).await.unwrap();
    assert_eq!(committed.logs[0].applied, 1);
    session.run("BEGIN", vec![]).await.unwrap();
    session.run("SAVEPOINT child", vec![]).await.unwrap();
    insert(
        &session,
        "INSERT INTO items VALUES('b','bananas',0)",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(
        session.transactional_frontier(t.items).await.unwrap(),
        (committed.clone(), 0)
    );
    session.run("ROLLBACK TO child", vec![]).await.unwrap();
    assert_eq!(
        session.transactional_frontier(t.items).await.unwrap(),
        (committed.clone(), 0)
    );
    session.run("RELEASE child", vec![]).await.unwrap();
    session.cancel().await;
    session.run("ROLLBACK", vec![]).await.unwrap();
    assert_eq!(
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .frontier_cache_stats()
            .1,
        0
    );
    assert_eq!(replica.frontier(t.items).await.unwrap(), committed);
    assert_eq!(
        select(&session, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(1)]]
    );
    session.run("BEGIN", vec![]).await.unwrap();
    assert_eq!(session.transactional_frontier(t.items).await.unwrap().1, 0);
    drop(session);
    assert_eq!(replica.frontier(t.items).await.unwrap(), committed);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_handle_recovers_its_commit_after_restart() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let node = PrivateKey::new();
    let path = dir.path().join("a.db");
    let alice = t.open(&path, &node, t.full(), &both).await;
    let session = sql(&alice, &both).await;
    session.run("BEGIN", vec![]).await.unwrap();
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    let first = session.commit([7; 32]).await.unwrap().committed.unwrap();
    session.run("BEGIN", vec![]).await.unwrap();
    session
        .run("UPDATE items SET title='apples'", vec![])
        .await
        .unwrap();
    let recorded = session.commit([6; 32]).await.unwrap().committed.unwrap();
    drop(session);
    drop(alice);

    let alice = t.open(&path, &node, t.full(), &both).await;
    let session = sql(&alice, &both).await;
    assert_eq!(
        select(&session, "SELECT title FROM items").await,
        vec![vec![text("apples")]],
        "the committed row survives a restart"
    );
    session.run("BEGIN", vec![]).await.unwrap();
    session
        .run("UPDATE items SET title='apples'", vec![])
        .await
        .unwrap();
    let recovered = session.commit([6; 32]).await.unwrap().committed.unwrap();
    assert_eq!(
        recovered
            .frames
            .iter()
            .map(|frame| frame.id().unwrap())
            .collect::<Vec<_>>(),
        recorded
            .frames
            .iter()
            .map(|frame| frame.id().unwrap())
            .collect::<Vec<_>>(),
        "the same handle and rows after a restart return the recorded frames and author none"
    );
    session.run("BEGIN", vec![]).await.unwrap();
    session.run("DELETE FROM items", vec![]).await.unwrap();
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    let republished = session.commit([8; 32]).await.unwrap().committed.unwrap();
    assert_eq!(
        republished.frames.len(),
        1,
        "deleting and reinserting a key republishes its row in a new transaction"
    );
    session.run("BEGIN", vec![]).await.unwrap();
    session
        .run("UPDATE items SET title='apples'", vec![])
        .await
        .unwrap();
    let assigned = session.commit([9; 32]).await.unwrap().committed.unwrap();
    assert_eq!(
        assigned.frames.len(),
        1,
        "an intent publishes its statement, even an equal assignment"
    );
    assert_ne!(
        assigned.frames[0].id().unwrap(),
        first.frames[0].id().unwrap()
    );
    session.run("BEGIN", vec![]).await.unwrap();
    session
        .run("UPDATE items SET done=1", vec![])
        .await
        .unwrap();
    assert!(
        matches!(session.commit([9; 32]).await, Err(Error::Conflict)),
        "reusing a handle with other rows conflicts"
    );
    assert_eq!(
        select(&session, "SELECT done FROM items").await,
        vec![vec![SqlValue::Integer(0)]],
        "a conflicting handle changes nothing"
    );
    session.run("BEGIN", vec![]).await.unwrap();
    session
        .run("UPDATE items SET title='apples'", vec![])
        .await
        .unwrap();
    let replayed = session.commit([9; 32]).await.unwrap().committed.unwrap();
    assert_eq!(
        replayed.frames[0].id().unwrap(),
        assigned.frames[0].id().unwrap(),
        "the same rows under the same handle replay the recorded frame"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn concurrent_sql_edits_converge_in_every_typed_table() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let carol = t
        .open(
            &dir.path().join("c.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let a = sql(&alice, &both).await;
    let c = sql(&carol, &both).await;
    let seed = insert(&a, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    carol.receive_all(seed).await;
    let from_a = a
        .run("UPDATE items SET title='green apples'", vec![])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    let from_c = c
        .run("UPDATE items SET title='red apples',done=1", vec![])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    alice.receive_all(from_c).await;
    carol.receive_all(from_a).await;
    let rows = select(&a, "SELECT * FROM items").await;
    assert_eq!(rows, select(&c, "SELECT * FROM items").await);
    assert_eq!(rows[0][2], SqlValue::Integer(1));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_edit_concurrent_with_a_delete_applies_and_its_log_continues() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let carol = t
        .open(
            &dir.path().join("c.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let a = sql(&alice, &both).await;
    let c = sql(&carol, &both).await;
    let seed = insert(&a, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    carol.receive_all(seed).await;
    let deleted = a
        .run("DELETE FROM items WHERE key='a'", vec![])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    let renamed = c
        .run("UPDATE items SET title='late' WHERE key='a'", vec![])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    let later = insert(&c, "INSERT INTO items VALUES('b','bread',0)", vec![])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    for frame in renamed.into_iter().chain(later) {
        assert_eq!(
            alice.receive(frame).await.unwrap(),
            ReceiveStatus::Applied,
            "the edit merges into the tombstone and the editor's next frame does not wait for it"
        );
    }
    for frame in deleted {
        assert_eq!(carol.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    }
    let rows = select(&a, "SELECT key,title FROM items").await;
    assert_eq!(
        rows,
        vec![vec![text("b"), text("bread")]],
        "the delete wins over the concurrent edit"
    );
    assert_eq!(rows, select(&c, "SELECT key,title FROM items").await);
}

async fn rows_of(cursor: &mut crate::cursor::SnapshotCursor) -> Vec<Vec<SqlValue>> {
    let mut rows = Vec::new();
    loop {
        let page = cursor.next().await.unwrap();
        rows.extend(page.rows);
        if page.done {
            return rows;
        }
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn catalogs_cursors_and_joins_report_unavailable_rows_instead_of_absence() {
    use crate::cursor::CursorLimits;
    use crate::sql::Unavailable;
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let writer = sql(&alice, &both).await;
    insert(
        &writer,
        "INSERT INTO items VALUES('a','apples',0),('b','bread',0)",
        vec![],
    )
    .await
    .unwrap();
    insert(&writer, "INSERT INTO salaries VALUES('a',1000)", vec![])
        .await
        .unwrap();
    let join = "SELECT i.key, s.amount FROM items i LEFT JOIN salaries s USING(key) ORDER BY i.key";
    let mut cursor = writer
        .cursor(join, vec![], CursorLimits::default())
        .await
        .unwrap();
    assert_eq!(
        rows_of(&mut cursor).await,
        vec![
            vec![text("a"), SqlValue::Integer(1000)],
            vec![text("b"), SqlValue::Null]
        ],
        "a permitted absent companion is an ordinary NULL"
    );
    let catalog = writer.catalog();
    assert_eq!(
        catalog
            .iter()
            .map(|table| (table.name.as_str(), table.writable))
            .collect::<Vec<_>>(),
        [("drafts", true), ("items", true), ("salaries", true)],
        "the catalog names the session's tables and what it may write"
    );

    let granted_both_holding_only_the_public_key = t
        .open(
            &dir.path().join("b.db"),
            &PrivateKey::new(),
            t.full(),
            &[t.items],
        )
        .await;
    for frame in transfer(&alice, &granted_both_holding_only_the_public_key, t.items)
        .await
        .frames
    {
        granted_both_holding_only_the_public_key
            .receive(frame)
            .await
            .unwrap();
    }
    let reader = sql(&granted_both_holding_only_the_public_key, &both).await;
    let unavailable = SqlError::Unavailable {
        table: "salaries".into(),
        reason: Unavailable::KeyMissing,
    };
    assert_eq!(
        sql_error(reader.run(join, vec![]).await),
        unavailable,
        "without the private key the join is unavailable, never NULL salaries"
    );
    assert!(matches!(
        reader.cursor(join, vec![], CursorLimits::default()).await,
        Err(Error::Sql(error)) if error == unavailable
    ));
    assert_eq!(
        select(&reader, "SELECT key FROM items ORDER BY key").await,
        vec![vec![text("a")], vec![text("b")]]
    );
    assert_eq!(
        reader
            .catalog()
            .iter()
            .map(|table| table.name.as_str())
            .collect::<Vec<_>>(),
        ["drafts", "items"]
    );
    for denied in [
        "SELECT * FROM secure_row_records",
        "SELECT name FROM sqlite_schema",
    ] {
        assert!(
            reader
                .cursor(denied, vec![], CursorLimits::default())
                .await
                .is_err(),
            "denied objects stay denied for cursors too: {denied}"
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn secure_cursors_keep_their_snapshot_and_end_when_authority_changes() {
    use crate::cursor::CursorLimits;
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let session = sql(&alice, &both).await;
    for id in 0..5 {
        insert(
            &session,
            "INSERT INTO items VALUES(?,'x',0)",
            vec![text(&id.to_string())],
        )
        .await
        .unwrap();
    }
    let limits = CursorLimits {
        page_rows: 2,
        ..CursorLimits::default()
    };
    let mut cursor = session
        .cursor("SELECT key FROM items ORDER BY key", vec![], limits)
        .await
        .unwrap();
    let mut rows = cursor.next().await.unwrap().rows;
    session
        .run("DELETE FROM items WHERE key='4'", vec![])
        .await
        .expect("an open cursor holds no lease, so a write commits");
    rows.extend(rows_of(&mut cursor).await);
    assert_eq!(rows.len(), 5, "the cursor keeps its snapshot");
    cursor.reset().await.unwrap();
    cursor.next().await.unwrap();
    alice.forget_key(t.salaries).await;
    assert!(
        matches!(
            cursor.next().await,
            Err(Error::Sql(SqlError::AuthorityChanged))
        ),
        "losing the key revokes the session and discards undelivered pages"
    );
    assert!(cursor.next().await.is_err());
    assert!(
        session
            .cursor("SELECT key FROM items", vec![], limits)
            .await
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_cursor_pinning_the_log_past_its_limit_is_preempted() {
    use crate::cursor::CursorLimits;
    let t = Typed::new();
    let dir = Scratch::new().await;
    let path = dir.path().join("a.db");
    let alice = t
        .open(&path, &PrivateKey::new(), t.full(), &[t.items])
        .await;
    let session = sql(&alice, &[t.items]).await;
    let add = async |id: usize| {
        insert(
            &session,
            "INSERT INTO items VALUES(?,'x',0)",
            vec![text(&id.to_string())],
        )
        .await
        .unwrap();
    };
    add(0).await;
    let limits = CursorLimits {
        page_rows: 1,
        ..CursorLimits::default()
    };
    let mut cursor = session
        .cursor("SELECT key FROM items", vec![], limits)
        .await
        .unwrap();
    cursor.next().await.unwrap();
    add(1).await;
    alice.set_wal_limits(0, 0).await;
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    assert!(rocky_platform::fs::len(std::path::Path::new(&wal)).unwrap_or(0) > 0);
    alice
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .checkpoint_if_due()
        .await;
    assert_eq!(
        rocky_platform::fs::len(std::path::Path::new(&wal)).unwrap_or(0),
        0,
        "the explicit checkpoint sees limits lowered after the last commit"
    );
    assert!(matches!(
        cursor.next().await,
        Err(Error::Sql(SqlError::Aborted(AbortReason::Preempted)))
    ));
    let mut cursor = session
        .cursor("SELECT key FROM items", vec![], limits)
        .await
        .unwrap();
    assert_eq!(rows_of(&mut cursor).await.len(), 2);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_reader_reopening_after_preemption_waits_until_the_log_truncates() {
    use crate::cursor::CursorLimits;
    let t = Typed::new();
    let dir = Scratch::new().await;
    let path = dir.path().join("a.db");
    let alice = t
        .open(&path, &PrivateKey::new(), t.full(), &[t.items])
        .await;
    let session = sql(&alice, &[t.items]).await;
    let add = async |id: usize| {
        insert(
            &session,
            "INSERT INTO items VALUES(?,'x',0)",
            vec![text(&id.to_string())],
        )
        .await
        .unwrap();
    };
    let limits = CursorLimits {
        page_rows: 1,
        ..CursorLimits::default()
    };
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let wal = std::path::PathBuf::from(wal);
    add(0).await;
    let mut pinning = session
        .cursor("SELECT key FROM items", vec![], limits)
        .await
        .unwrap();
    pinning.next().await.unwrap();
    add(1).await;
    assert!(rocky_platform::fs::len(&wal).unwrap_or(0) > 0);
    let quiesces = alice.quiesces().await;
    let (reached, paused) = tokio::sync::oneshot::channel();
    let (resume, resumed) = tokio::sync::oneshot::channel();
    let mut held = alice
        .pause_quiet_truncation_for_test(reached, resumed)
        .await;
    alice.set_wal_limits(0, 0).await;
    let checkpoint = async {
        alice
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .checkpoint_if_due()
            .await;
    };
    let reader = async {
        paused.await.unwrap();
        assert!(matches!(
            pinning.next().await,
            Err(Error::Sql(SqlError::Aborted(AbortReason::Preempted)))
        ));
        let reopening = session.cursor("SELECT key FROM items", vec![], limits);
        let mut reopening = std::pin::pin!(reopening);
        let reopened = tokio::select! {
            biased;
            reopened = &mut reopening => Some(reopened),
            _ = held.wait_for(|held| *held >= 1) => None,
        };
        assert!(
            reopened.is_none(),
            "an immediate reopen waits at the gate instead of pinning the log again"
        );
        resume.send(()).unwrap();
        reopening.await.unwrap()
    };
    let ((), mut reopened) = tokio::join!(checkpoint, reader);
    assert_eq!(
        rocky_platform::fs::len(&wal).unwrap_or(0),
        0,
        "the truncation finishes before the reopened reader takes a snapshot"
    );
    assert_eq!(alice.quiesces().await, quiesces + 1, "a single preemption");
    assert_eq!(rows_of(&mut reopened).await.len(), 2);
    drop(reopened);
    add(2).await;
    alice
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .checkpoint_if_due()
        .await;
    assert_eq!(rocky_platform::fs::len(&wal).unwrap_or(0), 0);
    assert_eq!(
        alice.quiesces().await,
        quiesces + 1,
        "a commit with no pinning reader truncates without preempting"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_log_pinned_outside_the_cursor_host_is_not_retried_on_every_commit() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let path = dir.path().join("a.db");
    let alice = t
        .open(&path, &PrivateKey::new(), t.full(), &[t.items])
        .await;
    let session = sql(&alice, &[t.items]).await;
    let add = async |id: usize| {
        insert(
            &session,
            "INSERT INTO items VALUES(?,'x',0)",
            vec![text(&id.to_string())],
        )
        .await
        .unwrap();
    };
    let checkpoint = async || {
        alice
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .checkpoint_if_due()
            .await;
    };
    add(0).await;
    let outside = rocky_storage::files::raw(&path).unwrap();
    outside.execute_batch("BEGIN").unwrap();
    outside
        .query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))
        .unwrap();
    add(1).await;
    alice.set_wal_limits(0, 0).await;
    let (truncations, quiesces) = (alice.checkpoints().await, alice.quiesces().await);
    checkpoint().await;
    assert_eq!(alice.checkpoints().await, truncations + 1);
    assert_eq!(alice.quiesces().await, quiesces + 1);
    for id in 2..5 {
        add(id).await;
        checkpoint().await;
    }
    assert_eq!(
        (alice.checkpoints().await, alice.quiesces().await),
        (truncations + 1, quiesces + 1),
        "a failed quiet truncation waits for the log to grow before it preempts again"
    );
    outside.execute_batch("COMMIT").unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn secure_table_events_follow_commits_receipts_and_authority() {
    use crate::observe::Cause;
    let t = Typed::new();
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let alice = t
        .open(
            &dir.path().join("a.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let bob = t
        .open(
            &dir.path().join("b.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let mut items = alice.observe_tables(Some(&["items"])).unwrap();
    let mut salaries = alice.observe_tables(Some(&["salaries"])).unwrap();
    let mut received = bob.observe_tables(Some(&["items"])).unwrap();
    assert!(alice.observe_tables(Some(&["secure_row_records"])).is_err());
    let session = sql(&alice, &both).await;
    session.run("BEGIN", vec![]).await.unwrap();
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    session.run("ROLLBACK", vec![]).await.unwrap();
    assert_eq!(items.try_next().unwrap(), None);
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    let change = items.next().await.unwrap();
    assert_eq!(change.causes, [Cause::Local].into());
    let revision = *alice.observe_revision().borrow();
    assert_eq!(change.revisions.values().copied().max(), Some(revision));
    assert_eq!(salaries.try_next().unwrap(), None);
    alice.audit(t.salaries).await.unwrap();
    session
        .run("UPDATE items SET title='pears' WHERE key='a'", vec![])
        .await
        .unwrap();
    assert_eq!(items.next().await.unwrap().causes, [Cause::Local].into());
    assert_eq!(
        salaries.try_next().unwrap(),
        None,
        "audit's rolled-back typed rebuild does not leak into the next commit's events"
    );
    for frame in transfer(&alice, &bob, t.items).await.frames {
        bob.receive(frame).await.unwrap();
    }
    let change = received.next().await.unwrap();
    assert_eq!(
        change.causes,
        [Cause::Sync].into(),
        "a receipt wakes the receiver's observers"
    );
    alice.forget_key(t.salaries).await;
    assert_eq!(
        salaries.next().await.unwrap().causes,
        [Cause::Authority].into(),
        "losing a table's key wakes its observers"
    );
    assert_eq!(
        items.try_next().unwrap(),
        None,
        "an authority change that leaves a table's rows readable wakes no observer"
    );
    drop(session);
    drop(alice);
    assert!(items.next().await.is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn definition_compound_keys_indexes_real_and_blob_survive_bundles_and_reopen() {
    let publisher = PrivateKey::new();
    let definition = r#"{
        "format":10, "publisher": "PUBLISHER",
        "application": "samples", "tables": [
            {"name":"items", "class":"replayed", "columns":[
                {"name":"sensor", "type":"text", "not_null":true},
                {"name":"sequence", "type":"integer", "not_null":true},
                {"name":"reading", "type":"real"}, {"name":"raw", "type":"blob"}],
             "primary_key":["sensor","sequence"],
             "indexes":[{"name":"items_by_reading", "columns":[{"name":"reading", "descending":true}]}]},
            {"name":"salaries", "class":"replayed", "columns":[
                {"name":"key", "type":"text", "not_null":true}, {"name":"amount", "type":"integer", "not_null":true}],
             "primary_key":["key"]}
        ]
    }"#.replace("PUBLISHER", &publisher.public_key().as_bytes().iter().map(|byte| format!("{byte:02x}")).collect::<String>());
    let schema = rocky_core::SchemaDefinition::parse(definition.as_bytes())
        .unwrap()
        .build()
        .unwrap();
    let t = Typed::with_schema(publisher, schema);
    let dir = Scratch::new().await;
    let both = [t.items, t.salaries];
    let author = PrivateKey::new();
    let alice = t
        .open(&dir.path().join("a.db"), &author, t.full(), &both)
        .await;
    let bob = t
        .open(
            &dir.path().join("b.db"),
            &PrivateKey::new(),
            t.full(),
            &both,
        )
        .await;
    let session = sql(&alice, &both).await;
    session.run("BEGIN", vec![]).await.unwrap();
    let row = vec![
        text("s"),
        SqlValue::Integer(7),
        SqlValue::real(1.25).unwrap(),
        SqlValue::Blob(vec![0, 255, 42]),
    ];
    insert(&session, "INSERT INTO items VALUES(?,?,?,?)", row.clone())
        .await
        .unwrap();
    insert(&session, "INSERT INTO salaries VALUES('s',10)", vec![])
        .await
        .unwrap();
    let frames = session
        .commit([87; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    assert_eq!(frames.len(), 2);
    for frame in frames {
        bob.receive(frame).await.unwrap();
    }
    let reader = sql(&bob, &both).await;
    assert_eq!(
        select(&reader, "SELECT sensor,sequence,reading,raw FROM items").await,
        vec![row.clone()]
    );
    assert!(
        insert(
            &session,
            "INSERT INTO items VALUES('s',7,2.0,X'00')",
            vec![]
        )
        .await
        .is_err()
    );
    drop(session);
    drop(alice);
    let reopened = t
        .open(&dir.path().join("a.db"), &author, t.full(), &both)
        .await;
    assert_eq!(
        select(
            &sql(&reopened, &both).await,
            "SELECT sensor,sequence,reading,raw FROM items"
        )
        .await,
        vec![row]
    );
    drop(reader);
    drop(bob);
    drop(reopened);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn identity_descriptor_installs_shared_columns_and_reopens() {
    let fixture = identity_fixture();
    let directory = rocky_platform::fs::Scratch::new().await;
    let path = directory.path().join("identity.sqlite");
    let node = PrivateKey::new();
    for _ in 0..2 {
        let replica = fixture
            .open(
                &path,
                &node,
                fixture.full(),
                &[fixture.items, fixture.salaries],
            )
            .await;
        let sql = replica
            .session([fixture.items, fixture.salaries].into())
            .await
            .unwrap()
            .sql(SqlOptions::default())
            .await
            .unwrap();
        for name in ["items", "salaries", "drafts"] {
            let result = sql
                .run(&format!("SELECT id FROM {name}"), vec![])
                .await
                .unwrap();
            assert_eq!(result.columns, ["id"]);
            assert!(result.rows.is_empty());
        }
        drop(sql);
        drop(replica);
    }
}

fn identity_fixture() -> Typed {
    let publisher = PrivateKey::new();
    let definition = r#"{
        "format":10, "publisher": "PUBLISHER",
        "application": "identity-descriptor", "tables": [
            {"name": "items", "class": "replayed", "columns": [
                {"name": "key", "type": "text", "not_null": true},
                {"name": "title", "type": "text", "not_null": true}], "primary_key": ["id"]},
            {"name": "salaries", "class": "replayed", "columns": [
                {"name": "amount", "type": "integer", "not_null": true}], "primary_key": ["id"]},
            {"name": "drafts", "class": "local_only", "columns": [
                {"name": "body", "type": "text"}], "primary_key": ["id"]}
        ]
    }"#
    .replace("PUBLISHER", &publisher.public_key().to_string());
    let schema = rocky_core::SchemaDefinition::parse(definition.as_bytes())
        .unwrap()
        .build()
        .unwrap();
    Typed::with_schema(publisher, schema)
}
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn ids_reserve_inside_a_transaction_and_continue_after_delete_checkpoint_and_reopen() {
    let fixture = identity_fixture();
    let directory = rocky_platform::fs::Scratch::new().await;
    let path = directory.path().join("identities.sqlite");
    let node = PrivateKey::new();
    let key = node.clone();
    let replica = fixture
        .open(
            &path,
            &node,
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    let chain_stored_with_the_first_operation = crate::wire::Chain::new(
        replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .config
            .credentials
            .clone(),
    )
    .unwrap()
    .digest();
    assert_eq!(replica.chain_census().await, (BTreeSet::new(), 0));
    let session_sql = sql(&replica, &[fixture.items]).await;
    assert!(
        session_sql
            .next_id("items")
            .await
            .unwrap_err()
            .to_string()
            .contains("active transaction")
    );
    session_sql.run("BEGIN", vec![]).await.unwrap();
    let first = session_sql.next_id("items").await.unwrap();
    let own = crate::writer::writer(&key.public_key());
    assert_eq!((first.writer(), first.sequence()), (own, 1));
    assert_eq!(session_sql.next_id("items").await.unwrap().sequence(), 2);
    session_sql.run("ROLLBACK", vec![]).await.unwrap();
    session_sql.run("BEGIN", vec![]).await.unwrap();
    assert_eq!(session_sql.next_id("items").await.unwrap(), first);
    session_sql
        .run(
            "INSERT INTO items(key,title,id) VALUES(?,?,?)",
            vec![text("a"), text("first"), first.sql()],
        )
        .await
        .unwrap();
    let commit = session_sql
        .commit([123; 32])
        .await
        .unwrap()
        .committed
        .unwrap();
    let operation = commit.frames[0].operation().unwrap();
    assert_eq!(operation.header.public_key, key.public_key());
    assert_eq!(
        operation
            .header
            .extensions
            .author(&operation.header.public_key),
        Some(own)
    );
    assert_eq!(
        operation.header.extensions.row_format,
        crate::wire::ROW_FORMAT
    );
    assert_eq!(
        operation.header.extensions.chain,
        chain_stored_with_the_first_operation
    );
    session_sql
        .run("DELETE FROM items WHERE key='a'", vec![])
        .await
        .unwrap();
    drop(session_sql);
    compacted_base(&replica, &fixture.publisher, fixture.items).await;
    replica.audit(fixture.items).await.unwrap();
    drop(replica);
    let replica = fixture
        .open(
            &path,
            &node,
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    let session_sql = sql(&replica, &[fixture.items]).await;
    session_sql.run("BEGIN", vec![]).await.unwrap();
    assert_eq!(session_sql.next_id("items").await.unwrap().sequence(), 2);
    session_sql.run("ROLLBACK", vec![]).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn identity_reservations_follow_savepoints_and_one_node_signer_form_one_bundle() {
    let fixture = identity_fixture();
    let directory = rocky_platform::fs::Scratch::new().await;
    let node = PrivateKey::new();
    let replica = fixture
        .open(
            &directory.path().join("writer.sqlite"),
            &node,
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    let writer = sql(&replica, &[fixture.items, fixture.salaries]).await;
    writer.run("BEGIN", vec![]).await.unwrap();
    let item = writer.next_id("items").await.unwrap();
    writer.run("SAVEPOINT child", vec![]).await.unwrap();
    let unused = writer.next_id("items").await.unwrap();
    writer.run("ROLLBACK TO child", vec![]).await.unwrap();
    assert_eq!(writer.next_id("items").await.unwrap(), unused);
    let salary = writer.next_id("salaries").await.unwrap();
    writer
        .run(
            "INSERT INTO items(key,title,id) VALUES('z','item',?)",
            vec![item.sql()],
        )
        .await
        .unwrap();
    writer
        .run(
            "INSERT INTO items(key,title,id) VALUES('a','second',?)",
            vec![unused.sql()],
        )
        .await
        .unwrap();
    writer
        .run(
            "INSERT INTO salaries(amount,id) VALUES(5,?)",
            vec![salary.sql()],
        )
        .await
        .unwrap();
    let committed = writer.commit([124; 32]).await.unwrap().committed.unwrap();
    assert_eq!(committed.frames.len(), 2);
    let signers: BTreeSet<_> = committed
        .frames
        .iter()
        .map(|frame| frame.operation().unwrap().header.public_key)
        .collect();
    assert_eq!(signers, [node.public_key()].into());
    let peer = fixture
        .open(
            &directory.path().join("peer.sqlite"),
            &PrivateKey::new(),
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    for frame in committed.frames.iter().rev() {
        peer.receive(frame.clone()).await.unwrap();
    }
    let reader = sql(&peer, &[fixture.items, fixture.salaries]).await;
    assert_eq!(
        reader
            .run("SELECT id FROM items ORDER BY id", vec![])
            .await
            .unwrap()
            .rows,
        vec![vec![item.sql()], vec![unused.sql()]]
    );
    assert_eq!(
        reader
            .run("SELECT id FROM salaries", vec![])
            .await
            .unwrap()
            .rows,
        vec![vec![salary.sql()]]
    );
    for (statement, values) in [(
        "UPDATE items SET id=? WHERE key='z'",
        vec![
            rocky_core::RowIdentity::new(item.writer(), item.sequence() + 10)
                .unwrap()
                .sql(),
        ],
    )] {
        assert!(writer.run(statement, values).await.is_err());
    }
    assert_eq!(
        writer
            .run("SELECT id FROM items WHERE key='z'", vec![])
            .await
            .unwrap()
            .rows,
        vec![vec![item.sql()]]
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn failed_identity_commit_releases_its_sequence_and_forged_frame_author_is_refused() {
    let fixture = identity_fixture();
    let directory = rocky_platform::fs::Scratch::new().await;
    let node = PrivateKey::new();
    let replica = fixture
        .open(
            &directory.path().join("writer.sqlite"),
            &node,
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    let key = node.clone();
    replica.inject_failure(0, 1).await;
    let writer = sql(&replica, &[fixture.items]).await;
    writer.run("BEGIN", vec![]).await.unwrap();
    let id = writer.next_id("items").await.unwrap();
    writer
        .run(
            "INSERT INTO items(key,title,id) VALUES('a','first',?)",
            vec![id.sql()],
        )
        .await
        .unwrap();
    assert!(writer.commit([125; 32]).await.is_err());
    assert_eq!(writer.state().await, SqlState::Idle);
    writer.run("BEGIN", vec![]).await.unwrap();
    assert_eq!(writer.next_id("items").await.unwrap(), id);
    writer
        .run(
            "INSERT INTO items(key,title,id) VALUES('a','first',?)",
            vec![id.sql()],
        )
        .await
        .unwrap();
    let frame = writer
        .commit([125; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames
        .remove(0);
    let operation = frame.operation().unwrap();
    let plaintext = crate::wire::Opened::open(
        &fixture.key(fixture.items).key,
        &operation.header.extensions,
        frame.body_bytes(),
    )
    .unwrap()
    .payload;
    let mut extensions = operation.header.extensions;
    extensions.authored = false;
    let forged = crate::wire::author(
        &key,
        crate::wire::FrameDraft {
            extensions,
            chain: frame.chain().unwrap().clone(),
            encryption_key: &fixture.key(fixture.items).key,
            log: (
                operation.header.seq_num,
                operation.header.backlink,
                operation.header.timestamp,
            ),
            previous: operation.header.previous,
            partition: None,
            plaintext: &plaintext,
        },
    )
    .unwrap();
    let peer = fixture
        .open(
            &directory.path().join("peer.sqlite"),
            &PrivateKey::new(),
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    let error = peer.receive(forged).await.unwrap_err().to_string();
    assert!(
        error.contains("node author missing"),
        "a frame that claims no table author cannot create a row identity: {error}"
    );
    assert_eq!(
        peer.receive(frame.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn local_identity_columns_refuse_reserved_kinds_and_mutation() {
    let fixture = identity_fixture();
    let directory = rocky_platform::fs::Scratch::new().await;
    let replica = fixture
        .open(
            &directory.path().join("local_ids.sqlite"),
            &PrivateKey::new(),
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    let writer = sql(&replica, &[fixture.items]).await;
    let id = rocky_core::RowIdentity::new(rocky_core::WriterId::from_bytes([99; 32]), 1).unwrap();
    writer
        .run(
            "INSERT INTO drafts(id,body) VALUES(?,'local')",
            vec![id.sql()],
        )
        .await
        .unwrap();
    for (statement, values) in [
        (
            "UPDATE drafts SET id=?",
            vec![
                rocky_core::RowIdentity::new(id.writer(), id.sequence() + 1)
                    .unwrap()
                    .sql(),
            ],
        ),
        ("INSERT INTO drafts(id,body) VALUES(0,'virtual')", vec![]),
    ] {
        assert!(writer.run(statement, values).await.is_err());
    }
    assert_eq!(
        writer
            .run("SELECT id FROM drafts", vec![])
            .await
            .unwrap()
            .rows,
        vec![vec![id.sql()]]
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn two_nodes_of_one_principal_write_every_table_under_their_own_keys() {
    let fixture = identity_fixture();
    let directory = rocky_platform::fs::Scratch::new().await;
    let principal = PrivateKey::new();
    let nodes = [PrivateKey::new(), PrivateKey::new()];
    let root = crate::credentials::Authority::owner(&fixture.publisher)
        .issue(
            principal.public_key(),
            fixture.full(),
            crate::tests::validity(),
        )
        .unwrap();
    let prefix = CredentialChain(vec![root]);
    let initial = prefix.clone().certify(&principal, &nodes[0]).unwrap();
    let next = prefix
        .clone()
        .certify_member(
            &principal,
            &nodes[1],
            1,
            Some(initial.certificate().unwrap().id().unwrap()),
        )
        .unwrap();
    let mut membership = fixture.policy.clone();
    for chain in [&initial, &next] {
        membership
            .link(chain.certificate().unwrap().clone())
            .unwrap();
    }

    let open = async |name: &str, index: usize, number: u32| {
        let replica = EncryptedReplica::open(
            directory.path().join(name),
            fixture.app(),
            SecureConfig {
                publisher: fixture.publisher.public_key(),
                manifest: fixture.manifest.clone(),
                revocations: membership.clone(),
                node: nodes[index].clone(),
                credentials: prefix
                    .clone()
                    .certify_member(
                        &principal,
                        &nodes[index],
                        u64::from(number - 100),
                        match number {
                            100 => None,
                            101 => Some(initial.certificate().unwrap().id().unwrap()),
                            _ => Some(next.certificate().unwrap().id().unwrap()),
                        },
                    )
                    .unwrap(),
                keys: [fixture.items, fixture.salaries]
                    .into_iter()
                    .map(|scope| fixture.key(scope))
                    .collect(),
            },
        )
        .await
        .unwrap();
        replica
            .install_tables(
                fixture.schema.clone(),
                fixture.domain,
                [
                    ("items".into(), fixture.items.encryption),
                    ("salaries".into(), fixture.salaries.encryption),
                ]
                .into(),
            )
            .await
            .unwrap();
        replica
    };
    let first = open("first.sqlite", 0, 100).await;
    let second = open("second.sqlite", 1, 101).await;
    let mut ids = Vec::new();
    for (index, replica) in [&first, &second].into_iter().enumerate() {
        let writer = sql(replica, &[fixture.items, fixture.salaries]).await;
        writer.run("BEGIN", vec![]).await.unwrap();
        let item = writer.next_id("items").await.unwrap();
        let salary = writer.next_id("salaries").await.unwrap();
        assert_eq!(
            item.writer(),
            crate::writer::writer(&nodes[index].public_key())
        );
        assert_eq!(salary.writer(), item.writer());
        writer
            .run(
                "INSERT INTO items(key,title,id) VALUES(?,'item',?)",
                vec![text(&index.to_string()), item.sql()],
            )
            .await
            .unwrap();
        writer
            .run(
                "INSERT INTO salaries(amount,id) VALUES(5,?)",
                vec![salary.sql()],
            )
            .await
            .unwrap();
        let committed = writer
            .commit([200 + index as u8; 32])
            .await
            .unwrap()
            .committed
            .unwrap();
        assert_eq!(committed.frames.len(), 2);
        for frame in committed.frames {
            let operation = frame.operation().unwrap();
            assert_eq!(operation.header.public_key, nodes[index].public_key());
            assert_eq!(
                operation
                    .header
                    .extensions
                    .author(&operation.header.public_key),
                Some(item.writer())
            );
            if index == 0 {
                second.receive(frame).await.unwrap();
            } else {
                first.receive(frame).await.unwrap();
            }
        }
        ids.push((item, salary));
        for scope in [fixture.items, fixture.salaries] {
            assert_eq!(
                compacted_base(replica, &fixture.publisher, scope)
                    .await
                    .signer(),
                nodes[index].public_key()
            );
        }
    }
    assert_ne!(ids[0].0, ids[1].0);
    assert_ne!(ids[0].1, ids[1].1);
    assert_eq!(
        select(
            &sql(&second, &[fixture.items]).await,
            "SELECT count(*) FROM items"
        )
        .await,
        vec![vec![SqlValue::Integer(2)]],
        "the second's writes follow outcome records it published for the first's transaction"
    );
    let node_key_reused_in_a_fresh_database = open("conflicting.sqlite", 0, 102).await;
    let writer = sql(&node_key_reused_in_a_fresh_database, &[fixture.items]).await;
    writer.run("BEGIN", vec![]).await.unwrap();
    let id = writer.next_id("items").await.unwrap();
    writer
        .run(
            "INSERT INTO items(key,title,id) VALUES('conflict','item',?)",
            vec![id.sql()],
        )
        .await
        .unwrap();
    let committed = writer.commit([202; 32]).await.unwrap().committed.unwrap();
    assert_eq!(id, ids[0].0);
    let error = second
        .receive(committed.frames[0].clone())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("author log fork"),
        "a replica holding the log refuses a restarted log and row sequence: {error}"
    );
    let changed = prefix
        .certify_member(
            &principal,
            &nodes[0],
            2,
            Some(next.certificate().unwrap().id().unwrap()),
        )
        .unwrap();
    first.renew_credentials(changed).await.unwrap();
    let writer = sql(&first, &[fixture.items]).await;
    writer.run("BEGIN", vec![]).await.unwrap();
    let renewed = writer.next_id("items").await.unwrap();
    assert_eq!(
        renewed.writer(),
        ids[0].0.writer(),
        "another membership sequence for the same node keeps its writer"
    );
    assert!(renewed > ids[0].0);
    writer.run("ROLLBACK", vec![]).await.unwrap();
}

async fn author_salaries(
    replica: &EncryptedReplica<TypedRows>,
    scope: ResourceScope,
    count: i64,
    handle: u8,
) -> (Vec<rocky_core::RowIdentity>, Vec<EncryptedFrame>) {
    let session = sql(replica, &[scope]).await;
    session.run("BEGIN", vec![]).await.unwrap();
    let mut ids = Vec::new();
    for amount in 0..count {
        let id = session.next_id("salaries").await.unwrap();
        session
            .run(
                "INSERT INTO salaries(amount,id) VALUES(?,?)",
                vec![SqlValue::Integer(amount), id.sql()],
            )
            .await
            .unwrap();
        ids.push(id);
    }
    let frames = session
        .commit([handle; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    (ids, frames)
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn devices_authoring_from_one_snapshot_merge_in_both_orders_without_collisions() {
    let fixture = identity_fixture();
    let directory = rocky_platform::fs::Scratch::new().await;
    let scope = fixture.salaries;
    let open = async |name: &str| {
        let key = PrivateKey::new();
        let replica = fixture
            .open(&directory.path().join(name), &key, fixture.full(), &[scope])
            .await;
        (key, replica)
    };
    let (origin_key, origin) = open("origin.sqlite").await;
    let (old_ids, old) = author_salaries(&origin, scope, 2, 1).await;
    let ((left_key, left), (right_key, right)) =
        (open("left.sqlite").await, open("right.sqlite").await);
    for device in [&left, &right] {
        for frame in &old {
            assert_eq!(
                device.receive(frame.clone()).await.unwrap(),
                ReceiveStatus::Applied
            );
        }
    }
    let (left_ids, left_frames) = author_salaries(&left, scope, 3, 2).await;
    let (right_ids, right_frames) = author_salaries(&right, scope, 3, 3).await;
    for (ids, key) in [
        (&old_ids, &origin_key),
        (&left_ids, &left_key),
        (&right_ids, &right_key),
    ] {
        let writer = crate::writer::writer(&key.public_key());
        let sequences: Vec<u64> = ids.iter().map(|id| id.sequence()).collect();
        assert!(ids.iter().all(|id| id.writer() == writer));
        assert_eq!(
            sequences,
            (1..=ids.len() as u64).collect::<Vec<_>>(),
            "each device numbers its rows from its own key's first sequence"
        );
    }
    let (_, forward) = open("forward.sqlite").await;
    let (_, backward) = open("backward.sqlite").await;
    let deliver = async |replica: &EncryptedReplica<TypedRows>, frames: &[&[EncryptedFrame]]| {
        for frame in frames.iter().flat_map(|frames| frames.iter()) {
            assert_eq!(
                replica.receive(frame.clone()).await.unwrap(),
                ReceiveStatus::Applied
            );
        }
    };
    deliver(&left, &[&right_frames]).await;
    deliver(&right, &[&left_frames]).await;
    deliver(&forward, &[&old, &left_frames, &right_frames]).await;
    deliver(&backward, &[&old, &right_frames, &left_frames]).await;
    let mut expected: Vec<_> = [old_ids, left_ids, right_ids].concat();
    expected.sort();
    expected.dedup();
    assert_eq!(expected.len(), 8, "every row keeps a distinct identity");
    let rows = |replica| async move {
        select(
            &sql(replica, &[scope]).await,
            "SELECT id,amount FROM salaries ORDER BY id",
        )
        .await
    };
    let merged = rows(&left).await;
    assert_eq!(
        merged.iter().map(|row| row[0].clone()).collect::<Vec<_>>(),
        expected.iter().map(|id| id.sql()).collect::<Vec<_>>()
    );
    for replica in [&origin, &left, &right, &forward, &backward] {
        if !std::ptr::eq(replica, &origin) {
            assert_eq!(rows(replica).await, merged);
            assert_eq!(
                replica.snapshot(scope).await.unwrap().rows,
                left.snapshot(scope).await.unwrap().rows
            );
        }
        assert_eq!(replica.equivocations().await.unwrap(), 0);
        assert_eq!(replica.revocations().await.statements().count(), 0);
    }
    let session = sql(&left, &[scope]).await;
    for packed_integer_or_its_text in ["4294967297", "'4294967297'"] {
        let statement =
            format!("INSERT INTO salaries(amount,id) VALUES(1,{packed_integer_or_its_text})");
        let error = session
            .run(&statement, vec![])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("require a 40-byte row identity"), "{error}");
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn cursor_readiness_and_snapshot_do_not_wait_for_the_writer_lease() {
    use crate::cursor::CursorLimits;
    let t = Typed::new();
    let dir = Scratch::new().await;
    let replica = t
        .open(
            &dir.path().join("read.db"),
            &PrivateKey::new(),
            t.full(),
            &[t.items, t.salaries],
        )
        .await;
    let reader = sql(&replica, &[t.items, t.salaries]).await;
    insert(
        &reader,
        "INSERT INTO items VALUES('a','committed',0)",
        vec![],
    )
    .await
    .unwrap();
    let writer = replica.inner.lock(crate::replica::Class::Background).await;
    let mut cursor = rocky_platform::time::timeout(
        std::time::Duration::from_millis(500),
        reader.cursor("SELECT title FROM items", vec![], CursorLimits::default()),
    )
    .await
    .expect("read queued behind writer")
    .unwrap();
    assert_eq!(
        cursor.next().await.unwrap().rows,
        vec![vec![text("committed")]]
    );
    drop(writer);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn ordinary_sql_commits_do_not_restart_held_history_scans() {
    let t = Typed::new();
    let dir = Scratch::new().await;
    let replica = t
        .open(
            &dir.path().join("held-sql.db"),
            &PrivateKey::new(),
            t.full(),
            &[t.items],
        )
        .await;
    let session = sql(&replica, &[t.items]).await;
    insert(&session, "INSERT INTO items VALUES('a','first',0)", vec![])
        .await
        .unwrap();
    assert!(replica.held_changes().await.unwrap().is_empty());
    session.run("BEGIN", vec![]).await.unwrap();
    session
        .run("UPDATE items SET title='second' WHERE key='a'", vec![])
        .await
        .unwrap();
    session.run("COMMIT", vec![]).await.unwrap();
    crate::sqlite::trace_statements(true);
    assert!(replica.held_changes().await.unwrap().is_empty());
    let statements = crate::sqlite::trace_statements(false);
    assert!(
        !statements
            .iter()
            .any(|sql| sql.starts_with("SELECT o.hash,p.scope"))
    );
}

#[cfg(feature = "qualification")]
#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn sql_begin_and_writing_autocommit_are_interactive() {
    let t = Typed::new();
    let directory = Scratch::new().await;
    let replica = t
        .open(
            &directory.path().join("classes.db"),
            &PrivateKey::new(),
            t.full(),
            &[t.items],
        )
        .await;
    let sql = sql(&replica, &[t.items]).await;
    insert(&sql, "INSERT INTO items VALUES('probe','first',0)", vec![])
        .await
        .unwrap();
    let measurements = replica.lock_measurements();
    for explicit in [true, false] {
        measurements.start();
        if explicit {
            sql.run("BEGIN", vec![]).await.unwrap();
        }
        sql.run("UPDATE items SET done=1-done WHERE key='probe'", vec![])
            .await
            .unwrap();
        if explicit {
            sql.run("COMMIT", vec![]).await.unwrap();
        }
        let snapshot = measurements.stop();
        let interactive: Vec<_> = snapshot
            .holders
            .keys()
            .filter(|(_, class)| class == "Interactive")
            .collect();
        assert!(!interactive.is_empty(), "{snapshot:?}");
        assert!(interactive[0].0.contains("replica/sql.rs:"));
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_cursor_racing_a_commit_reads_once_readers_are_refreshed() {
    use crate::cursor::CursorLimits;
    let directory = Scratch::new().await;
    let fixture = Typed::new();
    let replica = fixture
        .open(
            &directory.path().join("race.db"),
            &PrivateKey::new(),
            fixture.full(),
            &[fixture.items],
        )
        .await;
    let session = sql(&replica, &[fixture.items]).await;
    let inner = replica.inner.lock(crate::replica::Class::Background).await;
    inner
        .conn
        .execute("UPDATE secure_read_epoch SET epoch=epoch+1 WHERE id=1", ())
        .await
        .unwrap();
    let cursor = session.cursor("SELECT key FROM items", vec![], CursorLimits::default());
    tokio::pin!(cursor);
    tokio::select! {
        biased;
        () = replica.inner.interactive_queued() => {}
        opened = &mut cursor => panic!(
            "the cursor opened without meeting the stale epoch and queueing for the lock: {:?}",
            opened.err()
        ),
    }
    inner.refresh_readers().await;
    drop(inner);
    let opened = cursor.await;
    assert!(opened.is_ok(), "{:?}", opened.err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_direct_replayed_import_blocks_the_full_audit_until_its_switch() {
    let f = Typed::new();
    let directory = Scratch::new().await;
    let (a_key, b_key) = (PrivateKey::new(), PrivateKey::new());
    let grants = vec![f.writer(f.items, "items")];
    let a = f
        .open(
            &directory.path().join("audit-a.db"),
            &a_key,
            grants.clone(),
            &[f.items],
        )
        .await;
    let writer = sql(&a, &[f.items]).await;
    insert(
        &writer,
        "INSERT INTO items(key,title,done) VALUES('a','row',0)",
        vec![],
    )
    .await
    .unwrap();
    drop(writer);
    let statements = seal_and_compact(&a, &f.publisher, f.domain).await;
    let checkpoint = a.checkpoint(f.items).await.unwrap();
    let id = checkpoint.id().unwrap();
    let b = f
        .open(
            &directory.path().join("audit-b.db"),
            &b_key,
            grants,
            &[f.items],
        )
        .await;
    b.publish_tip_statements(&statements).await.unwrap();
    b.follow_bookmark(f.domain, "main").await.unwrap();
    let credentials = b
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .credentials
        .clone();
    a.admit_peer(b_key.public_key(), &credentials, b.followed_tip().await)
        .await
        .unwrap();
    b.begin_rebootstrap(checkpoint).await.unwrap();
    let page = a
        .checkpoint_page(
            b_key.public_key(),
            &credentials,
            f.items,
            id,
            None,
            64 * 1024,
        )
        .await
        .unwrap();
    assert!(page.next.is_none());
    b.stage_checkpoint_page(page).await.unwrap();
    assert!(matches!(b.audit(f.items).await, Err(Error::Blocked(_))));
    b.finish_rebootstrap(f.items).await.unwrap();
    assert_eq!(b.audit(f.items).await.unwrap().rows, 1);
}
