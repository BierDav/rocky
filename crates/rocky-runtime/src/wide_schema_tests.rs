//! Wide-schema bootstrap, additive indexes and persistent explicit placement.
use super::*;
use crate::scaling_option_tests::NODE;
use inventory_app::{InventoryApp, Policy};
use rocky_core::*;
use tempfile::TempDir;

pub(crate) fn note(replica: &Replica<WideApp>, handle: u8, text: &str) -> MutationReceipt {
    replica
        .mutate_with_handle(
            MutationHandle([handle; 32]),
            Mutation {
                entity: None,
                payload: text.as_bytes().to_vec(),
            },
        )
        .unwrap()
}
#[derive(Clone, Copy)]
pub(crate) struct WideApp {
    schema: &'static [&'static str],
    indexes: &'static [&'static str],
    domain: Option<u16>,
}
pub(crate) fn app(tables: usize) -> WideApp {
    let mut schema = InventoryApp.schema().to_vec();
    for index in 0..tables {
        schema.push(Box::leak(
            format!("CREATE TABLE extra_{index}(value INTEGER)").into_boxed_str(),
        ));
    }
    WideApp {
        schema: Box::leak(schema.into_boxed_slice()),
        indexes: &[],
        domain: None,
    }
}
pub(crate) fn test_scope(domain: u16) -> Scope {
    let mut bytes = [0; 16];
    bytes[..2].copy_from_slice(&domain.to_be_bytes());
    Scope {
        mesh: MeshId([11; 16]),
        app: inventory_app::app_id(),
        domain: DomainId(bytes),
    }
}
pub(crate) fn installation(mut app: WideApp, domain: u16) -> DomainInstallation<WideApp> {
    if app.domain.is_some() {
        app.domain = Some(domain);
    }
    let scope = test_scope(domain);
    let caller = PrincipalId([7; 16]);
    DomainInstallation {
        app,
        policy: Policy {
            scope,
            caller,
            reference: [9; 32],
            note_prefix: None,
        },
        identity: Identity {
            scope,
            node: NODE,
            caller,
        },
        config: RuntimeConfig {
            authors: vec![(NODE, caller)],
            delivery_targets: vec![],
            full_nodes: Vec::new(),
        },
    }
}
fn placement(count: u16) -> Vec<DomainPlacement> {
    (0..count)
        .map(|domain| DomainPlacement {
            scope: test_scope(domain),
            file: (domain as usize * 7 + domain as usize / 7) % 4,
        })
        .collect()
}
pub(crate) fn options() -> RuntimeOptions {
    RuntimeOptions {
        files: 4,
        ..RuntimeOptions::default()
    }
}
impl Application for WideApp {
    type State = <InventoryApp as Application>::State;
    type Error = <InventoryApp as Application>::Error;
    type Policy = Policy;
    fn validate(&self, op: &Envelope) -> Result<(), Self::Error> {
        Application::validate(&InventoryApp, op)
    }
    fn admit(&self, op: &Envelope, policy: &Policy) -> Result<(), Self::Error> {
        Application::admit(&InventoryApp, op, policy)
    }
    fn apply_ready(
        &self,
        state: &mut Self::State,
        op: &Envelope,
        boundary: AdmissionBoundary,
    ) -> Result<(), Self::Error> {
        InventoryApp.apply_ready(state, op, boundary)
    }
}
impl CompiledSchema for WideApp {
    fn schema(&self) -> &'static [&'static str] {
        self.schema
    }
    fn additive_indexes(&self) -> &'static [&'static str] {
        self.indexes
    }
    fn projection_query(&self) -> &'static str {
        InventoryApp.projection_query()
    }
    fn checkpoint_tables(&self) -> &'static [&'static str] {
        InventoryApp.checkpoint_tables()
    }
}
impl StoredApplication for WideApp {
    type Row = <InventoryApp as StoredApplication>::Row;
    fn descriptor(&self) -> ApplicationDescriptor {
        InventoryApp.descriptor()
    }
    fn manifest(&self) -> Option<ApplicationManifest> {
        let mut manifest = inventory_app::manifest();
        let domains: Vec<_> = self
            .domain
            .map_or_else(|| (0..256).collect(), |domain| vec![domain]);
        manifest.domains = domains
            .into_iter()
            .map(|domain| DomainRule {
                domain: test_scope(domain).domain,
                author_log: AuthorLogScope::Domain,
                rule: "probe".into(),
            })
            .collect();
        Some(manifest)
    }

