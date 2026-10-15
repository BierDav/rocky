use super::*;
use inventory_app::{InventoryApp, Policy};
use rocky_core::{DomainId, MeshId};
use tempfile::TempDir;

fn scope() -> Scope {
    Scope {
        mesh: MeshId([11; 16]),
        app: inventory_app::app_id(),
        domain: DomainId([1; 16]),
    }
}
async fn open(dir: &TempDir) -> Replica<InventoryApp> {
    Replica::open(
        dir.path().join("sdk.db"),
        InventoryApp,
        Policy {
            scope: scope(),
            caller: PrincipalId([7; 16]),
            reference: [9; 32],
            note_prefix: None,
        },
        Identity {
            scope: scope(),
            node: NodeId([3; 16]),
            caller: PrincipalId([7; 16]),
        },
    )
    .await
    .unwrap()
}
async fn call(session: &Session<InventoryApp>, request: Request) -> Reply {
    let bytes = serde_json::to_vec(&Call {
        version: VERSION,
        scope: scope(),
        request,
    })
    .unwrap();
    serde_json::from_slice(&session.dispatch_json(&bytes).await).unwrap()
}
fn ok(reply: Reply) -> Value {
    match reply {
        Reply::Ok { value, .. } => value,
        reply => panic!("{reply:?}"),
    }
}
fn query(sql: &str) -> Query {
    Query {
        sql: sql.into(),
        values: vec![],
        max_rows: 1024,
        max_bytes: None,
        cursor: None,
    }
}
#[tokio::test]
async fn subscription_registration_races_coalescing_cancellation_and_scope_denial() {
    let dir = TempDir::new().unwrap();
    let replica = open(&dir).await;
    let session = Session::new(replica.clone());
    let id = ok(call(
        &session,
        Request::Subscribe {
            query: query("SELECT note FROM drafts ORDER BY note"),
        },
    )
    .await)["subscription"]
        .as_str()
        .unwrap()
        .to_owned();
    // Commit between registration and first collection.
    ok(call(
        &session,
        Request::Mutate {
            handle: [1; 32],
            entity: None,
            payload: b"first".to_vec(),
            baseline: None,
        },
    )
    .await);
    let initial = ok(call(
        &session,
        Request::Next {
            subscription: id.clone(),
        },
    )
    .await);
    assert_eq!(initial["rows"][0][0]["value"], "first");
    for value in 2..=10 {
        ok(call(
            &session,
            Request::Mutate {
                handle: [value; 32],
                entity: None,
                payload: format!("note-{value}").into_bytes(),
                baseline: None,
            },
        )
        .await);
    }
    let latest = ok(call(
        &session,
        Request::Next {
            subscription: id.clone(),
        },
    )
    .await);
    assert_eq!(latest["rows"].as_array().unwrap().len(), 10);
    let waiting = call(
        &session,
        Request::Next {
            subscription: id.clone(),
        },
    );
    let release = async {
        tokio::task::yield_now().await;
        call(&session, Request::Unsubscribe { subscription: id }).await
    };
    let (waiting, release) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(waiting, release)
    })
    .await
    .unwrap();
    assert!(matches!(waiting, Reply::Error { error, .. } if error.code == "closed"));
    ok(release);
    let mut foreign = scope();
    foreign.domain = DomainId([2; 16]);
    assert!(
        matches!(session.dispatch(Call { version:VERSION, scope:foreign, request:Request::Read { query:query("SELECT note FROM drafts") } }).await, Reply::Error { error, .. } if error.code == "denied")
    );
    assert!(matches!(
        call(
            &session,
            Request::Read {
                query: query("SELECT * FROM rocky_operations")
            }
        )
        .await,
        Reply::Error { .. }
    ));
    replica.close().await.unwrap();
}
#[tokio::test]
async fn precise_handles_preconditions_restart_and_explicit_strong_reads() {
    let dir = TempDir::new().unwrap();
    let replica = open(&dir).await;
    let session = Session::new(replica.clone());
    let original = ok(call(
        &session,
        Request::Mutate {
            handle: [1; 32],
            entity: None,
            payload: b"precise".to_vec(),
            baseline: Some("0".into()),
        },
    )
    .await);
    assert!(matches!(
        call(
            &session,
            Request::Mutate {
                handle: [2; 32],
                entity: None,
                payload: b"stale".to_vec(),
                baseline: Some("0".into())
            }
        )
        .await,
        Reply::Error { .. }
    ));
    assert!(
        matches!(call(&session, Request::Mutate { handle:[1;32], entity:None, payload:b"different".to_vec(), baseline:None }).await, Reply::Error { error, .. } if error.code == "conflict")
    );
    // The removed strong read is no request of this contract.
    assert!(
        matches!(serde_json::from_slice::<Reply>(&session.dispatch_json(format!(r#"{{"version":{VERSION},"scope":{},"request":{{"type":"read_strong","query":{{"sql":"SELECT 1","values":[]}}}}}}"#, serde_json::to_string(&session.scope()).unwrap()).as_bytes()).await).unwrap(), Reply::Error { error, .. } if error.code == "denied")
    );
    let exact = ok(call(
        &session,
        Request::Read {
            query: query("SELECT 9223372036854775807"),
        },
    )
    .await);
    assert_eq!(exact["rows"][0][0]["value"], "9223372036854775807");
    assert!(
        matches!(serde_json::from_slice::<Reply>(&session.dispatch_json(br#"{"version":2}"#).await).unwrap(), Reply::Error { error, .. } if error.code == "denied")
    );
    drop(session);
    replica.close().await.unwrap();
    drop(replica);
    let replica = open(&dir).await;
    let session = Session::new(replica.clone());
    let retried = ok(call(
        &session,
        Request::Mutate {
            handle: [1; 32],
            entity: None,
            payload: b"precise".to_vec(),
            baseline: Some("0".into()),
        },
    )
    .await);
    assert_eq!(original, retried);
    let progress = ok(call(&session, Request::MutationProgress { handle: [1; 32] }).await);
    assert_eq!(progress["state"], "committed");
    assert_eq!(progress["operation"], original);
    replica.close().await.unwrap();
}

#[tokio::test]
async fn noisy_session_quota_and_close_preserve_other_sessions() {
    let dir = TempDir::new().unwrap();
    let replica = open(&dir).await;
    let noisy = Session::new(replica.clone());
    let other = Session::new(replica.clone());
    for _ in 0..64 {
        ok(call(
            &noisy,
            Request::Subscribe {
                query: query("SELECT COUNT(*) FROM drafts"),
            },
        )
        .await);
    }
    assert!(
        matches!(call(&noisy, Request::Subscribe { query: query("SELECT COUNT(*) FROM drafts") }).await,
        Reply::Error { error, .. } if error.code == "quota")
    );
    let id = ok(call(
        &other,
        Request::Subscribe {
            query: query("SELECT COUNT(*) FROM drafts"),
        },
    )
    .await)["subscription"]
        .as_str()
        .unwrap()
        .to_owned();
    noisy.close().unwrap();
    assert!(
        matches!(call(&noisy, Request::Read { query: query("SELECT COUNT(*) FROM drafts") }).await,
        Reply::Error { error, .. } if error.code == "closed")
    );
    let snapshot = ok(call(&other, Request::Next { subscription: id }).await);
    assert_eq!(snapshot["rows"][0][0]["value"], "0");
    other.close().unwrap();
    let new = Session::new(replica.clone());
    for _ in 0..64 {
        ok(call(
            &new,
            Request::Subscribe {
                query: query("SELECT COUNT(*) FROM drafts"),
            },
        )
        .await);
    }
    new.close().unwrap();
    replica.close().await.unwrap();
}
