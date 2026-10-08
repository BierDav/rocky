//! Stored rows are the state: a transition loads only what it touches, and
//! open no longer needs application state in memory.
use super::*;
use inventory_app::{InventoryApp, Policy};
use rocky_core::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

const NODE: NodeId = NodeId([3; 16]);
const CALLER: PrincipalId = PrincipalId([7; 16]);

fn scope() -> Scope {
    scope_in(DomainId([1; 16]))
}
fn scope_in(domain: DomainId) -> Scope {
    Scope {
        mesh: MeshId([11; 16]),
        app: inventory_app::app_id(),
        domain,
    }
}
/// The inventory app, recording the most rows any window held and how many
/// windows were built.
#[derive(Clone, Default)]
struct Watched {
    widest: Arc<AtomicUsize>,
    windows: Arc<AtomicUsize>,
    /// Declares the drafts table `WITHOUT ROWID`.
    rowless: bool,
}
impl Application for Watched {
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
impl CompiledSchema for Watched {
    fn schema(&self) -> &'static [&'static str] {
        if self.rowless {
            &[
                "CREATE TABLE drafts(node BLOB,sequence INTEGER,note TEXT,lamport INTEGER,stamp_node BLOB,stamp_sequence INTEGER,PRIMARY KEY(node,sequence)) WITHOUT ROWID",
                "CREATE TABLE stock(id INTEGER PRIMARY KEY,remaining INTEGER NOT NULL,accepted INTEGER NOT NULL)",
                "CREATE TABLE jobs(node BLOB,sequence INTEGER,quantity INTEGER,PRIMARY KEY(node,sequence))",
            ]
        } else {
            InventoryApp.schema()
        }
    }
    fn projection_query(&self) -> &'static str {
        InventoryApp.projection_query()
    }
    fn checkpoint_tables(&self) -> &'static [&'static str] {
        InventoryApp.checkpoint_tables()
    }
}
impl StoredApplication for Watched {
    type Row = <InventoryApp as StoredApplication>::Row;
    fn descriptor(&self) -> ApplicationDescriptor {
        InventoryApp.descriptor()
    }
    fn manifest(&self) -> Option<ApplicationManifest> {
        InventoryApp.manifest()
    }
    fn projection_rows(&self, state: &Self::State) -> Vec<Vec<SqlValue>> {
        InventoryApp.projection_rows(state)
    }
    fn touched(&self, op: &Envelope) -> Vec<ProjectionStatement> {
        InventoryApp.touched(op)
    }
    fn window(&self, rows: Vec<Vec<SqlValue>>) -> Result<Self::State, Self::Error> {
        self.widest.fetch_max(rows.len(), Ordering::Relaxed);
        self.windows.fetch_add(1, Ordering::Relaxed);
        InventoryApp.window(rows)
    }
    fn project(&self, state: &Self::State, op: &Envelope) -> Vec<ProjectionStatement> {
        InventoryApp.project(state, op)
    }
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Self::Row>, Self::Error> {
        InventoryApp.decode_rows(rows)
    }
    fn admit_read(
        &self,
        scope: Scope,
        caller: PrincipalId,
        policy: &Policy,
    ) -> Result<(), Self::Error> {
        InventoryApp.admit_read(scope, caller, policy)
    }
    fn read_views(
        &self,
        context: AdmissionContext,
        policy: &Policy,
    ) -> Result<Vec<ReadView>, Self::Error> {
        InventoryApp.read_views(context, policy)
    }
}
async fn open(
    dir: &TempDir,
    node: NodeId,
    options: RuntimeOptions,
) -> Result<Runtime<Watched>, Error> {
    open_app(dir, node, options, Watched::default()).await
}
async fn open_app(
    dir: &TempDir,
    node: NodeId,
    options: RuntimeOptions,
    app: Watched,
) -> Result<Runtime<Watched>, Error> {
    open_domains(dir, node, options, app, &[scope()]).await
}
async fn open_domains(
    dir: &TempDir,
    node: NodeId,
    options: RuntimeOptions,
    app: Watched,
    scopes: &[Scope],
) -> Result<Runtime<Watched>, Error> {
    let installations = scopes
        .iter()
        .map(|&scope| DomainInstallation {
            app: app.clone(),
            policy: Policy {
                scope,
                caller: CALLER,
                reference: [9; 32],
                note_prefix: None,
            },
            identity: Identity {
                scope,
                node,
                caller: CALLER,
            },
            config: RuntimeConfig {
                authors: vec![(NODE, CALLER), (NodeId([4; 16]), CALLER)],
                delivery_targets: Vec::new(),
                full_nodes: Vec::new(),
            },
        })
        .collect();
    let name = format!("{}.db", node.0[0]);
    Runtime::open_with_options(dir.path().join(name), installations, options).await
}
async fn write(replica: &Replica<Watched>, entity: Option<OperationId>, text: &str) -> Envelope {
    replica
        .mutate(Mutation {
            entity,
            payload: text.as_bytes().to_vec(),
        })
        .unwrap()
        .committed()
        .await
        .unwrap()
}
async fn count(replica: &Replica<Watched>) -> i64 {
    count_in(replica, scope()).await
}
async fn count_in(replica: &Replica<Watched>, scope: Scope) -> i64 {
    let query = BoundQuery {
        sql: "SELECT COUNT(*) FROM drafts".into(),
        values: Vec::new(),
    };
    match replica
        .read(query, ReadOptions::new(scope, 1))
        .await
        .unwrap()
        .rows[0][0]
    {
        SqlValue::Integer(count) => count,
        _ => panic!("count"),
    }
}