    fn projection_rows(&self, state: &Self::State) -> Vec<Vec<SqlValue>> {
        InventoryApp.projection_rows(state)
    }
    fn touched(&self, op: &Envelope) -> Vec<ProjectionStatement> {
        InventoryApp.touched(op)
    }
    fn window(&self, rows: Vec<Vec<SqlValue>>) -> Result<Self::State, Self::Error> {
        InventoryApp.window(rows)
    }
    fn project(&self, state: &Self::State, op: &Envelope) -> Vec<ProjectionStatement> {
        InventoryApp.project(state, op)
    }
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Self::Row>, Self::Error> {
        InventoryApp.decode_rows(rows)
    }
    fn read_views(
        &self,
        context: AdmissionContext,
        policy: &Policy,
    ) -> Result<Vec<ReadView>, Self::Error> {
        InventoryApp.read_views(context, policy)
    }
    fn admit_read(
        &self,
        scope: Scope,
        caller: PrincipalId,
        policy: &Policy,
    ) -> Result<(), Self::Error> {
        InventoryApp.admit_read(scope, caller, policy)
    }
}

#[tokio::test]
async fn placement_extends_without_moving_existing_domains_and_opens_selected_files() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("placed.db");
    let app = WideApp {
        domain: Some(0),
        ..app(0)
    };
    let runtime = Runtime::open_placed(&path, vec![installation(app, 0)], placement(1), options())
        .await
        .unwrap();
    note(&runtime.domain(test_scope(0)).unwrap(), 1, "original")
        .committed()
        .await
        .unwrap();
    runtime.close().await.unwrap();
    let runtime = Runtime::open_placed(
        &path,
        (0..256).map(|domain| installation(app, domain)).collect(),
        placement(256),
        options(),
    )
    .await
    .unwrap();
    let pending: Vec<_> = (1..256)
        .map(|domain| {
            note(
                &runtime.domain(test_scope(domain)).unwrap(),
                1,
                &format!("domain-{domain}"),
            )
        })
        .collect();
    for receipt in pending {
        receipt.committed().await.unwrap();
    }
    // Query independent domains across the four file workers in bounded batches.
    for first in (0..256).step_by(8) {
        let mut reads = tokio::task::JoinSet::new();
        for domain in first..first + 8 {
            let replica = runtime.domain(test_scope(domain)).unwrap();
            reads.spawn(async move {
                let snapshot = replica.query().await.unwrap();
                assert_eq!(snapshot.rows.len(), 1);
                assert_eq!(
                    snapshot.rows[0].1,
                    if domain == 0 {
                        "original".into()
                    } else {
                        format!("domain-{domain}")
                    }
                );
                assert_eq!(snapshot.revision.get(), 1);
                assert!(matches!(
                    replica
                        .read(
                            BoundQuery {
                                sql: "SELECT * FROM drafts".into(),
                                values: vec![]
                            },
                            ReadOptions::new(test_scope((domain + 1) % 256), 10)
                        )
                        .await,
                    Err(Error::Protocol(ProtocolError::ForeignScope))
                ));
            });
        }
        while let Some(result) = reads.join_next().await {
            result.unwrap();
        }
    }
    assert!(runtime.domain(test_scope(256)).is_err());
    runtime.close().await.unwrap();
    // The second process serves file 3 while file 0 is locked by another owner.
    let (_owner, _conn) = crate::database::DatabaseOwner::open(&path).await.unwrap();
    let runtime =
        Runtime::open_placed(&path, vec![installation(app, 1)], placement(256), options())
            .await
            .unwrap();
    assert_eq!(
        runtime
            .domain(test_scope(1))
            .unwrap()
            .query()
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    assert!(runtime.domain(test_scope(0)).is_err());
    runtime.close().await.unwrap();
    let mut moved = placement(256);
    moved[1].file = 2;
    assert!(matches!(
        Runtime::open_placed(&path, vec![installation(app, 1)], moved, options()).await,
        Err(Error::Incompatible(_))
    ));
    assert!(matches!(
        Runtime::open_placed(
            &path,
            vec![installation(app, 256)],
            placement(256),
            options()
        )
        .await,
        Err(Error::Incompatible(_))
    ));
}

