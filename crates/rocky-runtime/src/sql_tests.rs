//! Faults and process death inside a SQL transaction's commit. The app here
//! is a second, minimal implementation of `SqlWrites`: whole-row registers
//! keyed by a text id. The typed application's gate is in `rocky-sdk`.
use super::*;
use rocky_core::{
    AdmissionBoundary, Affinity, Application, ApplicationDescriptor, CapturedRow, Collation,
    ColumnClass, InstalledSchema, Intent, LOGICAL_SCHEMA_FORMAT, LogicalColumn, LogicalSchema,
    LogicalTable, MeshId, ProjectionSchema, ProjectionStatement, SchemaSource, Sql, SqlValue,
    SqlWrites, Stamp, TableClass, TransactionToken, UpdatePolicy, Versions,
};
use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use tempfile::TempDir;

const CASE: &str = "ROCKY_SQL_CRASH_CASE";
const DIR: &str = "ROCKY_SQL_CRASH_DIR";
const NOTES: &str = "CREATE TABLE notes(key TEXT NOT NULL,body TEXT NOT NULL,id INTEGER NOT NULL,row_kind INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(key))";
const INDEX: &str = "CREATE UNIQUE INDEX notes_identity ON notes(id)";
const STATE: &str = "CREATE TABLE note_state(id TEXT PRIMARY KEY NOT NULL,body TEXT,lamport INTEGER NOT NULL,node BLOB NOT NULL,sequence INTEGER NOT NULL) WITHOUT ROWID";
const SELECT: &str = "SELECT id,body,lamport,node,sequence FROM note_state WHERE id=?";

