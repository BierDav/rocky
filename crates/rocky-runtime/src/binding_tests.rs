//! Faults and process death in a bound database's commit and in each step
//! of a placement migration. The typed application's gate is in `rocky-sdk`.
use super::*;
use crate::sql_tests::{DOMAINS, Notes, note, run, schema};
use crate::wide_schema_tests::{app, installation, options, test_scope};
use rocky_core::{DatabaseBinding, DomainId, MeshId, SqlValue, TransactionToken};
use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use tempfile::TempDir;

const CASE: &str = "ROCKY_BINDING_CRASH_CASE";
const DIR: &str = "ROCKY_BINDING_CRASH_DIR";
const TOKEN: TransactionToken = TransactionToken([9; 32]);
const BODY: [&str; 2] = [
    "INSERT INTO left_notes(key,body,id) VALUES('a','left',4294967424)",
    "INSERT INTO right_notes(key,body,id) VALUES('a','right',4294967424)",
];

fn scope(domain: DomainId) -> Scope {
    Scope {
        mesh: MeshId([1; 16]),
        app: schema().app_id(),
        domain,
    }
}
fn domain(domain: DomainId) -> DomainInstallation<Notes> {
    let (node, caller) = (NodeId([1; 16]), PrincipalId([7; 16]));
    DomainInstallation {
        app: Notes {
            schema: schema(),
            skewed: Default::default(),
        },
        policy: (),
        identity: Identity {
            scope: scope(domain),
            node,
            caller,
        },
        config: RuntimeConfig {
            authors: vec![(node, caller)],
            ..RuntimeConfig::default()
        },
    }
}
/// Both note domains in one file, bound as `left_notes` and `right_notes`.
async fn open(dir: &Path) -> (Runtime<Notes>, Database<Notes>, [Replica<Notes>; 2]) {
    let binding = DatabaseBinding::new("notes")
        .table("left_notes", scope(DOMAINS[0]), "notes")
        .and_then(|binding| binding.table("right_notes", scope(DOMAINS[1]), "notes"))
        .unwrap();
    let placement = DOMAINS.map(|domain| DomainPlacement {
        scope: scope(domain),
        file: 0,
    });
    let runtime = Runtime::open_bound(
        dir.join("notes.db"),
        DOMAINS.map(domain).into(),
        placement.into(),
        vec![binding],
        RuntimeOptions::default(),
    )
    .await
    .unwrap();
    let database = runtime.database("notes").unwrap();
    let replicas = DOMAINS.map(|domain| runtime.domain(scope(domain)).unwrap());
    (runtime, database, replicas)
}
async fn body(session: &SqlSession) {
    run(session, "BEGIN").await;
    for sql in BODY {
        run(session, sql).await;
    }
}
async fn notes(replicas: &[Replica<Notes>; 2]) -> [Vec<(String, String)>; 2] {
    [
        replicas[0].query().await.unwrap().rows,
        replicas[1].query().await.unwrap().rows,
    ]
}
async fn audited(replicas: &[Replica<Notes>; 2]) {
    for replica in replicas {
        replica.checkpoint().await.unwrap();
        assert!(replica.audit().await.unwrap().recomputed);
    }
}

#[tokio::test]
async fn a_fault_in_one_domain_or_in_the_commit_keeps_neither_domain() {
    let dir = TempDir::new().unwrap();
    let (runtime, database, replicas) = open(dir.path()).await;
    let session = database.sql_session(SqlOptions::default()).await.unwrap();
    let mut revisions = replicas
        .clone()
        .map(|replica| replica.inner.revisions.clone());
    for revision in &mut revisions {
        revision.borrow_and_update();
    }
    // The second domain's step fails after the first sealed, then the
    // engine's commit fails after both did.
    for case in 0..2 {
        match case {
            0 => crate::tests::fault(&replicas[1], worker::Fault::FailBeforeCommit).await,
            _ => crate::tests::fault(&replicas[0], worker::Fault::FailCommit).await,
        }
        body(&session).await;
        let failed = session.commit(TOKEN).await.unwrap_err();
        assert!(matches!(failed, Error::InjectedFailure), "{failed}");
        assert_eq!(notes(&replicas).await, [vec![], vec![]]);
        assert_eq!(database.sql_outcome(TOKEN).await.unwrap(), None);
        for revision in &revisions {
            assert!(!revision.has_changed().unwrap());
        }
        let count = "SELECT (SELECT count(*) FROM left_notes)+(SELECT count(*) FROM right_notes)";
        assert_eq!(run(&session, count).await.rows, [[SqlValue::Integer(0)]]);
    }
    // The same body then commits once, from each domain's first sequence.
    body(&session).await;
    let outcome = session.commit(TOKEN).await.unwrap().committed.unwrap();
    let sequences: Vec<_> = outcome
        .operations
        .iter()
        .map(|id| id.sequence.get())
        .collect();
    assert_eq!((sequences, outcome.domains.len()), (vec![1, 1], 2));
    assert_eq!(
        notes(&replicas).await,
        [vec![note("a", "left")], vec![note("a", "right")]]
    );
    audited(&replicas).await;
    drop(session);
    runtime.close().await.unwrap();
}