#[tokio::test]
async fn additive_indexes_preserve_rows_and_definition_and_refuse_edits_or_removal() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("indexes.db");
    let base = app(0);
    let indexed = WideApp {
        indexes: &["CREATE INDEX draft_note ON drafts(note)"],
        ..base
    };
    assert_eq!(installed_definition(&base), installed_definition(&indexed));
    let runtime = Runtime::open(&path, vec![installation(base, 0)])
        .await
        .unwrap();
    note(&runtime.domain(test_scope(0)).unwrap(), 1, "kept")
        .committed()
        .await
        .unwrap();
    runtime.close().await.unwrap();
    let runtime = Runtime::open(&path, vec![installation(indexed, 0)])
        .await
        .unwrap();
    assert_eq!(
        runtime
            .domain(test_scope(0))
            .unwrap()
            .query()
            .await
            .unwrap()
            .rows
            .len(),
        1
    );
    runtime.close().await.unwrap();
    let conn = crate::sqlite::Connection::open_read_only(&path).unwrap();
    assert_eq!(
        conn.rows(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='index' AND name LIKE '%_draft_note'",
            ()
        )
        .unwrap(),
        vec![vec![crate::sqlite::Value::Integer(1)]]
    );
    drop(conn);
    let changed = WideApp {
        indexes: &["CREATE INDEX draft_note ON drafts(lamport)"],
        ..base
    };
    let unique = WideApp {
        indexes: &["CREATE UNIQUE INDEX unique_note ON drafts(note)"],
        ..indexed
    };
    for app in [base, changed, unique] {
        assert!(matches!(
            Runtime::open(&path, vec![installation(app, 0)]).await,
            Err(Error::Incompatible(_))
        ));
    }
    Runtime::open(&path, vec![installation(indexed, 0)])
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
}

#[tokio::test]
async fn bootstrap_child() {
    let Ok(path) = std::env::var("ROCKY_WIDE_CRASH_PATH") else {
        return;
    };
    match std::env::var("ROCKY_WIDE_CRASH_MODE").as_deref() {
        Ok("placed-complete") => {
            crate::placement::ABORT_BEFORE_READY.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        _ => {
            crate::bootstrap::ABORT_AFTER_STEP.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if std::env::var("ROCKY_WIDE_CRASH_MODE").as_deref() == Ok("unscoped") {
        let domain = installation(app(100), 0);
        let _ = Replica::open_with_config(
            path,
            domain.app,
            domain.policy,
            domain.identity,
            domain.config,
        )
        .await;
    } else if std::env::var("ROCKY_WIDE_CRASH_MODE").is_ok() {
        let _ = Runtime::open_placed(
            path,
            vec![installation(app(100), 0)],
            vec![DomainPlacement {
                scope: test_scope(0),
                file: 3,
            }],
            options(),
        )
        .await;
    } else {
        let _ = Runtime::open(path, vec![installation(app(100), 0)]).await;
    }
    panic!("bootstrap did not abort");
}
#[tokio::test]
async fn interrupted_schema_resumes_committed_steps_and_never_opens_half_installed() {
    use std::os::unix::process::ExitStatusExt;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("wide.db");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "wide_schema_tests::bootstrap_child",
            "--exact",
            "--test-threads=1",
        ])
        .env("ROCKY_WIDE_CRASH_PATH", &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(6));
    let conn = crate::sqlite::Connection::open_read_only(&path).unwrap();
    assert_eq!(
        conn.rows("SELECT position FROM rocky_bootstrap", ())
            .unwrap(),
        vec![vec![crate::sqlite::Value::Integer(64)]]
    );
    assert!(
        conn.rows("SELECT binding FROM rocky_runtime", ())
            .unwrap()
            .is_empty()
    );
    drop(conn);
    let runtime = Runtime::open(&path, vec![installation(app(100), 0)])
        .await
        .unwrap();
    note(&runtime.domain(test_scope(0)).unwrap(), 1, "resumed")
        .committed()
        .await
        .unwrap();
    runtime.close().await.unwrap();
    let conn = crate::sqlite::Connection::open_read_only(&path).unwrap();
    assert_eq!(
        conn.rows(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name LIKE '%_extra_%' AND type='table'",
            ()
        )
        .unwrap(),
        vec![vec![crate::sqlite::Value::Integer(200)]]
    );
    assert_eq!(
        conn.rows(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name='rocky_bootstrap'",
            ()
        )
        .unwrap(),
        vec![vec![crate::sqlite::Value::Integer(0)]]
    );
}

#[tokio::test]
async fn a_failed_ddl_step_rolls_back_and_a_different_resume_plan_is_refused() {
    let raw = crate::sqlite::Connection::open_in_memory().unwrap();
    let conn = crate::scoped_connection::ScopedConnection::new(raw, None, &[""; 0]);
    let mut plan: Vec<_> = (0..70)
        .map(|index| (format!("CREATE TABLE t{index}(value)"), vec![]))
        .collect();
    plan[68].0 = "CREATE TABLE broken(".into();
    assert!(
        crate::bootstrap::install(&conn, b"binding", plan.clone())
            .await
            .is_err()
    );
    assert_eq!(
        crate::storage::rows(&conn, "SELECT position FROM rocky_bootstrap")
            .await
            .unwrap(),
        vec![vec![SqlValue::Integer(64)]]
    );
    assert_eq!(
        crate::storage::rows(&conn, "SELECT COUNT(*) FROM sqlite_schema WHERE name='t64'")
            .await
            .unwrap(),
        vec![vec![SqlValue::Integer(0)]]
    );
    plan[68].0 = "CREATE TABLE t68(value)".into();
    assert!(matches!(
        crate::bootstrap::install(&conn, b"binding", plan).await,
        Err(Error::Incompatible(_))
    ));
}

#[tokio::test]
async fn placed_bootstrap_resumes_before_and_after_the_file_completion_marker() {
    use std::os::unix::process::ExitStatusExt;
    for mode in ["placed-partial", "placed-complete"] {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("wide.db");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "wide_schema_tests::bootstrap_child",
                "--exact",
                "--test-threads=1",
            ])
            .env("ROCKY_WIDE_CRASH_PATH", &path)
            .env("ROCKY_WIDE_CRASH_MODE", mode)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(6));
        assert!(!path.exists());
        let runtime = Runtime::open_placed(
            &path,
            vec![installation(app(100), 0)],
            vec![DomainPlacement {
                scope: test_scope(0),
                file: 3,
            }],
            options(),
        )
        .await
        .unwrap();
        note(&runtime.domain(test_scope(0)).unwrap(), 1, mode)
            .committed()
            .await
            .unwrap();
        runtime.close().await.unwrap();
        let file = crate::layout::file_path(&path, 3);
        std::fs::remove_file(file).unwrap();
        assert!(matches!(
            Runtime::open_placed(
                &path,
                vec![installation(app(100), 0)],
                vec![DomainPlacement {
                    scope: test_scope(0),
                    file: 3
                }],
                options()
            )
            .await,
            Err(Error::Incompatible(_))
        ));
    }
}

