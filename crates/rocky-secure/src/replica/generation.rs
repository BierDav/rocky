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
    /// When the earliest signed transition into the current generation was
    /// signed; none for a first generation.
    pub baseline: Option<u64>,
    pub plan: Option<super::migrate::ProjectionPlan>,
}
/// What an open replica holds of its definition, swapped whole when it
/// adopts a generation.
struct Definition<A> {
    app: Arc<A>,
    permission_fields: HashSet<Commitment>,
    config: SecureConfig,
    chain: Chain,
    history: Vec<KeyMaterial>,
    manifest: Commitment,
    definition: ApplicationManifest,
    tables: HashSet<Commitment>,
    fields: HashSet<Commitment>,
    domains: HashSet<rocky_core::DomainId>,
}
impl<A> Definition<A> {
    fn swap(&mut self, inner: &mut Inner<A>) {
        std::mem::swap(&mut self.app, &mut inner.app);
        std::mem::swap(&mut self.permission_fields, &mut inner.permission_fields);
        std::mem::swap(&mut self.config, &mut inner.config);
        std::mem::swap(&mut self.chain, &mut inner.chain);
        std::mem::swap(&mut self.history, &mut inner.history);
        std::mem::swap(&mut self.manifest, &mut inner.manifest);
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
/// What the recorded lineage removed and re-added, by table and field id.
/// Frames, checkpoint pages and stored rows are read through it, the same
/// way on every device. See `docs/runtime/removal.md`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Names {
    /// Named by an earlier generation, not by the current one.
    pub(super) retired_tables: HashSet<Commitment>,
    pub(super) retired_fields: HashSet<Commitment>,
    /// Removed once and named again: writes stamped before the clock of
    /// the re-adding generation's baseline are ignored.
    pub(super) tables: BTreeMap<Commitment, u64>,
    pub(super) fields: BTreeMap<Commitment, u64>,
    /// The table of each current field.
    owners: BTreeMap<Commitment, Commitment>,
}
impl PartialEq for Names {
    /// The same rules, whatever tables were added.
    fn eq(&self, other: &Self) -> bool {
        self.retired_tables == other.retired_tables
            && self.retired_fields == other.retired_fields
            && self.tables == other.tables
            && self.fields == other.fields
    }
}
impl Names {
    /// The names of a lineage, oldest manifest first, with the signing time
    /// of each transition out of all but the last.
    fn of(manifests: &[ApplicationManifest], times: &[u64]) -> Self {
        let Some((current, earlier)) = manifests.split_last() else {
            return Self::default();
        };
        let ids = |items: &[rocky_core::DefinitionItem]| -> HashSet<Commitment> {
            items.iter().map(|item| item.id).collect()
        };
        let tables: Vec<_> = manifests
            .iter()
            .map(|manifest| ids(&manifest.entities))
            .collect();
        let fields: Vec<_> = manifests
            .iter()
            .map(|manifest| ids(&manifest.fields))
            .collect();
        let (now_tables, now_fields) = (ids(&current.entities), ids(&current.fields));
        let mut names = Self::default();
        for manifest in earlier {
            names.retired_tables.extend(
                manifest
                    .entities
                    .iter()
                    .map(|item| item.id)
                    .filter(|id| !now_tables.contains(id)),
            );
            names.retired_fields.extend(
                manifest
                    .fields
                    .iter()
                    .map(|item| item.id)
                    .filter(|id| !now_fields.contains(id)),
            );
        }
        // The last generation that lacked a current name re-added it when
        // an earlier one had it.
        let readded = |sets: &[HashSet<Commitment>], id: &Commitment| -> Option<u64> {
            let last = sets[..earlier.len()]
                .iter()
                .rposition(|set| !set.contains(id))?;
            sets[..last]
                .iter()
                .any(|set| set.contains(id))
                .then(|| times[last])
        };
        for id in &now_tables {
            if let Some(clock) = readded(&tables, id) {
                names.tables.insert(*id, clock);
            }
        }
        for id in &now_fields {
            if let Some(clock) = readded(&fields, id) {
                names.fields.insert(*id, clock);
            }
        }
        for field in &current.fields {
            if let Some(table) = current
                .entities
                .iter()
                .find(|table| field.name.starts_with(&format!("{}.", table.name)))
            {
                names.owners.insert(field.id, table.id);
            }
        }
        // A re-added table's fields start again with it.
        for (table, clock) in names.tables.clone() {
            let Some(name) = current.entities.iter().find(|item| item.id == table) else {
                continue;
            };
            for field in current
                .fields
                .iter()
                .filter(|field| field.name.starts_with(&format!("{}.", name.name)))
            {
                let entry = names.fields.entry(field.id).or_insert(clock);
                *entry = (*entry).max(clock);
            }
        }
        names
    }
    /// Whether `table` has a re-added name, or one whose baseline moved
    /// earlier, since `before`: writes to it ignored so far apply now.
    pub(super) fn pulls_again(&self, before: &Self, table: Commitment) -> bool {
        let earlier = |now: &u64, was: Option<&u64>| was.is_none_or(|was| now < was);
        self.tables
            .get(&table)
            .is_some_and(|now| earlier(now, before.tables.get(&table)))
            || self.fields.iter().any(|(field, now)| {
                self.owners.get(field) == Some(&table) && earlier(now, before.fields.get(field))
            })
    }
    /// Whether every write of a frame of `table` stamped at `clock` is
    /// ignored: the table was added again after it.
    pub(super) fn ignores_frame(&self, table: Commitment, clock: u64) -> bool {
        self.retired_tables.contains(&table)
            || self
                .tables
                .get(&table)
                .is_some_and(|baseline| clock < *baseline)
    }
    /// `write` as the current generation applies it when stamped at
    /// `clock`: without retired fields, nor re-added ones stamped before
    /// their baseline. None when nothing of it applies.
    pub(super) fn current(&self, write: &RowWrite, clock: u64) -> Option<RowWrite> {
        let table = write.change.scope.table;
        if self.retired_tables.contains(&table)
            || self
                .tables
                .get(&table)
                .is_some_and(|baseline| clock < *baseline)
        {
            return None;
        }
        let ignored = |field: &Commitment| {
            self.retired_fields.contains(field)
                || self
                    .fields
                    .get(field)
                    .is_some_and(|baseline| clock < *baseline)
        };
        let images = [&write.change.before, &write.change.after];
        if !write.assigned.iter().any(ignored)
            && !images.iter().any(|image| {
                image
                    .as_ref()
                    .is_some_and(|fields| fields.keys().any(ignored))
            })
        {
            return Some(write.clone());
        }
        let strip = |image: &Option<RowFields>| {
            image.as_ref().map(|fields| {
                fields
                    .iter()
                    .filter(|(field, _)| !ignored(field))
                    .map(|(field, value)| (*field, value.clone()))
                    .collect::<RowFields>()
            })
        };
        let stripped = RowWrite {
            change: RowChange {
                before: strip(&write.change.before),
                after: strip(&write.change.after),
                ..write.change.clone()
            },
            assigned: write
                .assigned
                .iter()
                .copied()
                .filter(|field| !ignored(field))
                .collect(),
        };
        // An update of ignored fields alone changes nothing.
        if stripped.change.before.is_some()
            && stripped.change.after.is_some()
            && stripped.published_fields().is_empty()
        {
            return None;
        }
        Some(stripped)
    }
    /// A stored row record as the current generation keeps it: without
    /// retired fields, nor re-added ones stamped before their baseline.
    /// Returns whether it changed.
    pub(super) fn strip(&self, record: &mut Materialized) -> bool {
        let ignored: Vec<Commitment> = record
            .fields
            .keys()
            .chain(record.stamps.keys())
            .filter(|field| {
                self.retired_fields.contains(*field)
                    || self.fields.get(*field).is_some_and(|baseline| {
                        record
                            .stamps
                            .get(*field)
                            .is_none_or(|stamp| stamp.clock < *baseline)
                    })
            })
            .copied()
            .collect();
        for field in &ignored {
            record.fields.remove(field);
            record.stamps.remove(field);
        }
        !ignored.is_empty()
    }
    /// Whether a stored row of `table` was created before the table was
    /// added again, so that no device keeps it.
    pub(super) fn before_readding(&self, table: Commitment, record: &Materialized) -> bool {
        self.tables.get(&table).is_some_and(|baseline| {
            record
                .created
                .as_ref()
                .is_none_or(|stamp| stamp.clock < *baseline)
        })
    }
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
    encode(&("rocky.encrypted-replica.v16", manifest, node))
}
/// A newer definition keeps what replicas of the older one agree on: its
/// methods, operations and domains, and every table and field it still
/// names, unchanged. Tables and fields may be removed; every device drops
/// them. A breaking transition keeps fields by identity and may require
/// them: it admits no operation of an earlier generation.
fn extends(
    old: &ApplicationManifest,
    new: &ApplicationManifest,
    transition: &rocky_core::GenerationTransition,
) -> Result<(), Error> {
    let kept = |old: &[rocky_core::DefinitionItem], new: &[rocky_core::DefinitionItem]| {
        old.iter().all(|item| {
            new.iter()
                .find(|other| other.id == item.id)
                .is_none_or(|other| other == item)
        })
    };
    if old.publisher != new.publisher
        || old.app != new.app
        || old.name != new.name
        || old.projection != new.projection
        || old.domains != new.domains
        || old.operations != new.operations
        || !(kept(&old.fields, &new.fields) || transition.breaking())
        || !old.methods.iter().all(|item| new.methods.contains(item))
    {
        return Err(incompatible(
            "the new definition does not keep the methods and domains, and the tables and fields it names, of the installed one",
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
    pub(crate) fn verify_reference_chain_with(
        &self,
        chain: &CredentialChain,
        node: rocky_credentials::PeerId,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        let manifest = chain
            .0
            .first()
            .map_or(self.manifest, |root| root.capability.manifest);
        let revocations = self.revocations_for(manifest, revocations)?;
        let (mut grants, selectors) = chain
            .verify_reference_selectors(self.config.publisher, manifest, node, revocations, time)
            .map_err(credential)?;
        for (scope, tuple) in &self.partitions {
            for selector in &selectors {
                if let Some(grant) = selector
                    .resolve(scope.domain, scope.table, tuple)
                    .map_err(credential)?
                {
                    grants.push(grant);
                }
            }
        }
        Ok(grants)
    }
    pub(crate) fn verify_reference_chain(
        &self,
        chain: &CredentialChain,
        node: rocky_credentials::PeerId,
        time: u64,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        self.verify_reference_chain_with(chain, node, &self.config.revocations, time)
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
            _ => 0,
        };
        self.previous = previous;
        self.names = self.lineage_names(&self.definition).await?;
        Ok(())
    }
    /// The names the recorded lineage, ending at `current`, removed and
    /// re-added.
    async fn lineage_names(&self, current: &ApplicationManifest) -> Result<Names, Error> {
        let stored = rows(
            &self.conn,
            "SELECT signed,transition FROM secure_generations ORDER BY schema,handler",
        )
        .await?;
        let publisher = self.config.publisher;
        let (mut manifests, mut times) = (Vec::new(), Vec::new());
        for row in stored {
            let [SqlValue::Blob(signed), SqlValue::Blob(transition)] = row.as_slice() else {
                return Err(Error::Corrupt("generation record".into()));
            };
            let manifest = SignedManifest::from_bytes(signed)
                .and_then(|signed| signed.verify(publisher))
                .map_err(credential)?;
            let transition = SignedTransition::from_bytes(transition)
                .and_then(|signed| signed.verify(publisher))
                .map_err(credential)?;
            if manifest.hash() != current.hash() {
                manifests.push(manifest);
                times.push(transition.time);
            }
        }
        manifests.push(current.clone());
        Ok(Names::of(&manifests, &times))
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
        // What the installed generation's lineage names, before the step.
        let before = self
            .lineage_names(&steps[0].manifest.verify(publisher).map_err(credential)?)
            .await?;
        if steps.len() + self.previous.len() > MAX_GENERATIONS {
            return Err(incompatible("too many definition generations"));
        }
        // The chain: each step's manifest, its transition to the next, and
        // the configured manifest at the end.
        let mut records = Vec::new();
        // Every generation stays admitted, except behind a cut.
        let mut horizon = self.horizon;
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
            extends(&manifest, &next, &transition)?;
            if transition.breaking() {
                horizon = transition.after.schema;
            }
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
        self.apply_names(&before).await?;
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
        let mut horizon = 0;
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
            extends(&manifest, &next, &transition)?;
            if transition.breaking() {
                horizon = transition.after.schema;
            }
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
    /// The stored transition out of `manifest`, verified.
    async fn stored_transition(
        &self,
        manifest: Commitment,
    ) -> Result<Option<rocky_core::GenerationTransition>, Error> {
        let stored = collect_rows(
            self.conn
                .query(
                    "SELECT transition FROM secure_generations WHERE manifest=?",
                    [manifest.to_vec()],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Blob(bytes)]) = stored.first().map(Vec::as_slice) else {
            return Ok(None);
        };
        let signed = SignedTransition::from_bytes(bytes).map_err(credential)?;
        Ok(Some(
            signed.verify(self.config.publisher).map_err(credential)?,
        ))
    }
    /// The signing time of the transition into the current generation.
    async fn baseline(&self) -> Result<Option<u64>, Error> {
        let Some(previous) = self.previous.last() else {
            return Ok(None);
        };
        Ok(self
            .stored_transition(previous.manifest)
            .await?
            .filter(|transition| transition.to == self.manifest)
            .map(|transition| transition.time))
    }
    /// Frames this replica refused as signed under an unknown generation
    /// are pending again once that generation is admitted.
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
                if self.admits(header.extensions.manifest).is_ok() {
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
            baseline: inner.baseline().await?,
            plan: inner.plan_status().await?,
        })
    }
    /// Records the earlier generations among `steps` this replica lacks,
    /// such as one a peer that has not migrated still writes under: each
    /// step's manifest is the publisher's, and its transition leads to a
    /// generation the replica knows or to another such step. Frames refused
    /// under them are admitted again. Returns how many it recorded.
    pub async fn learn_generations(&self, steps: &[GenerationStep]) -> Result<usize, Error> {
        let mut inner = self.inner.lock().await;
        let publisher = inner.config.publisher;
        let mut verified = Vec::new();
        for step in steps {
            let manifest = step.manifest.verify(publisher).map_err(credential)?;
            let transition = step.transition.verify(publisher).map_err(credential)?;
            if transition.app != manifest.app
                || transition.from != manifest.hash()
                || transition.before != generation(&manifest)
            {
                return Err(incompatible(
                    "a step's transition does not leave its manifest",
                ));
            }
            verified.push((manifest, transition, step));
        }
        // Each newly known generation links the ones before it.
        let mut known: BTreeSet<Commitment> = inner
            .previous
            .iter()
            .map(|previous| previous.manifest)
            .chain([inner.manifest])
            .collect();
        let mut learned = Vec::new();
        loop {
            let found = verified.iter().position(|(manifest, transition, _)| {
                !known.contains(&manifest.hash()) && known.contains(&transition.to)
            });
            let Some(index) = found else { break };
            let (manifest, transition, step) = verified.swap_remove(index);
            if transition.breaking() {
                // Behind a cut nothing is admitted; such a step is not learned.
                continue;
            }
            known.insert(manifest.hash());
            learned.push((manifest, step));
        }
        if learned.is_empty() {
            return Ok(0);
        }
        if learned.len() + inner.previous.len() > MAX_GENERATIONS {
            return Err(incompatible("too many definition generations"));
        }
        let count = learned.len();
        let recorded = transaction!(inner, |inner| {
            for (manifest, step) in &learned {
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_generations VALUES(?,?,?,?,?)",
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
            let before = inner.names.clone();
            inner.load_generations().await?;
            inner.apply_names(&before).await?;
            inner.readmit().await
        });
        if let Err(error) = recorded {
            inner.load_generations().await?;
            return Err(error);
        }
        inner.pull_again().await?;
        drop(inner);
        self.settle().await?;
        Ok(count)
    }
    /// Keeps `transition` out of an earlier generation when it makes the
    /// recorded step and was signed earlier: issuers that signed the same
    /// schema apart made one generation, whose baseline is the earliest.
    /// Returns whether the recorded transition changed.
    pub async fn learn_baseline(&self, transition: &SignedTransition) -> Result<bool, Error> {
        let mut inner = self.inner.lock().await;
        let verified = transition
            .verify(inner.config.publisher)
            .map_err(credential)?;
        let Some(recorded) = inner.stored_transition(verified.from).await? else {
            return Ok(false);
        };
        if !recorded.same_step(&verified) || verified.time >= recorded.time {
            return Ok(false);
        }
        let bytes = transition.to_bytes().map_err(credential)?;
        let learned = transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "UPDATE secure_generations SET transition=? WHERE manifest=?",
                    bindings(vec![
                        SqlValue::Blob(bytes.clone()),
                        SqlValue::Blob(verified.from.to_vec()),
                    ]),
                )
                .await?;
            // An earlier baseline admits writes to a re-added name it ignored.
            let before = inner.names.clone();
            inner.names = inner.lineage_names(&inner.definition).await?;
            inner.apply_names(&before).await
        });
        if let Err(error) = learned {
            inner.load_generations().await?;
            return Err(error);
        }
        inner.pull_again().await?;
        Ok(true)
    }
    /// Moves this open replica to a newer generation of its definition, as
    /// opening with `upgrade` would: the lineage is verified before anything
    /// changes, the generation activates in one transaction and the typed
    /// tables switch through the projection plan before this returns.
    /// Frames of the generation refused before are admitted afterwards.
    /// Sessions end; table observers stay registered. A crash after the
    /// activation resumes the plan when the file is opened at the new
    /// generation. Returns whether the generation changed.
    pub async fn adopt_generation(
        &self,
        app: A,
        mut config: SecureConfig,
        upgrade: Upgrade,
    ) -> Result<bool, Error> {
        let manifest = config
            .manifest
            .verify(config.publisher)
            .map_err(credential)?;
        if app.manifest().hash() != manifest.hash() {
            return Err(denied("compiled application differs from signed manifest"));
        }
        super::check_config(&config, &manifest)?;
        let (current, history) = super::keys::split(std::mem::take(&mut config.keys));
        config.keys = current;
        let mut inner = self.inner.lock().await;
        if inner.manifest == manifest.hash() {
            return Ok(false);
        }
        if inner.config.publisher != config.publisher
            || inner.config.node.public_key() != config.node.public_key()
        {
            return Err(denied("a generation keeps the publisher and the node"));
        }
        let identity = identity(manifest.hash(), config.node.public_key())?;
        let mut definition = Definition {
            permission_fields: app.permission_fields().into_iter().collect(),
            app: Arc::new(app),
            chain: Chain::new(config.credentials.clone())?,
            config,
            history,
            manifest: manifest.hash(),
            tables: manifest.entities.iter().map(|item| item.id).collect(),
            fields: manifest.fields.iter().map(|item| item.id).collect(),
            domains: manifest.domains.iter().map(|item| item.domain).collect(),
            definition: manifest,
        };
        definition.swap(&mut inner);
        let activated = transaction!(inner, |inner| {
            let stored = inner.stored_identity().await?;
            if !inner.activate(&stored, Some(&upgrade)).await? {
                return Err(incompatible(
                    "the lineage does not start at the installed generation",
                ));
            }
            inner.load_revocations(None).await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET identity=? WHERE id=1",
                    [identity.clone()],
                )
                .await?;
            Ok(())
        });
        if let Err(error) = activated {
            // Nothing was written: the installed generation stays.
            definition.swap(&mut inner);
            inner.load_generations().await?;
            return Err(error);
        }
        inner
            .chains
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        inner.load_generations().await?;
        inner.finish_plan().await?;
        inner.load_partitions().await?;
        inner.local_grants(now()?)?;
        inner.validate_keys(now()?)?;
        inner.adopt_keys().await?;
        inner.load_direct().await?;
        inner.pull_again().await?;
        inner.bump_authority();
        inner.publish_revision().await?;
        drop(inner);
        self.settle().await?;
        Ok(true)
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
