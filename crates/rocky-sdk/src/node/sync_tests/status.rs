use super::change_driven::{frozen, start, targets, transport};
use super::*;
use crate::{
    AttachmentState, AttemptOperation, AttemptOutcome, CoordinationMode, LinkState, SkipReason,
};

#[tokio::test(start_paused = true)]
async fn empty_pass_disabled_control_and_suspension_are_independent_facts() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(root.path(), &hub, "a", None).await?;
            let first = a.observed_status().await?;
            assert_eq!(first.network.connected_peer_count, 0);
            assert_eq!(first.coordination.mode, CoordinationMode::NotConfigured);
            assert_eq!(first.network.relay.relays, None);
            let report = a.sync(&*transport(&a).await?).await?.0;
            assert_eq!(report["peers"], json!([]));
            assert!(report["skip"].is_null());
            let client = a.client(1)?;
            a.dispatch(&client, &json!({"type":"network_background"}))
                .await?;
            start(&a, true).await?;
            let suspended = a.observed_status().await?;
            assert!(suspended.network.networking_enabled);
            assert!(suspended.network.worker_enabled);
            assert!(!suspended.network.foreground);
            assert!(!suspended.network.worker_running);
            let report: crate::ReconciliationReport =
                serde_json::from_value(a.sync(&*transport(&a).await?).await?.0)?;
            assert_eq!(report.skip, Some(SkipReason::Suspended));
            a.dispatch(
                &client,
                &json!({"type":"networking_control","enabled":false}),
            )
            .await?;
            let disabled = a.observed_status().await?;
            assert!(!disabled.network.networking_enabled);
            assert!(!disabled.network.foreground);
            assert_ne!(disabled.token, suspended.token);
            a.detach(None).await;
            let report: crate::ReconciliationReport =
                serde_json::from_value(a.sync_optional(None).await?.0)?;
            assert_eq!(report.skip, Some(SkipReason::NetworkingDisabled));
            assert_eq!(
                a.observed_status().await?.network.attachment,
                AttachmentState::Detached
            );
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn expiry_alone_wakes_a_status_wait_and_observation_does_not_start_sync()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(root.path(), &hub, "a", None).await?;
            let expires = a.settings.lock().await.expires();
            *a.status_clock
                .lock()
                .unwrap_or_else(PoisonError::into_inner) =
                Some((expires - 1, tokio::time::Instant::now()));
            let before = a.observed_status().await?;
            let stable = a.observed_status().await?;
            assert_eq!(before.token, stable.token);
            assert!(!before.authority.expired);
            let client = a.client(1)?;
            let wait = a.readiness_after(&client, &before.token);
            let advance = async {
                tokio::task::yield_now().await;
                tokio::time::advance(Duration::from_secs(1)).await;
            };
            let (status, ()) = tokio::join!(wait, advance);
            assert_eq!(status?["authority"]["expired"], true);
            assert!(!a.observed_status().await?.network.worker_enabled);
            a.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn a_short_exchange_and_a_failed_peer_do_not_become_a_connection_verdict()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((
                    &a,
                    Profile::Writer {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            let c = enrolled(
                &root.path().join("c"),
                &hub,
                "c",
                Some((
                    &a,
                    Profile::Writer {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            targets(&a, &[b.clone(), c.clone()]).await?;
            c.detach(None).await;
            let report: crate::ReconciliationReport =
                serde_json::from_value(a.sync(&*transport(&a).await?).await?.0)?;
            assert!(report.peers.iter().any(|peer| {
                peer.attempts.iter().any(|attempt| {
                    attempt.operation == AttemptOperation::Reconcile
                        && attempt.outcome == Some(AttemptOutcome::Completed)
                })
            }));
            assert!(report.peers.iter().any(|peer| {
                peer.attempts
                    .iter()
                    .any(|attempt| attempt.outcome == Some(AttemptOutcome::Failed))
            }));
            let status = a.observed_status().await?;
            assert_eq!(status.network.connected_peer_count, 0);
            assert!(
                status
                    .network
                    .peers
                    .iter()
                    .all(|peer| matches!(peer.link.state, LinkState::Closed | LinkState::Closing))
            );
            assert!(!status.authority.expired);
            assert!(status.authority.verified_revocation.is_none());
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}
