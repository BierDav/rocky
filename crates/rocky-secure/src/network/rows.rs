//! Row transfer: pulls by subscription, ranges by frontier, pushes, scope
//! lists and remote reads.
use super::*;

/// Frames in one transfer page.
pub(super) const MAX_SECURE_PAGE: usize = 32;

#[derive(Serialize, Deserialize)]
pub(super) enum RowMessage {
    MigrationQuery {
        publication: Commitment,
    },
    MigrationRecord {
        descriptor: Option<Box<rocky_credentials::SignedMigrationPublication>>,
    },
    Range {
        frontier: ScopeFrontier,
        after: Option<(PublicKey, u64)>,
        quota: usize,
    },
    RangeFrames {
        page: Box<RangePage>,
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
        voids: Vec<SignedVoid>,
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

/// What one catch-up pass of a scope did.
#[derive(Clone, Debug, Default)]
pub struct CatchUp {
    /// Optional base advice, returned before applying the accompanying frames.
    pub state: Option<crate::StateAdvice>,
    /// Frames received, including duplicates.
    pub frames: usize,
    pub pages: usize,
    /// This replica is behind the peer's pruned_prefix and must rebootstrap.
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
    /// Encoded bytes of the frames sent.
    pub bytes: usize,
    /// The peer is behind this replica's pruned_prefix and must rebootstrap from it.
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
    /// Exact lookup authorized by local followed-path or retained history requirements.
    pub async fn acquire_migration_publication<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        tip: rocky_core::TipId,
    ) -> Result<bool, Error> {
        if !replica.migration_tip_required(tip).await? {
            return Err(denied("migration publication is not required"));
        }
        let publication = rocky_core::commitment("rocky.sdk.migration.v3", &tip.0);
        self.send(RowMessage::MigrationQuery { publication })
            .await?;
        let RowMessage::MigrationRecord { descriptor } = self.reply::<RowMessage>().await? else {
            return Err(denied("unexpected migration descriptor response"));
        };
        let Some(descriptor) = descriptor else {
            return Ok(false);
        };
        if descriptor.statement.publication != publication || descriptor.statement.tip != tip {
            return Err(denied("unsolicited migration descriptor"));
        }
        replica.admit_migration_publication(*descriptor).await?;
        Ok(true)
    }
    async fn acquire_required_migrations<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<(), Error> {
        for tip in replica.missing_migration_publications().await? {
            if !self.acquire_migration_publication(replica, tip).await? {
                return Err(denied("required migration descriptor unavailable"));
            }
        }
        Ok(())
    }
    /// Pulls every frame of `scope` this replica lacks in bounded pages and
    /// pipelines the next request while applying each page. Stops at once when this
    /// replica is behind the peer's pruned_prefix.
    pub async fn catch_up<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<CatchUp, Error> {
        self.catch_up_with_state(replica, scope, quota, false).await
    }

    /// Stops before receiving a page with advice when `take_state` is set.
    /// The caller may import it or resume with advice disabled.
    pub async fn catch_up_with_state<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
        take_state: bool,
    ) -> Result<CatchUp, Error> {
        self.ensure_holding(replica, &[scope]).await?;
        self.acquire_required_migrations(replica).await?;
        let mut report = CatchUp::default();
        let mut after = None;
        // The frontier last reported as held; only peer bookkeeping follows
        // it, and a stale one only costs frames sent again.
        let mut held = None;
        let mut asked = false;
        loop {
            if !asked && self.low(quota) {
                report.exhausted = true;
                return Ok(report);
            }
            if !asked {
                let frontier = match held.take() {
                    Some(frontier) => frontier,
                    None => replica.frontier(scope).await?,
                };
                self.send(RowMessage::Range {
                    frontier,
                    after,
                    quota,
                })
                .await?;
            }
            let RowMessage::RangeFrames { page } = self.reply::<RowMessage>().await? else {
                return Err(denied("unexpected range response"));
            };
            if page.scope != scope
                || page.frames.len() + page.voids.len() > MAX_SECURE_PAGE
                || page.frontier.scope != scope
                || (page.behind && !(page.frames.is_empty() && page.voids.is_empty()))
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
            if take_state && let Some(state) = page.state {
                self.complete_frames(page.frames)?;
                report.state = Some(state);
                return Ok(report);
            }
            // Only a committed frontier is acknowledged. The cursor skips the
            // page being applied even though its frontier is not held yet.
            asked = page.next.is_some() && !self.low(quota);
            if asked {
                self.send(RowMessage::Range {
                    frontier: replica.frontier(scope).await?,
                    after: page.next,
                    quota,
                })
                .await?;
            }
            report.frames += page.frames.len();
            receive_voids(replica, &page.voids, &[scope]).await?;
            let (frames, incomplete) = self.complete_frames(page.frames)?;
            report.incomplete += incomplete;
            // The page is received in shared transactions and one flush.
            let span = tracing::debug_span!(
                "page_apply",
                scope = rocky_core::scope_label(&scope),
                frames = frames.len(),
                applied = tracing::field::Empty,
                outcome = tracing::field::Empty
            );
            let statuses = tracing::Instrument::instrument(
                replica.receive_migration_all(frames),
                span.clone(),
            )
            .await;
            let mut applied = 0u64;
            for status in statuses {
                match status {
                    Ok(ReceiveStatus::Applied) => applied += 1,
                    Ok(_) => {}
                    Err(error) => {
                        span.record("outcome", error.category());
                        return Err(error);
                    }
                }
            }
            span.record("applied", applied);
            span.record("outcome", "ok");
            let frontier = replica.frontier(scope).await?;
            report.lag = frontier.lag_behind(&page.frontier);
            held = Some(frontier.clone());
            if !asked {
                self.send(RowMessage::Held { frontier }).await?;
                self.acknowledged("unexpected acknowledgment response")
                    .await?;
            }
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
            if page.frames.is_empty() && page.voids.is_empty() {
                return Ok(pushed);
            }
            pushed.frames += page.frames.len();
            pushed.bytes = page.frames.iter().fold(pushed.bytes, |bytes, frame| {
                bytes.saturating_add(frame.weight())
            });
            self.carry_chains(&mut page.frames).await?;
            self.send(RowMessage::Push {
                scope,
                frames: page.frames,
                voids: page.voids,
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
        if !matches!(&message, RowMessage::MigrationQuery { .. }) {
            replica
                .inner
                .lock(crate::replica::Class::Background)
                .await
                .same_tip_peer(self.peer)?;
        }
        match message {
            RowMessage::MigrationQuery { publication } => {
                let inner = replica.inner.lock(crate::replica::Class::Background).await;
                self.credentials
                    .verify_selectors(
                        inner.config.publisher,
                        self.peer,
                        &inner.config.revocations,
                        now()?,
                    )
                    .map_err(credential)?;
                drop(inner);
                let descriptor = replica
                    .migration_publication(publication)
                    .await?
                    .map(Box::new);
                self.send(RowMessage::MigrationRecord { descriptor }).await
            }
            RowMessage::Range {
                frontier,
                after,
                quota,
            } => {
                if quota > super::MAX_PAGE_BYTES {
                    return Err(denied("native page quota"));
                }
                replica
                    .acknowledge_frontier(self.peer, &self.credentials, &frontier)
                    .await?;
                let mut page = replica
                    .range_page(self.peer, &self.credentials, &frontier, after, quota)
                    .await?;
                self.carry_chains(&mut page.frames).await?;
                self.send(RowMessage::RangeFrames {
                    page: Box::new(page),
                })
                .await
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
            RowMessage::Push {
                scope,
                frames,
                voids,
            } => {
                replica
                    .authorize_scope_peer(self.peer, &self.credentials, scope)
                    .await?;
                if frames.len() + voids.len() > MAX_SECURE_PAGE {
                    return Err(denied("native page quota"));
                }
                for frame in &frames {
                    if frame.scope()? != scope {
                        return Err(denied("pushed frame outside its scope"));
                    }
                }
                receive_voids(replica, &voids, &[scope]).await?;
                // An incomplete frame is not received; the frontier that
                // answers the push shows the sender what is still missing.
                let (frames, _) = self.complete_frames(frames)?;
                for status in replica.receive_migration_all(frames).await {
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

/// Admits the void records a page carries before its frames: each must be
/// of a requested scope. A refused record is skipped; the frames after it
/// keep waiting.
pub(super) async fn receive_voids<A: RowApplication>(
    replica: &EncryptedReplica<A>,
    voids: &[SignedVoid],
    scopes: &[ResourceScope],
) -> Result<(), Error> {
    if voids.len() > MAX_SECURE_PAGE {
        return Err(denied("native page quota"));
    }
    for void in voids {
        if !scopes.contains(&void.scope()?) {
            return Err(denied("void record outside the requested scopes"));
        }
    }
    for status in replica.receive_voids(voids).await {
        status?;
    }
    Ok(())
}

#[cfg(test)]
mod format_tests {
    use super::*;
    #[test]
    fn parent_transfer_pages_with_removed_metadata_are_refused() {
        #[derive(Serialize)]
        enum Parent {
            Pull {
                scopes: Vec<ResourceScope>,
                after: Option<Hash>,
                quota: usize,
                cuts: Vec<()>,
            },
            Page {
                frames: Vec<EncryptedFrame>,
                next: Option<Hash>,
                pruned: Vec<ResourceScope>,
                voids: Vec<SignedVoid>,
                cuts: Vec<()>,
            },
        }
        for message in [
            Parent::Pull {
                scopes: vec![],
                after: None,
                quota: 1024,
                cuts: vec![],
            },
            Parent::Page {
                frames: vec![],
                next: None,
                pruned: vec![],
                voids: vec![],
                cuts: vec![],
            },
        ] {
            let bytes = encode(&message).unwrap();
            assert!(decode_bounded::<RowMessage>(&bytes, MAX_SECURE_FRAME).is_err());
        }
    }
}
