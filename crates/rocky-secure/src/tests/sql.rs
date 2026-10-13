//! SQL sessions on typed tables of an encrypted replica.
use super::*;
mod catch_up;
use crate::sql::{AbortReason, SqlError, SqlOptions, SqlState};
use crate::storage::collect_rows;
use rocky_core::{
    Affinity, Collation, ColumnClass, DomainId, LOGICAL_SCHEMA_FORMAT, LogicalColumn,
    LogicalSchema, LogicalTable, RowApplication, SchemaSource, Scope, SqlValue, TableClass,
    TypedRows, UpdatePolicy,
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
        logical: name.into(),
        sql_name: name.into(),
        affinity,
        not_null,
        default: None,
        collation: Collation::Binary,
        class: ColumnClass::Mergeable,
    }
}
fn table(name: &str, sql: &str, mut columns: Vec<LogicalColumn>) -> LogicalTable {
    columns.push(column("id", Affinity::Integer, true));
    let mut kind = column("row_kind", Affinity::Integer, true);
    kind.default = Some("0".into());
    columns.push(kind);
    let sql = format!(
        "{},id INTEGER NOT NULL,row_kind INTEGER NOT NULL DEFAULT 0)",
        sql.trim_end_matches(')')
    );
    LogicalTable {
        logical: name.into(),
        sql_name: name.into(),
        class: TableClass::Mergeable,
        create_sql: sql,
        auto_generate: false,
        columns,
        primary_key: vec!["key".into()],
        partition_by: Vec::new(),
        partition_group: None,
        indexes: vec![rocky_core::LogicalIndex {
            name: format!("{name}_identity"),
            unique: true,
            columns: vec![rocky_core::IndexColumn {
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
impl Typed {
    fn new(policy: UpdatePolicy) -> Self {
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
            update_policy: policy,
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
        let user = PrivateKey::new();
        let capability = |subject, kind, delegations| Capability {
            manifest: self
                .manifest
                .verify(self.publisher.public_key())
                .unwrap()
                .hash(),
            subject,
            kind,
            grants: grants.clone(),
            not_before: crate::tests::lifetime().0,
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
    async fn open(
        &self,
        path: &std::path::Path,
        node: &PrivateKey,
        grants: Vec<ResourceGrant>,
        keys: &[ResourceScope],
    ) -> EncryptedReplica<TypedRows> {
        let author_grants = grants.clone();
        let replica = EncryptedReplica::open(
            path,
            self.app(),
            SecureConfig {
                publisher: self.publisher.public_key(),
                manifest: self.manifest.clone(),
                revocations: self.policy.clone(),
                node: node.clone(),
                credentials: self.credentials(node, grants),
                keys: keys.iter().map(|scope| self.key(*scope)).collect(),
            },
        )
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
        if self.schema.application == "secure-sql" || self.schema.application == "samples" {
            for table in ["items", "salaries"] {
                let material = rocky_core::commitment(table, node.as_bytes());
                let number = u32::from_le_bytes(material[..4].try_into().unwrap()) & 0x7fff_ffff;
                let number = number.max(1);
                let scope = if table == "items" {
                    self.items
                } else {
                    self.salaries
                };
                let grants: Vec<_> = author_grants
                    .iter()
                    .filter(|grant| grant.scope == scope)
                    .cloned()
                    .collect();
                if !grants.iter().any(|grant| {
                    grant
                        .rights
                        .iter()
                        .any(|right| [Right::Insert, Right::Update, Right::Delete].contains(right))
                }) {
                    continue;
                }
                let (key, chain) = row_author_with_grants(self, node, table, number, grants);
                replica.register_row_author(key, chain).await.unwrap();
            }
        }
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
/// Every frame of `scope` that `source` may send to `target`.
async fn transfer(
    source: &EncryptedReplica<TypedRows>,
    target: &EncryptedReplica<TypedRows>,
    scope: ResourceScope,
) -> CiphertextPage {
    let (node, credentials) = {
        let inner = target.inner.lock().await;
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

// Historical SQL scenarios now allocate current identities.
async fn insert(
    session: &SecureSql<TypedRows>,
    sql: &str,
    values: Vec<SqlValue>,
) -> Result<SecureRows, Error> {
    let implicit = session.state().await == SqlState::Idle;
    if implicit {
        session.run("BEGIN", vec![]).await?;
    }
    let result = async {
        let (head, tail) = sql.split_once(" VALUES").unwrap();
        let name = head
            .trim_start_matches("INSERT INTO ")
            .split('(')
            .next()
            .unwrap()
            .trim();
        let columns = if let Some((_, columns)) = head.split_once('(') {
            columns.trim_end_matches(')').to_owned()
        } else {
            match name {
                "items" if tail.starts_with("(?,?,?,?)") || tail.starts_with("('s',7,") => {
                    "sensor,sequence,reading,raw"
                }
                "items" => "key,title,done",
                "salaries" => "key,amount",
                "drafts" => "key,body",
                _ => unreachable!(),
            }
            .to_owned()
        };
        let mut rewritten = format!("INSERT INTO {name}({columns},id) VALUES");
        let mut quoted = false;
        let mut depth = 0;
        for ch in tail.chars() {
            if ch == '\'' {
                quoted = !quoted;
            }
            if !quoted {
                if ch == '(' {
                    depth += 1;
                }
                if ch == ')' {
                    depth -= 1;
                    if depth == 0 {
                        let id = if name == "drafts" {
                            4294967297
                        } else {
                            session.next_id(name).await?.integer()
                        };
                        rewritten.push_str(&format!(",{id}"));
                    }
                }
            }
            rewritten.push(ch);
        }
        session.run(&rewritten, values).await
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

#[tokio::test]
async fn a_sql_transaction_over_two_reader_scopes_commits_one_bundle() {
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
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
    // The transaction reads its own writes.
    assert_eq!(
        select(&session, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(3)]]
    );
    let committed = session.commit([1; 32]).await.unwrap();
    assert_eq!(committed.state, SqlState::Idle);
    let frames = committed.committed.unwrap().frames;
    assert_eq!(frames.len(), 2);
    assert_eq!(alice.snapshot(t.items).await.unwrap().rows.len(), 3);
    assert_eq!(alice.snapshot(t.salaries).await.unwrap().rows.len(), 1);

    // A public reader projects the public partition into its typed table and
    // cannot read the private one.
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
        ]
    );
    assert_eq!(
        sql_error(reader.run("SELECT * FROM salaries", vec![]).await),
        SqlError::Denied
    );
    assert_eq!(
        select(&reader, "SELECT count(*) FROM drafts").await,
        vec![vec![SqlValue::Integer(0)]]
    );

    // A full reader shows neither table before the whole bundle arrived.
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
    // Incomplete, not empty: the read is refused until the bundle is whole.
    assert_eq!(
        sql_error(full.run("SELECT count(*) FROM items", vec![]).await),
        SqlError::Unavailable {
            table: "items".into(),
            reason: crate::sql::Unavailable::Incomplete
        }
    );
    carol.receive(ordered[1].clone()).await.unwrap();
    assert_eq!(
        select(
            &full,
            "SELECT i.title,s.amount FROM items i JOIN salaries s USING(key)"
        )
        .await,
        vec![vec![text("apples"), SqlValue::Integer(1000)]]
    );

    // Single statements commit by themselves; peers follow updates and deletes.
    let updated = session
        .run("UPDATE items SET done=1 WHERE key='b'", vec![])
        .await
        .unwrap();
    let deleted = session
        .run("DELETE FROM items WHERE key='c'", vec![])
        .await
        .unwrap();
    for frame in updated
        .committed
        .unwrap()
        .frames
        .into_iter()
        .chain(deleted.committed.unwrap().frames)
    {
        assert_eq!(bob.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    }
    assert_eq!(
        select(&reader, "SELECT key,done FROM items ORDER BY key").await,
        vec![
            vec![text("a"), SqlValue::Integer(0)],
            vec![text("b"), SqlValue::Integer(1)],
        ]
    );
    // A deleted business key can be inserted with a fresh identity.
    insert(&session, "INSERT INTO items VALUES('c','again',0)", vec![])
        .await
        .unwrap();
    assert_eq!(
        select(&session, "SELECT count(*) FROM items WHERE key='c'").await,
        vec![vec![SqlValue::Integer(1)]]
    );
}

#[tokio::test]
async fn statements_outside_the_session_and_unpermitted_fields_are_refused() {
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
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
        "SELECT * FROM secure_rows",
        "SELECT * FROM sqlite_schema",
        "PRAGMA table_info(items)",
        "CREATE TABLE extra(a)",
        "DROP TABLE items",
        "ATTACH DATABASE ':memory:' AS other",
    ] {
        assert!(session.run(refused, vec![]).await.is_err(), "{refused}");
    }

    // A narrower session cannot name the other scope's table.
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
        SqlError::Denied
    );

    // A read-only principal reads but cannot write.
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
        SqlError::Denied
    );
    // A transaction that tried continues to report it until rolled back.
    reader.run("BEGIN", vec![]).await.unwrap();
    assert!(reader.run("DELETE FROM items", vec![]).await.is_err());
    assert!(matches!(
        sql_error(reader.run("SELECT 1", vec![]).await),
        SqlError::Aborted(AbortReason::Poisoned(_))
    ));
    reader.run("ROLLBACK", vec![]).await.unwrap();
    assert_eq!(
        select(&reader, "SELECT title FROM items").await,
        vec![vec![text("apples")]]
    );

    // A principal that may update titles only: the commit admits every
    // changed field, and a refused commit leaves no row behind.
    let carol = t
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
    let page = transfer(&alice, &carol, t.items).await;
    for status in carol.receive_all(page.frames).await {
        status.unwrap();
    }
    let editor = sql(&carol, &[t.items]).await;
    let titled = editor
        .run("UPDATE items SET title='pears' WHERE key='a'", vec![])
        .await
        .unwrap();
    assert_eq!(titled.committed.unwrap().frames.len(), 1);
    assert!(
        editor
            .run("UPDATE items SET done=1 WHERE key='a'", vec![])
            .await
            .is_err()
    );
    assert_eq!(
        select(&editor, "SELECT title,done FROM items").await,
        vec![vec![text("pears"), SqlValue::Integer(0)]]
    );
    // The table is writable, but inserting is not: the commit refuses it.
    assert!(matches!(
        insert(&editor, "INSERT INTO items VALUES('z','new',0)", vec![]).await,
        Err(Error::Rejected(_))
    ));
    assert_eq!(
        select(&editor, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(1)]]
    );
}

#[tokio::test]
async fn authority_changes_wait_for_the_lease_and_end_the_session() {
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
    let both = [t.items, t.salaries];
    let node = PrivateKey::new();
    let alice = t
        .open(&dir.path().join("a.db"), &node, t.full(), &both)
        .await;
    let root = alice.inner.lock().await.config.credentials.0[0]
        .id()
        .unwrap();
    let session = sql(&alice, &both).await;
    session.run("BEGIN", vec![]).await.unwrap();
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    // A revocation is linearized by the replica: it waits for the open
    // transaction, which commits under the authority it began with.
    let learning = {
        let alice = alice.clone();
        let policy = crate::tests::revoking(&t.publisher, t.app().manifest().app, &[], &[root]);
        tokio::spawn(async move { alice.learn_revocations(&policy).await })
    };
    tokio::task::yield_now().await;
    assert!(!learning.is_finished());
    let committed = session.commit([1; 32]).await.unwrap();
    assert_eq!(committed.committed.unwrap().frames.len(), 1);
    learning.await.unwrap().unwrap();
    // Afterwards every statement of the old session is refused.
    assert!(session.run("SELECT * FROM items", vec![]).await.is_err());
    assert!(alice.session(both.into()).await.is_err());

    // Credentials no statement ends open a new session.
    alice
        .renew_credentials(t.credentials(&node, t.full()))
        .await
        .unwrap();
    let renewed = sql(&alice, &both).await;
    assert_eq!(
        select(&renewed, "SELECT title FROM items").await,
        vec![vec![text("apples")]]
    );
    // Logout ends it; a new session finds the rows unavailable, not empty.
    alice.logout().await;
    assert!(renewed.run("SELECT * FROM items", vec![]).await.is_err());
    let after = alice.session(both.into()).await.unwrap();
    let after = after.sql(SqlOptions::default()).await.unwrap();
    assert_eq!(
        sql_error(after.run("SELECT * FROM items", vec![]).await),
        SqlError::Unavailable {
            table: "items".into(),
            reason: crate::sql::Unavailable::KeyMissing
        }
    );
}

#[tokio::test]
async fn credentials_expiring_inside_an_open_transaction_refuse_it() {
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
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

    // A SQL transaction began under valid credentials; the chain expires
    // while it is open. Its next statement and its commit are refused.
    let session = sql(&alice, &both).await;
    session.run("BEGIN", vec![]).await.unwrap();
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    CLOCK_SKEW.with(|skew| skew.set(7200));
    assert!(
        insert(&session, "INSERT INTO items VALUES('b','bread',0)", vec![])
            .await
            .is_err()
    );
    // The refused commit rolls the transaction back and frees the replica.
    assert!(session.commit([1; 32]).await.is_err());
    assert_eq!(session.state().await, SqlState::Idle);
    assert!(session.run("SELECT * FROM items", vec![]).await.is_err());

    // A staged secure transaction is admitted again at commit.
    CLOCK_SKEW.with(|skew| skew.set(0));
    let staged = alice.session(both.into()).await.unwrap();
    let table = t.schema.table("items").unwrap();
    let typed = t.app();
    let row = [
        text("c"),
        text("cheese"),
        SqlValue::Integer(0),
        SqlValue::Integer(4294967297),
        SqlValue::Integer(0),
    ];
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
    CLOCK_SKEW.with(|skew| skew.set(7200));
    assert!(transaction.commit([2; 32]).await.is_err());
    assert!(alice.session(both.into()).await.is_err());

    // Nothing committed; renewed credentials open a session again.
    CLOCK_SKEW.with(|skew| skew.set(0));
    assert!(alice.snapshot(t.items).await.unwrap().rows.is_empty());
    assert!(!alice.handle_committed([1; 32]).await.unwrap());
    assert!(!alice.handle_committed([2; 32]).await.unwrap());
    alice
        .renew_credentials(t.credentials(&node, t.full()))
        .await
        .unwrap();
    assert_eq!(
        select(&sql(&alice, &both).await, "SELECT count(*) FROM items").await,
        vec![vec![SqlValue::Integer(0)]]
    );
}

#[tokio::test]
async fn cancel_and_timeout_roll_back_and_free_the_replica() {
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
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

    // An expired transaction is rolled back and the replica is free again.
    let short = alice
        .session(both.into())
        .await
        .unwrap()
        .sql(SqlOptions {
            transaction_timeout: std::time::Duration::from_millis(50),
            ..SqlOptions::default()
        })
        .await
        .unwrap();
    short.run("BEGIN", vec![]).await.unwrap();
    insert(&short, "INSERT INTO items VALUES('b','bread',0)", vec![])
        .await
        .unwrap();
    // Another writer waits for the lease; the timeout releases it.
    let written = insert(&session, "INSERT INTO items VALUES('c','cheese',0)", vec![])
        .await
        .unwrap();
    assert_eq!(written.committed.unwrap().frames.len(), 1);
    assert_eq!(
        sql_error(short.run("COMMIT", vec![]).await),
        SqlError::Aborted(AbortReason::Timeout)
    );
    assert_eq!(
        select(&session, "SELECT key FROM items").await,
        vec![vec![text("c")]]
    );

    // A dropped session rolls its transaction back.
    let dropped = sql(&alice, &both).await;
    dropped.run("BEGIN", vec![]).await.unwrap();
    insert(&dropped, "INSERT INTO items VALUES('d','dates',0)", vec![])
        .await
        .unwrap();
    drop(dropped);
    assert_eq!(
        select(&session, "SELECT key FROM items").await,
        vec![vec![text("c")]]
    );
}

#[tokio::test]
async fn a_handle_recovers_its_commit_after_restart_and_policies_shape_masks() {
    let t = Typed::new(UpdatePolicy::Assigned);
    let dir = TempDir::new().unwrap();
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
    drop(session);
    drop(alice);

    // The same handle and rows after a restart return the recorded frames
    // and roll the repeated transaction back.
    let alice = t.open(&path, &node, t.full(), &both).await;
    let session = sql(&alice, &both).await;
    assert_eq!(
        select(&session, "SELECT title FROM items").await,
        vec![vec![text("apples")]]
    );
    session.run("BEGIN", vec![]).await.unwrap();
    session.run("DELETE FROM items", vec![]).await.unwrap();
    insert(&session, "INSERT INTO items VALUES('a','apples',0)", vec![])
        .await
        .unwrap();
    // Deleting and reinserting a key republishes its row: a new transaction.
    let republished = session.commit([8; 32]).await.unwrap().committed.unwrap();
    assert_eq!(republished.frames.len(), 1);
    session.run("BEGIN", vec![]).await.unwrap();
    session
        .run("UPDATE items SET title='apples'", vec![])
        .await
        .unwrap();
    // Under the assigned policy an equal assignment is published.
    let assigned = session.commit([9; 32]).await.unwrap().committed.unwrap();
    assert_eq!(assigned.frames.len(), 1);
    assert_ne!(
        assigned.frames[0].id().unwrap(),
        first.frames[0].id().unwrap()
    );
    // Reusing a handle with other rows conflicts and changes nothing.
    session.run("BEGIN", vec![]).await.unwrap();
    session
        .run("UPDATE items SET done=1", vec![])
        .await
        .unwrap();
    assert!(matches!(
        session.commit([9; 32]).await,
        Err(Error::Conflict)
    ));
    assert_eq!(
        select(&session, "SELECT done FROM items").await,
        vec![vec![SqlValue::Integer(0)]]
    );
    // The same rows under the same handle replay.
    session.run("BEGIN", vec![]).await.unwrap();
    session
        .run("UPDATE items SET title='apples'", vec![])
        .await
        .unwrap();
    let replayed = session.commit([9; 32]).await.unwrap().committed.unwrap();
    assert_eq!(
        replayed.frames[0].id().unwrap(),
        assigned.frames[0].id().unwrap()
    );
}

#[tokio::test]
async fn concurrent_sql_edits_converge_in_every_typed_table() {
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
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
    assert_eq!(
        alice.snapshot(t.items).await.unwrap().rows,
        carol.snapshot(t.items).await.unwrap().rows
    );
}

#[tokio::test]
async fn an_edit_concurrent_with_a_delete_applies_and_its_log_continues() {
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
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
    // The edit merges into the tombstone: the delete wins, and the
    // editor's next frame is not left waiting for it.
    for frame in renamed.into_iter().chain(later) {
        assert_eq!(alice.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    }
    for frame in deleted {
        assert_eq!(carol.receive(frame).await.unwrap(), ReceiveStatus::Applied);
    }
    let rows = select(&a, "SELECT key,title FROM items").await;
    assert_eq!(rows, vec![vec![text("b"), text("bread")]]);
    assert_eq!(rows, select(&c, "SELECT key,title FROM items").await);
    assert_eq!(
        alice.snapshot(t.items).await.unwrap().rows,
        carol.snapshot(t.items).await.unwrap().rows
    );
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

#[tokio::test]
async fn catalogs_cursors_and_joins_report_unavailable_rows_instead_of_absence() {
    use crate::cursor::CursorLimits;
    use crate::sql::Unavailable;
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
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
    // A permitted absent companion is an ordinary NULL.
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
        ]
    );
    // The catalog names the session's tables and what it may write.
    let catalog = writer.catalog();
    assert_eq!(
        catalog
            .iter()
            .map(|table| (table.name.as_str(), table.writable))
            .collect::<Vec<_>>(),
        [("drafts", true), ("items", true), ("salaries", true)]
    );

    // Granted both scopes but holding only the public key: the join is
    // unavailable, never NULL salaries.
    let bob = t
        .open(
            &dir.path().join("b.db"),
            &PrivateKey::new(),
            t.full(),
            &[t.items],
        )
        .await;
    for frame in transfer(&alice, &bob, t.items).await.frames {
        bob.receive(frame).await.unwrap();
    }
    let reader = sql(&bob, &both).await;
    let unavailable = SqlError::Unavailable {
        table: "salaries".into(),
        reason: Unavailable::KeyMissing,
    };
    assert_eq!(sql_error(reader.run(join, vec![]).await), unavailable);
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
    // Denied objects stay denied for cursors too.
    for denied in [
        "SELECT * FROM secure_rows",
        "SELECT name FROM sqlite_schema",
    ] {
        assert!(
            reader
                .cursor(denied, vec![], CursorLimits::default())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn secure_cursors_keep_their_snapshot_and_end_when_authority_changes() {
    use crate::cursor::CursorLimits;
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
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
    // The cursor holds no lease: a write commits while it is open.
    session
        .run("DELETE FROM items WHERE key='4'", vec![])
        .await
        .unwrap();
    rows.extend(rows_of(&mut cursor).await);
    assert_eq!(rows.len(), 5);
    cursor.reset().await.unwrap();
    cursor.next().await.unwrap();
    // Losing the key revokes the session: undelivered pages are discarded.
    alice.evict_key(t.salaries).await;
    assert!(matches!(
        cursor.next().await,
        Err(Error::Sql(SqlError::Unavailable {
            reason: crate::sql::Unavailable::Revoked,
            ..
        }))
    ));
    assert!(cursor.next().await.is_err());
    assert!(
        session
            .cursor("SELECT key FROM items", vec![], limits)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn secure_table_events_follow_commits_receipts_and_authority() {
    use crate::observe::Cause;
    let t = Typed::new(UpdatePolicy::Changed);
    let dir = TempDir::new().unwrap();
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
    let mut items = alice.observe_tables(Some(&["items"])).await.unwrap();
    let mut salaries = alice.observe_tables(Some(&["salaries"])).await.unwrap();
    let mut received = bob.observe_tables(Some(&["items"])).await.unwrap();
    assert!(alice.observe_tables(Some(&["secure_rows"])).await.is_err());
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
    // A receipt wakes the receiver's observers.
    for frame in transfer(&alice, &bob, t.items).await.frames {
        bob.receive(frame).await.unwrap();
    }
    let change = received.next().await.unwrap();
    assert_eq!(change.causes, [Cause::Sync].into());
    // An authority change wakes every observer.
    alice.evict_key(t.salaries).await;
    assert_eq!(
        salaries.next().await.unwrap().causes,
        [Cause::Authority].into()
    );
    assert_eq!(
        items.next().await.unwrap().causes,
        [Cause::Authority].into()
    );
    drop(session);
    drop(alice);
    assert!(items.next().await.is_err());
}

#[tokio::test]
async fn definition_compound_keys_indexes_real_and_blob_survive_bundles_and_reopen() {
    let publisher = PrivateKey::new();
    let definition = r#"{
        "format":5, "publisher": "PUBLISHER",
        "application": "samples", "tables": [
            {"name":"items", "class":"mergeable", "columns":[
                {"name":"sensor", "type":"text", "not_null":true},
                {"name":"sequence", "type":"integer", "not_null":true},
                {"name":"reading", "type":"real"}, {"name":"raw", "type":"blob"}],
             "primary_key":["sensor","sequence"],
             "indexes":[{"name":"items_by_reading", "columns":[{"name":"reading", "descending":true}]}]},
            {"name":"salaries", "class":"mergeable", "columns":[
                {"name":"key", "type":"text", "not_null":true}, {"name":"amount", "type":"integer", "not_null":true}],
             "primary_key":["key"]}
        ]
    }"#.replace("PUBLISHER", &publisher.public_key().as_bytes().iter().map(|byte| format!("{byte:02x}")).collect::<String>());
    let schema = rocky_core::SchemaDefinition::parse(definition.as_bytes())
        .unwrap()
        .build()
        .unwrap();
    let t = Typed::with_schema(publisher, schema);
    let dir = TempDir::new().unwrap();
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

#[tokio::test]
async fn identity_descriptor_installs_shared_columns_and_reopens() {
    let fixture = identity_fixture();
    let directory = tempfile::tempdir().unwrap();
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
                .run(
                    &format!("SELECT id,row_kind FROM {name} WHERE row_kind=0"),
                    vec![],
                )
                .await
                .unwrap();
            assert_eq!(result.columns, ["id", "row_kind"]);
            assert!(result.rows.is_empty());
        }
        drop(sql);
        drop(replica);
    }
}

fn identity_fixture() -> Typed {
    let publisher = PrivateKey::new();
    // Use definition generation, not handwritten identity DDL.
    let definition = r#"{
        "format":5, "publisher": "PUBLISHER",
        "application": "identity-descriptor", "tables": [
            {"name": "items", "class": "mergeable", "columns": [
                {"name": "key", "type": "text", "not_null": true},
                {"name": "title", "type": "text", "not_null": true}], "primary_key": ["key"]},
            {"name": "salaries", "class": "mergeable", "columns": [
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
fn row_author(
    fixture: &Typed,
    node: &PrivateKey,
    table: &str,
    number: u32,
) -> (PrivateKey, CredentialChain) {
    let grants = vec![fixture.writer(
        if table == "items" {
            fixture.items
        } else {
            fixture.salaries
        },
        table,
    )];
    row_author_with_grants(fixture, node, table, number, grants)
}
fn row_author_with_grants(
    fixture: &Typed,
    node: &PrivateKey,
    table: &str,
    number: u32,
    grants: Vec<ResourceGrant>,
) -> (PrivateKey, CredentialChain) {
    let key = PrivateKey::from_bytes(&rocky_core::commitment(table, node.as_bytes()));
    let assigner = PrivateKey::new();
    let issued = crate::credentials::Authority {
        publisher: &fixture.publisher,
        manifest: fixture
            .manifest
            .verify(fixture.publisher.public_key())
            .unwrap()
            .hash(),
    }
    .issue_terms(
        assigner.public_key(),
        grants.clone(),
        (1, Some(crate::credentials::AuthorRange::ALL)),
        crate::tests::validity(),
    )
    .unwrap();
    let credentials = CredentialChain(vec![issued])
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
    (key, credentials)
}

#[tokio::test]
async fn ids_reserve_inside_a_transaction_and_continue_after_delete_checkpoint_and_reopen() {
    let fixture = identity_fixture();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("identities.sqlite");
    let node = PrivateKey::new();
    let (key, credentials) = row_author(&fixture, &node, "items", 17);
    let replica = fixture
        .open(
            &path,
            &node,
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    replica
        .register_row_author(key.clone(), credentials.clone())
        .await
        .unwrap();
    // A registered row author keeps its chain stored before any operation
    // names it.
    let author_chain = crate::wire::Chain::new(credentials.clone())
        .unwrap()
        .digest();
    assert_eq!(replica.chain_census().await, ([author_chain].into(), 0));
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
    assert_eq!((first.author(), first.sequence()), (17, 1));
    assert_eq!(session_sql.next_id("items").await.unwrap().sequence(), 2);
    session_sql.run("ROLLBACK", vec![]).await.unwrap();
    session_sql.run("BEGIN", vec![]).await.unwrap();
    assert_eq!(session_sql.next_id("items").await.unwrap(), first);
    session_sql
        .run(
            "INSERT INTO items(key,title,id) VALUES(?,?,?)",
            vec![text("a"), text("first"), SqlValue::Integer(first.integer())],
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
    assert_eq!(operation.header.extensions.author, Some(17));
    assert_eq!(operation.header.extensions.row_format, 8);
    assert_eq!(operation.header.extensions.chain, author_chain);
    session_sql
        .run("DELETE FROM items WHERE key='a'", vec![])
        .await
        .unwrap();
    drop(session_sql);
    replica.checkpoint(fixture.items).await.unwrap();
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
    replica.register_row_author(key, credentials).await.unwrap();
    let session_sql = sql(&replica, &[fixture.items]).await;
    session_sql.run("BEGIN", vec![]).await.unwrap();
    assert_eq!(session_sql.next_id("items").await.unwrap().sequence(), 2);
    session_sql.run("ROLLBACK", vec![]).await.unwrap();
}

#[tokio::test]
async fn identity_reservations_follow_savepoints_and_distinct_table_signers_form_one_bundle() {
    let fixture = identity_fixture();
    let directory = tempfile::tempdir().unwrap();
    let node = PrivateKey::new();
    let replica = fixture
        .open(
            &directory.path().join("writer.sqlite"),
            &node,
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    let (items_key, items_chain) = row_author(&fixture, &node, "items", 11);
    let (salaries_key, salaries_chain) = row_author(&fixture, &node, "salaries", 12);
    replica
        .register_row_author(items_key.clone(), items_chain)
        .await
        .unwrap();
    replica
        .register_row_author(salaries_key.clone(), salaries_chain)
        .await
        .unwrap();
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
            vec![SqlValue::Integer(item.integer())],
        )
        .await
        .unwrap();
    writer
        .run(
            "INSERT INTO items(key,title,id) VALUES('a','second',?)",
            vec![SqlValue::Integer(unused.integer())],
        )
        .await
        .unwrap();
    writer
        .run(
            "INSERT INTO salaries(amount,id) VALUES(5,?)",
            vec![SqlValue::Integer(salary.integer())],
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
    assert_eq!(
        signers,
        [items_key.public_key(), salaries_key.public_key()].into()
    );
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
            .run("SELECT id,row_kind FROM items ORDER BY id", vec![])
            .await
            .unwrap()
            .rows,
        vec![
            vec![SqlValue::Integer(item.integer()), SqlValue::Integer(0)],
            vec![SqlValue::Integer(unused.integer()), SqlValue::Integer(0)]
        ]
    );
    assert_eq!(
        reader
            .run("SELECT id,row_kind FROM salaries", vec![])
            .await
            .unwrap()
            .rows,
        vec![vec![
            SqlValue::Integer(salary.integer()),
            SqlValue::Integer(0)
        ]]
    );
    for (statement, values) in [
        (
            "UPDATE items SET id=? WHERE key='z'",
            vec![SqlValue::Integer(item.integer() + 10)],
        ),
        ("UPDATE items SET row_kind=1 WHERE key='z'", vec![]),
    ] {
        assert!(writer.run(statement, values).await.is_err());
    }
    assert_eq!(
        writer
            .run("SELECT id,row_kind FROM items WHERE key='z'", vec![])
            .await
            .unwrap()
            .rows,
        vec![vec![
            SqlValue::Integer(item.integer()),
            SqlValue::Integer(0)
        ]]
    );
}

#[tokio::test]
async fn failed_identity_commit_releases_its_sequence_and_forged_frame_author_is_refused() {
    let fixture = identity_fixture();
    let directory = tempfile::tempdir().unwrap();
    let node = PrivateKey::new();
    let replica = fixture
        .open(
            &directory.path().join("writer.sqlite"),
            &node,
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    let (key, chain) = row_author(&fixture, &node, "items", 31);
    replica
        .register_row_author(key.clone(), chain)
        .await
        .unwrap();
    replica.inject_failure(0, 1).await;
    let writer = sql(&replica, &[fixture.items]).await;
    writer.run("BEGIN", vec![]).await.unwrap();
    let id = writer.next_id("items").await.unwrap();
    writer
        .run(
            "INSERT INTO items(key,title,id) VALUES('a','first',?)",
            vec![SqlValue::Integer(id.integer())],
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
            vec![SqlValue::Integer(id.integer())],
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
    let plaintext = crate::wire::decrypt(
        &fixture.key(fixture.items).key,
        &encode(&operation.header.extensions).unwrap(),
        &frame.body,
    )
    .unwrap();
    let mut extensions = operation.header.extensions;
    extensions.author = Some(32);
    let forged = crate::wire::author(
        &key,
        extensions,
        frame.chain().unwrap().clone(),
        &fixture.key(fixture.items).key,
        (
            operation.header.seq_num,
            operation.header.backlink,
            operation.header.timestamp,
        ),
        operation.header.previous,
        &plaintext,
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
    assert!(
        peer.receive(forged)
            .await
            .unwrap_err()
            .to_string()
            .contains("author number")
    );
    assert_eq!(
        peer.receive(frame.clone()).await.unwrap(),
        ReceiveStatus::Applied
    );
    // A new authenticated log operation cannot reuse a creation sequence,
    // even at another natural key.
    let operation = frame.operation().unwrap();
    let app = fixture.app();
    let table = app.table(&fixture.items.table).unwrap();
    let values = vec![
        SqlValue::Text("reused".into()),
        SqlValue::Text("bad".into()),
        SqlValue::Integer(id.integer()),
        SqlValue::Integer(0),
    ];
    let change = rocky_core::RowChange {
        scope: fixture.items,
        row: app
            .row_id(fixture.items.domain.domain, table, &values)
            .unwrap(),
        before: None,
        after: Some(app.fields(table, &values).unwrap()),
    };
    let reused = crate::wire::author(
        &key,
        operation.header.extensions.clone(),
        frame.chain().unwrap().clone(),
        &fixture.key(fixture.items).key,
        (
            operation.header.seq_num + 1,
            Some(operation.hash),
            operation.header.timestamp + 1,
        ),
        vec![],
        &crate::wire::RowPayload::witness(
            crate::wire::RowPayload::rows(&[change]).unwrap(),
            vec![],
            None,
            vec![],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(peer.receive(reused).await.unwrap(), ReceiveStatus::Rejected);
    let signed = peer.checkpoint(fixture.items).await.unwrap();
    assert_eq!(signed.statement.format, 9);
    assert_eq!(signed.statement.identities[0].sequence, 1);
}

#[tokio::test]
async fn natural_key_relink_converges_and_old_identity_edits_never_touch_replacement() {
    let publisher = PrivateKey::new();
    let definition = r#"{"format":5,"publisher":"PUBLISHER","application":"pair_identity","tables":[
        {"name":"items","class":"mergeable","columns":[{"name":"left","type":"text","not_null":true},{"name":"right","type":"text","not_null":true},{"name":"title","type":"text","not_null":true}],"primary_key":["left","right"]},
        {"name":"salaries","class":"mergeable","columns":[{"name":"amount","type":"integer","not_null":true}],"primary_key":["id"]}
    ]}"#.replace("PUBLISHER", &publisher.public_key().to_string());
    let schema = rocky_core::SchemaDefinition::parse(definition.as_bytes())
        .unwrap()
        .build()
        .unwrap();
    let fixture = Typed::with_schema(publisher, schema);
    let directory = tempfile::tempdir().unwrap();
    let nodes = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
    let mut replicas = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let replica = fixture
            .open(
                &directory.path().join(format!("{index}.sqlite")),
                node,
                fixture.full(),
                &[fixture.items, fixture.salaries],
            )
            .await;
        let (key, chain) = row_author(&fixture, node, "items", 41 + index as u32);
        replica.register_row_author(key, chain).await.unwrap();
        replicas.push(replica);
    }
    let a = sql(&replicas[0], &[fixture.items]).await;
    let b = sql(&replicas[1], &[fixture.items]).await;
    a.run("BEGIN", vec![]).await.unwrap();
    let first = a.next_id("items").await.unwrap();
    a.run(
        "INSERT INTO items(left,right,title,id) VALUES('list','item','original',?)",
        vec![SqlValue::Integer(first.integer())],
    )
    .await
    .unwrap();
    let original = a.commit([130; 32]).await.unwrap().committed.unwrap().frames;
    for peer in &replicas[1..] {
        for frame in &original {
            peer.receive(frame.clone()).await.unwrap();
        }
    }
    let late = b
        .run(
            "UPDATE items SET title='late' WHERE left='list' AND right='item'",
            vec![],
        )
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    // Unlink and relink in one transaction names two different incarnations.
    a.run("BEGIN", vec![]).await.unwrap();
    a.run(
        "DELETE FROM items WHERE left='list' AND right='item'",
        vec![],
    )
    .await
    .unwrap();
    let second = a.next_id("items").await.unwrap();
    a.run(
        "INSERT INTO items(left,right,title,id) VALUES('list','item','replacement',?)",
        vec![SqlValue::Integer(second.integer())],
    )
    .await
    .unwrap();
    let replacement = a.commit([131; 32]).await.unwrap().committed.unwrap().frames;
    // Concurrent delete names the first id, never the replacement.
    let deleted = b
        .run(
            "DELETE FROM items WHERE left='list' AND right='item'",
            vec![],
        )
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    for peer in &replicas {
        for frames in [&replacement, &deleted, &late] {
            for frame in frames.iter().rev() {
                peer.receive(frame.clone()).await.unwrap();
            }
        }
    }
    for peer in &replicas {
        let rows = sql(peer, &[fixture.items])
            .await
            .run(
                "SELECT id,title,row_kind FROM items WHERE row_kind=0",
                vec![],
            )
            .await
            .unwrap()
            .rows;
        assert_eq!(
            rows,
            vec![vec![
                SqlValue::Integer(second.integer()),
                text("replacement"),
                SqlValue::Integer(0)
            ]]
        );
        let checkpoint = peer.checkpoint(fixture.items).await.unwrap();
        assert_eq!(
            checkpoint
                .statement
                .identities
                .iter()
                .find(|mark| mark.author == 41)
                .unwrap()
                .sequence,
            2
        );
    }
    // A fresh replica bootstraps incarnations and permanent marks, including tombstones.
    let fresh_node = PrivateKey::new();
    let fresh = fixture
        .open(
            &directory.path().join("fresh.sqlite"),
            &fresh_node,
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    let signed = replicas[0].checkpoint(fixture.items).await.unwrap();
    let checkpoint_id = signed.id().unwrap();
    fresh.begin_rebootstrap(signed).await.unwrap();
    let credentials = fixture.credentials(&fresh_node, fixture.full());
    let mut after = None;
    loop {
        let page = replicas[0]
            .checkpoint_page(
                fresh_node.public_key(),
                &credentials,
                fixture.items,
                checkpoint_id,
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
    fresh.finish_rebootstrap(fixture.items).await.unwrap();
    assert_eq!(
        sql(&fresh, &[fixture.items])
            .await
            .run("SELECT id,title FROM items", vec![])
            .await
            .unwrap()
            .rows,
        vec![vec![
            SqlValue::Integer(second.integer()),
            text("replacement")
        ]]
    );
}

#[tokio::test]
async fn multi_frame_relinks_verify_the_final_sql_projection() {
    let fixture = identity_fixture();
    let directory = tempfile::tempdir().unwrap();
    let node = PrivateKey::new();
    let replica = fixture
        .open(
            &directory.path().join("many.sqlite"),
            &node,
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    let (key, chain) = row_author(&fixture, &node, "items", 55);
    replica.register_row_author(key, chain).await.unwrap();
    let writer = sql(&replica, &[fixture.items]).await;
    writer.run("BEGIN", vec![]).await.unwrap();
    for n in 0..20 {
        let id = writer.next_id("items").await.unwrap();
        writer
            .run(
                "INSERT INTO items(key,title,id) VALUES(?,'old',?)",
                vec![text(&n.to_string()), SqlValue::Integer(id.integer())],
            )
            .await
            .unwrap();
    }
    writer.run("COMMIT", vec![]).await.unwrap();
    writer.run("BEGIN", vec![]).await.unwrap();
    writer.run("DELETE FROM items", vec![]).await.unwrap();
    for n in 0..20 {
        let id = writer.next_id("items").await.unwrap();
        writer
            .run(
                "INSERT INTO items(key,title,id) VALUES(?,'new',?)",
                vec![text(&n.to_string()), SqlValue::Integer(id.integer())],
            )
            .await
            .unwrap();
    }
    let frames = writer
        .run("COMMIT", vec![])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    assert!(frames.len() > 1);
    replica.checkpoint(fixture.items).await.unwrap();
    replica.rebuild_projection(fixture.items).await.unwrap();
    assert_eq!(
        writer
            .run(
                "SELECT COUNT(*) FROM items WHERE title='new' AND row_kind=0",
                vec![]
            )
            .await
            .unwrap()
            .rows,
        vec![vec![SqlValue::Integer(20)]]
    );
}

#[tokio::test]
async fn local_identity_columns_refuse_reserved_kinds_and_mutation() {
    let fixture = identity_fixture();
    let directory = tempfile::tempdir().unwrap();
    let replica = fixture
        .open(
            &directory.path().join("local_ids.sqlite"),
            &PrivateKey::new(),
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    let writer = sql(&replica, &[fixture.items]).await;
    let id = rocky_core::RowIdentity::new(99, 1).unwrap();
    writer
        .run(
            "INSERT INTO drafts(id,body) VALUES(?,'local')",
            vec![SqlValue::Integer(id.integer())],
        )
        .await
        .unwrap();
    for (statement, values) in [
        ("UPDATE drafts SET row_kind=1", vec![]),
        (
            "UPDATE drafts SET id=?",
            vec![SqlValue::Integer(id.integer() + 1)],
        ),
        ("INSERT INTO drafts(id,body) VALUES(0,'virtual')", vec![]),
        (
            "INSERT INTO drafts(id,body,row_kind) VALUES(?,'puppet',2)",
            vec![SqlValue::Integer(id.integer() + 2)],
        ),
    ] {
        assert!(writer.run(statement, values).await.is_err());
    }
    assert_eq!(
        writer
            .run("SELECT id,row_kind FROM drafts", vec![])
            .await
            .unwrap()
            .rows,
        vec![vec![SqlValue::Integer(id.integer()), SqlValue::Integer(0)]]
    );
}

#[tokio::test]
async fn incarnation_key_index_reopens_and_tracks_rebuild_and_eviction() {
    let fixture = identity_fixture();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("indexed.sqlite");
    let node = PrivateKey::new();
    let replica = fixture
        .open(
            &path,
            &node,
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    let (key, chain) = row_author(&fixture, &node, "items", 71);
    replica.register_row_author(key, chain).await.unwrap();
    let writer = sql(&replica, &[fixture.items]).await;
    for title in ["old", "replacement"] {
        writer.run("BEGIN", vec![]).await.unwrap();
        writer.run("DELETE FROM items", vec![]).await.unwrap();
        let id = writer.next_id("items").await.unwrap();
        writer
            .run(
                "INSERT INTO items(key,title,id) VALUES('key',?,?)",
                vec![text(title), SqlValue::Integer(id.integer())],
            )
            .await
            .unwrap();
        writer.run("COMMIT", vec![]).await.unwrap();
    }
    {
        let inner = replica.inner.lock().await;
        let indexed = collect_rows(
            inner
                .conn
                .query("SELECT key FROM secure_row_keys", ())
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(indexed.len(), 2, "tombstones remain indexed");
        assert_eq!(
            indexed[0], indexed[1],
            "both incarnations share the business key"
        );
        let plan = collect_rows(inner.conn.query(
            "EXPLAIN QUERY PLAN SELECT r.row,r.fields FROM secure_row_keys k JOIN secure_rows r ON r.scope=k.scope AND r.row=k.row WHERE k.scope=? AND k.key=? AND k.row>? ORDER BY k.row LIMIT 256",
            [encode(&fixture.items).unwrap(), vec![0; 32], Vec::new()],
        ).await.unwrap()).await.unwrap();
        assert!(plan.iter().any(|row| row.iter().any(|value| matches!(value, SqlValue::Text(detail) if detail.contains("secure_row_keys_key") && detail.contains("scope=? AND key=? AND row>?")))), "{plan:?}");
    }
    drop(writer);
    drop(replica);
    let reopened = fixture
        .open(
            &path,
            &node,
            fixture.full(),
            &[fixture.items, fixture.salaries],
        )
        .await;
    reopened.rebuild_projection(fixture.items).await.unwrap();
    assert_eq!(
        sql(&reopened, &[fixture.items])
            .await
            .run("SELECT title FROM items", vec![])
            .await
            .unwrap()
            .rows,
        vec![vec![text("replacement")]]
    );
    let inner = reopened.inner.lock().await;
    let indexed = collect_rows(
        inner
            .conn
            .query("SELECT key FROM secure_row_keys", ())
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(indexed.len(), 2);
    inner
        .conn
        .execute(
            "DELETE FROM secure_rows WHERE scope=?",
            [encode(&fixture.items).unwrap()],
        )
        .await
        .unwrap();
    assert!(
        collect_rows(
            inner
                .conn
                .query("SELECT key FROM secure_row_keys", ())
                .await
                .unwrap()
        )
        .await
        .unwrap()
        .is_empty()
    );
}
