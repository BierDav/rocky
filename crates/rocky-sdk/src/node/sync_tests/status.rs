use super::change_driven::{frozen, start, targets, transport};
use super::*;
use crate::test_snapshot as snap;
use crate::{AttachmentState, AttemptOperation, AttemptOutcome, LinkState};

#[tokio::test(start_paused = true)]
async fn status_names_the_sync_mode_and_an_empty_pass_is_not_a_skip() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(root.path(), &hub, "a", None).await?;
            let first = a.observed_status().await?;
            assert_eq!(first.network.connected_peer_count, 0);
            assert_eq!(first.network.sync, crate::SyncMode::Manual);
            let report = a.sync(&*transport(&a).await?).await?.0;
            assert_eq!(report["peers"], json!([]));
            assert!(report["skip"].is_null());
            start(&a, true).await?;
            let live = a.observed_status().await?;
            assert_eq!(live.network.sync, crate::SyncMode::Live);
            snap::value(a.set_networking_enabled(false)).await?;
            let disabled = a.observed_status().await?;
            assert_eq!(disabled.network.sync, crate::SyncMode::Off);
            assert_ne!(disabled.token, live.token);
            a.detach().await;
            let off = a
                .sync_optional(None)
                .await
                .err()
                .ok_or("an Off pass fails")?;
            assert_eq!(
                off.downcast_ref::<ServiceError>(),
                Some(&ServiceError::SyncOff)
            );
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
            let creator = enrolled(&root.path().join("c"), &hub, "c", None).await?;
            let writer = crate::Access::Full;
            let a = enrolled(&root.path().join("a"), &hub, "a", Some((&creator, writer))).await?;
            creator.detach().await;
            let expires = a.settings.lock().await.expires();
            *a.status_clock
                .lock()
                .unwrap_or_else(PoisonError::into_inner) =
                Some((expires - 1, tokio::time::Instant::now()));
            let before = a.observed_status().await?;
            let stable = a.observed_status().await?;
            assert_eq!(before.token, stable.token);
            assert!(!before.authority.expired);
            let wait = a.readiness_after(&before.token);
            let advance = async {
                tokio::task::yield_now().await;
                tokio::time::advance(Duration::from_secs(1)).await;
            };
            let (status, ()) = tokio::join!(wait, advance);
            assert!(status?.authority.expired);
            assert_eq!(
                a.observed_status().await?.network.sync,
                crate::SyncMode::Manual
            );
            a.detach().await;
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
                Some((&a, crate::Access::Full)),
            )
            .await?;
            let c = enrolled(
                &root.path().join("c"),
                &hub,
                "c",
                Some((&a, crate::Access::Full)),
            )
            .await?;
            targets(&a, &[b.clone(), c.clone()]).await?;
            c.detach().await;
            let report: crate::SyncReport =
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
            a.detach().await;
            b.detach().await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn a_scope_the_peer_does_not_share_is_not_a_blocker() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let profile = crate::Access::Full;
            let b = enrolled(&root.path().join("b"), &hub, "b", Some((&a, profile))).await?;
            targets(&a, std::slice::from_ref(&b)).await?;
            let report: crate::SyncReport =
                serde_json::from_value(a.sync(&*transport(&a).await?).await?.0)?;
            assert!(!report.peers[0].refused.is_empty(), "{report:?}");
            let blockers = a.blockers().await?;
            assert!(
                !blockers.iter().any(|blocker| matches!(
                    blocker.kind,
                    rocky_core::BlockerKind::SyncRequest { .. }
                )),
                "{blockers:?}"
            );
            a.detach().await;
            b.detach().await;
            Ok(())
        }))
        .await
}

async fn committed_frames(
    node: &Node,
    key: &str,
) -> Result<Vec<rocky_secure::EncryptedFrame>, Failure> {
    let sql = node.sql_session(SqlOptions::default()).await?;
    sql.run("BEGIN", Vec::new()).await?;
    let id = sql.next_id("items").await?;
    sql.run(
        "INSERT INTO items(key,title,id) VALUES(?,'Milk',?)",
        vec![SqlValue::Text(key.into()), id.sql()],
    )
    .await?;
    let committed = sql
        .commit(commitment("rocky.sdk.mutation.v1", key.as_bytes()))
        .await?;
    Ok(committed.committed.ok_or("a committed bundle")?.frames)
}

async fn read_failure(node: &Node) -> Result<crate::Error, Failure> {
    match read_rows(node, "SELECT count(*) FROM items").await {
        Ok(rows) => Err(format!("an incomplete table was read: {rows:?}").into()),
        Err(error) => Ok(crate::Error::from_failure(&error)),
    }
}

#[tokio::test(start_paused = true)]
async fn an_incomplete_read_names_the_blocker_status_reports_until_the_data_arrives()
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
                Some((&a, crate::Access::Full)),
            )
            .await?;
            let mut frames = Vec::new();
            for key in ["a", "b"] {
                frames.extend(committed_frames(&a, key).await?);
            }
            let later = frames.pop().ok_or("a later frame")?;
            assert_eq!(
                b.replica.receive(later).await?,
                rocky_secure::ReceiveStatus::PendingDependencies
            );

            let failure = read_failure(&b).await?;
            let crate::ErrorKind::Blocked {
                table: Some(table),
                blockers,
            } = failure.kind()
            else {
                return Err(format!("expected an incomplete table, found {failure:?}").into());
            };
            assert_eq!(table, "items");
            assert!(failure.retryable());
            let pending = blockers
                .iter()
                .find(|blocker| {
                    matches!(
                        blocker.kind,
                        rocky_core::BlockerKind::PendingFrames {
                            reason: rocky_core::WaitReason::Dependencies,
                            ..
                        }
                    )
                })
                .ok_or_else(|| format!("no pending-frames blocker in {blockers:?}"))?;
            assert_eq!(pending.clears_by, rocky_core::ClearsBy::AnyPeer);
            assert_eq!(failure.blockers(), &**blockers);
            assert!(
                b.observed_status().await?.data.blockers.contains(pending),
                "status reports the blocker the read names"
            );

            for frame in frames {
                b.replica.receive(frame).await?;
            }
            assert_eq!(
                read_rows(&b, "SELECT count(*) FROM items").await?,
                vec![vec![SqlValue::Integer(2)]]
            );
            assert!(
                !b.observed_status().await?.data.blockers.contains(pending),
                "the blocker clears once the missing frame arrives"
            );
            a.detach().await;
            b.detach().await;
            Ok(())
        }))
        .await
}