/// Local and incoming transitions are reduced on the one row they touch,
/// whatever the domain holds, and open builds no window at all.
#[tokio::test]
async fn transitions_load_only_the_rows_they_touch() {
    let dir = TempDir::new().unwrap();
    let app = Watched::default();
    let open = |node| open_app(&dir, node, RuntimeOptions::default(), app.clone());
    let source = open(NODE).await.unwrap();
    let a = source.domain(scope()).unwrap();
    let mut operations = Vec::new();
    let first = write(&a, None, "first").await;
    operations.push(first.clone());
    for index in 0..40 {
        operations.push(write(&a, None, &format!("note-{index}")).await);
    }
    operations.push(write(&a, Some(first.id), "edited").await);
    let target = open(NodeId([4; 16])).await.unwrap();
    let b = target.domain(scope()).unwrap();
    for operation in &operations {
        b.receive(operation.encode().unwrap()).await.unwrap();
    }
    // Create and edit of one row meet in passes of 32 operations.
    b.process_pending().await.unwrap();
    assert_eq!(app.widest.load(Ordering::Relaxed), 1);
    // One window per transition: 42 local and 42 incoming.
    assert_eq!(app.windows.load(Ordering::Relaxed), 84);
    assert_eq!((count(&a).await, count(&b).await), (41, 41));
    assert_eq!(b.query().await.unwrap().rows, a.query().await.unwrap().rows);
    source.close().await.unwrap();
    target.close().await.unwrap();
    // Within the quota open audits by replay, which needs no window either.
    let source = open(NODE).await.unwrap();
    assert_eq!(app.windows.load(Ordering::Relaxed), 84);
    let next = write(&source.domain(scope()).unwrap(), None, "after reopen").await;
    assert_eq!((next.id.sequence.get(), next.lamport.get()), (43, 43));
    source.close().await.unwrap();
}

