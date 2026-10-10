//! Definition generations of a secure replica: a publisher-signed lineage
//! from the installed manifest to a newer one, activated atomically at
//! open. Operations, credentials and checkpoints of earlier generations
//! inside the horizon stay admissible under their original signed bytes.
//! See `docs/runtime/generations.md`.
use super::*;
use rocky_core::{ApplicationManifest, Generation, LogicalSchema, Scope};
use rocky_credentials::SignedTransition;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_generations(manifest BLOB PRIMARY KEY,schema INTEGER NOT NULL,handler INTEGER NOT NULL,signed BLOB NOT NULL,transition BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_horizon(id INTEGER PRIMARY KEY CHECK(id=1),horizon INTEGER NOT NULL)",
];
/// Earlier generations a replica keeps admitting.
pub const MAX_GENERATIONS: usize = 64;

/// One generation a transition leaves: its signed manifest and the
/// transition to the next.
#[derive(Clone, Debug)]
pub struct GenerationStep {
    pub manifest: SignedManifest,
    pub transition: SignedTransition,
}
/// What activates `SecureConfig::manifest` on a file installed with an
/// earlier generation: the steps from the installed manifest, oldest
/// first, and the typed tables of the new definition when typed tables are
/// installed. The configured revocations and credentials are the new ones.
#[derive(Clone, Debug)]
pub struct Upgrade {
    pub steps: Vec<GenerationStep>,
    pub tables: Option<(LogicalSchema, Scope, BTreeMap<String, Commitment>)>,
}
/// The generations a replica admits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationStatus {
    pub manifest: Commitment,
    pub current: Generation,
    /// Operations of schema generations below this are refused.
    pub horizon: u32,
    /// Earlier generations, oldest first, with their manifests.
    pub previous: Vec<(Generation, Commitment)>,
    pub plan: Option<super::migrate::ProjectionPlan>,
}
/// An earlier generation.
#[derive(Clone)]
pub(crate) struct Previous {
    pub(super) manifest: Commitment,
    pub(super) generation: Generation,
}
fn generation(manifest: &ApplicationManifest) -> Generation {
    Generation {
        schema: manifest.schema,
        handler: manifest.handler,
    }
}
fn incompatible(reason: impl Into<String>) -> Error {
    Error::Incompatible(reason.into())
}
/// The file identity of `manifest` at `node`.
pub(super) fn identity(manifest: Commitment, node: PublicKey) -> Result<Vec<u8>, Error> {
    encode(&("rocky.encrypted-replica.v12", manifest, node))
}
/// A newer definition keeps everything replicas of the older one agree
/// on: its tables, fields, methods, operations and domains.
fn extends(old: &ApplicationManifest, new: &ApplicationManifest) -> Result<(), Error> {
    let kept = |old: &[rocky_core::DefinitionItem], new: &[rocky_core::DefinitionItem]| {
        old.iter().all(|item| new.contains(item))
    };
    let ids = |items: &[rocky_core::DefinitionItem]| -> BTreeSet<Commitment> {
        items.iter().map(|item| item.id).collect()
    };
    if old.publisher != new.publisher
        || old.app != new.app
        || old.name != new.name
        || old.projection != new.projection
        || old.domains != new.domains
        || old.operations != new.operations
        || !kept(&old.fields, &new.fields)
        || !kept(&old.methods, &new.methods)
        || !ids(&old.entities).is_subset(&ids(&new.entities))
    {
        return Err(incompatible(
            "the new definition does not keep every table, field, method and domain of the installed one",
        ));
    }
    Ok(())
}

