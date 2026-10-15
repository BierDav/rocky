//! Definition generations of a secure replica: a publisher-signed lineage
//! from the installed manifest to a newer one, activated atomically at
//! open. Operations, credentials and checkpoints of earlier generations
//! stay admissible under their original signed bytes. The transitions are
//! kept; an earlier generation's manifest is a cache, dropped once nothing
//! retained names it and learned again from a peer that still writes it.
//! See `docs/runtime/generations.md`.
use super::*;
use rocky_core::{ApplicationManifest, Generation, LogicalSchema, Scope};
use rocky_credentials::SignedTransition;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_generations(manifest BLOB PRIMARY KEY,schema INTEGER NOT NULL,signed BLOB,transition BLOB NOT NULL)",
];
/// Earlier generations a replica keeps admitting.
pub const MAX_GENERATIONS: usize = 64;
/// Stored artifacts that may name a manifest: frames, checkpoints, horizon
/// statements and acknowledgments and existence bases. A
/// generation's manifest is kept while any of them, or a credential chain
/// of a retained frame or registered author, names it. The chains peers
/// last authenticated with do not keep it: a peer presents its chain again
/// and the sync fetches the manifest.
const REFERENCES: &[(&str, &str)] = &[
    ("secure_operations", "header"),
    ("secure_roles", "config"),
    ("secure_checkpoints", "signed"),
    ("secure_checkpoint_archive", "signed"),
    ("secure_rebootstrap", "signed"),
    ("secure_horizon_relay_finalized", "checkpoint"),
    ("secure_mergeable_horizon", "statement"),
    ("secure_horizon_bootstrap", "statement"),
    ("secure_horizon_ack", "ack"),
    ("secure_existence_base", "attachment"),
    ("secure_existence_archive", "attachment"),
    ("secure_existence_stage", "attachment"),
];