#[derive(Debug)]
pub(crate) struct Refused(&'static str);
impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Refused {}

pub(crate) struct Notes {
    pub(crate) schema: LogicalSchema,
    /// Test switch: the projection writes another body than the reducer holds.
    pub(crate) skewed: Arc<AtomicBool>,
}
/// Domains the notes manifest declares, for the shared runtime's tests.
pub(crate) const DOMAINS: [rocky_core::DomainId; 2] =
    [rocky_core::DomainId([4; 16]), rocky_core::DomainId([5; 16])];
pub(crate) fn schema() -> LogicalSchema {
    let column = |name: &str| LogicalColumn {
        sql_name: name.into(),
        affinity: Affinity::Text,
        not_null: true,
        default: None,
        collation: Collation::Binary,
        class: ColumnClass::Mergeable,
    };
    LogicalSchema {
        format: LOGICAL_SCHEMA_FORMAT,
        publisher: [3; 32],
        application: "notes".into(),
        source: SchemaSource {
            kind: "test".into(),
            version: 1,
            identity: "notes".into(),
        },
        update_policy: UpdatePolicy::Changed,
        tables: vec![LogicalTable {
            sql_name: "notes".into(),
            class: TableClass::Mergeable,
            create_sql: NOTES.into(),
            auto_generate: false,
            columns: vec![
                column("key"),
                column("body"),
                LogicalColumn {
                    affinity: Affinity::Integer,
                    ..column("id")
                },
                LogicalColumn {
                    affinity: Affinity::Integer,
                    default: Some("0".into()),
                    ..column("row_kind")
                },
            ],
            primary_key: vec!["key".into()],
            partition_by: Vec::new(),
            partition_group: None,
            indexes: vec![rocky_core::LogicalIndex {
                name: "notes_identity".into(),
                unique: true,
                columns: vec![rocky_core::IndexColumn {
                    name: "id".into(),
                    descending: false,
                }],
                create_sql: INDEX.into(),
                on_conflict: None,
            }],
            foreign_keys: Vec::new(),

            existence_log: false,
        }],
        views: Vec::new(),
    }
}
fn decode(payload: &[u8]) -> Result<(String, Option<String>), Refused> {
    let (tag, rest) = payload.split_first().ok_or(Refused("empty"))?;
    let text = std::str::from_utf8(rest).map_err(|_| Refused("text"))?;
    let (id, body) = text.split_once('\n').ok_or(Refused("shape"))?;
    Ok((id.into(), (*tag == 1).then(|| body.to_string())))
}
type Entry = (Option<String>, Stamp);
impl Application for Notes {
    type State = BTreeMap<String, Entry>;
    type Error = Refused;
    type Policy = ();
    fn validate(&self, envelope: &Envelope) -> Result<(), Refused> {
        decode(&envelope.payload).map(|_| ())
    }
    fn admit(&self, envelope: &Envelope, _: &()) -> Result<(), Refused> {
        self.validate(envelope)
    }
    fn apply_ready(
        &self,
        state: &mut Self::State,
        envelope: &Envelope,
        _: AdmissionBoundary,
    ) -> Result<(), Refused> {
        let (id, body) = decode(&envelope.payload)?;
        let stamp = Stamp::from(envelope);
        if state.get(&id).is_none_or(|(_, held)| stamp > *held) {
            state.insert(id, (body, stamp));
        }
        Ok(())
    }
}
impl ProjectionSchema for Notes {
    fn installed_schema(&self) -> InstalledSchema {
        InstalledSchema {
            statements: vec![
                Sql::Borrowed(STATE),
                Sql::Borrowed(NOTES),
                Sql::Borrowed(INDEX),
            ],
            additive_indexes: Vec::new(),
            projection_query: Sql::Borrowed(
                "SELECT id,body,lamport,node,sequence FROM note_state ORDER BY id",
            ),
            checkpoint_tables: vec![Sql::Borrowed("note_state"), Sql::Borrowed("notes")],
        }
    }
}
fn fixture_id(key: &SqlValue) -> i64 {
    let SqlValue::Text(key) = key else {
        panic!("fixture key")
    };
    let sequence = key.bytes().fold(1u32, |sum, byte| {
        sum.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    rocky_core::RowIdentity::new(1, sequence).unwrap().integer()
}
fn stored(id: &str, (body, stamp): &Entry) -> Vec<SqlValue> {
    vec![
        SqlValue::Text(id.into()),
        body.clone().map_or(SqlValue::Null, SqlValue::Text),
        SqlValue::Integer(stamp.lamport.get() as i64),
        SqlValue::Blob(stamp.node.0.to_vec()),
        SqlValue::Integer(stamp.sequence.get() as i64),
    ]
}
fn statement(sql: &'static str, values: Vec<SqlValue>) -> ProjectionStatement {
    ProjectionStatement {
        sql: Sql::Borrowed(sql),
        values,
    }
}
impl StoredApplication for Notes {
    type Row = (String, String);
    fn descriptor(&self) -> ApplicationDescriptor {
        ApplicationDescriptor {
            definition: "rocky.test.notes.v1",
            operations: Versions::V1,
            projection: 1,
        }
    }
    fn manifest(&self) -> Option<rocky_core::ApplicationManifest> {
        Some(self.schema.manifest(&DOMAINS))
    }
    fn logical_schema(&self) -> Option<&LogicalSchema> {
        Some(&self.schema)
    }
    fn sql_writes(&self) -> Option<&dyn SqlWrites<Self>> {
        Some(self)
    }
    fn projection_rows(&self, state: &Self::State) -> Vec<Vec<SqlValue>> {
        state.iter().map(|(id, entry)| stored(id, entry)).collect()
    }
    fn touched(&self, operation: &Envelope) -> Vec<ProjectionStatement> {
        match decode(&operation.payload) {
            Ok((id, _)) => vec![statement(SELECT, vec![SqlValue::Text(id)])],
            Err(_) => Vec::new(),
        }
    }
    fn window(&self, rows: Vec<Vec<SqlValue>>) -> Result<Self::State, Refused> {
        let mut state = BTreeMap::new();
        for row in rows {
            let [
                SqlValue::Text(id),
                body,
                SqlValue::Integer(lamport),
                SqlValue::Blob(node),
                SqlValue::Integer(sequence),
            ] = row.as_slice()
            else {
                return Err(Refused("stored row"));
            };
            let body = match body {
                SqlValue::Text(body) => Some(body.clone()),
                _ => None,
            };
            let invalid = |_| Refused("stamp");
            let stamp = Stamp {
                lamport: rocky_core::LamportTime::new(*lamport as u64).map_err(invalid)?,
                node: NodeId(node.as_slice().try_into().map_err(|_| Refused("node"))?),
                sequence: rocky_core::AuthorSequence::new(*sequence as u64).map_err(invalid)?,
            };
            state.insert(id.clone(), (body, stamp));
        }
        Ok(state)
    }
    fn project(&self, state: &Self::State, _: &Envelope) -> Vec<ProjectionStatement> {
        let mut statements = Vec::new();
        for (id, entry) in state {
            statements.push(statement(
                "INSERT INTO note_state VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body,lamport=excluded.lamport,node=excluded.node,sequence=excluded.sequence",
                stored(id, entry),
            ));
            let id = SqlValue::Text(id.clone());
            statements.push(match &entry.0 {
                Some(body) => {
                    let skew = if self.skewed.load(Relaxed) { "!" } else { "" };
                    statement(
                        "INSERT INTO notes(key,body,id) VALUES(?,?,?) ON CONFLICT(key) DO UPDATE SET body=excluded.body",
                        vec![id.clone(), SqlValue::Text(format!("{body}{skew}")), SqlValue::Integer(fixture_id(&id))],
                    )
                }
                None => statement("DELETE FROM notes WHERE key=?", vec![id]),
            });
        }
        statements
    }
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Self::Row>, Refused> {
        Ok(self
            .window(rows)?
            .into_iter()
            .filter_map(|(id, (body, _))| Some((id, body?)))
            .collect())
    }
    fn admit_read(&self, _: Scope, _: PrincipalId, _: &()) -> Result<(), Refused> {
        Ok(())
    }
}
impl SqlWrites<Notes> for Notes {
    fn located(&self, _: &CapturedRow) -> Result<Vec<ProjectionStatement>, Refused> {
        Ok(Vec::new())
    }
    fn intent(
        &self,
        _: &BTreeMap<String, Entry>,
        row: &CapturedRow,
    ) -> Result<Option<Intent>, Refused> {
        let text = |image: &Option<Vec<SqlValue>>, column: usize| match image.as_deref() {
            Some(values) => match values.get(column) {
                Some(SqlValue::Text(text)) => Ok(Some(text.clone())),
                _ => Err(Refused("captured row")),
            },
            None => Ok(None),
        };
        let id = text(&row.before, 0)?.or(text(&row.after, 0)?);
        let Some(id) = id.filter(|_| row.before != row.after) else {
            return Ok(None);
        };
        let body = text(&row.after, 1)?;
        let mut payload = vec![if body.is_some() { 1 } else { 2 }];
        payload.extend(format!("{id}\n{}", body.unwrap_or_default()).into_bytes());
        Ok(Some(Intent {
            entity: None,
            payload,
        }))
    }
}

fn identity() -> Identity {
    Identity {
        scope: Scope {
            mesh: MeshId([1; 16]),
            app: schema().app_id(),
            domain: rocky_core::DomainId([4; 16]),
        },
        node: NodeId([1; 16]),
        caller: PrincipalId([7; 16]),
    }
}
async fn open(dir: &Path) -> (Replica<Notes>, Arc<AtomicBool>) {
    open_with(dir, RuntimeOptions::default()).await
}
async fn open_with(dir: &Path, options: RuntimeOptions) -> (Replica<Notes>, Arc<AtomicBool>) {
    let skewed = Arc::new(AtomicBool::new(false));
    let app = Notes {
        schema: schema(),
        skewed: skewed.clone(),
    };
    let config = RuntimeConfig {
        authors: vec![(identity().node, identity().caller)],
        ..RuntimeConfig::default()
    };
    let path = dir.join("notes.db");
    let replica = Replica::open_with_options(path, app, (), identity(), config, options)
        .await
        .unwrap();
    (replica, skewed)
}
pub(crate) async fn run(session: &SqlSession, sql: &str) -> SqlRows {
    session
        .run(sql, Vec::new())
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}
async fn notes(replica: &Replica<Notes>) -> Vec<(String, String)> {
    replica.query().await.unwrap().rows
}
pub(crate) fn note(id: &str, body: &str) -> (String, String) {
    (id.into(), body.into())
}
const BODY: [&str; 2] = [
    "INSERT INTO notes(key,body,id) VALUES('b','second',4294967425)",
    "UPDATE notes SET body='first, edited' WHERE key='a'",
];
const TOKEN: TransactionToken = TransactionToken([9; 32]);

#[tokio::test]
async fn a_failed_commit_step_or_commit_publishes_nothing() {
    let dir = TempDir::new().unwrap();
    let (replica, skewed) = open(dir.path()).await;
    let session = replica.sql_session(SqlOptions::default()).await.unwrap();
    let first = run(
        &session,
        "INSERT INTO notes(key,body,id) VALUES('a','first',4294967424)",
    )
    .await;
    assert_eq!(first.committed.unwrap().operations.len(), 1);
    let mut revisions = replica.inner.revisions.clone();
    let mut changes = replica.inner.changes.clone();
    let revision = *revisions.borrow_and_update();
    changes.borrow_and_update();

    // Protocol insertion fails, the engine's commit fails, the projection
    // disagrees with the transaction: each time nothing is kept or seen.
    for case in 0..3 {
        match case {
            0 => crate::tests::fault(&replica, worker::Fault::FailBeforeCommit).await,
            1 => crate::tests::fault(&replica, worker::Fault::FailCommit).await,
            _ => skewed.store(true, Relaxed),
        }
        run(&session, "BEGIN").await;
        for sql in BODY {
            run(&session, sql).await;
        }
        // The session reads its provisional rows until the commit fails.
        assert_eq!(
            run(&session, "SELECT count(*) FROM notes").await.rows.len(),
            1
        );
        let failed = session.commit(TOKEN).await.unwrap_err();
        match case {
            2 => assert!(
                matches!(failed, Error::Sql(SqlError::Capture(_))),
                "{failed}"
            ),
            _ => assert!(matches!(failed, Error::InjectedFailure), "{failed}"),
        }
        assert_eq!(failed.sql_certainty(), Certainty::Definite);
        skewed.store(false, Relaxed);
        assert_eq!(notes(&replica).await, [note("a", "first")]);
        assert_eq!(
            run(&session, "SELECT key,body FROM notes").await.rows,
            [[SqlValue::Text("a".into()), SqlValue::Text("first".into())]]
        );
        assert_eq!(replica.sql_outcome(TOKEN).await.unwrap(), None);
        assert!(!revisions.has_changed().unwrap() && !changes.has_changed().unwrap());
        assert_eq!(replica.query().await.unwrap().revision, revision);
        // A lone statement fails the same way and leaves no transaction.
        if case == 0 {
            crate::tests::fault(&replica, worker::Fault::FailBeforeCommit).await;
            let lone = session.run(BODY[0], Vec::new()).await.unwrap_err();
            assert!(matches!(lone, Error::InjectedFailure), "{lone}");
            assert_eq!(run(&session, "SELECT 1").await.state, SqlState::Idle);
        }
    }
    // The same body then commits once, with the next sequences.
    run(&session, "BEGIN").await;
    for sql in BODY {
        run(&session, sql).await;
    }
    let outcome = session.commit(TOKEN).await.unwrap().committed.unwrap();
    let sequences: Vec<_> = outcome
        .operations
        .iter()
        .map(|id| id.sequence.get())
        .collect();
    assert_eq!(sequences, [2, 3]);
    assert_eq!(*revisions.borrow_and_update(), outcome.revision);
    assert_eq!(
        notes(&replica).await,
        [note("a", "first, edited"), note("b", "second")]
    );
    replica.checkpoint().await.unwrap();
    assert!(replica.audit().await.unwrap().recomputed);
}

/// Does nothing in an ordinary run; a parent test selects where to die.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_child() {
    let (Ok(case), Ok(dir)) = (std::env::var(CASE), std::env::var(DIR)) else {
        return;
    };
    let (replica, _) = open(Path::new(&dir)).await;
    let session = replica.sql_session(SqlOptions::default()).await.unwrap();
    run(
        &session,
        "INSERT INTO notes(key,body,id) VALUES('a','first',4294967424)",
    )
    .await;
    match case.as_str() {
        "open" => {}
        // After the step released its savepoint, before the engine's commit.
        "sealed" => crate::tests::fault(&replica, worker::Fault::AbortBeforeCommit).await,
        // Committed and flushed, before the caller is answered.
        "committed" => crate::tests::fault(&replica, worker::Fault::AbortAfterCommit).await,
        // Inside the commit step, after its protocol rows were written.
        "writing" => crate::tests::fault(&replica, worker::Fault::AbortAfterWrites(1)).await,
        other => panic!("unknown crash case {other}"),
    }
    run(&session, "BEGIN").await;
    for sql in BODY {
        run(&session, sql).await;
    }
    if case == "open" {
        // The transaction is still open when the process dies.
        std::process::abort();
    }
    let _ = session.commit(TOKEN).await;
    panic!("the injected fault did not end the process");
}
async fn crashed(case: &str) -> (TempDir, Replica<Notes>) {
    let dir = TempDir::new().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["sql_tests::crash_child", "--exact", "--test-threads=1"])
        .env(CASE, case)
        .env(DIR, dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(6), "{case}: {status:?}");
    let (replica, _) = open(dir.path()).await;
    (dir, replica)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_death_before_the_commit_leaves_no_transaction() {
    for case in ["open", "writing", "sealed"] {
        let (_dir, replica) = crashed(case).await;
        // Reopening validated counters and digests against the history.
        assert_eq!(notes(&replica).await, [note("a", "first")], "{case}");
        assert_eq!(replica.sql_outcome(TOKEN).await.unwrap(), None, "{case}");
        // The token is unused, so the caller runs the body again: once.
        let session = replica.sql_session(SqlOptions::default()).await.unwrap();
        run(&session, "BEGIN").await;
        for sql in BODY {
            run(&session, sql).await;
        }
        let outcome = session.commit(TOKEN).await.unwrap().committed.unwrap();
        assert_eq!((outcome.operations.len(), outcome.replayed), (2, false));
        assert_eq!(
            notes(&replica).await,
            [note("a", "first, edited"), note("b", "second")]
        );
        replica.checkpoint().await.unwrap();
        assert!(replica.audit().await.unwrap().recomputed, "{case}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lost_acknowledgment_is_recovered_by_its_token_once() {
    let (_dir, replica) = crashed("committed").await;
    let expected = [note("a", "first, edited"), note("b", "second")];
    assert_eq!(notes(&replica).await, expected);
    let outcome = replica.sql_outcome(TOKEN).await.unwrap().unwrap();
    let sequences: Vec<_> = outcome
        .operations
        .iter()
        .map(|id| id.sequence.get())
        .collect();
    assert_eq!((sequences, outcome.replayed), (vec![2, 3], true));
    assert_eq!(outcome.class, CommitClass::Flushed);
    for id in &outcome.operations {
        let stored = replica
            .lookup_mutation(operation_handle(*id))
            .await
            .unwrap();
        assert_eq!(stored.map(|operation| operation.id), Some(*id));
    }
    // Sending the commit again under the token changes nothing.
    let session = replica.sql_session(SqlOptions::default()).await.unwrap();
    run(&session, "BEGIN").await;
    run(&session, "UPDATE notes SET body='other' WHERE key='a'").await;
    let conflict = session.commit(TOKEN).await.unwrap_err();
    assert!(matches!(conflict, Error::Conflict), "{conflict}");
    assert_eq!(notes(&replica).await, expected);
    assert_eq!(replica.query().await.unwrap().revision, outcome.revision);
    replica.checkpoint().await.unwrap();
    assert!(replica.audit().await.unwrap().recomputed);
}

/// A waiting write of another domain in the file commits when the lease
/// ends, and requests that write nothing are answered meanwhile.
#[tokio::test]
async fn a_lease_delays_other_writes_by_at_most_its_timeout() {
    let dir = TempDir::new().unwrap();
    let (replica, _) = open(dir.path()).await;
    let timeout = std::time::Duration::from_millis(80);
    let session = replica
        .sql_session(SqlOptions {
            transaction_timeout: timeout,
            ..SqlOptions::default()
        })
        .await
        .unwrap();
    let other = replica.sql_session(SqlOptions::default()).await.unwrap();
    run(&session, "BEGIN IMMEDIATE").await;
    run(
        &session,
        "INSERT INTO notes(key,body,id) VALUES('held','lease',4299089590)",
    )
    .await;
    let started = std::time::Instant::now();
    // The write waits behind the lease; the read does not.
    let (waited, read) = tokio::join!(
        other.run(
            "INSERT INTO notes(key,body,id) VALUES('w','waited',4294967446)",
            Vec::new()
        ),
        replica.lookup_mutation(MutationHandle([1; 32]))
    );
    assert_eq!(read.unwrap(), None);
    assert_eq!(waited.unwrap().committed.unwrap().operations.len(), 1);
    assert!(started.elapsed() < timeout * 20);
    assert_eq!(notes(&replica).await, [note("w", "waited")]);
    let stats = replica.writer_stats();
    let held = stats.lease_max_micros;
    assert!((70_000..2_000_000).contains(&held), "{stats:?}");
    assert!(matches!(
        session.run("SELECT 1", Vec::new()).await,
        Err(Error::Sql(SqlError::Aborted(AbortReason::Timeout)))
    ));
}

/// Token records leave with the mutation handles of their time, so a file
/// that tokens every commit does not grow without bound.
#[tokio::test]
async fn token_records_are_pruned_with_the_handles_of_their_time() {
    let dir = TempDir::new().unwrap();
    let options = RuntimeOptions {
        retention: RetentionPolicy {
            window: 0,
            handles: 2,
            automatic: false,
            ..RetentionPolicy::default()
        },
        ..RuntimeOptions::default()
    };
    let (replica, _) = open_with(dir.path(), options).await;
    let session = replica.sql_session(SqlOptions::default()).await.unwrap();
    let token = |value: u8| TransactionToken([value; 32]);
    let insert = session
        .prepare("INSERT INTO notes(key,body,id) VALUES(?,'n',?)")
        .await
        .unwrap();
    let commit = async |value: u8| {
        let key = SqlValue::Text(format!("n{value}"));
        let id = vec![key.clone(), SqlValue::Integer(fixture_id(&key))];
        let rows = session.execute_with_token(&insert, id, token(value));
        rows.await.unwrap().committed.unwrap()
    };
    for value in 1..=5 {
        assert_eq!(commit(value).await.operations.len(), 1);
    }
    // A tokened transaction without an operation covers the sequence before it.
    run(&session, "BEGIN").await;
    run(&session, "UPDATE notes SET body=body WHERE key='n1'").await;
    let empty = session.commit(token(6)).await.unwrap().committed.unwrap();
    assert!(empty.operations.is_empty());
    let recorded = async |value: u8| replica.sql_outcome(token(value)).await.unwrap().is_some();
    // Nothing goes before the history behind it is pruned.
    for value in 1..=6 {
        assert!(recorded(value).await, "{value}");
    }
    replica.checkpoint().await.unwrap();
    replica.prune().await.unwrap();
    assert_eq!(
        replica
            .retention_status()
            .await
            .unwrap()
            .retained_operations,
        0
    );
    // The two newest sequences keep their handles and their tokens.
    let kept = [false, false, false, true, true, true];
    for (value, kept) in (1..=6).zip(kept) {
        assert_eq!(recorded(value).await, kept, "{value}");
    }
    let stored = replica.lookup_mutation(operation_handle(empty_before(&replica).await));
    assert!(matches!(stored.await, Err(Error::Pruned { .. })));
    // A forgotten token is unknown: a commit under it is a new commit.
    let again = session.execute_with_token(
        &insert,
        vec![
            SqlValue::Text("again".into()),
            SqlValue::Integer(fixture_id(&SqlValue::Text("again".into()))),
        ],
        token(1),
    );
    let again = again.await.unwrap().committed.unwrap();
    assert_eq!((again.operations.len(), again.replayed), (1, false));
    // The record survives pruning inside the window and a restart.
    drop(session);
    replica.close().await.unwrap();
    let (replica, _) = open_with(dir.path(), options).await;
    assert!(replica.sql_outcome(token(5)).await.unwrap().is_some());
    replica.checkpoint().await.unwrap();
    assert!(replica.audit().await.unwrap().recomputed);
}
/// The newest local operation, whose handle is inside the retry window.
async fn empty_before(replica: &Replica<Notes>) -> OperationId {
    let frontier = replica.query().await.unwrap().frontier;
    let (node, sequence) = frontier.0[0];
    OperationId { node, sequence }
}