impl<A: RowApplication> Inner<A> {
    pub(crate) fn generation(&self) -> Generation {
        generation(&self.definition)
    }
    /// Whether a new signed artifact of `manifest` is admitted, and under
    /// which earlier generation. Generations behind the horizon are refused
    /// explicitly; their authors upgrade and rebootstrap.
    pub(crate) fn admits(&self, manifest: Commitment) -> Result<Option<&Previous>, Error> {
        self.lookup(manifest, true)
    }
    /// The recorded generation of retained history, whatever the horizon:
    /// what was admitted once stays verifiable.
    pub(crate) fn known(&self, manifest: Commitment) -> Result<Option<&Previous>, Error> {
        self.lookup(manifest, false)
    }
    fn lookup(&self, manifest: Commitment, admitting: bool) -> Result<Option<&Previous>, Error> {
        if manifest == self.manifest {
            return Ok(None);
        }
        match self
            .previous
            .iter()
            .find(|previous| previous.manifest == manifest)
        {
            Some(previous) if !admitting || previous.generation.schema >= self.horizon => {
                Ok(Some(previous))
            }
            Some(previous) => Err(incompatible(format!(
                "schema generation {} is behind the horizon {}; its author must upgrade and rebootstrap",
                previous.generation.schema, self.horizon
            ))),
            None => Err(incompatible(
                "signed under a newer or unknown definition generation; upgrade the application",
            )),
        }
    }
    /// The generation `manifest` names, current or admitted; with
    /// `admitting` unset, any recorded one.
    pub(crate) fn generation_of(
        &self,
        manifest: Commitment,
        admitting: bool,
    ) -> Result<Generation, Error> {
        Ok(match self.lookup(manifest, admitting)? {
            None => self.generation(),
            Some(previous) => previous.generation,
        })
    }
    /// Verifies a chain under the generation its root capability names and
    /// the held revocations. Every generation shares the one set.
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
        let manifest = chain
            .0
            .first()
            .map_or(self.manifest, |root| root.capability.manifest);
        let revocations = self.revocations_for(manifest, &self.config.revocations)?;
        self.resolve_chain_scopes_at(chain, manifest, key, revocations, time, scopes)
    }
    /// `verify_chain` under `revocations`: the held set, or none for a
    /// confirmed result verified at the time it was issued.
    pub(crate) fn verify_chain_with(
        &self,
        chain: &CredentialChain,
        key: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        let manifest = chain
            .0
            .first()
            .map_or(self.manifest, |root| root.capability.manifest);
        let revocations = self.revocations_for(manifest, revocations)?;
        self.resolve_chain_at(chain, manifest, key, revocations, time)
    }
    /// `revocations` once `manifest` names an admitted generation, or with
    /// a set other than the held one, any recorded generation.
    pub(super) fn revocations_for<'a>(
        &self,
        manifest: Commitment,
        revocations: &'a RevocationSet,
    ) -> Result<&'a RevocationSet, Error> {
        self.lookup(
            manifest,
            std::ptr::eq(revocations, &self.config.revocations),
        )?;
        Ok(revocations)
    }
    /// Loads the earlier generations and the horizon at open.
    pub(super) async fn load_generations(&mut self) -> Result<(), Error> {
        let stored = rows(
            &self.conn,
            "SELECT manifest,schema,handler FROM secure_generations ORDER BY schema,handler",
        )
        .await?;
        let mut previous = Vec::new();
        for row in stored {
            let [
                SqlValue::Blob(manifest),
                SqlValue::Integer(schema),
                SqlValue::Integer(handler),
            ] = row.as_slice()
            else {
                return Err(Error::Corrupt("generation record".into()));
            };
            let corrupt = || Error::Corrupt("generation record".into());
            previous.push(Previous {
                manifest: manifest.as_slice().try_into().map_err(|_| corrupt())?,
                generation: Generation {
                    schema: u32::try_from(*schema).map_err(|_| corrupt())?,
                    handler: u32::try_from(*handler).map_err(|_| corrupt())?,
                },
            });
        }
        self.horizon = match rows(&self.conn, "SELECT horizon FROM secure_horizon WHERE id=1")
            .await?
            .first()
            .map(Vec::as_slice)
        {
            Some([SqlValue::Integer(horizon)]) => {
                u32::try_from(*horizon).map_err(|_| Error::Corrupt("horizon".into()))?
            }
            _ => self.definition.schema,
        };
        self.previous = previous;
        Ok(())
    }
    /// Activates the upgrade inside the open transaction when the file is
    /// installed with its first step's manifest. Everything is verified
    /// before anything is written; an unsupported change activates nothing.
    /// Returns whether it activated.
    pub(super) async fn activate(
        &mut self,
        stored: &[u8],
        upgrade: Option<&Upgrade>,
    ) -> Result<bool, Error> {
        let Some(upgrade) = upgrade else {
            return Ok(false);
        };
        let publisher = self.config.publisher;
        let node = self.config.node.public_key();
        // The lineage may begin before the installed generation: the
        // upgrade starts at the step the file is installed with.
        let mut start = None;
        for (index, step) in upgrade.steps.iter().enumerate() {
            let manifest = step.manifest.verify(publisher).map_err(credential)?;
            if identity(manifest.hash(), node)? == stored {
                start = Some(index);
                break;
            }
        }
        let Some(start) = start else {
            return Ok(false);
        };
        let steps = &upgrade.steps[start..];
        if steps.len() + self.previous.len() > MAX_GENERATIONS {
            return Err(incompatible("too many definition generations"));
        }
        // The chain: each step's manifest, its transition to the next, and
        // the configured manifest at the end.
        let mut records = Vec::new();
        let mut horizon = 0;
        for (index, step) in steps.iter().enumerate() {
            let manifest = step.manifest.verify(publisher).map_err(credential)?;
            let next = match steps.get(index + 1) {
                Some(next) => next.manifest.verify(publisher).map_err(credential)?,
                None => self.definition.clone(),
            };
            let transition = step.transition.verify(publisher).map_err(credential)?;
            if transition.app != manifest.app
                || transition.from != manifest.hash()
                || transition.to != next.hash()
                || transition.before != generation(&manifest)
                || transition.after != generation(&next)
            {
                return Err(incompatible("the upgrade's transitions do not chain"));
            }
            extends(&manifest, &next)?;
            horizon = transition.horizon;
            records.push((manifest, step));
        }
        // Typed tables move to the new schema through a projection plan;
        // the plan classifies the change before anything is written.
        if self.installed_typed().await?.is_some() {
            let (schema, domain, readers) = upgrade
                .tables
                .as_ref()
                .ok_or_else(|| incompatible("the upgrade must name the new typed tables"))?;
            let domains: Vec<_> = self
                .definition
                .domains
                .iter()
                .map(|rule| rule.domain)
                .collect();
            let mut expected = schema.manifest_at(&domains, self.definition.schema);
            expected.methods = self.definition.methods.clone();
            expected.handler = self.definition.handler;
            if expected.hash() != self.manifest {
                return Err(incompatible(
                    "the new typed tables differ from the signed manifest",
                ));
            }
            self.record_plan(schema, *domain, readers).await?;
        }
        for (manifest, step) in &records {
            let generation = generation(manifest);
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO secure_generations VALUES(?,?,?,?,?)",
                    bindings(vec![
                        SqlValue::Blob(manifest.hash().to_vec()),
                        SqlValue::Integer(i64::from(generation.schema)),
                        SqlValue::Integer(i64::from(generation.handler)),
                        SqlValue::Blob(step.manifest.to_bytes().map_err(credential)?),
                        SqlValue::Blob(step.transition.to_bytes().map_err(credential)?),
                    ]),
                )
                .await?;
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO secure_horizon VALUES(1,?)",
                [i64::from(horizon)],
            )
            .await?;
        self.load_generations().await?;
        self.readmit().await?;
        #[cfg(test)]
        if ACTIVATION_FAULT.with(|fault| fault.replace(false)) {
            return Err(Error::InjectedFailure);
        }
        Ok(true)
    }
    /// Records the earlier generations of a lineage on a new file, which
    /// then admits their operations like a replica that lived through
    /// them. The lineage must end at the configured manifest.
    pub(super) async fn seed(&mut self, upgrade: Option<&Upgrade>) -> Result<(), Error> {
        let Some(upgrade) = upgrade else {
            return Ok(());
        };
        let publisher = self.config.publisher;
        if upgrade.steps.len() > MAX_GENERATIONS {
            return Err(incompatible("too many definition generations"));
        }
        let mut horizon = self.definition.schema;
        for (index, step) in upgrade.steps.iter().enumerate() {
            let manifest = step.manifest.verify(publisher).map_err(credential)?;
            let next = match upgrade.steps.get(index + 1) {
                Some(next) => next.manifest.verify(publisher).map_err(credential)?,
                None => self.definition.clone(),
            };
            let transition = step.transition.verify(publisher).map_err(credential)?;
            if transition.app != manifest.app
                || transition.from != manifest.hash()
                || transition.to != next.hash()
                || transition.before != generation(&manifest)
                || transition.after != generation(&next)
            {
                return Err(incompatible("the lineage's transitions do not chain"));
            }
            extends(&manifest, &next)?;
            horizon = transition.horizon;
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO secure_generations VALUES(?,?,?,?,?)",
                    bindings(vec![
                        SqlValue::Blob(manifest.hash().to_vec()),
                        SqlValue::Integer(i64::from(manifest.schema)),
                        SqlValue::Integer(i64::from(manifest.handler)),
                        SqlValue::Blob(step.manifest.to_bytes().map_err(credential)?),
                        SqlValue::Blob(step.transition.to_bytes().map_err(credential)?),
                    ]),
                )
                .await?;
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO secure_horizon VALUES(1,?)",
                [i64::from(horizon)],
            )
            .await?;
        Ok(())
    }
    /// Frames this replica refused as signed under an unknown generation
    /// are pending again once that generation is the current one.
    async fn readmit(&self) -> Result<(), Error> {
        let mut after = Vec::new();
        loop {
            let chunk = collect_rows(
                self.conn
                    .query(
                        "SELECT hash,header FROM secure_operations WHERE status='rejected' AND hash>? ORDER BY hash LIMIT 64",
                        [after.clone()],
                    )
                    .await?,
            )
            .await?;
            for record in &chunk {
                let [SqlValue::Blob(hash), SqlValue::Blob(header)] = record.as_slice() else {
                    return Err(Error::Corrupt("rejected operation".into()));
                };
                after = hash.clone();
                let header: p2panda_core::Header<Extensions> = decode(header)?;
                if header.extensions.manifest == self.manifest {
                    self.conn
                        .execute(
                            "UPDATE secure_operations SET status='pending' WHERE hash=?",
                            [hash.clone()],
                        )
                        .await?;
                }
            }
            if chunk.len() < 64 {
                return Ok(());
            }
        }
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The current generation, the earlier ones still admitted and a
    /// pending projection plan.
    pub async fn generations(&self) -> Result<GenerationStatus, Error> {
        let inner = self.inner.lock().await;
        Ok(GenerationStatus {
            manifest: inner.manifest,
            current: inner.generation(),
            horizon: inner.horizon,
            previous: inner
                .previous
                .iter()
                .map(|previous| (previous.generation, previous.manifest))
                .collect(),
            plan: inner.plan_status().await?,
        })
    }
    /// The sequence and head of the newest confirmed result this replica
    /// holds for `domain`; `None` before the first. An activation is
    /// proposed only when this equals the coordinator's head.
    pub async fn confirmed_head(&self, domain: Scope) -> Result<Option<(u64, Commitment)>, Error> {
        let inner = self.inner.lock().await;
        let stored = collect_rows(
            inner
                .conn
                .query(
                    "SELECT result FROM secure_results WHERE domain=? ORDER BY sequence DESC LIMIT 1",
                    [encode(&domain)?],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Blob(bytes)]) = stored.first().map(Vec::as_slice) else {
            return Ok(None);
        };
        let result: crate::ConfirmedCommand = decode(bytes)?;
        let receipt = result.receipt()?;
        Ok(Some((receipt.sequence, receipt.confirmation.new_head)))
    }
    #[cfg(test)]
    pub(crate) fn fail_next_activation() {
        ACTIVATION_FAULT.with(|fault| fault.set(true));
    }
}
#[cfg(test)]
thread_local! {
    /// Fails the next activation of this test thread before it commits.
    pub(super) static ACTIVATION_FAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
