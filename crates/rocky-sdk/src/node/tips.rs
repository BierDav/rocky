use super::*;
use crate::tips::{
    Bookmark, BookmarkMove, KeptSeals, LosingMove, TipAuthorMark, TipCheckpoint, TipCompaction,
    TipDevice, TipFrontierLog, TipInspection, TipNode, TipOpening, TipPruneBlock, TipRecovery,
    TipScopeFrontier, TipSeal, TipStatus,
};
use rocky_core::{Scope, TipId};
use rocky_credentials::{
    BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, Coordination, CredentialChain,
    GENESIS_LINE_FORMAT, GenesisLineStatement, SEAL_FORMAT, SealAction, SealStatement,
    SignedBookmarkMove, SignedDatabaseSettings, SignedGenesisLine, SignedSeal, SignedTipOpening,
    TIP_OPENING_FORMAT, TipGraph, TipOpeningStatement,
};
use rocky_secure::TipStatement;

fn hex(digest: &rocky_core::Commitment) -> String {
    HEXLOWER.encode(digest)
}
fn parse_digest(text: &str) -> Result<rocky_core::Commitment, Failure> {
    HEXLOWER
        .decode(text.as_bytes())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| refused("expected a hex digest"))
}

pub(super) struct Signer {
    pub(super) node: PrivateKey,
    pub(super) identity: PublicKey,
    pub(super) credentials: CredentialChain,
    pub(super) links: CredentialChain,
    opens: bool,
}
pub(super) struct HeldSigner {
    signer: Signer,
    chain: Option<Chain>,
}
impl std::ops::Deref for HeldSigner {
    type Target = Signer;
    fn deref(&self) -> &Signer {
        &self.signer
    }
}
pub(super) struct Chain {
    _held: tokio::sync::OwnedMutexGuard<()>,
    next: std::sync::Mutex<rocky_credentials::ChainLink>,
    known: Vec<TipStatement>,
    branch: std::sync::atomic::AtomicBool,
}
pub(super) trait Linked: Sized {
    type Statement: rocky_credentials::OpensStage;
    fn endorse_linked(
        statement: Self::Statement,
        signer: rocky_credentials::Signer<'_>,
        chain: &CredentialChain,
        link: Option<rocky_credentials::ChainLink>,
    ) -> Result<Self, rocky_credentials::Error>;
    fn id(&self) -> Result<rocky_core::Commitment, rocky_credentials::Error>;
    fn known_link(
        known: &TipStatement,
        body: &Self::Statement,
    ) -> Option<rocky_credentials::ChainLink>;
}
macro_rules! linked {
    ($($signed:ty => $statement:ty, $variant:ident),*) => {$(
        impl Linked for $signed {
            type Statement = $statement;
            fn known_link(
                known: &TipStatement,
                body: &$statement,
            ) -> Option<rocky_credentials::ChainLink> {
                match known {
                    TipStatement::$variant(signed) if signed.statement == *body => signed.link,
                    _ => None,
                }
            }
            fn endorse_linked(
                statement: $statement,
                signer: rocky_credentials::Signer<'_>,
                chain: &CredentialChain,
                link: Option<rocky_credentials::ChainLink>,
            ) -> Result<Self, rocky_credentials::Error> {
                <$signed>::endorse_linked(statement, signer, chain, link)
            }
            fn id(&self) -> Result<rocky_core::Commitment, rocky_credentials::Error> {
                <$signed>::id(self)
            }
        }
    )*};
}
linked!(
    SignedSeal => SealStatement, Seal,
    SignedTipOpening => TipOpeningStatement, Opening,
    SignedBookmarkMove => BookmarkMoveStatement, Bookmark,
    SignedGenesisLine => GenesisLineStatement, GenesisLine,
    rocky_credentials::SignedCheckpointRecord => rocky_credentials::CheckpointRecordStatement, Checkpoint
);
impl HeldSigner {
    pub(super) fn endorse<T: Linked>(&self, statement: T::Statement) -> Result<T, Failure> {
        if let Some(link) = self.chain.as_ref().and_then(|chain| {
            chain
                .known
                .iter()
                .find_map(|known| T::known_link(known, &statement))
        }) {
            return Ok(T::endorse_linked(
                statement,
                rocky_credentials::Signer::node(&self.node, &self.credentials)?,
                &self.links,
                Some(link),
            )?);
        }
        let mut next = self
            .chain
            .as_ref()
            .map(|chain| chain.next.lock().unwrap_or_else(PoisonError::into_inner));
        let mut link = next.as_deref().copied();
        if let (Some(chain), Some(live)) = (self.chain.as_ref(), link.as_mut())
            && chain
                .branch
                .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            live.stage = Some(
                rocky_credentials::OpensStage::opened_stage(&statement)?
                    .ok_or("a staged schema switch signs its seal first")?,
            );
        }
        let signed = T::endorse_linked(
            statement,
            rocky_credentials::Signer::node(&self.node, &self.credentials)?,
            &self.links,
            link,
        )?;
        if let (Some(next), Some(link)) = (next.as_mut(), link) {
            **next = link.next(signed.id()?);
        }
        Ok(signed)
    }
}

