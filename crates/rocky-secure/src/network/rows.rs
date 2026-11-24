use super::*;

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
    RevokedHeadsQuery {
        scope: ResourceScope,
        heads: Vec<Hash>,
    },
    RevokedHeads {
        heads: Vec<crate::wire::RevokedHead>,
    },
}

#[derive(Clone, Debug, Default)]
pub struct CatchUp {
    pub state: Option<crate::StateAdvice>,
    pub frames: usize,
    pub pages: usize,
    pub behind: bool,
    pub bytes: usize,
    pub lag: u64,
    pub exhausted: bool,
    pub incomplete: usize,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pushed {
    pub frames: usize,
    pub bytes: usize,
    pub behind: bool,
    pub exhausted: bool,
}
#[derive(Clone, Debug)]
pub struct ScopeSync {
    pub rebootstrap: Option<RebootstrapReport>,
    pub catch_up: CatchUp,
}

impl NativeSession {
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
    pub async fn catch_up<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
        quota: usize,
    ) -> Result<CatchUp, Error> {
        self.catch_up_with_state(replica, scope, quota, false).await
    }

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
        let mut last_reported_held = None;
        let mut asked = false;
        loop {
            if !asked && self.budget_low(quota) {
                report.exhausted = true;
                return Ok(report);
            }
            if !asked {
                let frontier = match last_reported_held.take() {
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
            replica
                .acknowledge_frontier(self.peer, &self.credentials, &page.frontier)
                .await?;
            if take_state && let Some(state) = page.state {
                self.complete_frames(page.frames)?;
                report.state = Some(state);
                return Ok(report);
            }
            asked = page.next.is_some() && !self.budget_low(quota);
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
            last_reported_held = Some(frontier.clone());
            if !asked {
                self.send(RowMessage::Held { frontier }).await?;
                self.acknowledged("unexpected acknowledgment response")
                    .await?;
            }
            after = page.next;
            if after.is_none() {
                replica.restored(scope).await?;
                self.resolve_revoked_heads(replica, scope).await?;
                return Ok(report);
            }
        }
    }
    async fn resolve_revoked_heads<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        let heads = replica.missing_heads(scope).await?;
        if heads.is_empty() {
            return Ok(());
        }
        self.send(RowMessage::RevokedHeadsQuery { scope, heads })
            .await?;
        let RowMessage::RevokedHeads { heads } = self.reply::<RowMessage>().await? else {
            return Err(denied("unexpected revoked heads response"));
        };
        replica.receive_revoked_heads(scope, heads).await
    }
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
            if self.budget_low(quota) {
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
                    .verified(
                        inner.config.publisher,
                        &inner.config.revocations,
                        now()?,
                        inner.whole_tables(),
                    )
                    .map_err(credential)?
                    .selectors(self.peer, &inner.config.revocations, now()?)
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
                let (frames, _missing_shown_by_answering_frontier) =
                    self.complete_frames(frames)?;
                for status in replica.receive_migration_all(frames.clone()).await {
                    status?;
                }
                let frontier = replica.frontier(scope).await?;
                self.send(RowMessage::Frontier { frontier }).await
            }
            RowMessage::RevokedHeadsQuery { scope, heads } => {
                let heads = replica
                    .revoked_heads(self.peer, &self.credentials, scope, &heads)
                    .await?;
                self.send(RowMessage::RevokedHeads { heads }).await
            }
            _ => Err(denied("unexpected native request")),
        }
    }
}

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
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
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
