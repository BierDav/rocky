//! Local recovery runs without a peer: at open, after keys, grants,
//! schemas or the tip graph change, after a commit while a blocker remains
//! and every `TICK` while local work clears one. Never inside a network
//! call or a peer session.
use super::*;
use rocky_core::{Blocker, ClearsBy};
use rocky_secure::EarlierEpochWrites;

/// How often local recovery runs again while local work clears a blocker.
const TICK: Duration = Duration::from_secs(30);
/// The least time between two runs a commit triggered.
const SPACING: Duration = Duration::from_secs(1);

impl Node {
    /// One local recovery pass, then the blockers left. Sealing for
    /// members publishes statements, so a due seal wakes the sync pass,
    /// which runs it with the network.
    pub(crate) async fn recover_locally(&self) -> Result<Vec<Blocker>, Failure> {
        let recovered = self.replica.recover_locally().await;
        if self.replica.earlier_epoch_due().await && self.signs_for_members().await {
            self.wake_sync();
        }
        recovered?;
        self.complete_pending_follow().await?;
        self.blockers().await
    }
    /// In a sync pass, before the exchanges: a signer seals for members
    /// when writes of an earlier key epoch arrived past the last seal, and
    /// lists the bases it holds, so a scope only a listed checkpoint
    /// restores finds a peer that serves it.
    pub(super) async fn publish_for_members(&self) {
        if !self.signs_for_members().await {
            return;
        }
        let scope = self.domain().scope;
        let published = async {
            // After a revocation members restore rows only from a served
            // base: a lost one is made again at the newest finalized seal,
            // one of an earlier epoch signed again, and one that misses
            // sealed writes of an earlier epoch moves to a later seal.
            match self.replica.earlier_epoch_writes(scope).await? {
                EarlierEpochWrites::Unsealed => return self.seal_for_members().await,
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
                .record_changed("member_seal_failed", json!({"error": error.to_string()}));
        }
    }
    /// Whether this node seals for members: it signs tip statements and
    /// follows a bookmark.
    async fn signs_for_members(&self) -> bool {
        self.replica.followed_bookmark().await.is_some()
            && self.tip_signer(self.domain().scope).await.is_ok()
    }
    /// Drives local recovery until the database closes.
    pub(crate) async fn drive_recovery(self: Arc<Self>) {
        let mut context = self.replica.observe_recovery().await;
        let mut revisions = self.replica.observe_revision();
        let mut frames = self.replica.observe_frames().await;
        // Sync passes, explicit ones too, and tip changes.
        let mut passes = self.topics.subscribe();
        let mut wakes = self.sync_events.subscribe();
        loop {
            // Mark before the pass: changes during recovery must wake the
            // next pass rather than being swallowed after it completes.
            context.borrow_and_update();
            revisions.borrow_and_update();
            frames.borrow_and_update();
            passes.borrow_and_update();
            wakes.borrow_and_update();
            let blockers = match self.recover_locally().await {
                Ok(blockers) => blockers,
                Err(error) => {
                    self.activity.record_changed(
                        "local_recovery_failed",
                        json!({"error": error.to_string()}),
                    );
                    self.blockers().await.unwrap_or_default()
                }
            };
            let local = blockers
                .iter()
                .any(|blocker| blocker.clears_by == ClearsBy::LocalWork)
                || self.settings.lock().await.pending_follow.is_some();
            let tick = tokio::time::sleep(TICK);
            tokio::pin!(tick);
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
                }
                if self.settings.lock().await.pending_follow.is_some()
                    || !self.blockers().await.unwrap_or_default().is_empty()
                {
                    break;
                }
            }
            tokio::time::sleep(SPACING).await;
        }
    }
}