impl Node {
    pub(crate) async fn follow_new_bookmark(&self, name: &str) -> Result<(), Failure> {
        use crate::request::TipChange;
        let root = super::Root::default();
        self.change_tip(
            TipChange::Create {
                name: name.into(),
                tip: None,
            },
            &root,
        )
        .await?;
        self.change_tip(TipChange::Follow { name: name.into() }, &root)
            .await?;
        Ok(())
    }

    pub(super) async fn change_tip(
        &self,
        change: crate::request::TipChange,
        root: &super::Root,
    ) -> Result<TipStatus, Failure> {
        use crate::request::TipChange;
        let scope = self.domain().scope;
        match change {
            TipChange::Status => {}
            TipChange::Seal { close } => self.seal_followed(scope, close).await?,
            TipChange::Recover => self.recover_followed(scope, root).await?,
            TipChange::Pull { seal } => {
                self.replica
                    .want_seal(scope, parse_digest(&seal)?)
                    .await
                    .map_err(refused)?;
                self.wake_sync();
            }
            TipChange::GenesisMinimum { minimum_in_sync } => {
                self.sign_genesis_minimum(scope, minimum_in_sync, root)
                    .await?;
            }
            TipChange::DeclareCoordinated => self.declare_coordinated(scope, root).await?,
            TipChange::Create { name, tip } => {
                let tip = tip.as_deref().map(parse_digest).transpose()?.map(TipId);
                self.create_bookmark(scope, &name, tip).await?;
            }
            TipChange::Move { name, tip } => {
                self.move_bookmark(scope, &name, Some(TipId(parse_digest(&tip)?)))
                    .await?;
            }
            TipChange::Delete { name } => self.move_bookmark(scope, &name, None).await?,
            TipChange::FollowWhenReady { name } => {
                let inspection = self.inspect_tips(scope).await?;
                if !inspection
                    .bookmarks
                    .iter()
                    .any(|bookmark| bookmark.name == name && bookmark.tip.is_some())
                {
                    return Err(refused("unknown or deleted bookmark"));
                }
                self.configure(|settings| {
                    settings.pending_follow = Some(name);
                    Ok(())
                })
                .await?;
                self.complete_pending_follow().await?;
                self.wake_sync();
            }
            TipChange::Follow { name } => {
                let before = self.replica.followed_tip().await;
                self.replica
                    .follow_bookmark(scope, &name)
                    .await
                    .map_err(refused)?;
                self.configure(|settings| {
                    settings.pending_follow = None;
                    Ok(())
                })
                .await?;
                self.followed_tip_changed(before).await;
                self.ensure_tip_node().await?;
                self.observations.touch();
            }
            TipChange::Drop { tip } => {
                self.replica
                    .drop_tip(TipId(parse_digest(&tip)?))
                    .await
                    .map_err(refused)?;
                self.observations.touch();
            }
            TipChange::InstallTrust { grant } => {
                let link = HEXLOWER
                    .decode(grant.as_bytes())
                    .ok()
                    .and_then(|bytes| {
                        rocky_credentials::decode_bounded::<rocky_credentials::SignedCapability>(
                            &bytes,
                            rocky_credentials::MAX_CREDENTIAL_BYTES,
                        )
                        .ok()
                    })
                    .ok_or_else(|| refused("grant must be a hex trusted grant"))?;
                let mut settings = self.settings.lock().await;
                let mut draft = (**settings).clone();
                draft.install_trust(&self.vault, link).map_err(refused)?;
                settings.commit(Shared::new(draft), &self.replica).await?;
                self.apply(&mut settings, Vec::new()).await?;
                drop(settings);
                self.observations.touch();
                self.wake_sync();
            }
        }
        self.tip_status(scope).await
    }

    pub(super) async fn set_kept_seals(&self, config: KeptSeals) -> Result<KeptSeals, Failure> {
        self.replica
            .keep_seals(rocky_secure::KeptSeals {
                bookmarked: config.bookmarked,
                latest: config.latest,
                every: config.every,
            })
            .await?;
        self.observations.touch();
        self.kept_seals().await
    }

    pub(super) async fn compact_tips(&self, keep: usize) -> Result<TipCompaction, Failure> {
        let scope = self.domain().scope;
        let report = self.replica.compact(scope, keep).await?;
        self.list_checkpoints(scope).await?;
        self.replica.drop_unrequired_schemas().await?;
        self.observations.touch();
        let blocked = report
            .blocked
            .iter()
            .map(|(scope, block)| {
                Ok(TipPruneBlock {
                    scope: *scope,
                    reason: format!("{block:?}"),
                })
            })
            .collect::<Result<_, Failure>>()?;
        self.compaction(scope, report.pruned, blocked).await
    }

