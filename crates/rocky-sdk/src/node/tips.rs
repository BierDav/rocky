use super::*;
use rocky_core::{Scope, TipId};
use rocky_credentials::{
    BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, CredentialChain, GENESIS_LINE_FORMAT,
    GenesisLineStatement, SEAL_FORMAT, SealAction, SealStatement, SignedBookmarkMove,
    SignedGenesisLine, SignedSeal, SignedTipOpening, TipGraph, TipOpeningStatement,
};
use rocky_secure::TipStatement;

pub(crate) const BOOKMARK: &str = "main";
const GENESIS_MINIMUM_IN_SYNC: u32 = 1;

fn hex(digest: &rocky_core::Commitment) -> String {
    HEXLOWER.encode(digest)
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
    pub(crate) async fn follow_main(&self) -> Result<(), Failure> {
        let scope = self.domain().scope;
        self.create_bookmark(scope, BOOKMARK).await?;
        self.replica
            .follow_bookmark(scope, BOOKMARK)
            .await
            .map_err(refused)?;
        self.ensure_tip_node().await?;
        self.sign_genesis_minimum(scope).await?;
        self.observations.touch();
        Ok(())
    }

    pub(super) async fn follow_main_when_held(&self) -> Result<(), Failure> {
        if self.replica.followed_bookmark().await.is_some() {
            return Ok(());
        }
        let scope = self.domain().scope;
        if self
            .replica
            .tip_graph(scope)
            .await?
            .bookmark(BOOKMARK)?
            .is_none()
        {
            return Ok(());
        }
        if let Some(start) = self.replica.missing_bookmark_start(scope, BOOKMARK).await? {
            self.replica.want_seal(scope, start).await?;
            self.wake_sync();
            return Ok(());
        }
        let before = self.replica.followed_tip().await;
        self.replica
            .follow_bookmark(scope, BOOKMARK)
            .await
            .map_err(refused)?;
        self.followed_tip_changed(before).await;
        self.ensure_tip_node().await?;
        self.observations.touch();
        Ok(())
    }

    pub(super) async fn install_trust(&self, grant: &str) -> Result<(), Failure> {
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
        Ok(())
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
    ) -> Result<SignedSeal, Failure> {
        let graph = self.replica.tip_graph(scope).await?;
        if self.replica.followed_tip().await != tip {
            return Err(refused("only a device following a tip seals it"));
        }
        if graph.seal(tip)?.is_some() {
            return Err(refused("this tip is already sealed"));
        }
        let action = SealAction::Continue {
            schema: graph.schema(tip)?,
        };
        let frontier = self
            .replica
            .seal_frontier(scope, false)
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

    pub(super) async fn seal_followed(&self, scope: Scope) -> Result<(), Failure> {
        let (followed, name) = self
            .replica
            .followed_bookmark()
            .await
            .filter(|(followed, _)| *followed == scope)
            .ok_or_else(|| refused("this device follows no tip yet"))?;
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
        let seal = self.seal_statement(&signer, scope, tip).await?;
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
        self.seal_followed(scope).await?;
        self.replica.compact(scope, 0).await?;
        self.list_checkpoints(scope).await
    }

    async fn sign_genesis_minimum(&self, scope: Scope) -> Result<(), Failure> {
        let statement = GenesisLineStatement {
            format: GENESIS_LINE_FORMAT,
            scope,
            minimum_in_sync: GENESIS_MINIMUM_IN_SYNC,
        };
        let signer = self
            .hold_chain(self.tip_signer(scope).await?, scope)
            .await?;
        let line = signer.endorse::<SignedGenesisLine>(statement)?;
        self.publish_tips(
            scope,
            Some(signer),
            vec![TipStatement::GenesisLine(line)],
            |graph| match graph.line_minimum(graph.genesis())? {
                Some(_) => Ok(()),
                None => Err(refused("the genesis line's minimum was not set")),
            },
        )
        .await
    }

    async fn create_bookmark(&self, scope: Scope, name: &str) -> Result<(), Failure> {
        let tip = self.replica.followed_tip().await;
        let signer = self
            .hold_chain(self.tip_signer(scope).await?, scope)
            .await?;
        let statement = Self::bookmark_move(&signer, scope, name, None, Some(tip))?;
        self.publish_tips(scope, Some(signer), vec![statement], |graph| {
            if graph.bookmark(name)? != Some(tip) {
                return Err(refused("the bookmark did not take effect"));
            }
            Ok(())
        })
        .await
    }

    pub(super) async fn finalize_seals(&self) {
        if self.replica.followed_bookmark().await.is_none() {
            return;
        }
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
}