/// Does nothing in an ordinary run; a parent test selects where to die.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crash_child() {
    let (Ok(case), Ok(dir)) = (std::env::var(CASE), std::env::var(DIR)) else {
        return;
    };
    if let Ok(step) = case.parse::<usize>() {
        crate::relocation::ABORT_AFTER.store(step, std::sync::atomic::Ordering::Relaxed);
        let moves = vec![DomainPlacement {
            scope: test_scope(0),
            file: 2,
        }];
        let _ = migrate_placement(Path::new(&dir).join("placed.db"), moves).await;
        panic!("the migration did not end the process");
    }
    let (_runtime, database, replicas) = open(Path::new(&dir)).await;
    let session = database.sql_session(SqlOptions::default()).await.unwrap();
    let fault = match case.as_str() {
        // Both domains sealed, before the engine's commit.
        "sealed" => worker::Fault::AbortBeforeCommit,
        // Committed and flushed, before the caller is answered.
        "committed" => worker::Fault::AbortAfterCommit,
        other => panic!("unknown crash case {other}"),
    };
    crate::tests::fault(&replicas[0], fault).await;
    body(&session).await;
    let _ = session.commit(TOKEN).await;
    panic!("the injected fault did not end the process");
}
fn crash(case: &str, dir: &Path) {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["binding_tests::crash_child", "--exact", "--test-threads=1"])
        .env(CASE, case)
        .env(DIR, dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(6), "{case}: {status:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_death_commits_both_domains_or_neither() {
    let dir = TempDir::new().unwrap();
    crash("sealed", dir.path());
    let (runtime, database, replicas) = open(dir.path()).await;
    assert_eq!(notes(&replicas).await, [vec![], vec![]]);
    assert_eq!(database.sql_outcome(TOKEN).await.unwrap(), None);
    runtime.close().await.unwrap();

    crash("committed", dir.path());
    let (runtime, database, replicas) = open(dir.path()).await;
    assert_eq!(
        notes(&replicas).await,
        [vec![note("a", "left")], vec![note("a", "right")]]
    );
    let outcome = database.sql_outcome(TOKEN).await.unwrap().unwrap();
    assert_eq!(
        (
            outcome.domains.len(),
            outcome.operations.len(),
            outcome.replayed
        ),
        (2, 2, true)
    );
    assert_eq!(outcome.class, CommitClass::Flushed);
    // Sending the commit again under the token changes nothing.
    let session = database.sql_session(SqlOptions::default()).await.unwrap();
    run(&session, "BEGIN").await;
    run(&session, "DELETE FROM left_notes").await;
    let conflict = session.commit(TOKEN).await.unwrap_err();
    assert!(matches!(conflict, Error::Conflict), "{conflict}");
    assert_eq!(notes(&replicas).await[0], [note("a", "left")]);
    audited(&replicas).await;
    drop(session);
    runtime.close().await.unwrap();
}

#[tokio::test]
async fn an_interrupted_migration_resumes_from_every_step() {
    let placed = |file: usize| {
        vec![
            DomainPlacement {
                scope: test_scope(0),
                file,
            },
            DomainPlacement {
                scope: test_scope(1),
                file: 0,
            },
        ]
    };
    let serve = async |path: &Path, file: usize| {
        let domains = vec![installation(app(3), 0), installation(app(3), 1)];
        Runtime::open_placed(path, domains, placed(file), options()).await
    };
    let template = TempDir::new().unwrap();
    let source = template.path().join("placed.db");
    let runtime = serve(&source, 0).await.unwrap();
    for domain in 0..2 {
        let replica = runtime.domain(test_scope(domain)).unwrap();
        let text = format!("domain-{domain}");
        crate::wide_schema_tests::note(&replica, 1, &text)
            .committed()
            .await
            .unwrap();
    }
    runtime.close().await.unwrap();
    for step in 1..=4 {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("placed.db");
        for entry in std::fs::read_dir(template.path()).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), dir.path().join(entry.file_name())).unwrap();
        }
        crash(&step.to_string(), dir.path());
        // Neither assignment opens while the migration is pending.
        for file in [0, 2] {
            assert!(
                matches!(serve(&path, file).await, Err(Error::Incompatible(_))),
                "step {step}"
            );
        }
        let other = vec![DomainPlacement {
            scope: test_scope(0),
            file: 3,
        }];
        assert!(matches!(
            migrate_placement(&path, other).await,
            Err(Error::Incompatible(_))
        ));
        assert_eq!(
            migrate_placement(&path, Vec::new()).await.unwrap(),
            placed(2)
        );
        let runtime = serve(&path, 2).await.unwrap();
        for domain in 0..2 {
            let replica = runtime.domain(test_scope(domain)).unwrap();
            let snapshot = replica.query().await.unwrap();
            assert_eq!(snapshot.rows.len(), 1, "step {step}");
            assert_eq!(snapshot.rows[0].1, format!("domain-{domain}"));
            assert_eq!(snapshot.revision.get(), 1);
            crate::wide_schema_tests::note(&replica, 2, "after")
                .committed()
                .await
                .unwrap();
            replica.checkpoint().await.unwrap();
            assert!(replica.audit().await.unwrap().recomputed, "step {step}");
        }
        runtime.close().await.unwrap();
        // The file the domain left keeps only the other domain's tables.
        let left = crate::sqlite::Connection::open(&path).unwrap();
        let moved = format!("{}\\_%", crate::runtime::namespace(test_scope(0)));
        let stayed = format!("{}\\_%", crate::runtime::namespace(test_scope(1)));
        let count = |like: String| {
            let sql = "SELECT count(*) FROM sqlite_schema WHERE name LIKE ? ESCAPE '\\'";
            left.rows(sql, [like]).unwrap()
        };
        assert_eq!(count(moved), [[crate::sqlite::Value::Integer(0)]]);
        assert_ne!(count(stayed), [[crate::sqlite::Value::Integer(0)]]);
    }
}
