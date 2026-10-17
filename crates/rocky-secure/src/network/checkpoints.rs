//! Checkpoint offers and paged imports: bootstrap and rebootstrap of a scope.
use super::*;
use rows::{CatchUp, ScopeSync};

/// Bytes of one checkpoint page: the largest a frame holds, leaving room
/// for the framing.
pub(super) const CHECKPOINT_PAGE: usize = MAX_SECURE_FRAME - 8192;

#[derive(Serialize, Deserialize)]
pub(super) enum CheckpointMessage {
    Pruned {
        scope: ResourceScope,
        after: Option<PublicKey>,
    },
    PrunedLogs {
        page: PrunedPage,
    },
    ArchiveProgress {
        after: Option<Commitment>,
        complete: bool,
    },
    ArchiveBegin {
        signed: Box<SignedCheckpoint>,
    },
    ArchivePage {
        page: Box<CheckpointPage>,
    },
    CheckpointOffer {
        scope: ResourceScope,
    },
    /// The peer's checkpoint of a scope at a seal it keeps.
    SealOffer {
        scope: ResourceScope,
        seal: Commitment,
    },
    Checkpoint {
        signed: Option<Box<SignedCheckpoint>>,
        /// The cut records the checkpoint's binding names.
        cuts: Vec<CutEvidence>,
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

/// How pulling a seal's state from one peer ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealPull {
    /// This node holds the seal's state now.
    Held,
    /// The peer keeps no state there for some scope.
    Unavailable,
    /// The session's byte budget ran low; staged pages remain.
    Exhausted,
}

impl NativeSession {
    /// Pulls `seal`'s state from the peer for every held scope of the
    /// followed domain that lacks it: the peer's base there, or a
    /// checkpoint it signs of the seal it keeps. Scopes the peer cannot
    /// serve are left to another peer. Nothing changes in the rows;
    /// following a tip whose path passes the seal installs it.
    pub async fn pull_seal<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        seal: Commitment,
    ) -> Result<SealPull, Error> {
        let Some((domain, _)) = replica.followed_bookmark().await else {
            return Err(denied("follow a bookmark before pulling a seal"));
        };
        let mut missing = false;
        for scope in replica.seal_pull_scopes(domain, seal).await? {
            self.ensure_holding(replica, &[scope]).await?;
            self.send(CheckpointMessage::SealOffer { scope, seal })
                .await?;
            let Some(signed) = self.offered(replica, scope).await? else {
                missing = true;
                continue;
            };
            if signed.statement.seal != Some(seal) {
                return Err(denied("the peer offers another seal's checkpoint"));
            }
            let checkpoint = signed.id()?;
            let (after, complete) = match replica.rebootstrap_progress(scope).await? {
                Some((staged, cursor, complete)) if staged == checkpoint => (cursor, complete),
                _ => {
                    replica.begin_seal_pull(*signed).await?;
                    (None, false)
                }
            };
            if !self
                .fetch_pages(
                    replica,
                    scope,
                    checkpoint,
                    (after, complete),
                    CHECKPOINT_PAGE,
                )
                .await?
            {
                return Ok(SealPull::Exhausted);
            }
            replica.finish_seal_pull(scope).await?;
        }
        Ok(if missing {
            SealPull::Unavailable
        } else {
            SealPull::Held
        })
    }
    /// Pulls from the peer every seal this replica wants. Returns false
    /// once the session's budget runs low.
    pub async fn pull_wanted_seals<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<bool, Error> {
        let Some((domain, _)) = replica.followed_bookmark().await else {
            return Ok(true);
        };
        for seal in replica.wanted_seals(domain).await? {
            if self.pull_seal(replica, seal).await? == SealPull::Exhausted {
                return Ok(false);
            }
        }
        Ok(true)
    }
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
        self.ensure_holding(replica, &[scope]).await?;
        let signed = self
            .checkpoint_offer(replica, scope)
            .await?
            .ok_or_else(|| denied("the peer offers no checkpoint"))?;
        self.import_within(replica, signed, quota).await
    }
    /// Moves every held scope of the followed domain whose replay base left
    /// the followed path, or was pruned at another cut, to a seal of the
    /// path: it imports the peer's checkpoint when the graph lists it
    /// there, else replays frames, from the peer when it pruned them and the
    /// peer pruned none. A scope neither can rebuild waits. Returns false
    /// once the session's budget runs low.
    pub async fn rebootstrap_at_seals<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<bool, Error> {
        let Some((domain, _)) = replica.followed_bookmark().await else {
            return Ok(true);
        };
        let needs = replica.seal_rebootstraps(domain).await?;
        if needs.is_empty()
            && replica
                .compaction_status(domain)
                .await?
                .rebootstrap
                .is_none()
        {
            return Ok(true);
        }
        for need in needs {
            let scope = need.scope;
            if !need.listed.is_empty() {
                self.ensure_holding(replica, &[scope]).await?;
                if let Some(signed) = self.checkpoint_offer(replica, scope).await?
                    && need.listed.contains(&signed.id()?)
                {
                    if self
                        .import_within(replica, signed, CHECKPOINT_PAGE)
                        .await?
                        .is_none()
                        || (need.logged && !self.pull_existence_base(replica, scope, None).await?)
                    {
                        return Ok(false);
                    }
                    continue;
                }
            }
            match need.replay {
                FrameReplay::Local => replica.replay_at_seals(scope, false).await?,
                FrameReplay::Peers if self.retains_history(replica, scope).await? => {
                    replica.replay_at_seals(scope, true).await?
                }
                FrameReplay::Peers | FrameReplay::Unavailable => {}
            }
        }
        replica.finish_seal_rebootstrap(domain).await?;
        Ok(true)
    }
    /// Whether the peer pruned no frame of `scope`.
    async fn retains_history<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        self.ensure_holding(replica, &[scope]).await?;
        self.send(CheckpointMessage::Pruned { scope, after: None })
            .await?;
        let CheckpointMessage::PrunedLogs { page } = self.reply::<CheckpointMessage>().await?
        else {
            return Err(denied("unexpected pruned_prefix response"));
        };
        if page.scope != scope {
            return Err(denied("pruned_prefix outside subscription"));
        }
        Ok(page.logs.is_empty())
    }
    /// The checkpoint the peer offers for each scope, in order, with its
    /// stored rows; `None` where it offers none this replica may import:
    /// what a bootstrap plans with before it imports anything.
    pub async fn offered_rows<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scopes: &[ResourceScope],
    ) -> Result<Vec<Option<(Commitment, u64)>>, Error> {
        self.ensure_holding(replica, scopes).await?;
        let mut offered = Vec::with_capacity(scopes.len());
        for scope in scopes {
            offered.push(match self.checkpoint_offer(replica, *scope).await? {
                Some(signed)
                    if signed.statement.seal.is_some()
                        || !replica.owns_checkpoint_signer(&signed).await =>
                {
                    Some((signed.id()?, signed.statement.rows.count))
                }
                _ => None,
            });
        }
        Ok(offered)
    }
    /// The peer's current checkpoint of `scope`, if it offers one.
    async fn checkpoint_offer<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<Option<Box<SignedCheckpoint>>, Error> {
        self.send(CheckpointMessage::CheckpointOffer { scope })
            .await?;
        self.offered(replica, scope).await
    }
    /// The answer to an offer of `scope`.
    async fn offered<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<Option<Box<SignedCheckpoint>>, Error> {
        let CheckpointMessage::Checkpoint { signed, cuts } =
            self.reply::<CheckpointMessage>().await?
        else {
            return Err(denied("unexpected checkpoint offer response"));
        };
        if signed
            .as_ref()
            .is_some_and(|signed| signed.statement.scope != scope)
        {
            return Err(denied("checkpoint outside subscription"));
        }
        super::rows::receive_cuts(replica, &cuts, &[scope]).await?;
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
        let progress = match replica.rebootstrap_progress(scope).await? {
            Some((staged, cursor, complete)) if staged == checkpoint => (cursor, complete),
            _ => {
                replica.begin_rebootstrap(*signed).await?;
                (None, false)
            }
        };
        if !self
            .fetch_pages(replica, scope, checkpoint, progress, quota)
            .await?
        {
            return Ok(None);
        }
        let report = replica.finish_rebootstrap(scope).await?;
        Ok(Some(report))
    }
    /// Stages the pages of `checkpoint` after `after` until it is
    /// complete. Returns false once the session's budget runs low.
    async fn fetch_pages<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        checkpoint: Commitment,
        (mut after, mut complete): (Option<Commitment>, bool),
        quota: usize,
    ) -> Result<bool, Error> {
        // Whether the page after `after` is already asked for.
        let mut asked = false;
        while !complete {
            if !asked {
                if self.low(quota) {
                    return Ok(false);
                }
                self.send(CheckpointMessage::CheckpointPull {
                    scope,
                    checkpoint,
                    after,
                    quota,
                })
                .await?;
            }
            let CheckpointMessage::CheckpointRows { page } =
                self.reply::<CheckpointMessage>().await?
            else {
                return Err(denied("unexpected checkpoint response"));
            };
            if page.scope != scope || page.checkpoint != checkpoint || page.after != after {
                return Err(denied("checkpoint page outside request"));
            }
            (after, complete) = (page.next, page.next.is_none());
            // The peer cuts the next page while this one is verified and
            // stored: a page costs the slower of the two, not their sum.
            asked = !complete && !self.low(quota);
            if asked {
                self.send(CheckpointMessage::CheckpointPull {
                    scope,
                    checkpoint,
                    after,
                    quota,
                })
                .await?;
            }
            replica.stage_checkpoint_page(*page).await?;
        }
        Ok(true)
    }
    /// Serves until the session ends or an answer fails; returns how many
    /// checkpoint pages the peer asked for.
    #[cfg(test)]
    pub(crate) async fn serve_counting_pages<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> usize {
        let mut pages = 0;
        while let Ok(message) = self.answered().await {
            pages += usize::from(matches!(
                message,
                Message::Checkpoints(CheckpointMessage::CheckpointPull { .. })
            ));
            if self.answer(replica, message).await.is_err() {
                break;
            }
        }
        pages
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
        self.ensure_holding(replica, &[scope]).await?;
        let Some(signed) = self.checkpoint_offer(replica, scope).await? else {
            return Ok(None);
        };
        if signed.statement.seal.is_none() && replica.owns_checkpoint_signer(&signed).await {
            return Ok(None);
        }
        // Switching to the installed checkpoint again would only rewrite its rows.
        let installed = replica.checkpoint_installed(&signed).await?;
        let rebootstrap = match installed {
            true => None,
            false => self.import_within(replica, signed, CHECKPOINT_PAGE).await?,
        };
        let catch_up = if installed || rebootstrap.is_some() {
            self.catch_up(replica, scope, quota).await?
        } else {
            CatchUp {
                exhausted: true,
                ..Default::default()
            }
        };
        if catch_up.behind {
            return Err(denied(
                "still behind the peer's pruned_prefix after bootstrap",
            ));
        }
        Ok(Some(ScopeSync {
            rebootstrap,
            catch_up,
        }))
    }
    /// Rebootstraps `scope` from the peer if this replica is behind its
    /// pruned_prefix, then catches up.
    pub async fn sync_scope<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<ScopeSync, Error> {
        let held_exhausted = if replica.tip_statements(None, 1).await?.is_empty() {
            false
        } else {
            self.sync_held_changes(replica, scope, quota).await?
        };
        let mut catch_up = self.catch_up(replica, scope, quota).await?;
        catch_up.exhausted |= held_exhausted;
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
                return Err(denied(
                    "still behind the peer's pruned_prefix after rebootstrap",
                ));
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
            CheckpointMessage::Pruned { scope, after } => {
                let page = replica
                    .pruned_page(self.peer, &self.credentials, scope, after)
                    .await?;
                self.send(CheckpointMessage::PrunedLogs { page }).await
            }
            CheckpointMessage::ArchiveBegin { signed } => {
                let (after, complete) = replica
                    .begin_checkpoint_archive(self.peer, &self.credentials, *signed)
                    .await?;
                self.send(CheckpointMessage::ArchiveProgress { after, complete })
                    .await
            }
            CheckpointMessage::ArchivePage { page } => {
                replica
                    .archive_checkpoint_page(self.peer, &self.credentials, *page)
                    .await?;
                self.send(Message::Acknowledged).await
            }
            CheckpointMessage::CheckpointOffer { scope } => {
                let signed = replica
                    .checkpoint_offer(self.peer, &self.credentials, scope)
                    .await?;
                let named = signed
                    .as_ref()
                    .map_or(0, |signed| signed.statement.coordinated.cuts);
                let mut cuts = replica.cuts_for(&[scope], &[]).await?;
                cuts.retain(|cut| cut.record.statement.sequence <= named);
                self.send(CheckpointMessage::Checkpoint {
                    signed: signed.map(Box::new),
                    cuts,
                })
                .await
            }
            CheckpointMessage::SealOffer { scope, seal } => {
                let signed = replica
                    .seal_checkpoint_offer(self.peer, &self.credentials, scope, seal)
                    .await?;
                let named = signed
                    .as_ref()
                    .map_or(0, |signed| signed.statement.coordinated.cuts);
                let mut cuts = replica.cuts_for(&[scope], &[]).await?;
                cuts.retain(|cut| cut.record.statement.sequence <= named);
                self.send(CheckpointMessage::Checkpoint {
                    signed: signed.map(Box::new),
                    cuts,
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