/// One generation a transition leaves: the transition to the next and,
/// where held, the generation's signed manifest.
#[derive(Clone, Debug)]
pub struct GenerationStep {
    pub manifest: Option<SignedManifest>,
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
    /// Earlier generations whose manifests this replica holds, oldest
    /// first.
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
    node_author: Option<identity::NodeAuthor>,
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
        std::mem::swap(&mut self.node_author, &mut inner.node_author);
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
/// way on every device. It comes from the signed transitions alone, so it
/// holds whatever manifests were dropped. See `docs/runtime/removal.md`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Names {
    /// Removed by a transition and not named again since.
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
    /// The names of a lineage's transitions, oldest first, read with the
    /// fields and tables of the `current` manifest.
    fn of(steps: &[rocky_core::GenerationTransition], current: &ApplicationManifest) -> Self {
        // An id is retired from its removal until a later step names it
        // again, which re-adds it at that step's signing time.
        let follow =
            |changes: &dyn Fn(&rocky_core::GenerationTransition) -> &rocky_core::ItemChanges| {
                let (mut retired, mut readded) = (HashSet::new(), BTreeMap::new());
                for step in steps {
                    for id in &changes(step).removed {
                        retired.insert(*id);
                        readded.remove(id);
                    }
                    for id in &changes(step).named {
                        if retired.remove(id) {
                            readded.insert(*id, step.time);
                        }
                    }
                }
                (retired, readded)
            };
        let (retired_tables, tables) = follow(&|step| &step.tables);
        let (retired_fields, fields) = follow(&|step| &step.fields);
        let mut names = Self {
            retired_tables,
            retired_fields,
            tables,
            fields,
            owners: BTreeMap::new(),
        };
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
/// An earlier generation, recorded by its transition.
#[derive(Clone)]
pub(crate) struct Previous {
    pub(super) manifest: Commitment,
    pub(super) generation: Generation,
    /// Whether this replica holds its signed manifest.
    pub(super) held: bool,
}
fn generation(manifest: &ApplicationManifest) -> Generation {
    Generation {
        schema: manifest.schema,
    }
}
fn incompatible(reason: impl Into<String>) -> Error {
    Error::Incompatible(reason.into())
}
/// The one encrypted replica file format; files of another are refused.
pub(super) const FILE_IDENTITY: &str = "rocky.encrypted-replica.v22";
/// The file identity of `manifest` at `node`.
pub(super) fn identity(manifest: Commitment, node: PublicKey) -> Result<Vec<u8>, Error> {
    encode(&(FILE_IDENTITY, manifest, node))
}
/// A newer definition keeps what replicas of the older one agree on: its
/// operations and domains, and every table and field it still
/// names. A field may become required; rows that lack it or hold NULL read
/// its default. Tables and fields may be removed; every device drops them.
/// `steps`, the transitions from `old` to `new`, account for each removal,
/// and a name removed in between may come back changed. One step names and
/// removes exactly what the two manifests differ in.
fn extends(
    old: &ApplicationManifest,
    new: &ApplicationManifest,
    steps: &[&rocky_core::GenerationTransition],
) -> Result<(), Error> {
    let removed: HashSet<Commitment> = steps
        .iter()
        .flat_map(|step| step.tables.removed.iter().chain(&step.fields.removed))
        .copied()
        .collect();
    let required = |field: &rocky_core::DefinitionItem| rocky_core::DefinitionItem {
        shape: field
            .shape
            .replace("\"not_null\":false", "\"not_null\":true"),
        ..field.clone()
    };
    let fields_kept = old.fields.iter().all(|field| {
        removed.contains(&field.id)
            || new.fields.contains(field)
            || new.fields.contains(&required(field))
    });
    let tables_kept = old.entities.iter().all(|table| {
        removed.contains(&table.id) || new.entities.iter().any(|other| other.id == table.id)
    });
    let exact = match steps {
        [step] => {
            step.tables == rocky_core::ItemChanges::between(&old.entities, &new.entities)
                && step.fields == rocky_core::ItemChanges::between(&old.fields, &new.fields)
        }
        _ => true,
    };
    if !exact {
        return Err(incompatible(
            "a transition does not name and remove what its manifests differ in",
        ));
    }
    if old.publisher != new.publisher
        || old.app != new.app
        || old.name != new.name
        || old.projection != new.projection
        || old.domains != new.domains
        || old.operations != new.operations
        || !fields_kept
        || !tables_kept
    {
        return Err(incompatible(
            "the new definition does not keep the operations and domains, and the tables and fields it names, of the installed one",
        ));
    }
    Ok(())
}

impl<A: RowApplication> Inner<A> {
    pub(crate) fn generation(&self) -> Generation {
        generation(&self.definition)
    }
    /// Whether a new signed artifact of `manifest` is admitted, and under
    /// which earlier generation: one whose manifest this replica holds. A
    /// dropped one is learned again from the peer that sent the artifact.
    pub(crate) fn admits(&self, manifest: Commitment) -> Result<Option<&Previous>, Error> {
        self.lookup(manifest, true)
    }
    /// The recorded generation of retained history, held or dropped: what
    /// was admitted once stays verifiable by its signed transition.
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
            Some(previous) if !admitting || previous.held => Ok(Some(previous)),
            Some(_) => Err(incompatible(
                "signed under a generation whose manifest this replica dropped; fetch it from the peer",
            )),
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
    /// Loads the earlier generations at open.
    pub(super) async fn load_generations(&mut self) -> Result<(), Error> {
        let stored = rows(
            &self.conn,
            "SELECT manifest,schema,signed IS NOT NULL FROM secure_generations ORDER BY schema",
        )
        .await?;
        let mut previous = Vec::new();
        for row in stored {
            let [
                SqlValue::Blob(manifest),
                SqlValue::Integer(schema),
                SqlValue::Integer(held),
            ] = row.as_slice()
            else {
                return Err(Error::Corrupt("generation record".into()));
            };
            let corrupt = || Error::Corrupt("generation record".into());
            previous.push(Previous {
                manifest: manifest.as_slice().try_into().map_err(|_| corrupt())?,
                generation: Generation {
                    schema: u32::try_from(*schema).map_err(|_| corrupt())?,
                },
                held: *held != 0,
            });
        }
        self.previous = previous;
        self.names = self.lineage_names().await?;
        Ok(())
    }
    /// The names the recorded transitions removed and re-added.
    async fn lineage_names(&self) -> Result<Names, Error> {
        let stored = rows(
            &self.conn,
            "SELECT transition FROM secure_generations ORDER BY schema",
        )
        .await?;
        let publisher = self.config.publisher;
        let mut steps = Vec::with_capacity(stored.len());
        for row in stored {
            let [SqlValue::Blob(transition)] = row.as_slice() else {
                return Err(Error::Corrupt("generation record".into()));
            };
            steps.push(
                SignedTransition::from_bytes(transition)
                    .and_then(|signed| signed.verify(publisher))
                    .map_err(credential)?,
            );
        }
        Ok(Names::of(&steps, &self.definition))
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
            let transition = step.transition.verify(publisher).map_err(credential)?;
            if identity(transition.from, node)? == stored {
                start = Some(index);
                break;
            }
        }
        let Some(start) = start else {
            return Ok(false);
        };
        let steps = &upgrade.steps[start..];
        // What the installed generation's lineage names, before the step.
        let before = self.lineage_names().await?;
        if steps.len() + self.previous.len() > MAX_GENERATIONS {
            return Err(incompatible("too many definition generations"));
        }
        let records = self.chain_steps(steps)?;
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
            let expected = schema.manifest_at(&domains, self.definition.schema);
            if expected.hash() != self.manifest {
                return Err(incompatible(
                    "the new typed tables differ from the signed manifest",
                ));
            }
            self.record_plan(schema, *domain, readers).await?;
        }
        for (transition, step) in &records {
            self.record_step(transition, step).await?;
        }
        self.load_generations().await?;
        // Unsent changes the removal would split are held before anything
        // the switch drops is gone.
        self.hold_removed(&before).await?;
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
        if upgrade.steps.len() > MAX_GENERATIONS {
            return Err(incompatible("too many definition generations"));
        }
        for (transition, step) in &self.chain_steps(&upgrade.steps)? {
            self.record_step(transition, step).await?;
        }
        Ok(())
    }
    /// Verifies that `steps` chain into the configured definition: each
    /// transition is the publisher's and leads to the next step's, the last
    /// to the configured manifest; a held manifest is the one its
    /// transition leaves, and each held manifest, and the configured one,
    /// extends the held one before it. Returns the verified transitions.
    fn chain_steps<'a>(
        &self,
        steps: &'a [GenerationStep],
    ) -> Result<Vec<(rocky_core::GenerationTransition, &'a GenerationStep)>, Error> {
        let publisher = self.config.publisher;
        let mut verified = Vec::with_capacity(steps.len());
        let mut earlier: Option<ApplicationManifest> = None;
        // The transitions since the last held manifest.
        let mut between = Vec::new();
        for step in steps {
            let transition = step.transition.verify(publisher).map_err(credential)?;
            if transition.app != self.definition.app {
                return Err(incompatible("the lineage's transitions do not chain"));
            }
            if let Some(signed) = &step.manifest {
                let manifest = signed.verify(publisher).map_err(credential)?;
                if transition.from != manifest.hash() || transition.before != generation(&manifest)
                {
                    return Err(incompatible(
                        "a step's transition does not leave its manifest",
                    ));
                }
                if let Some(earlier) = &earlier {
                    extends(earlier, &manifest, &between.iter().collect::<Vec<_>>())?;
                }
                earlier = Some(manifest);
                between.clear();
            }
            between.push(transition.clone());
            verified.push((transition, step));
        }
        for (index, (transition, _)) in verified.iter().enumerate() {
            let (to, after) = match verified.get(index + 1) {
                Some((next, _)) => (next.from, next.before),
                None => (self.manifest, generation(&self.definition)),
            };
            if transition.to != to || transition.after != after {
                return Err(incompatible("the lineage's transitions do not chain"));
            }
        }
        if let Some(earlier) = &earlier {
            extends(
                earlier,
                &self.definition,
                &between.iter().collect::<Vec<_>>(),
            )?;
        }
        Ok(verified)
    }
    /// Records a verified step: its transition, and its manifest when the
    /// step holds one; a manifest already held stays.
    async fn record_step(
        &self,
        transition: &rocky_core::GenerationTransition,
        step: &GenerationStep,
    ) -> Result<(), Error> {
        let signed = match &step.manifest {
            Some(manifest) => SqlValue::Blob(manifest.to_bytes().map_err(credential)?),
            None => SqlValue::Null,
        };
        self.conn
            .execute(
                "INSERT INTO secure_generations VALUES(?,?,?,?) ON CONFLICT(manifest) DO UPDATE SET signed=coalesce(excluded.signed,signed),transition=excluded.transition",
                bindings(vec![
                    SqlValue::Blob(transition.from.to_vec()),
                    SqlValue::Integer(i64::from(transition.before.schema)),
                    signed,
                    SqlValue::Blob(step.transition.to_bytes().map_err(credential)?),
                ]),
            )
            .await?;
        Ok(())
    }
    /// Records a learned step as [`Self::record_step`] does, but keeps a
    /// transition already recorded: it may carry an earlier baseline.
    async fn record_known(
        &self,
        transition: &rocky_core::GenerationTransition,
        step: &GenerationStep,
    ) -> Result<(), Error> {
        let signed = match &step.manifest {
            Some(manifest) => SqlValue::Blob(manifest.to_bytes().map_err(credential)?),
            None => SqlValue::Null,
        };
        self.conn
            .execute(
                "INSERT INTO secure_generations VALUES(?,?,?,?) ON CONFLICT(manifest) DO UPDATE SET signed=coalesce(excluded.signed,signed)",
                bindings(vec![
                    SqlValue::Blob(transition.from.to_vec()),
                    SqlValue::Integer(i64::from(transition.before.schema)),
                    signed,
                    SqlValue::Blob(step.transition.to_bytes().map_err(credential)?),
                ]),
            )
            .await?;
        Ok(())
    }
    /// Whether anything this replica retains names `manifest`: a stored
    /// artifact holding the manifest's hash in canonical or raw form, such
    /// as a retained frame's header. Chains name no generation. A stored
    /// row that merely contains those bytes keeps the manifest too, which
    /// errs on keeping it.
    async fn names_manifest(&self, manifest: Commitment) -> Result<bool, Error> {
        let (canonical, raw) = (encode(&manifest)?, manifest.to_vec());
        for (table, column) in REFERENCES {
            let found = collect_rows(
                self.conn
                    .query(
                        &format!(
                            "SELECT 1 FROM {table} WHERE instr({column},?1)>0 OR instr({column},?2)>0 LIMIT 1"
                        ),
                        [canonical.clone(), raw.clone()],
                    )
                    .await?,
            )
            .await?;
            if !found.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
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
            previous: inner
                .previous
                .iter()
                .filter(|previous| previous.held)
                .map(|previous| (previous.generation, previous.manifest))
                .collect(),
            baseline: inner.baseline().await?,
            plan: inner.plan_status().await?,
        })
    }
    /// Records the manifests among `steps` of earlier generations this
    /// replica lacks or dropped, such as one a peer that has not migrated
    /// still writes under: each manifest is the publisher's, and the
    /// transitions of `steps` and those recorded lead from it to a
    /// generation the replica knows. Frames refused under them are
    /// admitted again. Returns how many it recorded.
    pub async fn learn_generations(&self, steps: &[GenerationStep]) -> Result<usize, Error> {
        let mut inner = self.inner.lock().await;
        let publisher = inner.config.publisher;
        let mut verified = Vec::new();
        for step in steps {
            let transition = step.transition.verify(publisher).map_err(credential)?;
            let manifest = match &step.manifest {
                Some(signed) => {
                    let manifest = signed.verify(publisher).map_err(credential)?;
                    if transition.app != manifest.app
                        || transition.from != manifest.hash()
                        || transition.before != generation(&manifest)
                    {
                        return Err(incompatible(
                            "a step's transition does not leave its manifest",
                        ));
                    }
                    Some(manifest)
                }
                None => None,
            };
            if transition.app != inner.definition.app {
                return Err(incompatible("a step's transition is another application's"));
            }
            verified.push((manifest.is_some(), transition, step));
        }
        // Known generations, and those a transition leads from to one.
        let mut linked: BTreeSet<Commitment> = inner
            .previous
            .iter()
            .map(|previous| previous.manifest)
            .chain([inner.manifest])
            .collect();
        loop {
            let before = linked.len();
            for (_, transition, _) in &verified {
                if linked.contains(&transition.to) {
                    linked.insert(transition.from);
                }
            }
            if linked.len() == before {
                break;
            }
        }
        let held: BTreeSet<Commitment> = inner
            .previous
            .iter()
            .filter(|previous| previous.held)
            .map(|previous| previous.manifest)
            .chain([inner.manifest])
            .collect();
        let mut learned: Vec<(rocky_core::GenerationTransition, &GenerationStep)> = Vec::new();
        for (with_manifest, transition, step) in verified {
            let wanted = with_manifest
                && !held.contains(&transition.from)
                && linked.contains(&transition.to)
                && !learned
                    .iter()
                    .any(|(known, _)| known.from == transition.from);
            // A transition this replica lacks links a step it learns.
            let link = !inner
                .previous
                .iter()
                .any(|previous| previous.manifest == transition.from)
                && transition.from != inner.manifest
                && linked.contains(&transition.from);
            if wanted || link {
                learned.push((transition, step));
            }
        }
        let count = learned
            .iter()
            .filter(|(_, step)| step.manifest.is_some())
            .count();
        if count == 0 {
            return Ok(0);
        }
        let added = learned
            .iter()
            .filter(|(transition, _)| {
                !inner
                    .previous
                    .iter()
                    .any(|previous| previous.manifest == transition.from)
            })
            .count();
        if added + inner.previous.len() > MAX_GENERATIONS {
            return Err(incompatible("too many definition generations"));
        }
        let recorded = transaction!(inner, |inner| {
            for (transition, step) in &learned {
                inner.record_known(transition, step).await?;
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
    /// Forgets the manifest of each earlier generation that no retained
    /// frame, credential, checkpoint, horizon statement or existence base
    /// names; its transition stays, so retained history remains
    /// verifiable and a peer that still writes it is asked for it again.
    /// Returns the manifests dropped.
    pub async fn drop_unreferenced_generations(&self) -> Result<Vec<Commitment>, Error> {
        let mut inner = self.inner.lock().await;
        let candidates: Vec<Commitment> = inner
            .previous
            .iter()
            .filter(|previous| previous.held)
            .map(|previous| previous.manifest)
            .collect();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let mut dropped = Vec::new();
        for manifest in candidates {
            if !inner.names_manifest(manifest).await? {
                dropped.push(manifest);
            }
        }
        if dropped.is_empty() {
            return Ok(dropped);
        }
        let result = transaction!(inner, |inner| {
            for manifest in &dropped {
                inner
                    .conn
                    .execute(
                        "UPDATE secure_generations SET signed=NULL WHERE manifest=?",
                        [manifest.to_vec()],
                    )
                    .await?;
            }
            inner.load_generations().await
        });
        if let Err(error) = result {
            inner.load_generations().await?;
            return Err(error);
        }
        Ok(dropped)
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
            inner.names = inner.lineage_names().await?;
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
            node_author: Some(identity::NodeAuthor::new(&config)?),
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
            inner.bind_node_author(&inner.config.credentials).await?;
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
