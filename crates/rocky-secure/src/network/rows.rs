//! Row transfer: pulls by subscription, ranges by frontier, pushes, scope
//! lists and remote reads.
use super::*;
use horizon::HorizonMessage;

/// Frames in one transfer page.
pub(super) const MAX_SECURE_PAGE: usize = 32;

#[derive(Serialize, Deserialize)]
pub(super) enum RowMessage {
    Pull {
        scopes: Vec<ResourceScope>,
        after: Option<Hash>,
        quota: usize,
    },
    Page {
        frames: Vec<EncryptedFrame>,
        next: Option<Hash>,
        /// Subscribed scopes whose history is pruned at the sender.
        pruned: Vec<ResourceScope>,
    },
    Ack {
        id: Hash,
        applied: bool,
    },
    Range {
        frontier: ScopeFrontier,
        after: Option<(PublicKey, u64)>,
        quota: usize,
    },
    RangeFrames {
        page: RangePage,
    },
    /// The puller's frontier after a page, as acknowledgment.
    Held {
        frontier: ScopeFrontier,
    },
    FrontierQuery {
        scope: ResourceScope,
    },
    Frontier {
        frontier: ScopeFrontier,
    },
    Push {
        scope: ResourceScope,
        frames: Vec<EncryptedFrame>,
    },
    ScopesQuery {
        after: Option<ResourceScope>,
    },
    Scopes {
        scopes: Vec<ResourceScope>,
        next: Option<ResourceScope>,
    },
    Read {
        scope: ResourceScope,
        after: Option<Commitment>,
        max_rows: usize,
    },
    ReadRows {
        package: EncryptedPackage,
    },
}

/// The outcome of one pull.
#[derive(Clone, Debug)]
pub struct Pulled {
    pub next: Option<Hash>,
    /// Frames refused because the session never carried their chain.
    pub incomplete: usize,
    /// Scopes whose pruned history at the peer this replica does not hold.
    pub rebootstrap: Vec<ResourceScope>,
}
/// What one catch-up pass of a scope did.
#[derive(Clone, Debug, Default)]
pub struct CatchUp {
    /// Frames received, including duplicates.
    pub frames: usize,
    pub pages: usize,
    /// This replica is behind the peer's horizon and must rebootstrap.
    pub behind: bool,
    /// Encoded bytes of the frames received.
    pub bytes: usize,
    /// Applied operations the peer reported that this replica still lacks.
    pub lag: u64,
    /// The session's byte budget ran low; continue on a new session.
    pub exhausted: bool,
    /// Frames refused because the session never carried their chain; a
    /// pass on another session receives them.
    pub incomplete: usize,
}
/// What one push of a scope did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pushed {
    pub frames: usize,
    /// The peer is behind this replica's horizon and must rebootstrap from it.
    pub behind: bool,
    /// The session's byte budget ran low; continue on a new session.
    pub exhausted: bool,
}
/// One sync of a scope: rebootstrap if needed, then catch up.
#[derive(Clone, Debug)]
pub struct ScopeSync {
    pub rebootstrap: Option<RebootstrapReport>,
    pub catch_up: CatchUp,
}