#[tokio::test]
async fn unscoped_wide_bootstrap_resumes_without_admitting_partial_identity() {
    use std::os::unix::process::ExitStatusExt;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("unscoped.db");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "wide_schema_tests::bootstrap_child",
            "--exact",
            "--test-threads=1",
        ])
        .env("ROCKY_WIDE_CRASH_PATH", &path)
        .env("ROCKY_WIDE_CRASH_MODE", "unscoped")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(6));
    let conn = crate::sqlite::Connection::open_read_only(&path).unwrap();
    assert!(
        conn.rows("SELECT identity FROM rocky_replica", ())
            .unwrap()
            .is_empty()
    );
    drop(conn);
    let domain = installation(app(100), 0);
    let replica = Replica::open_with_config(
        &path,
        domain.app,
        domain.policy,
        domain.identity,
        domain.config,
    )
    .await
    .unwrap();
    note(&replica, 1, "unscoped").committed().await.unwrap();
    assert_eq!(replica.query().await.unwrap().rows[0].1, "unscoped");
    replica.close().await.unwrap();
}

#[tokio::test]
async fn repeated_checkpoint_schema_keeps_base_unique_constraints() {
    let conn = crate::sqlite::Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE items(value INTEGER)", ())
        .await
        .unwrap();
    let sql =
        crate::bootstrap::idempotent("CREATE UNIQUE INDEX distinct_value ON items(value)").unwrap();
    conn.execute(&sql, ()).await.unwrap();
    conn.execute(&sql, ()).await.unwrap();
    conn.execute("INSERT INTO items VALUES(1)", ())
        .await
        .unwrap();
    assert!(
        conn.execute("INSERT INTO items VALUES(1)", ())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn additive_index_does_not_adopt_an_unrecorded_physical_index() {
    let base = app(0);
    let raw = crate::sqlite::Connection::open_in_memory().unwrap();
    let conn = crate::scoped_connection::ScopedConnection::new(raw, None, base.schema());
    for sql in base.schema() {
        conn.execute(*sql, ()).await.unwrap();
    }
    conn.execute("CREATE INDEX draft_note ON drafts(lamport)", ())
        .await
        .unwrap();
    let indexed = WideApp {
        indexes: &["CREATE INDEX draft_note ON drafts(note)"],
        ..base
    };
    conn.execute("BEGIN IMMEDIATE", ()).await.unwrap();
    assert!(matches!(
        crate::schema_evolution::apply(&conn, &indexed).await,
        Err(Error::Incompatible(_))
    ));
    conn.execute("ROLLBACK", ()).await.unwrap();
}
