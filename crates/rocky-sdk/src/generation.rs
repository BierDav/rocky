//! Schema generations of an SDK domain. A changed application
//! schema either rebuilds local tables, or moves the domain to the next
//! signed generation: an issuer signs it, a member fetches it from an
//! issuer with credentials for it. The lineage keeps every signed
//! transition; an earlier generation's manifest only while the replica
//! holds it. See `docs/runtime/generations.md`.
use crate::{
    Failure,
    domain::{Domain, logical},
    enrollment::{Settings, exchange, key_role},
    schema::Schema,
    secrets::Vault,
    service::ServiceError,
    transport::{Protocol, Transport},
};
use data_encoding::HEXLOWER;
use rocky_core::{Commitment, Generation, GenerationTransition, ItemChanges};
use rocky_credentials::SignedTransition;
use rocky_secure::{GenerationStep, RevocationSet, SignedManifest, Upgrade};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

/// Asks a device, on the renewal protocol, for the domain's generations; every device answers.
pub(crate) const OFFER: &[u8] = b"rocky.sdk.generations.v1\n";
/// Earlier generations a domain keeps.
const MAX_LINEAGE: usize = 32;

/// A generation a domain left: the signed transition to the next and,
/// while the replica holds it, the generation's signed manifest. All of it
/// is public.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    pub transition: String,
}
impl Step {
    fn signed(&self) -> Result<GenerationStep, Failure> {
        Ok(GenerationStep {
            manifest: self.signed_manifest()?,
            transition: self.signed_transition()?,
        })
    }
    fn signed_manifest(&self) -> Result<Option<SignedManifest>, Failure> {
        Ok(match &self.manifest {
            Some(manifest) => Some(SignedManifest::from_bytes(
                &HEXLOWER.decode(manifest.as_bytes())?,
            )?),
            None => None,
        })
    }
    fn signed_transition(&self) -> Result<SignedTransition, Failure> {
        Ok(SignedTransition::from_bytes(
            &HEXLOWER.decode(self.transition.as_bytes())?,
        )?)
    }
    /// The transition out of this generation, the publisher's.
    pub(crate) fn verified(
        &self,
        publisher: rocky_credentials::PublicKey,
    ) -> Result<GenerationTransition, Failure> {
        Ok(self.signed_transition()?.verify(publisher)?)
    }
}
/// `migration.json` beside the identity, from before an issuer signs a
/// generation whose schema has migrations until they commit: the
/// application version it migrates from. The replica's publication record,
/// committed with the migration, is the done mark.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Migration {
    pub from: u32,
}
impl Migration {
    pub(crate) fn path(identity: &Path) -> std::path::PathBuf {
        identity.with_file_name("migration.json")
    }
    pub(crate) fn read(identity: &Path) -> Result<Option<Self>, Failure> {
        match std::fs::read(Self::path(identity)) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    pub(crate) fn write(&self, identity: &Path) -> Result<(), Failure> {
        rocky_credentials::write_private(&Self::path(identity), &serde_json::to_vec(self)?)?;
        Ok(())
    }
    pub(crate) fn remove(identity: &Path) -> Result<(), Failure> {
        match std::fs::remove_file(Self::path(identity)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }
}
/// Whether `schema` has migrations above application version `from`.
pub(crate) fn migrates(schema: &Schema, from: u32) -> bool {
    schema
        .migrations
        .iter()
        .any(|migration| migration.version > from && migration.version <= schema.version)
}
/// An issuer's current definition and how it got there.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    lineage: Vec<Step>,
    schema: Schema,
    manifest: String,
    revocations: RevocationSet,
}

/// The schema generation a lineage leads to; the field ids its transitions
/// added to, or required of, tables that already existed: rows written
/// before them may lack them; and the table and field ids its transitions
/// removed.
pub(crate) fn lineage(
    steps: &[Step],
    publisher: rocky_credentials::PublicKey,
) -> Result<(u32, BTreeSet<Commitment>, BTreeSet<Commitment>), Failure> {
    let (mut generation, mut added, mut removed) = (1, BTreeSet::new(), BTreeSet::new());
    for step in steps {
        let transition = step.verified(publisher)?;
        generation = transition.after.schema;
        added.extend(transition.added);
        removed.extend(transition.tables.removed);
        removed.extend(transition.fields.removed);
    }
    Ok((generation, added, removed))
}

impl Settings {
    /// What the replica needs to activate or admit earlier generations.
    pub(crate) fn upgrade(&self, domain: &Domain) -> Result<Option<Upgrade>, Failure> {
        if self.lineage.is_empty() {
            return Ok(None);
        }
        Ok(Some(Upgrade {
            steps: self
                .lineage
                .iter()
                .map(Step::signed)
                .collect::<Result<_, _>>()?,
            tables: Some((domain.logical().clone(), domain.scope, domain.readers())),
        }))
    }
    /// This issuer's answer to a member asking for the domain's generations.
    pub(crate) fn offer(&self) -> Result<Vec<u8>, Failure> {
        Ok(serde_json::to_vec(&Offer {
            lineage: self.lineage.clone(),
            schema: self.schema.clone(),
            manifest: self.manifest.clone(),
            revocations: self.revocations.clone(),
        })?)
    }
    /// Moves the identity to `schema`. A local change only records it; the
    /// replica rebuilds its tables. A replicated change makes
    /// the next generation: signed here on an issuer, fetched from an
    /// issuer over `transport` on a member. Everything is saved before the
    /// replica opens, which activates it. Refused changes leave nothing.
    pub(crate) async fn evolve(
        &mut self,
        path: &Path,
        vault: &Vault,
        schema: &Schema,
        transport: Option<&dyn Transport>,
    ) -> Result<(), Failure> {
        let old = self.domain()?;
        let publisher = *self.publisher.as_bytes();
        let change = rocky_core::classify(old.logical(), &logical(schema, publisher)?).map_err(
            |refusal| format!("the application schema cannot replace the installed one: {refusal}"),
        )?;
        // Grants issued before would silently change meaning. An entry goes
        // with its table, or with the table it followed.
        let kept = |name: &str| schema.tables.iter().any(|table| table.name == name);
        if let Some(table) = self.schema.tables.iter().find(|table| {
            let (before, after) = (
                self.schema.granted_like.get(&table.name),
                schema.granted_like.get(&table.name),
            );
            kept(&table.name)
                && before != after
                && !(after.is_none() && before.is_some_and(|like| !kept(like)))
        }) {
            return Err(format!(
                "the application schema cannot replace the installed one: {} changed whom it is granted like; declare that only when adding a table",
                table.name
            )
            .into());
        }
        if !change.replicated() {
            let mut draft = self.clone();
            draft.schema = schema.clone();
            if draft.domain()?.manifest().hash() != old.manifest().hash() {
                return Err("a local schema change must keep the signed manifest".into());
            }
            draft.save(path)?;
            *self = draft;
            return Ok(());
        }
        if self.lineage.len() >= MAX_LINEAGE {
            return Err("the domain has too many generations".into());
        }
        if !self.issuer() {
            return self.fetch_or_wait(path, vault, schema, transport).await;
        }
        // Migrations run once, after signing, on an issuer that reads and
        // writes every partition; one another issuer signed is adopted.
        let from = Migration::read(path)?.map_or(self.schema.version, |pending| pending.from);
        if migrates(schema, from) {
            if let Some(transport) = transport
                && self
                    .fetch_generation(path, vault, schema, transport)
                    .await
                    .is_ok()
            {
                return Migration::remove(path);
            }
            if !self.may_migrate(&old)? {
                return Ok(());
            }
            Migration { from }.write(path)?;
        }
        self.sign_generation(path, vault, schema)
    }
    /// Whether this device may run migrations: an issuer that holds the
    /// key of, reads and writes every partition the domain knows.
    pub(crate) fn may_migrate(&self, domain: &Domain) -> Result<bool, Failure> {
        Ok(self.issuer()
            && self.partitions(domain)?.iter().all(|partition| {
                !partition.record.epochs.is_empty()
                    && partition
                        .members
                        .iter()
                        .all(|member| self.owns_partition(member))
            }))
    }
    /// A member fetches the generation of `schema` from an issuer that
    /// signed it. Until one does, or while none is reachable, the database
    /// stays at its generation and the application is reported ahead.
    async fn fetch_or_wait(
        &mut self,
        path: &Path,
        vault: &Vault,
        schema: &Schema,
        transport: Option<&dyn Transport>,
    ) -> Result<(), Failure> {
        if let Some(transport) = transport {
            // Any failure leaves the identity as it was.
            let _ = self.fetch_generation(path, vault, schema, transport).await;
        }
        Ok(())
    }
    /// How the schema an application declares stands to the installed
    /// generation: behind when it defines what a generation this domain
    /// left defined, unless it declares a later version than the installed
    /// schema, which may add a removed name back. Versions are local and
    /// unsigned, so they decide nothing else.
    pub(crate) fn application(&self, declared: &Schema) -> Result<crate::DeclaredSchema, Failure> {
        if *declared == self.schema {
            return Ok(crate::DeclaredSchema::Current);
        }
        if declared.version > self.schema.version {
            return Ok(crate::DeclaredSchema::Ahead);
        }
        for step in &self.lineage {
            let transition = step.verified(self.publisher)?;
            if self.manifest_at(declared, transition.before.schema)? == transition.from {
                return Ok(crate::DeclaredSchema::Behind);
            }
        }
        Ok(crate::DeclaredSchema::Ahead)
    }
    /// The hash of the manifest `schema` defines at schema generation
    /// `generation`.
    fn manifest_at(&self, schema: &Schema, generation: u32) -> Result<Commitment, Failure> {
        let publisher = *self.publisher.as_bytes();
        Ok(Domain::at(
            schema,
            publisher,
            self.mesh,
            self.domain,
            generation,
            &BTreeSet::new(),
        )?
        .manifest()
        .hash())
    }
    fn sign_generation(
        &mut self,
        path: &Path,
        vault: &Vault,
        schema: &Schema,
    ) -> Result<(), Failure> {
        let identity = self.identity(vault)?;
        let links = self.issuer_links();
        let old = self.domain()?;
        let before = old.manifest();
        let new = logical(schema, *self.publisher.as_bytes())?;
        let change = rocky_core::classify(old.logical(), &new)?;
        let added = field_ids(&new, &change.added())?;
        let (_, mut all, _) = lineage(&self.lineage, self.publisher)?;
        all.extend(added.iter().copied());
        let generation = before.schema + u32::from(change.replicated());
        let domain = Domain::at(
            schema,
            *self.publisher.as_bytes(),
            self.mesh,
            self.domain,
            generation,
            &all,
        )?;
        let after = domain.manifest();
        let transition = GenerationTransition {
            app: after.app,
            from: before.hash(),
            to: after.hash(),
            before: Generation {
                schema: before.schema,
            },
            after: Generation {
                schema: after.schema,
            },
            added,
            tables: ItemChanges::between(&before.entities, &after.entities),
            fields: ItemChanges::between(&before.fields, &after.fields),
            time: crate::enrollment::now_micros()?,
        };
        // The owner, or an issuer through its chain, signs.
        let signed = SignedTransition::endorse(&transition, &*identity, &links)?;
        let mut draft = self.clone();
        draft.lineage.push(Step {
            manifest: Some(self.manifest.clone()),
            transition: HEXLOWER.encode(&signed.to_bytes()?),
        });
        draft.schema = schema.clone();
        draft.manifest =
            HEXLOWER.encode(&SignedManifest::endorse(&after, &*identity, &links)?.to_bytes()?);
        draft.issue_for_generation(vault, &domain)?;
        draft.save(path)?;
        *self = draft;
        Ok(())
    }
    /// On an issuer, what a new generation needs locally: the first reader
    /// key of each table it adds, derived from the registry key so that
    /// issuers apart derive the same, and its own node certificate over
    /// every table. No other device needs anything: chains name no
    /// generation.
    fn issue_for_generation(&mut self, vault: &Vault, domain: &Domain) -> Result<(), Failure> {
        if !self.issuer() {
            return Ok(());
        }
        let registry = self.registry_key(vault)?;
        for (name, _) in domain.scopes() {
            if !self.keys.contains_key(name) {
                let key = reader_key(&registry, self.domain, name);
                let epoch = rocky_core::KeyEpoch::new(1, &key);
                let role = key_role(name, epoch);
                if vault.find(&role)?.is_none() {
                    vault.put(&role, &key)?;
                }
                let mut ids = self
                    .revocations
                    .statements()
                    .map(|signed| signed.statement.id())
                    .collect::<Result<Vec<_>, _>>()?;
                ids.sort_unstable();
                self.epoch_statements.insert(role, ids);
                self.keys.insert(name.to_owned(), vec![epoch]);
            }
        }
        self.credentials = self.issue_own(vault)?;
        Ok(())
    }
    /// The registry's current key: its highest epoch, the lowest of
    /// siblings, as writers choose it.
    fn registry_key(&self, vault: &Vault) -> Result<[u8; 32], Failure> {
        let epoch = self
            .keys
            .get(crate::domain::REGISTRY)
            .and_then(|epochs| {
                epochs.iter().copied().max_by(|a, b| {
                    a.generation
                        .cmp(&b.generation)
                        .then(b.secret_hash.cmp(&a.secret_hash))
                })
            })
            .ok_or("an issuer holds the registry key")?;
        Ok(vault.get(&key_role(crate::domain::REGISTRY, epoch))?)
    }
    /// Fetches the generation of `schema` from a device that runs it,
    /// issuers first, and adopts it.
    async fn fetch_generation(
        &mut self,
        path: &Path,
        vault: &Vault,
        schema: &Schema,
        transport: &dyn Transport,
    ) -> Result<(), Failure> {
        self.resolve_saved_routes(transport).await;
        let mut failure: Failure = ServiceError::NoRoute {
            peer: "a device at the generation",
            network: transport.kind().into(),
        }
        .into();
        for addresses in self.generation_routes(transport) {
            let attempt = async {
                let offer = ask(transport, &addresses).await?;
                self.adopt_offer(offer, Some(schema))?.settle(path, vault)
            };
            match attempt.await {
                Ok(draft) => {
                    *self = draft;
                    return Ok(());
                }
                Err(error) => failure = error,
            }
        }
        Err(failure)
    }
    /// This verified draft generation, with what an issuer derives for it,
    /// saved.
    fn settle(mut self, path: &Path, vault: &Vault) -> Result<Self, Failure> {
        let domain = self.domain()?;
        self.issue_for_generation(vault, &domain)?;
        self.save(path)?;
        Ok(self)
    }
    /// Routes over `transport` to the issuers, then to every other saved
    /// peer: any of them may serve the generations.
    fn generation_routes(&self, transport: &dyn Transport) -> Vec<Vec<String>> {
        let issuers = self
            .issuers
            .iter()
            .filter_map(|issuer| self.peers.get(issuer));
        let others = self
            .peers
            .iter()
            .filter(|(peer, _)| !self.issuers.contains(peer))
            .map(|(_, contact)| contact);
        issuers
            .chain(others)
            .map(|contact| contact.addresses(transport.kind()))
            .filter(|addresses| !addresses.is_empty())
            .collect()
    }
    /// The identity at the generation a device's `reply` to [`OFFER`]
    /// names; its signatures and lineage are verified and nothing is saved.
    #[cfg(test)]
    pub(crate) fn verify_offer(&self, reply: &[u8]) -> Result<Self, Failure> {
        self.adopt_offer(serde_json::from_slice(reply)?, None)
    }
    /// The identity at another device's generation, checked before anything
    /// is saved: every manifest is the publisher's, each schema derives its
    /// manifest, the transitions chain through this device's generation and
    /// end at the offered one. With `schema`, it must be the offered schema.
    fn adopt_offer(&self, offer: Offer, schema: Option<&Schema>) -> Result<Self, Failure> {
        if schema.is_some_and(|schema| offer.schema != *schema) {
            return Err(
                "the issuer runs another application schema; upgrade to the same application version"
                    .into(),
            );
        }
        let own = self.domain()?.manifest().hash();
        if offer.lineage.len() > MAX_LINEAGE
            || !offer.lineage.iter().any(|step| {
                step.verified(self.publisher)
                    .is_ok_and(|step| step.from == own)
            })
        {
            return Err("the offered generations do not include this device's".into());
        }
        let mut draft = self.clone();
        draft.lineage = offer.lineage;
        draft.schema = offer.schema;
        draft.manifest = offer.manifest;
        // The manifests this device holds stay held, its own among them.
        let mut held = self.lineage.clone();
        held.push(Step {
            manifest: Some(self.manifest.clone()),
            transition: String::new(),
        });
        for step in &mut draft.lineage {
            if step.manifest.is_none() {
                let from = step.verified(self.publisher)?.from;
                step.manifest = held
                    .iter()
                    .filter_map(|held| held.manifest.as_ref())
                    .find(|manifest| {
                        manifest_hash(manifest, self.publisher).is_ok_and(|hash| hash == from)
                    })
                    .cloned();
            }
        }
        draft.verify_lineage()?;
        // A step this device holds signed earlier keeps its baseline.
        keep_earlier(&mut draft.lineage, &self.lineage, self.publisher)?;
        draft.learn_revocations(&offer.revocations)?;
        // What this device meets for the first time needs a signer no held
        // statement ended: a revoked issuer moves nobody to a generation.
        let time = crate::enrollment::now()?;
        draft
            .manifest()?
            .verify_current(self.publisher, &draft.revocations, time)?;
        for step in &draft.lineage {
            if !self
                .lineage
                .iter()
                .any(|held| held.transition == step.transition)
            {
                step.signed_transition()?.verify_current(
                    self.publisher,
                    &draft.revocations,
                    time,
                )?;
            }
        }
        Ok(draft)
    }
    /// This identity with the transitions another device at the same
    /// generation signed earlier, and those transitions; none when it
    /// holds no earlier one. Issuers that signed one schema apart made one
    /// generation, whose baseline is the earliest signing time.
    pub(crate) fn earlier_baselines(
        &self,
        reply: &[u8],
    ) -> Result<Option<(Self, Vec<SignedTransition>)>, Failure> {
        let offer: Offer = serde_json::from_slice(reply)?;
        if manifest_hash(&offer.manifest, self.publisher)?
            != manifest_hash(&self.manifest, self.publisher)?
            || offer.lineage.len() != self.lineage.len()
            || offer
                .lineage
                .iter()
                .zip(&self.lineage)
                .any(|(theirs, ours)| {
                    theirs.verified(self.publisher).ok().map(|step| step.from)
                        != ours.verified(self.publisher).ok().map(|step| step.from)
                })
        {
            return Ok(None);
        }
        let mut draft = self.clone();
        let changed = keep_earlier(&mut draft.lineage, &offer.lineage, self.publisher)?;
        if changed.is_empty() {
            return Ok(None);
        }
        draft.verify_lineage()?;
        let transitions = changed
            .into_iter()
            .map(|index| draft.lineage[index].signed_transition())
            .collect::<Result<_, Failure>>()?;
        Ok(Some((draft, transitions)))
    }
    /// Checks the lineage and current generation as [`Self::adopt_offer`]
    /// describes: the transitions chain, each held manifest is the one its
    /// transition leaves, and the current schema derives the current
    /// manifest.
    pub(crate) fn verify_lineage(&self) -> Result<(), Failure> {
        let forged = || -> Failure { "the offered generation is not the publisher's".into() };
        let mut transitions = Vec::with_capacity(self.lineage.len());
        let mut manifests = Vec::with_capacity(self.lineage.len() + 1);
        for step in &self.lineage {
            let transition = step.verified(self.publisher)?;
            let held = match step.signed_manifest()? {
                Some(signed) => Some(signed.verify(self.publisher)?),
                None => None,
            };
            if held.as_ref().is_some_and(|manifest| {
                manifest.hash() != transition.from || manifest.schema != transition.before.schema
            }) {
                return Err(forged());
            }
            transitions.push(transition);
            manifests.push(held);
        }
        let manifest = self.manifest()?.verify(self.publisher)?;
        manifests.push(Some(manifest.clone()));
        // Where both manifests of a step are held, it names and removes
        // what they differ in.
        for (index, transition) in transitions.iter().enumerate() {
            if let (Some(old), Some(new)) = (&manifests[index], &manifests[index + 1])
                && (transition.tables != ItemChanges::between(&old.entities, &new.entities)
                    || transition.fields != ItemChanges::between(&old.fields, &new.fields))
            {
                return Err(forged());
            }
        }
        if manifest.hash() != self.domain()?.manifest().hash() {
            return Err(forged());
        }
        let ends = transitions
            .last()
            .is_none_or(|last| last.to == manifest.hash() && last.after.schema == manifest.schema);
        if !ends
            || transitions
                .windows(2)
                .any(|pair| pair[0].to != pair[1].from || pair[0].after != pair[1].before)
        {
            return Err(forged());
        }
        Ok(())
    }
    /// Forgets the manifests of the generations `dropped` names, which the
    /// replica dropped; their transitions stay. Returns whether any changed.
    pub(crate) fn forget_manifests(&mut self, dropped: &[Commitment]) -> Result<bool, Failure> {
        let mut changed = false;
        for step in &mut self.lineage {
            if step.manifest.is_some() && dropped.contains(&step.verified(self.publisher)?.from) {
                step.manifest = None;
                changed = true;
            }
        }
        Ok(changed)
    }
}

/// The hash of a hex signed manifest, the publisher's.
fn manifest_hash(
    manifest: &str,
    publisher: rocky_credentials::PublicKey,
) -> Result<Commitment, Failure> {
    Ok(
        SignedManifest::from_bytes(&HEXLOWER.decode(manifest.as_bytes())?)?
            .verify(publisher)?
            .hash(),
    )
}
/// The ids of `table.field` names of `schema`.
fn field_ids(
    schema: &rocky_core::LogicalSchema,
    fields: &BTreeSet<String>,
) -> Result<BTreeSet<Commitment>, Failure> {
    fields
        .iter()
        .map(|field| {
            let (table, column) = field.split_once('.').ok_or("a field names its table")?;
            let table = schema.table(table).ok_or("an added field's table")?;
            let column = table.column(column).ok_or("an added field's column")?;
            Ok(schema.field_id(table, column))
        })
        .collect()
}

/// The first reader key of a table a generation adds: derived from the
/// registry's current key, which every issuer holds and a revoked one no
/// longer receives, so issuers that sign the same schema apart hold the
/// same key. Revocation rotates it to random later epochs as any other.
fn reader_key(registry: &[u8; 32], domain: rocky_core::DomainId, table: &str) -> [u8; 32] {
    let mut input = registry.to_vec();
    input.extend(domain.0);
    input.extend(table.as_bytes());
    rocky_core::commitment("rocky.sdk.generation-reader.v1", &input)
}

/// Replaces each step of `lineage` whose transition `other` holds signed
/// earlier for the same step; returns the indexes replaced.
fn keep_earlier(
    lineage: &mut [Step],
    other: &[Step],
    publisher: rocky_credentials::PublicKey,
) -> Result<Vec<usize>, Failure> {
    let mut changed = Vec::new();
    for (index, step) in lineage.iter_mut().enumerate() {
        let current = step.verified(publisher)?;
        for earlier in other
            .iter()
            .filter(|other| other.transition != step.transition)
        {
            let candidate = earlier.verified(publisher)?;
            if candidate.same_step(&current) && candidate.time < current.time {
                step.transition = earlier.transition.clone();
                changed.push(index);
                break;
            }
        }
    }
    Ok(changed)
}

/// Asks a peer, on the renewal protocol, for the generations it runs.
async fn ask(transport: &dyn Transport, addresses: &[String]) -> Result<Offer, Failure> {
    Ok(serde_json::from_slice(
        &ask_bytes(transport, addresses).await?,
    )?)
}
/// A peer's reply to [`OFFER`].
pub(crate) async fn ask_bytes(
    transport: &dyn Transport,
    addresses: &[String],
) -> Result<Vec<u8>, Failure> {
    exchange(transport, addresses, Protocol::Renew, OFFER).await
}
