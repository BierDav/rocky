//! Approval invitations: a joining device waits until an issuer confirms
//! the code both devices show, and every request that ends unadmitted is
//! reported to the issuer's application.
use super::cluster::{Member, SCHEMA};
use crate::{AwaitingAdmission, Failure, JoinRefused, transport::memory::Hub};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

fn writer() -> Value {
    json!({"kind": "writer", "scopes": ["items"]})
}
/// The issuer's activity events of `kind`.
async fn events(issuer: &Member, kind: &str) -> Result<Vec<Value>, Failure> {
    let activity = issuer.call(json!({"type": "activity"})).await?;
    Ok(activity["events"]
        .as_array()
        .ok_or("missing events")?
        .iter()
        .filter(|event| event["kind"] == kind)
        .cloned()
        .collect())
}
async fn pending(issuer: &Member) -> Result<Vec<Value>, Failure> {
    Ok(issuer.call(json!({"type": "discovered"})).await?["pending"]
        .as_array()
        .ok_or("missing pending")?
        .clone())
}
/// Joins and expects to wait: the code the device shows.
async fn awaiting(device: &mut Member, hub: &Arc<Hub>, uri: &str) -> Result<String, Failure> {
    let error = device
        .join(hub, uri)
        .await
        .err()
        .ok_or("joined unadmitted")?;
    Ok(error
        .downcast_ref::<AwaitingAdmission>()
        .ok_or_else(|| format!("not awaiting admission: {error}"))?
        .code
        .clone())
}

// Mutations: skipping the code comparison in `admit`, or not ending
// requests when their invitation expired, each fail an assertion below.
#[tokio::test(start_paused = true)]
async fn a_requested_join_waits_for_admission_with_the_code_both_devices_show()
-> Result<(), Failure> {
    let _seeded = rocky_platform::entropy::simulation::seed(51);
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let mut issuer = Member::new(root.path(), "issuer", SCHEMA);
            issuer.create(&hub).await?;
            let invitation = issuer.call(json!({"type": "invitation"})).await?;
            assert_eq!(invitation["profile"], Value::Null);
            let uri = invitation["uri"].as_str().ok_or("missing uri")?.to_owned();

            // Pending until admitted; a retry keeps its request and code.
            let mut device = Member::new(root.path(), "laptop", SCHEMA);
            let code = awaiting(&mut device, &hub, &uri).await?;
            assert_eq!(awaiting(&mut device, &hub, &uri).await?, code);
            let listed = pending(&issuer).await?;
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0]["name"], "laptop");
            assert_eq!(listed[0]["code"], code.as_str());
            assert_eq!(listed[0]["invitation"], invitation["id"]);
            let node = listed[0]["node"].clone();
            assert_eq!(events(&issuer, "join_requested").await?.len(), 1);

            // A wrong code is refused and the request stays.
            let wrong = format!("{:06}", (code.parse::<u32>()? + 1) % 1_000_000);
            let admit = |code: &str| {
                json!({"type": "admit", "node": node, "code": code, "profile": writer()})
            };
            assert!(issuer.call(admit(&wrong)).await.is_err());
            assert_eq!(pending(&issuer).await?.len(), 1);
            assert_eq!(awaiting(&mut device, &hub, &uri).await?, code);

            // Pending and answered requests survive issuer restarts.
            issuer.close().await?;
            issuer.open(&hub).await?;
            assert_eq!(pending(&issuer).await?, listed);
            issuer.call(admit(&code)).await?;
            assert!(pending(&issuer).await?.is_empty());
            issuer.close().await?;
            issuer.open(&hub).await?;
            device.join(&hub, &uri).await?;
            let session = device.call(json!({"type": "session"})).await?;
            assert_eq!(session["node"], node);
            assert_eq!(events(&issuer, "identity_admitted").await?.len(), 1);

            // A refused device learns it at its next join.
            let mut refused = Member::new(root.path(), "phone", SCHEMA);
            awaiting(&mut refused, &hub, &uri).await?;
            let node = pending(&issuer).await?[0]["node"].clone();
            issuer
                .call(json!({"type": "refuse", "node": node}))
                .await?;
            let error = refused.join(&hub, &uri).await.err().ok_or("joined refused")?;
            assert!(error.downcast_ref::<JoinRefused>().is_some(), "{error}");
            assert_eq!(events(&issuer, "join_refused").await?.len(), 1);

            // An expired invitation ends its request and reports it.
            let mut late = Member::new(root.path(), "tablet", SCHEMA);
            awaiting(&mut late, &hub, &uri).await?;
            rocky_platform::entropy::simulation::pass(Duration::from_secs(11 * 60));
            assert!(pending(&issuer).await?.is_empty());
            let dropped = events(&issuer, "join_request_dropped").await?;
            let reasons: Vec<_> = dropped
                .iter()
                .map(|event| (event["name"].clone(), event["reason"].clone()))
                .collect();
            assert!(
                reasons.contains(&(json!("tablet"), json!("expired"))),
                "{dropped:?}"
            );
            assert!(late.join(&hub, &uri).await.is_err());
            Ok(())
        })
        .await
}