impl NativeSession {
    /// Each pull supplies current subscriptions. Leaving a scope removes it from later pulls.
    /// A scope whose history the peer pruned past what this replica holds
    /// fails the pull; `pull_page` reports such scopes instead.
    pub async fn pull<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scopes: Vec<ResourceScope>,
        after: Option<Hash>,
        quota: usize,
    ) -> Result<Option<Hash>, Error> {
        let pulled = self.pull_page(replica, scopes, after, quota).await?;
        if !pulled.rebootstrap.is_empty() {
            return Err(denied(
                "behind the peer's pruning horizon; rebootstrap the scope",
            ));
        }
        Ok(pulled.next)
    }
    /// One pull. Frames of a scope this replica is behind the peer's horizon
    /// for are not received; the scope is returned in `rebootstrap`.
    pub async fn pull_page<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scopes: Vec<ResourceScope>,
        after: Option<Hash>,
        quota: usize,
    ) -> Result<Pulled, Error> {
        self.ensure_holding(replica, &scopes).await?;
        self.send(RowMessage::Pull {
            scopes: scopes.clone(),
            after,
            quota,
        })
        .await?;
        let RowMessage::Page {
            frames,
            next,
            pruned,
        } = self.reply::<RowMessage>().await?
        else {
            return Err(denied("unexpected native response"));
        };
        let mut rebootstrap = Vec::new();
        for scope in pruned {
            if !scopes.contains(&scope) {
                return Err(denied("native response outside subscription"));
            }
            let mut after = None;
            loop {
                self.send(HorizonMessage::Horizon { scope, after }).await?;
                let HorizonMessage::HorizonLogs { page } = self.reply::<HorizonMessage>().await?
                else {
                    return Err(denied("unexpected horizon response"));
                };
                if page.scope != scope || page.logs.len() > MAX_SECURE_PAGE * 8 {
                    return Err(denied("horizon outside subscription"));
                }
                if replica.behind_horizon(&page).await? {
                    rebootstrap.push(scope);
                    break;
                }
                after = page.next;
                if after.is_none() {
                    break;
                }
            }
        }
        let mut held = Vec::with_capacity(frames.len());
        for frame in frames {
            if !rebootstrap.contains(&frame.scope()?) {
                held.push(frame);
            }
        }
        let frames = held;
        if frames.len() > MAX_SECURE_PAGE
            || frames
                .iter()
                .fold(0usize, |sum, frame| sum.saturating_add(frame.weight()))
                > quota
        {
            return Err(denied("native page quota"));
        }
        let (frames, incomplete) = self.complete_frames(frames)?;
        let mut ids = Vec::with_capacity(frames.len());
        for frame in &frames {
            if !scopes.contains(&frame.scope()?) {
                return Err(denied("native response outside subscription"));
            }
            ids.push(frame.id()?);
        }
        // The page is received in shared transactions; acknowledgments follow the commit.
        for (id, status) in ids.into_iter().zip(replica.receive_all(frames).await) {
            let status = status?;
            let applied = status == ReceiveStatus::Applied
                || (status == ReceiveStatus::Duplicate
                    && replica.receipt_status(id).await? == ReceiveStatus::Applied);
            if status != ReceiveStatus::Rejected {
                self.send(RowMessage::Ack { id, applied }).await?;
                self.acknowledged("unexpected acknowledgment response")
                    .await?;
            }
        }
        Ok(Pulled {
            next,
            incomplete,
            rebootstrap,
        })
    }
    /// Pulls every frame of `scope` this replica lacks in bounded pages and
    /// acknowledges its frontier after each page. Stops at once when this
    /// replica is behind the peer's horizon.
    pub async fn catch_up<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<CatchUp, Error> {
        self.ensure_holding(replica, &[scope]).await?;
        self.relay_mergeable_horizon(replica, scope).await?;
        self.send(HorizonMessage::MergeableCut { scope }).await?;
        let HorizonMessage::MergeableState { signed, behind } =
            self.reply::<HorizonMessage>().await?
        else {
            return Err(denied("unexpected mergeable horizon response"));
        };
        if signed
            .as_ref()
            .is_some_and(|signed| signed.statement.scope != scope)
        {
            return Err(denied("mergeable horizon outside requested scope"));
        }
        if behind || replica.horizon_rebootstrap_required(scope).await? {
            if let Some(signed) = signed {
                replica.require_horizon_rebootstrap(*signed).await?;
            }
            return Ok(CatchUp {
                behind: true,
                ..Default::default()
            });
        }
        if let Some(signed) = signed
            && replica.receive_horizon((*signed).clone()).await.is_err()
        {
            replica.require_horizon_rebootstrap(*signed).await?;
            return Ok(CatchUp {
                behind: true,
                ..Default::default()
            });
        }
        let mut report = CatchUp::default();
        let mut after = None;
        loop {
            if self.low(quota) {
                report.exhausted = true;
                return Ok(report);
            }
            let frontier = replica.frontier(scope).await?;
            self.send(RowMessage::Range {
                frontier,
                after,
                quota,
            })
            .await?;
            let RowMessage::RangeFrames { page } = self.reply::<RowMessage>().await? else {
                return Err(denied("unexpected range response"));
            };
            if page.scope != scope
                || page.frames.len() > MAX_SECURE_PAGE
                || page.frontier.scope != scope
                || (page.behind && !page.frames.is_empty())
            {
                return Err(denied("range page outside request"));
            }
            let mut bytes = 0usize;
            for frame in &page.frames {
                bytes = bytes.saturating_add(frame.weight());
                report.bytes = report.bytes.saturating_add(frame.weight());
                if frame.scope()? != scope {
                    return Err(denied("native response outside subscription"));
                }
            }
            if bytes > quota {
                return Err(denied("native page quota"));
            }
            report.pages += 1;
            if page.behind {
                report.behind = true;
                return Ok(report);
            }
            report.frames += page.frames.len();
            let (frames, incomplete) = self.complete_frames(page.frames)?;
            report.incomplete += incomplete;
            // The page is received in shared transactions and one flush.
            for status in replica.receive_all(frames).await {
                status?;
            }
            let held = replica.frontier(scope).await?;
            report.lag = held.lag_behind(&page.frontier);
            self.send(RowMessage::Held { frontier: held }).await?;
            self.acknowledged("unexpected acknowledgment response")
                .await?;
            // The sender's own frontier is its acknowledgment of what it holds.
            replica
                .acknowledge_frontier(self.peer, &self.credentials, &page.frontier)
                .await?;
            after = page.next;
            if after.is_none() {
                replica.restored(scope).await?;
                return Ok(report);
            }
        }
    }
    /// Sends the peer every frame of `scope` it lacks, in bounded pages; the
    /// peer answers each page with its frontier, recorded as acknowledgment.
    pub async fn push<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<Pushed, Error> {
        if replica.horizon_rebootstrap_required(scope).await? {
            return Err(denied("phase-3 rebootstrap required before sync or write"));
        }
        let mut frontier = self.peer_frontier(replica, scope).await?;
        let mut after = None;
        let mut pushed = Pushed::default();
        loop {
            if self.low(quota) {
                pushed.exhausted = true;
                return Ok(pushed);
            }
            let mut page = replica
                .range_page(self.peer, &self.credentials, &frontier, after, quota)
                .await?;
            if page.behind {
                pushed.behind = true;
                return Ok(pushed);
            }
            if page.frames.is_empty() {
                return Ok(pushed);
            }
            pushed.frames += page.frames.len();
            self.carry_chains(&mut page.frames).await?;
            self.send(RowMessage::Push {
                scope,
                frames: page.frames,
            })
            .await?;
            let RowMessage::Frontier { frontier: theirs } = self.reply::<RowMessage>().await?
            else {
                return Err(denied("unexpected push response"));
            };
            if theirs.scope != scope {
                return Err(denied("frontier outside request"));
            }
            replica
                .acknowledge_frontier(self.peer, &self.credentials, &theirs)
                .await?;
            frontier = theirs;
            after = page.next;
            if after.is_none() {
                return Ok(pushed);
            }
        }
    }
    /// The peer's frontier of `scope`, recorded as its acknowledgment.
    pub async fn peer_frontier<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<ScopeFrontier, Error> {
        self.ensure_holding(replica, &[scope]).await?;
        self.send(RowMessage::FrontierQuery { scope }).await?;
        let RowMessage::Frontier { frontier } = self.reply::<RowMessage>().await? else {
            return Err(denied("unexpected frontier response"));
        };
        if frontier.scope != scope {
            return Err(denied("frontier outside request"));
        }
        replica
            .acknowledge_frontier(self.peer, &self.credentials, &frontier)
            .await?;
        Ok(frontier)
    }
    /// Scopes the peer holds and this replica may hold too, 64 per page.
    pub async fn peer_scopes(
        &mut self,
        after: Option<ResourceScope>,
    ) -> Result<(Vec<ResourceScope>, Option<ResourceScope>), Error> {
        self.send(RowMessage::ScopesQuery { after }).await?;
        let RowMessage::Scopes { scopes, next } = self.reply::<RowMessage>().await? else {
            return Err(denied("unexpected scope list response"));
        };
        if scopes.len() > 64
            || scopes.windows(2).any(|pair| pair[0] >= pair[1])
            || scopes
                .first()
                .is_some_and(|first| after.is_some_and(|after| *first <= after))
        {
            return Err(denied("scope list outside request"));
        }
        Ok((scopes, next))
    }
    /// Reads rows of a scope this replica does not hold from the peer. The
    /// page states the peer's revision, frontier and completeness and is
    /// not stored.
    pub async fn read_remote<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        after: Option<Commitment>,
        max_rows: usize,
    ) -> Result<RemotePage, Error> {
        self.ensure_holding(replica, &[scope]).await?;
        self.send(RowMessage::Read {
            scope,
            after,
            max_rows,
        })
        .await?;
        let RowMessage::ReadRows { package } = self.reply::<RowMessage>().await? else {
            return Err(denied("unexpected remote read response"));
        };
        let page = replica
            .open_remote_read(self.peer, scope, after, &package)
            .await?;
        if page.rows.len() > max_rows {
            return Err(denied("remote read outside request"));
        }
        Ok(page)
    }
    pub(super) async fn answer_rows<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        message: RowMessage,
    ) -> Result<(), Error> {
        match message {
            RowMessage::Pull {
                scopes,
                after,
                quota,
            } => {
                // Leave room for canonical envelope and session signature overhead.
                if quota > MAX_SECURE_FRAME / 2 {
                    return Err(denied("native page quota"));
                }
                for scope in &scopes {
                    if replica.horizon_rebootstrap_required(*scope).await? {
                        return Err(denied(
                            "phase-3 rebootstrap required before serving row sync",
                        ));
                    }
                    if replica
                        .horizon_peer_behind(self.peer, &self.credentials, *scope)
                        .await?
                    {
                        return Err(denied("phase-3 rebootstrap required before sync or write"));
                    }
                }
                let mut page = replica
                    .transfer_page(self.peer, &self.credentials, &scopes, after, quota)
                    .await?;
                for frame in &page.frames {
                    self.served_frames.insert(frame.id()?, frame.scope()?);
                }
                self.carry_chains(&mut page.frames).await?;
                self.send(RowMessage::Page {
                    frames: page.frames,
                    next: page.next,
                    pruned: page.pruned,
                })
                .await
            }
            RowMessage::Ack { id, applied } => {
                replica
                    .acknowledge(self.peer, &self.credentials, id, applied)
                    .await?;
                self.send(Message::Acknowledged).await
            }
            RowMessage::Range {
                frontier,
                after,
                quota,
            } => {
                if quota > MAX_SECURE_FRAME / 2 {
                    return Err(denied("native page quota"));
                }
                if replica.horizon_rebootstrap_required(frontier.scope).await? {
                    return Err(denied(
                        "phase-3 rebootstrap required before serving row sync",
                    ));
                }
                if replica
                    .horizon_peer_behind(self.peer, &self.credentials, frontier.scope)
                    .await?
                {
                    return Err(denied("phase-3 rebootstrap required before sync or write"));
                }
                let mut page = replica
                    .range_page(self.peer, &self.credentials, &frontier, after, quota)
                    .await?;
                for frame in &page.frames {
                    self.served_frames.insert(frame.id()?, frame.scope()?);
                }
                self.carry_chains(&mut page.frames).await?;
                self.send(RowMessage::RangeFrames { page }).await
            }
            RowMessage::Held { frontier } => {
                replica
                    .acknowledge_frontier(self.peer, &self.credentials, &frontier)
                    .await?;
                self.send(Message::Acknowledged).await
            }
            RowMessage::FrontierQuery { scope } => {
                replica
                    .authorize_scope_peer(self.peer, &self.credentials, scope)
                    .await?;
                let frontier = replica.frontier(scope).await?;
                self.send(RowMessage::Frontier { frontier }).await
            }
            RowMessage::Push { scope, frames } => {
                if replica
                    .horizon_peer_behind(self.peer, &self.credentials, scope)
                    .await?
                {
                    return Err(denied("phase-3 rebootstrap required before sync or write"));
                }
                replica
                    .authorize_scope_peer(self.peer, &self.credentials, scope)
                    .await?;
                if frames.len() > MAX_SECURE_PAGE {
                    return Err(denied("native page quota"));
                }
                for frame in &frames {
                    if frame.scope()? != scope {
                        return Err(denied("pushed frame outside its scope"));
                    }
                }
                // An incomplete frame is not received; the frontier that
                // answers the push shows the sender what is still missing.
                let (frames, _) = self.complete_frames(frames)?;
                for status in replica.receive_all(frames).await {
                    status?;
                }
                let frontier = replica.frontier(scope).await?;
                self.send(RowMessage::Frontier { frontier }).await
            }
            RowMessage::ScopesQuery { after } => {
                let (scopes, next) = replica
                    .held_scopes_proved(self.peer, &self.credentials, after, Some(&self.proved))
                    .await?;
                self.send(RowMessage::Scopes { scopes, next }).await
            }
            RowMessage::Read {
                scope,
                after,
                max_rows,
            } => {
                let package = replica
                    .remote_read(self.peer, &self.credentials, scope, after, max_rows)
                    .await?;
                self.send(RowMessage::ReadRows { package }).await
            }
            _ => Err(denied("unexpected native request")),
        }
    }
}
