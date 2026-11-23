use super::*;
use rocky_core::{Blocker, ClearsBy};
use rocky_secure::EarlierEpochWrites;

const TICK: Duration = Duration::from_secs(30);
const SPACING: Duration = Duration::from_secs(1);

fn seen_before_pass<T>(watch: &mut watch::Receiver<T>) {
    watch.borrow_and_update();
}

impl Node {
    pub(crate) async fn recover_locally(&self) -> Result<Vec<Blocker>, Failure> {
        let recovered = self.replica.recover_locally().await;
        if self.replica.earlier_epoch_due().await && self.signs_for_members().await {
            self.wake_sync();
        }
        recovered?;
        self.follow_main_when_held().await?;
        self.blockers().await
    }
    pub(super) async fn publish_for_members(&self) -> Option<crate::SealSkip> {
        if !self.signs_for_members().await {
            return None;
        }
        let mut sealing = false;
        let scope = self.domain().scope;
        let published = async {
            match self.replica.earlier_epoch_writes(scope).await? {
                EarlierEpochWrites::Unsealed => {
                    sealing = true;
                    return self.seal_for_members().await;
                }
                EarlierEpochWrites::Unbased => {
                    self.replica.compact(scope, 0).await?;
                }
                EarlierEpochWrites::Covered => {
                    if self.replica.unserved_bases(scope).await? {
                        self.replica.compact(scope, 0).await?;
                    }
                }
            }
            self.list_checkpoints(scope).await
        }
        .await;
        if let Err(error) = published {
            self.activity
                .record_changed(crate::ActivityKind::MemberSealFailed {
                    error: (error.to_string()).to_owned(),
                });
            if sealing {
                return Some(crate::SealSkip::Failed(error.to_string()));
            }
        }
        None
    }
    pub(super) async fn seal_routinely(&self) -> Option<crate::SealSkip> {
        self.settings.lock().await.trusted_links()?;
        let scope = self.domain().scope;
        let sealed = async {
            let due = self.replica.unsealed_replayed_transactions().await?
                >= rocky_secure::ROUTINE_SEAL_TRANSACTIONS
                || self.replica.earlier_epoch_due().await;
            if self.replica.followed_bookmark().await.is_none() {
                return Ok(due.then_some(crate::SealSkip::NoFollowedBookmark));
            }
            if let Err(error) = self.tip_signer(scope).await {
                return Ok(due.then(|| crate::SealSkip::NoTipAuthority(error.to_string())));
            }
            if self.replica.unsealed_replayed_transactions().await?
                >= rocky_secure::ROUTINE_SEAL_TRANSACTIONS
            {
                self.seal_followed(scope).await?;
            }
            let awaiting = self.replica.awaiting_outcomes().await?;
            if !awaiting.is_empty() {
                self.activity
                    .record_changed(crate::ActivityKind::ReplayedAwaitingOutcomes {
                        transactions: awaiting.len(),
                    });
            }
            Ok::<_, Failure>(None)
        }
        .await;
        sealed.unwrap_or_else(|error| {
            self.activity
                .record_changed(crate::ActivityKind::RoutineSealFailed {
                    error: (error.to_string()).to_owned(),
                });
            Some(crate::SealSkip::Failed(error.to_string()))
        })
    }
    pub(super) fn seal_skipped(&self, reason: Option<crate::SealSkip>) {
        let mut skipped = self
            .seal_skipped
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let changed = match (&*skipped, reason) {
            (None, None) => false,
            (Some(_), None) => {
                tracing::info!("this trusted node seals again");
                *skipped = None;
                true
            }
            (current, Some(reason)) => {
                let same = matches!(
                    current,
                    Some(crate::Alert { kind: crate::AlertKind::SealSkipped { reason: held }, .. })
                        if *held == reason
                );
                if !same {
                    tracing::warn!(%reason, "this trusted node skipped a due seal");
                    let since = current
                        .as_ref()
                        .and_then(|alert| alert.since)
                        .or_else(|| self.status_time().ok());
                    *skipped = Some(crate::Alert {
                        kind: crate::AlertKind::SealSkipped { reason },
                        since,
                    });
                }
                !same
            }
        };
        drop(skipped);
        if changed {
            self.observations.touch();
        }
    }
    async fn signs_for_members(&self) -> bool {
        self.replica.followed_bookmark().await.is_some()
            && self.tip_signer(self.domain().scope).await.is_ok()
    }
    pub(crate) async fn drive_recovery(self: Arc<Self>) {
        let mut context = self.replica.observe_recovery().await;
        let mut revisions = self.replica.observe_revision();
        let mut frames = self.replica.observe_frames().await;
        let mut passes = self.topics.subscribe();
        let mut wakes = self.sync_events.subscribe();
        loop {
            seen_before_pass(&mut context);
            seen_before_pass(&mut revisions);
            seen_before_pass(&mut frames);
            seen_before_pass(&mut passes);
            seen_before_pass(&mut wakes);
            let blockers = match self.recover_locally().await {
                Ok(blockers) => blockers,
                Err(error) => {
                    self.activity
                        .record_changed(crate::ActivityKind::LocalRecoveryFailed {
                            error: (error.to_string()).to_owned(),
                        });
                    self.blockers().await.unwrap_or_default()
                }
            };
            let local = blockers
                .iter()
                .any(|blocker| blocker.clears_by == ClearsBy::LocalWork);
            let tick = rocky_platform::time::sleep(TICK);
            tokio::pin!(tick);
            let clock = self.replica.clock_wait().await.ok().flatten();
            let due = rocky_platform::time::sleep(clock.unwrap_or(TICK));
            tokio::pin!(due);
            loop {
                tokio::select! {
                    changed = context.changed() => match changed {
                        Ok(()) => break,
                        Err(_) => return,
                    },
                    changed = revisions.changed() => if changed.is_err() { return },
                    changed = frames.changed() => if changed.is_err() { return },
                    changed = passes.changed() => if changed.is_err() { return },
                    changed = wakes.changed() => if changed.is_err() { return },
                    () = &mut tick, if local => break,
                    () = &mut due, if clock.is_some() => break,
                }
                if !self.blockers().await.unwrap_or_default().is_empty() {
                    break;
                }
            }
            rocky_platform::time::sleep(SPACING).await;
        }
    }
}
