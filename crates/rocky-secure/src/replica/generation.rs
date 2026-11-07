//! Installed schema identity and tip-authenticated catalog lookups.
use super::*;
use rocky_core::{ApplicationManifest, Generation};

pub(super) const FILE_IDENTITY: &str = "rocky.encrypted-replica.v58";
pub(super) fn identity(tip: TipId, node: PublicKey) -> Result<Vec<u8>, Error> {
    encode(&(FILE_IDENTITY, tip, node))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationStatus {
    pub manifest: Commitment,
    pub current: Generation,
    pub plan: Option<super::migrate::ProjectionPlan>,
    pub engine: EngineStatus,
}
/// How this build's replay engine stands to the tips it follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineStatus {
    /// The followed tip names this build's engine.
    Current,
    /// The owner moved the bookmark to a tip on another engine: this build
    /// stays read-only at the old tip until Rocky is upgraded.
    UpgradeRequired,
    /// The followed tip names another engine: this build is read-only until
    /// the owner opens a tip with its engine. Identities are not ordered,
    /// so an older build that enrolled at a newer tip reports this too.
    AwaitingOwner,
}
pub(super) struct Definition<A> {
    pub(super) app: Arc<A>,
    pub(super) permission_fields: HashSet<Commitment>,
    pub(super) config: SecureConfig,
    pub(super) chain: Chain,
    pub(super) node_author: Option<identity::NodeAuthor>,
    pub(super) history: Vec<KeyMaterial>,
    pub(super) manifest: Commitment,
    pub(super) tip: TipId,
    pub(super) definition: ApplicationManifest,
    pub(super) tables: HashSet<Commitment>,
    pub(super) fields: HashSet<Commitment>,
    pub(super) domains: HashSet<rocky_core::DomainId>,
}
impl<A> Definition<A> {
    pub(super) fn swap(&mut self, inner: &mut Inner<A>) {
        inner.invalidate_context();
        std::mem::swap(&mut self.app, &mut inner.app);
        std::mem::swap(&mut self.permission_fields, &mut inner.permission_fields);
        std::mem::swap(&mut self.config, &mut inner.config);
        std::mem::swap(&mut self.chain, &mut inner.chain);
        std::mem::swap(&mut self.node_author, &mut inner.node_author);
        std::mem::swap(&mut self.history, &mut inner.history);
        std::mem::swap(&mut self.manifest, &mut inner.manifest);
        std::mem::swap(&mut self.tip, &mut inner.workspace.tip);
        std::mem::swap(&mut self.definition, &mut inner.definition);
        std::mem::swap(&mut self.tables, &mut inner.tables);
        std::mem::swap(&mut self.fields, &mut inner.fields);
        std::mem::swap(&mut self.domains, &mut inner.domains);
        // Grants resolved under the other definition are stale.
        *inner
            .local
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *inner
            .local_selectors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

/// Why a build whose engine differs from the followed tip's writes nothing.
pub(super) const ENGINE_AWAITS_OWNER: &str = "this build's replay engine differs from the followed tip's; replayed tables are read-only until the owner opens a tip with this engine";
/// Why a build does not follow a tip whose schema names another engine.
pub(super) const ENGINE_UPGRADE_REQUIRED: &str =
    "the tip's schema names another replay engine; upgrade Rocky to follow it";

impl<A: RowApplication> Inner<A> {
    /// Whether this build may replay the followed tip's intents: the
    /// installed schema names its engine. Otherwise the workspace follows
    /// read-only, and incoming frames wait as for a missing schema.
    pub(super) fn engine_matches(&self) -> bool {
        self.definition.engine == self.engine
    }
    async fn engine_status(&self) -> Result<EngineStatus, Error> {
        let pending = match rows(
            &self.conn,
            "SELECT manifest FROM secure_pending_schema WHERE id=1",
        )
        .await?
        .first()
        .map(Vec::as_slice)
        {
            Some([SqlValue::Blob(manifest)]) => Some(checkpoint::row_of(manifest)?),
            None => None,
            _ => return Err(Error::Corrupt("pending schema switch".into())),
        };
        let pending = pending.and_then(|manifest| self.catalog.get(&manifest));
        Ok(
            if pending.is_some_and(|pending| pending.engine != self.engine) {
                EngineStatus::UpgradeRequired
            } else if !self.engine_matches() {
                EngineStatus::AwaitingOwner
            } else {
                EngineStatus::Current
            },
        )
    }
    pub(crate) fn generation(&self) -> Generation {
        Generation {
            schema: self.definition.schema,
        }
    }
    pub(crate) fn admits(&self, manifest: Commitment) -> Result<(), Error> {
        self.known(manifest)
    }
    pub(crate) fn known(&self, manifest: Commitment) -> Result<(), Error> {
        if manifest == self.manifest || self.catalog.contains_key(&manifest) {
            return Ok(());
        }
        Err(Error::Incompatible("tip schema is not installed".into()))
    }
    pub(crate) fn generation_of(
        &self,
        manifest: Commitment,
        _admitting: bool,
    ) -> Result<Generation, Error> {
        if manifest == self.manifest {
            return Ok(self.generation());
        }
        self.catalog
            .get(&manifest)
            .map(|definition| Generation {
                schema: definition.schema,
            })
            .ok_or_else(|| Error::Incompatible("tip schema is not installed".into()))
    }
    /// Verifies a chain under the held revocations. A chain names no
    /// generation: every generation admits it.
    pub(crate) fn verify_chain(
        &self,
        chain: &CredentialChain,
        key: PublicKey,
        time: u64,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        self.verify_chain_with(chain, key, &self.config.revocations, time)
    }
    /// Verifies current authority, resolving selectors only for requested scopes.
    pub(crate) fn verify_chain_scopes(
        &self,
        chain: &CredentialChain,
        key: PublicKey,
        time: u64,
        scopes: &[ResourceScope],
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        self.resolve_chain_scopes_at(chain, key, &self.config.revocations, time, scopes)
    }
    /// `verify_chain` under `revocations`.
    pub(crate) fn verify_chain_with(
        &self,
        chain: &CredentialChain,
        key: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        self.resolve_chain_at(chain, key, revocations, time)
    }
    /// `revocations` once `manifest`, of a frame or statement, names a
    /// recorded generation; frames under a dropped generation fetch its
    /// manifest as they arrive.
    pub(super) fn revocations_for<'a>(
        &self,
        manifest: Commitment,
        revocations: &'a RevocationSet,
    ) -> Result<&'a RevocationSet, Error> {
        self.known(manifest)?;
        Ok(revocations)
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn generations(&self) -> Result<GenerationStatus, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        Ok(GenerationStatus {
            manifest: inner.manifest,
            current: inner.generation(),
            plan: inner.plan_status().await?,
            engine: inner.engine_status().await?,
        })
    }
    /// Stands `engine` in for this build's replay engine.
    #[cfg(test)]
    pub(crate) async fn replay_on(&self, engine: rocky_core::EngineIdentity) {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.engine = engine;
        inner.invalidate_context();
    }
}