/// Open does not replay history: the explicit audit finds a tampered row.
/// With default options a domain grows past the former operation quota,
/// checkpoints and prunes on its own, and reopens from the checkpoint.
#[tokio::test]
async fn open_does_not_replay_and_a_domain_outgrows_the_former_quota() {
    let dir = TempDir::new().unwrap();
    let options = RuntimeOptions::default();
    let edit = async |note: &str| {
        let db = crate::sqlite::Connection::open(&dir.path().join("3.db")).unwrap();
        let conn = crate::scoped_connection::ScopedConnection::new(
            db,
            Some(&crate::runtime::namespace(scope())),
            InventoryApp.schema(),
        );
        conn.execute(
            "UPDATE drafts SET note=? WHERE sequence=1",
            [note.to_string()],
        )
        .await
        .unwrap();
    };
    let runtime = open(&dir, NODE, options).await.unwrap();
    let replica = runtime.domain(scope()).unwrap();
    for index in 0..8 {
        write(&replica, None, &format!("note-{index}")).await;
    }
    runtime.close().await.unwrap();
    edit("tampered").await;
    let runtime = open(&dir, NODE, options).await.unwrap();
    let replica = runtime.domain(scope()).unwrap();
    // The row no longer matches the digest kept beside it.
    assert!(matches!(replica.audit().await, Err(Error::Corrupt(_))));
    runtime.close().await.unwrap();
    edit("note-0").await;
    let runtime = open(&dir, NODE, options).await.unwrap();
    let replica = runtime.domain(scope()).unwrap();
    assert!(replica.audit().await.unwrap().recomputed);
    let total = 4096 + 200;
    for index in 8..total {
        write(&replica, None, &format!("note-{index}")).await;
    }
    replica.checkpoint().await.unwrap();
    replica.prune().await.unwrap();
    runtime.close().await.unwrap();
    let runtime = open(&dir, NODE, options).await.unwrap();
    let replica = runtime.domain(scope()).unwrap();
    let total = total as u64;
    assert_eq!(count(&replica).await as u64, total);
    let snapshot = replica.sync_state().await.unwrap();
    let window = options.retention.window as u64;
    assert_eq!(
        snapshot.horizon.0,
        [(NODE, AuthorSequence::new(total - window).unwrap())]
    );
    // The retained window lies inside the held frontier.
    assert_eq!(snapshot.held, snapshot.applied);
    assert!(snapshot.received.is_empty());
    assert_eq!(
        snapshot.applied.0,
        [(NODE, AuthorSequence::new(total).unwrap())]
    );
    let next = write(&replica, None, "after the bound").await;
    assert_eq!(
        (next.id.sequence.get(), next.lamport.get()),
        (total + 1, total + 1)
    );
    let report = replica.audit().await.unwrap();
    assert!(report.recomputed);
    assert_eq!(report.rows, total + 1);
    runtime.close().await.unwrap();
}

/// A checkpoint of a `WITHOUT ROWID` table is exported over several pages
/// and imported into one domain of a multi-domain file; the other domain is
/// untouched and every domain audits, also after reopening.
#[tokio::test]
async fn a_rowless_checkpoint_imports_into_one_domain_of_a_shared_file() {
    let dir = TempDir::new().unwrap();
    let options = RuntimeOptions {
        retention: RetentionPolicy {
            window: 0,
            automatic: false,
            ..RetentionPolicy::default()
        },
        ..RuntimeOptions::default()
    };
    let app = Watched {
        rowless: true,
        ..Watched::default()
    };
    let scopes = [scope(), scope_in(DomainId([2; 16]))];
    let open = |node| open_domains(&dir, node, options, app.clone(), &scopes);
    let source = open(NODE).await.unwrap();
    let target = open(NodeId([4; 16])).await.unwrap();
    let (from, into) = (
        source.domain(scopes[0]).unwrap(),
        target.domain(scopes[0]).unwrap(),
    );
    let neighbour = target.domain(scopes[1]).unwrap();
    for index in 0..600 {
        write(&from, None, &format!("note-{index}")).await;
    }
    let local = write(&into, None, "unsent").await;
    write(&neighbour, None, "other domain").await;
    let before = neighbour.query().await.unwrap();
    from.checkpoint().await.unwrap();
    let (mut cursor, mut pages) = (None, 0);
    let report = loop {
        let page = from.checkpoint_page(cursor).await.unwrap();
        let page = CheckpointPage::decode(&page.encode()).unwrap();
        cursor = page.next;
        pages += 1;
        if let Some(report) = into.import_checkpoint(page).await.unwrap() {
            break report;
        }
    };
    assert!(pages >= 2, "{pages}");
    assert_eq!(report.reapplied, [local.id]);
    assert_eq!(count_in(&into, scopes[0]).await, 601);
    assert_eq!(neighbour.query().await.unwrap(), before);
    for replica in [&into, &neighbour] {
        assert!(replica.audit().await.unwrap().recomputed);
    }
    source.close().await.unwrap();
    target.close().await.unwrap();
    let target = open(NodeId([4; 16])).await.unwrap();
    let (into, neighbour) = (
        target.domain(scopes[0]).unwrap(),
        target.domain(scopes[1]).unwrap(),
    );
    assert_eq!(count_in(&into, scopes[0]).await, 601);
    assert_eq!(neighbour.query().await.unwrap(), before);
    for replica in [&into, &neighbour] {
        assert!(replica.audit().await.unwrap().recomputed);
    }
    target.close().await.unwrap();
}
