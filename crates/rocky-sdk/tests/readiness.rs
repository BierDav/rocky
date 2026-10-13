mod common;
use common::{Device, Failure};
use rocky_sdk::{Network, Session, transport::memory::Hub};
use serde_json::{Value, json};
use std::sync::Arc;

async fn call(session: &Session, scope: &Value, request: Value) -> Result<Value, Failure> {
    let reply = session
        .call(json!({"version": 1, "scope": scope, "request": request}).to_string())
        .await?;
    Ok(serde_json::from_str(&reply)?)
}

#[tokio::test(flavor = "multi_thread")]
async fn status_tracks_committed_work_and_logout_and_session_close_ends_only_its_wait()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let device = Device::create(root.path(), None)?;
    let scope: Value = serde_json::from_str(&device.database.scope())?;
    let session = device.database.session();
    let status = device.ok(json!({"type": "status"})).await;
    assert_eq!(status["authority"]["expired"], false);
    assert_eq!(status["network"]["attachment"], "detached");
    device.add(1, "apples").await;
    // A revision racing registration is returned immediately, never lost.
    let next = call(
        &session,
        &scope,
        json!({"type": "status_next", "after": status["token"]}),
    )
    .await?;
    assert_eq!(next["status"], "ok");
    assert_ne!(next["value"]["token"], status["token"]);
    let token = next["value"]["token"].clone();
    let wait = call(
        &session,
        &scope,
        json!({"type": "status_next", "after": token}),
    );
    let close = session.close();
    let (ended, ()) = tokio::join!(wait, close);
    // Whether admitted before close or after it, this session cannot keep waiting.
    assert!(ended.is_err() || ended?["status"] == "error");
    assert_eq!(device.titles().await, ["apples"]);
    device.close().await;
    // The sole issuer cannot log out; check authority loss on an enrolled writer.
    let hub = Arc::new(Hub::default());
    let issuer = Device::create(
        &root.path().join("issuer"),
        Some(Network::Owned(hub.connector("issuer"))),
    )?;
    let member = Device::join(
        &root.path().join("member"),
        &issuer.invite().await,
        Network::Owned(hub.connector("member")),
    )?;
    member.ok(json!({"type": "logout", "purge": false})).await;
    let logged_out = member.ok(json!({"type": "status"})).await;
    assert_eq!(logged_out["authority"]["logged_out"], true);
    assert_eq!(logged_out["network"]["connected_peer_count"], 0);
    member.close().await;
    issuer.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn pages_bound_encoded_bytes_and_continuations_belong_to_their_session_and_query()
-> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let device = Device::create(root.path(), None)?;
    for handle in 1..=3 {
        device
            .add(handle, &format!("{handle}{}", "x".repeat(400)))
            .await;
    }
    let scope: Value = serde_json::from_str(&device.database.scope())?;
    let session = device.database.session();
    let other = device.database.session();
    let query = json!({"sql": "SELECT title FROM items ORDER BY title", "values": [], "max_rows": 3, "max_bytes": 1000});
    let first = call(&session, &scope, json!({"type": "read", "query": query})).await?;
    assert_eq!(first["status"], "ok", "{first}");
    assert!(first.to_string().len() <= 1000);
    assert_eq!(first["value"]["rows"].as_array().expect("rows").len(), 1);
    let mut continued = query.clone();
    continued["cursor"] = first["value"]["next"].clone();
    assert!(continued["cursor"].is_string());
    let next = call(
        &session,
        &scope,
        json!({"type": "read", "query": continued}),
    )
    .await?;
    assert_eq!(next["status"], "ok");
    assert_ne!(next["value"]["rows"], first["value"]["rows"]);
    let cross = call(&other, &scope, json!({"type": "read", "query": continued})).await?;
    assert_eq!(cross["status"], "error");
    continued["sql"] = json!("SELECT completed FROM items ORDER BY title");
    let foreign_query = call(
        &session,
        &scope,
        json!({"type": "read", "query": continued}),
    )
    .await?;
    assert_eq!(foreign_query["status"], "error");
    let tiny = call(
        &session,
        &scope,
        json!({"type": "read", "query": {
            "sql": "SELECT title FROM items", "values": [], "max_rows": 3, "max_bytes": 256
        }}),
    )
    .await?;
    assert_eq!(tiny["status"], "error");
    session.close().await;
    other.close().await;
    device.close().await;
    Ok(())
}
