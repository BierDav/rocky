//! Checkpoint offers and paged imports: bootstrap and rebootstrap of a scope.
use super::*;
use horizon::HorizonMessage;
use rows::{CatchUp, ScopeSync};

/// Bytes of one checkpoint page. A page carries its scope's strong state
/// whole, the latest confirmed command with its credential chains included,
/// so it takes the largest page a frame holds, leaving room for the framing.
const CHECKPOINT_PAGE: usize = MAX_SECURE_FRAME - 8192;

#[derive(Serialize, Deserialize)]
pub(super) enum CheckpointMessage {
    CheckpointOffer {
        scope: ResourceScope,
    },
    Checkpoint {
        signed: Option<Box<SignedCheckpoint>>,
    },
    CheckpointPull {
        scope: ResourceScope,
        checkpoint: Commitment,
        after: Option<Commitment>,
        quota: usize,
    },
    CheckpointRows {
        page: Box<CheckpointPage>,
    },
}

impl NativeSession {
    /// Imports the peer's checkpoint of a scope and switches to it. An
    /// interrupted import of the same checkpoint continues where it stopped.
    pub async fn rebootstrap<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<RebootstrapReport, Error> {
        self.rebootstrap_within(replica, scope, quota)
            .await?
            .ok_or_else(|| {
                denied("native session byte budget exhausted; continue on a new session")
            })
    }
    /// Like `rebootstrap`, but returns `None` once the session's budget runs
    /// low; the staged pages remain and a new session continues.
    async fn rebootstrap_within<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<Option<RebootstrapReport>, Error> {
        let signed = self
            .checkpoint_offer(scope)
            .await?
            .ok_or_else(|| denied("the peer offers no checkpoint"))?;
        self.import_within(replica, signed, quota).await
    }
    /// The peer's current checkpoint of `scope`, if it offers one.
    async fn checkpoint_offer(
        &mut self,
        scope: ResourceScope,
    ) -> Result<Option<Box<SignedCheckpoint>>, Error> {
        self.send(CheckpointMessage::CheckpointOffer { scope })
            .await?;
        let CheckpointMessage::Checkpoint { signed } = self.reply::<CheckpointMessage>().await?
        else {
            return Err(denied("unexpected checkpoint offer response"));
        };
        if signed
            .as_ref()
            .is_some_and(|signed| signed.statement.scope != scope)
        {
            return Err(denied("checkpoint outside subscription"));
        }
        Ok(signed)
    }
    async fn import_within<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        signed: Box<SignedCheckpoint>,
        quota: usize,
    ) -> Result<Option<RebootstrapReport>, Error> {
        let scope = signed.statement.scope;
        let checkpoint = signed.id()?;
        let finalized_checkpoint = signed.statement.finalized.is_some();
        if let Some(finalized) = &signed.statement.finalized {
            for base in &finalized.statement.bases {
                let expected = ExistenceBaseAttachment {
                    horizon: finalized.clone(),
                    scope: base.scope,
                };
                if !self
                    .pull_existence_base(replica, base.scope, Some(expected.id()?))
                    .await?
                {
                    return Ok(None);
                }
            }
        }
        let (mut after, mut complete) = match replica.rebootstrap_progress(scope).await? {
            Some((staged, cursor, complete)) if staged == checkpoint => (cursor, complete),
            _ => {
                replica.begin_rebootstrap(*signed).await?;
                (None, false)
            }
        };
        while !complete {
            if self.low(quota) {
                return Ok(None);
            }
            self.send(CheckpointMessage::CheckpointPull {
                scope,
                checkpoint,
                after,
                quota,
            })
            .await?;
            let CheckpointMessage::CheckpointRows { page } =
                self.reply::<CheckpointMessage>().await?
            else {
                return Err(denied("unexpected checkpoint response"));
            };
            if page.scope != scope || page.checkpoint != checkpoint || page.after != after {
                return Err(denied("checkpoint page outside request"));
            }
            (after, complete) = (page.next, page.next.is_none());
            replica.stage_checkpoint_page(*page).await?;
        }
        let report = replica.finish_rebootstrap(scope).await?;
        if finalized_checkpoint && let Some(signed) = replica.admitted_horizon(scope).await? {
            self.send(HorizonMessage::HorizonRejoined {
                scope,
                horizon: signed.id().map_err(credential)?,
            })
            .await?;
            self.acknowledged("unexpected horizon rebootstrap acknowledgment")
                .await?;
        }
        Ok(Some(report))
    }
    /// Starts from a verified checkpoint even when the peer has retained history,
    /// then catches up. Exhausted sessions leave durable import progress.
    /// `None` when the peer offers no checkpoint, as a peer without
    /// whole-scope read does not.
    pub async fn bootstrap_scope<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<Option<ScopeSync>, Error> {
        if self.low(MAX_SECURE_FRAME) {
            return Ok(Some(ScopeSync {
                rebootstrap: None,
                catch_up: CatchUp {
                    exhausted: true,
                    ..Default::default()
                },
            }));
        }
        let Some(signed) = self.checkpoint_offer(scope).await? else {
            return Ok(None);
        };
        let rebootstrap = self.import_within(replica, signed, CHECKPOINT_PAGE).await?;
        let catch_up = if rebootstrap.is_some() {
            self.catch_up(replica, scope, quota).await?
        } else {
            CatchUp {
                exhausted: true,
                ..Default::default()
            }
        };
        if catch_up.behind {
            return Err(denied("still behind the peer's horizon after bootstrap"));
        }
        Ok(Some(ScopeSync {
            rebootstrap,
            catch_up,
        }))
    }
    /// Rebootstraps `scope` from the peer if this replica is behind its
    /// horizon, then catches up.
    pub async fn sync_scope<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<ScopeSync, Error> {
        let mut catch_up = self.catch_up(replica, scope, quota).await?;
        let mut rebootstrap = None;
        if catch_up.behind && self.low(MAX_SECURE_FRAME) {
            catch_up.exhausted = true;
        } else if catch_up.behind {
            rebootstrap = self
                .rebootstrap_within(replica, scope, CHECKPOINT_PAGE)
                .await?;
            if rebootstrap.is_none() {
                catch_up.exhausted = true;
                return Ok(ScopeSync {
                    rebootstrap,
                    catch_up,
                });
            }
            catch_up = self.catch_up(replica, scope, quota).await?;
            if catch_up.behind {
                return Err(denied("still behind the peer's horizon after rebootstrap"));
            }
        }
        Ok(ScopeSync {
            rebootstrap,
            catch_up,
        })
    }
    pub(super) async fn answer_checkpoints<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: CheckpointMessage,
    ) -> Result<(), Error> {
        match message {
            CheckpointMessage::CheckpointOffer { scope } => {
                let signed = replica
                    .checkpoint_offer(self.peer, &self.credentials, scope)
                    .await?;
                self.send(CheckpointMessage::Checkpoint {
                    signed: signed.map(Box::new),
                })
                .await
            }
            CheckpointMessage::CheckpointPull {
                scope,
                checkpoint,
                after,
                quota,
            } => {
                // The page bounds its encoding, signed frame included, by `quota`;
                // the remainder covers the envelope and session signature.
                if quota > CHECKPOINT_PAGE {
                    return Err(denied("native page quota"));
                }
                let page = replica
                    .checkpoint_page(
                        self.peer,
                        &self.credentials,
                        scope,
                        checkpoint,
                        after,
                        quota,
                    )
                    .await?;
                self.send(CheckpointMessage::CheckpointRows {
                    page: Box::new(page),
                })
                .await
            }
            _ => Err(denied("unexpected native request")),
        }
    }
}