    pub(super) async fn trust_identity(
        &self,
        identity: PublicKey,
        root: &super::Root,
    ) -> Result<crate::TrustGrant, Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let (link, changes) = self
            .settings
            .lock()
            .await
            .trust(&registry, identity, root.signer())
            .map_err(|error| -> Failure {
                if error.is::<crate::enrollment::PublisherRequired>() {
                    error
                } else {
                    refused(error)
                }
            })?;
        crate::registry::write(&self.replica, self.domain().registry(), &changes).await?;
        self.observations.touch();
        self.wake_sync();
        Ok(crate::TrustGrant {
            identity: identity.to_hex(),
            capability: hex(&link.id()?),
            grant: HEXLOWER.encode(&rocky_credentials::encode(&link)?),
        })
    }

    pub(super) async fn complete_pending_follow(&self) -> Result<(), Failure> {
        let Some(name) = self.settings.lock().await.pending_follow.clone() else {
            return Ok(());
        };
        let scope = self.domain().scope;
        let inspection = self.inspect_tips(scope).await?;
        let Some(tip) = inspection
            .bookmarks
            .iter()
            .find(|bookmark| bookmark.name == name)
            .and_then(|bookmark| bookmark.tip.as_ref())
        else {
            self.configure(|settings| {
                settings.pending_follow = None;
                Ok(())
            })
            .await?;
            self.activity
                .record(crate::ActivityKind::BookmarkFollowFailed {
                    name: (name).to_owned(),
                    reason: ("deleted").to_owned(),
                });
            return Ok(());
        };
        let seal = inspection
            .seals
            .iter()
            .find(|seal| seal.id == *tip && seal.canonical && !seal.revoked)
            .or_else(|| {
                inspection
                    .openings
                    .iter()
                    .find(|opening| opening.id == *tip)
                    .and_then(|opening| {
                        inspection
                            .seals
                            .iter()
                            .find(|seal| seal.id == opening.seal && seal.canonical && !seal.revoked)
                    })
            });
        if let Some(seal) = seal
            && seal.held.is_none()
        {
            let digest = HEXLOWER
                .decode(seal.id.as_bytes())?
                .try_into()
                .map_err(|_| refused("invalid seal digest"))?;
            self.replica.want_seal(scope, digest).await?;
            return Ok(());
        }
        let before = self.replica.followed_tip().await;
        self.replica.follow_bookmark(scope, &name).await?;
        self.configure(|settings| {
            if settings.pending_follow.as_deref() == Some(&name) {
                settings.pending_follow = None;
            }
            Ok(())
        })
        .await?;
        self.followed_tip_changed(before).await;
        self.ensure_tip_node().await?;
        self.observations.touch();
        Ok(())
    }

    pub(super) async fn tip_status(&self, scope: Scope) -> Result<TipStatus, Failure> {
        let observed = self.replica.tip_observation(scope).await?;
        Ok(TipStatus {
            bookmark: observed.bookmark,
            pending_follow: self.settings.lock().await.pending_follow.clone(),
            tip: hex(&observed.tip.0),
            operations_since_seal: observed.operations_since_seal,
            evidence_releasable: None,
            devices: observed
                .devices
                .into_iter()
                .map(|device| {
                    Ok(TipDevice {
                        node: device.node.to_hex(),
                        tip: device.tip.map(|tip| hex(&tip.0)),
                        frontiers: device
                            .frontiers
                            .into_iter()
                            .map(|frontier| {
                                Ok(TipScopeFrontier {
                                    scope: frontier.scope,
                                    logs: frontier
                                        .logs
                                        .into_iter()
                                        .map(|log| TipFrontierLog {
                                            author: log.author.to_hex(),
                                            held: log.held,
                                            applied: log.applied,
                                        })
                                        .collect(),
                                })
                            })
                            .collect::<Result<_, Failure>>()?,
                    })
                })
                .collect::<Result<_, Failure>>()?,
        })
    }

    pub(super) async fn compaction(
        &self,
        scope: Scope,
        pruned: u64,
        blocked: Vec<TipPruneBlock>,
    ) -> Result<TipCompaction, Failure> {
        let status = self.replica.compaction_status(scope).await?;
        Ok(TipCompaction {
            finalized: status.finalized.as_ref().map(hex),
            compacted: status.compacted.as_ref().map(hex),
            uncompacted: status.uncompacted.iter().map(hex).collect(),
            waiting: status.waiting,
            rebootstrap: status.rebootstrap.as_ref().map(hex),
            pruned,
            blocked,
        })
    }

    pub(super) async fn kept_seals(&self) -> Result<KeptSeals, Failure> {
        let config = self.replica.kept_seal_config().await?;
        Ok(KeptSeals {
            bookmarked: config.bookmarked,
            latest: config.latest,
            every: config.every,
        })
    }

    pub(super) async fn inspect_tips(&self, scope: Scope) -> Result<TipInspection, Failure> {
        let graph = self.replica.tip_graph(scope).await?;
        let mut held = std::collections::BTreeMap::new();
        for seal in &graph.seals {
            let id = seal.id()?;
            if let Some(how) = self.replica.held_seal(scope, id).await? {
                held.insert(id, how);
            }
        }
        let seals = graph
            .seals
            .iter()
            .map(|seal| {
                let statement = &seal.statement;
                Ok(TipSeal {
                    id: hex(&seal.id()?),
                    tip: hex(&statement.tip.0),
                    frontier: statement
                        .frontier
                        .iter()
                        .map(|mark| TipAuthorMark {
                            author: mark.author,
                            sequence: mark.sequence,
                        })
                        .collect(),
                    action: match statement.action {
                        SealAction::Continue { .. } => crate::TipSealAction::Continue,
                        SealAction::Close => crate::TipSealAction::Close,
                    },
                    schema: match statement.action {
                        SealAction::Continue { schema } => Some(hex(&schema)),
                        SealAction::Close => None,
                    },
                    canonical: graph.seal(statement.tip)? == Some(seal),
                    authority: graph.authority(seal.endorsement()),
                    revoked: graph.revoked.contains(&seal.id()?),
                    finalized: graph.is_final(seal.id()?)?,
                    checkpoints: graph
                        .checkpoints_at(seal.id()?)?
                        .into_iter()
                        .map(|record| {
                            Ok(TipCheckpoint {
                                id: hex(&record.id()?),
                                scope: record.statement.table,
                                checkpoint: hex(&record.statement.checkpoint),
                                signer: record.statement.signer.to_hex(),
                                authority: graph.authority(record.endorsement()),
                                rows: record.statement.rows,
                            })
                        })
                        .collect::<Result<_, Failure>>()?,
                    held: held.get(&seal.id()?).copied(),
                    recovery: statement.recovery.map(|recovery| TipRecovery {
                        retained: recovery.retained.position,
                        replaced: recovery.replaced.position,
                    }),
                })
            })
            .collect::<Result<_, Failure>>()?;
        let openings = graph
            .openings
            .iter()
            .map(|opening| {
                Ok(TipOpening {
                    id: hex(&opening.id()?),
                    seal: hex(&opening.statement.seal),
                    schema: hex(&opening.statement.schema),
                    minimum_in_sync: opening.statement.minimum_in_sync,
                })
            })
            .collect::<Result<_, Failure>>()?;
        let names = graph
            .moves
            .iter()
            .map(|movement| movement.statement.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let bookmarks = names
            .into_iter()
            .map(|name| {
                Ok(Bookmark {
                    tip: graph.bookmark(&name)?.map(|tip| hex(&tip.0)),
                    moves: graph
                        .moves
                        .iter()
                        .filter(|movement| movement.statement.name == name)
                        .map(|movement| {
                            Ok(BookmarkMove {
                                id: hex(&movement.id()?),
                                from: movement.statement.from.map(|tip| hex(&tip.0)),
                                to: movement.statement.to.map(|tip| hex(&tip.0)),
                                authority: graph.authority(movement.endorsement()),
                                revoked: graph.revoked.contains(&movement.id()?),
                            })
                        })
                        .collect::<Result<_, Failure>>()?,
                    losers: graph
                        .losing_moves(&name)?
                        .into_iter()
                        .enumerate()
                        .map(|(index, movement)| {
                            Ok(LosingMove {
                                name: format!("{name}/{}", index + 1),
                                movement: hex(&movement.id()?),
                                tip: movement.statement.to.map(|tip| hex(&tip.0)),
                            })
                        })
                        .collect::<Result<_, Failure>>()?,
                    name,
                })
            })
            .collect::<Result<_, Failure>>()?;
        let nodes = self
            .replica
            .tip_nodes(scope)
            .await?
            .into_iter()
            .map(|node| TipNode {
                node: node.node.to_hex(),
                followed_tip: node.followed_tip.map(|tip| hex(&tip.0)),
                kept_tips: node
                    .kept_tips
                    .map(|tips| tips.iter().map(|tip| hex(&tip.0)).collect()),
                kept_seals: node.kept_seals.map(|seals| seals.iter().map(hex).collect()),
            })
            .collect();
        Ok(TipInspection {
            scope,
            genesis: hex(&graph.genesis().0),
            schema: hex(&graph.genesis_schema),
            genesis_minimum: graph
                .line_minimum(graph.genesis())?
                .and_then(|minimum| u32::try_from(minimum).ok()),
            coordinator: graph
                .coordinator()?
                .map(|bundle| crate::tips::TipCoordinator {
                    root: HEXLOWER.encode(&bundle.identity.root),
                    genesis: hex(&bundle.identity.genesis),
                    url: bundle.url.clone(),
                }),
            seals,
            openings,
            bookmarks,
            nodes,
        })
    }

    async fn followed_tip_changed(&self, before: TipId) {
        if self.replica.followed_tip().await != before {
            self.wake_sync();
        }
    }

    pub(super) async fn list_checkpoints(&self, scope: Scope) -> Result<(), Failure> {
        let unlisted = self.replica.unlisted_checkpoints(scope).await?;
        if unlisted.is_empty() {
            return Ok(());
        }
        let Ok(signer) = self.tip_signer(scope).await else {
            return Ok(());
        };
        let signer = self.hold_chain(signer, scope).await?;
        let records = unlisted
            .into_iter()
            .map(|statement| {
                Ok(TipStatement::Checkpoint(Box::new(
                    signer.endorse(statement)?,
                )))
            })
            .collect::<Result<Vec<_>, Failure>>()?;
        self.replica.publish_tip_statements(&records).await?;
        self.wake_sync();
        Ok(())
    }

    pub(super) async fn tip_signer(&self, scope: Scope) -> Result<Signer, Failure> {
        let settings = self.settings.lock().await.clone();
        if let Some(statement) = self.revocation_notice.revoked(&settings).await? {
            return Err(Box::new(crate::enrollment::Revoked(statement)));
        }
        let node = Settings::node_key(&self.vault)?;
        let identity = settings.identity;
        let (links, opens) = if let Some(trusted) = settings.trusted_links().filter(|links| {
            links.0[0]
                .capability
                .extended()
                .is_ok_and(|e| e.trusted == Some(scope))
        }) {
            (trusted, true)
        } else if settings.issuer() {
            (settings.issuer_links(), false)
        } else {
            return Err(refused(
                "only a trusted identity or an issuer signs tip statements",
            ));
        };
        Ok(Signer {
            node,
            identity,
            credentials: settings.credentials.clone(),
            links,
            opens,
        })
    }
    pub(super) async fn hold_chain(
        &self,
        signer: Signer,
        scope: Scope,
    ) -> Result<HeldSigner, Failure> {
        if !signer.opens {
            return Ok(HeldSigner {
                signer,
                chain: None,
            });
        }
        let held = self.tip_chain.clone().lock_owned().await;
        let lane = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        let (next, known) = self.replica.own_chain(scope, signer.identity, lane).await?;
        Ok(HeldSigner {
            signer,
            chain: Some(Chain {
                _held: held,
                next: std::sync::Mutex::new(next),
                known,
                branch: std::sync::atomic::AtomicBool::new(false),
            }),
        })
    }

    pub(super) async fn hold_stage_chain(
        &self,
        signer: Signer,
        scope: Scope,
    ) -> Result<HeldSigner, Failure> {
        let mut held = self.hold_chain(signer, scope).await?;
        if let Some(chain) = held.chain.as_mut() {
            *chain.branch.get_mut() = true;
            chain.known.clear();
        }
        Ok(held)
    }
    async fn publish_tips(
        &self,
        scope: Scope,
        signer: Option<HeldSigner>,
        statements: Vec<TipStatement>,
        expected: impl FnOnce(&TipGraph) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let mut graph = self.replica.tip_graph(scope).await?;
        for statement in &statements {
            statement.append(&mut graph);
        }
        expected(&graph)?;
        let before = self.replica.followed_tip().await;
        self.prepare_terminal_credentials().await?;
        let published = self.replica.publish_tip_statements(&statements).await;
        drop(signer);
        self.wake_sync();
        published?;
        self.finalize_seals().await;
        self.followed_tip_changed(before).await;
        self.ensure_tip_node().await?;
        self.observations.touch();
        Ok(())
    }

    pub(super) fn bookmark_move(
        signer: &HeldSigner,
        scope: Scope,
        name: &str,
        from: Option<TipId>,
        to: Option<TipId>,
    ) -> Result<TipStatement, Failure> {
        Ok(TipStatement::Bookmark(
            signer.endorse::<SignedBookmarkMove>(BookmarkMoveStatement {
                format: BOOKMARK_MOVE_FORMAT,
                scope,
                name: name.into(),
                from,
                to,
            })?,
        ))
    }

    async fn seal_statement(
        &self,
        signer: &HeldSigner,
        scope: Scope,
        tip: TipId,
        close: bool,
    ) -> Result<SignedSeal, Failure> {
        let graph = self.replica.tip_graph(scope).await?;
        if self.replica.followed_tip().await != tip {
            return Err(refused(
                "only a device following a tip seals it; follow its bookmark first",
            ));
        }
        if graph.seal(tip)?.is_some() {
            return Err(refused("this tip is already sealed"));
        }
        let action = if close {
            SealAction::Close
        } else {
            SealAction::Continue {
                schema: graph.schema(tip)?,
            }
        };
        let frontier = self
            .replica
            .seal_frontier(scope, close)
            .await
            .map_err(refused)?;
        signer.endorse(SealStatement {
            format: SEAL_FORMAT,
            scope,
            tip,
            frontier,
            action,
            recovery: None,
        })
    }

    pub(super) async fn seal_followed(&self, scope: Scope, close: bool) -> Result<(), Failure> {
        let (followed, name) = self
            .replica
            .followed_bookmark()
            .await
            .filter(|(followed, _)| *followed == scope)
            .ok_or_else(|| refused("follow a bookmark before sealing"))?;
        let tip = self.replica.followed_tip().await;
        if self.replica.tip_graph(followed).await?.bookmark(&name)? != Some(tip) {
            return Err(refused(
                "the followed bookmark no longer points to the followed tip",
            ));
        }
        let signer = self
            .hold_chain(self.tip_signer(scope).await?, scope)
            .await?;
        self.replica.publish_missing_outcomes().await?;
        let seal = self.seal_statement(&signer, scope, tip, close).await?;
        let mut statements = vec![TipStatement::Seal(seal.clone())];
        let successor = seal.successor()?;
        if let Some(next) = successor {
            statements.push(Self::bookmark_move(
                &signer,
                scope,
                &name,
                Some(tip),
                Some(next),
            )?);
        }
        self.publish_tips(scope, Some(signer), statements, |graph| {
            if graph.bookmark(&name)? != successor.or(Some(tip)) {
                return Err(refused("another move of this bookmark takes precedence"));
            }
            Ok(())
        })
        .await
    }

    pub(super) async fn seal_for_members(&self) -> Result<(), Failure> {
        let scope = self.domain().scope;
        if self.replica.followed_bookmark().await.is_none() || self.tip_signer(scope).await.is_err()
        {
            return Ok(());
        }
        self.seal_followed(scope, false).await?;
        self.replica.compact(scope, 0).await?;
        self.list_checkpoints(scope).await
    }

    async fn recover_followed(&self, scope: Scope, root: &super::Root) -> Result<(), Failure> {
        let settings = self.settings.lock().await.clone();
        let root = crate::enrollment::root_signer(
            root.signer(),
            settings.publisher,
            "recovering a stuck tip",
        )?;
        let coordinator = self
            .replica
            .domain_log()
            .ok_or_else(|| refused("recovering needs this device's coordinated log composed"))?;
        let tip = self.replica.followed_tip().await;
        let graph = self.replica.tip_graph(scope).await?;
        if graph.seal(tip)?.is_some() {
            return Err(refused("this tip is already sealed"));
        }
        let log = rocky_core::TipLog { scope, tip };
        if graph.line_minimum(tip)?.is_none() {
            return Err(refused(
                "set the genesis line's owner-signed minimum before recovery",
            ));
        }
        let recovery = self
            .replica
            .recovery_proposal(log, coordinator.as_ref())
            .await?;
        let frontier = self
            .replica
            .seal_frontier(scope, false)
            .await
            .map_err(refused)?;
        let seal = SignedSeal::sign(
            SealStatement {
                format: SEAL_FORMAT,
                scope,
                tip,
                frontier,
                action: SealAction::Continue {
                    schema: graph.schema(tip)?,
                },
                recovery: Some(recovery),
            },
            root,
        )?;
        let id = seal.id()?;
        self.publish_tips(
            scope,
            None,
            vec![TipStatement::Seal(seal)],
            |graph| match graph.seal(tip)? {
                Some(winner) if winner.id()? == id => Ok(()),
                _ => Err(refused("another seal of this tip takes precedence")),
            },
        )
        .await
    }

    async fn sign_genesis_minimum(
        &self,
        scope: Scope,
        minimum: u32,
        root: &super::Root,
    ) -> Result<(), Failure> {
        let statement = GenesisLineStatement {
            format: GENESIS_LINE_FORMAT,
            scope,
            minimum_in_sync: minimum,
        };
        let takes_effect = |graph: &TipGraph| match graph.line_minimum(graph.genesis())? {
            Some(current) if current == minimum as usize => Ok(()),
            Some(current) => Err(refused(format!(
                "the genesis line's minimum stays {current}: the root's minimum governs, \
                 and a trusted node only raises it"
            ))),
            None => Err(refused("the genesis line's minimum was not set")),
        };
        if root.signer().is_some() {
            let settings = self.settings.lock().await.clone();
            let root = crate::enrollment::root_signer(
                root.signer(),
                settings.publisher,
                "signing the genesis line's minimum",
            )?;
            let line = SignedGenesisLine::sign(statement, root)?;
            return self
                .publish_tips(
                    scope,
                    None,
                    vec![TipStatement::GenesisLine(line)],
                    takes_effect,
                )
                .await;
        }
        let signer = self.tip_signer(scope).await?;
        if !signer.opens {
            return Err(refused(
                "only a trusted node or the root signs the genesis line's minimum",
            ));
        }
        let signer = self.hold_chain(signer, scope).await?;
        let line = signer.endorse::<SignedGenesisLine>(statement)?;
        self.publish_tips(
            scope,
            Some(signer),
            vec![TipStatement::GenesisLine(line)],
            takes_effect,
        )
        .await
    }

    pub(crate) async fn publish_settings(
        &self,
        signed: SignedDatabaseSettings,
    ) -> Result<(), Failure> {
        let id = signed.id()?;
        self.publish_tips(
            self.domain().scope,
            None,
            vec![TipStatement::Settings(signed)],
            |graph| match graph
                .settings
                .iter()
                .any(|held| held.id().is_ok_and(|held| held == id))
            {
                true => Ok(()),
                false => Err(refused("the database settings did not take effect")),
            },
        )
        .await
    }
    pub(super) async fn declare_coordinated(
        &self,
        scope: Scope,
        root: &super::Root,
    ) -> Result<(), Failure> {
        let coordinator = self.coordinator().ok_or_else(|| {
            refused("compose the coordinator before naming it in the database's settings")
        })?;
        let bundle = coordinator.bundle()?;
        let graph = self.replica.tip_graph(scope).await?;
        if graph
            .coordinator()?
            .is_some_and(|named| named.identity != bundle.identity)
        {
            return Err(super::coordinated::coordination_refusal(
                rocky_secure::Refusal::CoordinatorMismatch(
                    "the database's settings name another coordinator; changing it is not supported".into(),
                ),
            ));
        }
        if graph.coordinator()?.is_none()
            && graph
                .finality_coordinators()?
                .iter()
                .any(|named| *named != bundle.identity)
        {
            return Err(super::coordinated::coordination_refusal(
                rocky_secure::Refusal::CoordinatorMismatch(
                    "the database's finality records name another coordinator; compose and name that one".into(),
                ),
            ));
        }
        let Some(statement) = graph.next_settings(Coordination::Coordinated { bundle })? else {
            return Ok(());
        };
        let settings = self.settings.lock().await.clone();
        let root = crate::enrollment::root_signer(
            root.signer(),
            settings.publisher,
            "signing the database's settings",
        )?;
        let signed = SignedDatabaseSettings::sign(statement, root)?;
        self.publish_settings(signed).await
    }
    async fn prepare_terminal_credentials(&self) -> Result<(), Failure> {
        if self.replica.domain_log().is_none() {
            return Ok(());
        }
        let settings = self.settings.lock().await.clone();
        let Some(links) = settings.trusted_links() else {
            return Ok(());
        };
        let (node, _) = self.replica.terminal_author().await?;
        let certificate = settings
            .credentials
            .certificate()
            .filter(|certificate| certificate.capability.subject == node)
            .ok_or("missing certificate for the composed log's node")?;
        let chain = links.with_certificate(certificate.clone());
        self.replica.install_terminal_credentials(chain).await?;
        Ok(())
    }

    pub(super) async fn finalize_seals(&self) {
        if self.replica.followed_bookmark().await.is_some() {
            match self.replica.finalize_arrived(self.domain().scope).await {
                Ok(0) => {}
                Ok(_) => self.observations.touch(),
                Err(error) => {
                    self.activity
                        .record(crate::ActivityKind::FinalizationFailed {
                            error: (error.to_string()).to_owned(),
                        });
                }
            }
        }
        if self.replica.domain_log().is_some() {
            if let Err(error) = self.prepare_terminal_credentials().await {
                self.activity
                    .record(crate::ActivityKind::TerminalCredentialsFailed {
                        error: (error.to_string()).to_owned(),
                    });
            }
            let before = self.replica.followed_tip().await;
            match self.replica.settle_tip_statements().await {
                Ok(0) => {}
                Ok(_) => {
                    self.followed_tip_changed(before).await;
                    if let Err(error) = self.ensure_tip_node().await {
                        self.activity.record(crate::ActivityKind::LineNodeFailed {
                            error: (error.to_string()).to_owned(),
                        });
                    }
                    self.observations.touch();
                }
                Err(error) => {
                    self.activity
                        .record(crate::ActivityKind::SealSettlementFailed {
                            error: (error.to_string()).to_owned(),
                        });
                }
            }
            match self.replica.finalize_seals(self.domain().scope).await {
                Ok(finalization) => {
                    if finalization.admitted > 0 {
                        self.observations.touch();
                    }
                    self.finality_stalled(finalization.stalled.map(crate::FinalityStall::of));
                }
                Err(rocky_secure::Error::Incompatible(error)) => {
                    self.finality_stalled(Some(crate::FinalityStall::UnsupportedFormat { error }));
                }
                Err(error) => {
                    self.activity.record(crate::ActivityKind::FinalityFailed {
                        error: (error.to_string()).to_owned(),
                    });
                }
            }
        }
        if let Err(error) = self.compact_final_seal().await {
            self.activity
                .record(crate::ActivityKind::FinalCompactionFailed {
                    error: (error.to_string()).to_owned(),
                });
        }
    }

    fn finality_stalled(&self, reason: Option<crate::FinalityStall>) {
        let mut stalled = self
            .finality_stalled
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let same = match (&*stalled, &reason) {
            (None, None) => true,
            (
                Some(crate::Alert {
                    kind: crate::AlertKind::FinalityStalled { reason: held },
                    ..
                }),
                Some(reason),
            ) => held == reason,
            _ => false,
        };
        if same {
            return;
        }
        *stalled = reason.map(|reason| {
            tracing::warn!(%reason, "a seal stays unfinal");
            crate::Alert {
                kind: crate::AlertKind::FinalityStalled { reason },
                since: self.status_time().ok(),
            }
        });
        drop(stalled);
        self.observations.touch();
    }
    async fn compact_final_seal(&self) -> Result<(), Failure> {
        let scope = self.domain().scope;
        let graph = self.replica.tip_graph(scope).await?;
        if graph.finalities.is_empty() {
            return Ok(());
        }
        self.replica.finalize_path(scope).await?;
        let status = self.replica.compaction_status(scope).await?;
        let Some(seal) = status.finalized else {
            return Ok(());
        };
        if !graph.compacts_final(seal)? {
            return Ok(());
        }
        if status.compacted != Some(seal) || self.replica.needs_base_at(scope, seal).await? {
            self.replica.compact(scope, 0).await?;
        }
        self.list_checkpoints(scope).await?;
        Ok(())
    }

    pub(super) async fn open_tip(
        &self,
        scope: Scope,
        seal: rocky_core::Commitment,
        minimum_in_sync: u32,
    ) -> Result<TipId, Failure> {
        let graph = self.replica.tip_graph(scope).await?;
        let parent = graph.seal_by_id(seal).map_err(refused)?;
        let schema = graph.schema(parent.statement.tip).map_err(refused)?;
        let signer = self.tip_signer(scope).await?;
        if !signer.opens {
            return Err(refused("only the owner or a trusted identity opens tips"));
        }
        let signer = self.hold_chain(signer, scope).await?;
        self.replica.finalize_path(scope).await?;
        if self.replica.held_seal(scope, seal).await?.is_none() {
            return Err(refused(
                "this device does not hold the seal's state; pull it from a device that keeps it with the tips action pull, or seal now",
            ));
        }
        let opening = signer.endorse::<SignedTipOpening>(TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            schema,
            scope,
            seal,
            minimum_in_sync,
        })?;
        let tip = opening.tip()?;
        self.publish_tips(
            scope,
            Some(signer),
            vec![TipStatement::Opening(opening)],
            |_| Ok(()),
        )
        .await?;
        Ok(tip)
    }

    async fn create_bookmark(
        &self,
        scope: Scope,
        name: &str,
        tip: Option<TipId>,
    ) -> Result<(), Failure> {
        let graph = self.replica.tip_graph(scope).await?;
        if graph.bookmark(name)?.is_some() {
            return Err(refused("this bookmark exists; move it instead"));
        }
        let tip = match tip {
            Some(tip) => tip,
            None => self.replica.followed_tip().await,
        };
        let target = graph.canonical_tip(tip).map_err(refused)?;
        let signer = self
            .hold_chain(self.tip_signer(scope).await?, scope)
            .await?;
        let statement = Self::bookmark_move(&signer, scope, name, None, Some(tip))?;
        self.publish_tips(scope, Some(signer), vec![statement], |graph| {
            if graph.bookmark(name)? != Some(target) {
                return Err(refused(
                    "this bookmark name was used before; choose another name",
                ));
            }
            Ok(())
        })
        .await
    }

    async fn move_bookmark(
        &self,
        scope: Scope,
        name: &str,
        to: Option<TipId>,
    ) -> Result<(), Failure> {
        let graph = self.replica.tip_graph(scope).await?;
        let from = graph
            .bookmark(name)?
            .ok_or_else(|| refused("this bookmark does not exist"))?;
        let target = to
            .map(|tip| graph.canonical_tip(tip))
            .transpose()
            .map_err(refused)?;
        if target == Some(from) {
            return Err(refused("the bookmark already points to this tip"));
        }
        let signer = self
            .hold_chain(self.tip_signer(scope).await?, scope)
            .await?;
        let mut statements = Vec::new();
        if let Some(to) = to
            && !graph.continues_any(from, to)?
            && graph.seal(from)?.is_none()
        {
            let close = self.seal_statement(&signer, scope, from, true).await?;
            statements.push(TipStatement::Seal(close));
        }
        statements.push(Self::bookmark_move(&signer, scope, name, Some(from), to)?);
        self.publish_tips(scope, Some(signer), statements, |graph| {
            if graph.bookmark(name)? != target {
                return Err(refused("another move of this bookmark takes precedence"));
            }
            Ok(())
        })
        .await
    }
}
