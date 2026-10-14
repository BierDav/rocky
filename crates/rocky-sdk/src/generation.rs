//! Schema and handler generations of an SDK domain. A changed application
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
use rocky_core::{Commitment, Generation, GenerationTransition, ItemChanges, SchemaChange};
use rocky_credentials::{Profile, SignedTransition};
use rocky_secure::{GenerationStep, PrivateKey, RevocationSet, SignedManifest, Upgrade};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

/// Asks an issuer, on the renewal protocol, for the domain's generations.
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
/// `migration.json` beside the identity while an issuer requires fields:
/// the application version it migrates from, and whether the migrations
/// committed.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Migration {
    pub from: u32,
    pub done: bool,
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
    /// The id of the transition into the current generation.
    pub(crate) fn transition(&self) -> Result<Option<GenerationTransition>, Failure> {
        self.lineage
            .last()
            .map(|step| step.verified(self.publisher))
            .transpose()
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
    /// replica rebuilds its tables. A replicated or handler change makes
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
        let commands = match &schema.commands {
            Some(definitions) => definitions.build()?,
            None => Default::default(),
        };
        let handled =
            rocky_core::classify_commands(old.app.commands(), &commands).map_err(|refusal| {
                format!("the application's commands cannot replace the installed ones: {refusal}")
            })?;
        if let SchemaChange::Additive { required, .. } = &change
            && !required.is_empty()
        {
            return self
                .evolve_required(path, vault, schema, required, transport)
                .await;
        }
        if !change.replicated() && !handled {
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
        if self.issuer() {
            self.sign_generation(path, vault, schema)
        } else {
            self.fetch_or_wait(path, vault, schema, transport).await
        }
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
    /// Whether this issuer started requiring fields of `schema`: the
    /// installed schema holds them nullable, and the open migrates.
    pub(crate) fn started_requiring(&self, schema: &Schema) -> Result<bool, Failure> {
        if !self.issuer() {
            return Ok(false);
        }
        let publisher = *self.publisher.as_bytes();
        Ok(matches!(
            rocky_core::classify(self.domain()?.logical(), &logical(schema, publisher)?),
            Ok(SchemaChange::Additive { tables, fields, required, removed })
                if tables.is_empty() && fields.is_empty() && removed.is_empty() && !required.is_empty()
        ))
    }
    /// Moves to a schema that requires fields rows of the installed one
    /// lack. A member, or an issuer another issuer already moved, fetches
    /// the generation. Otherwise this issuer signs the schema with those
    /// fields nullable here; the open migrates and calls [`Self::require`].
    async fn evolve_required(
        &mut self,
        path: &Path,
        vault: &Vault,
        schema: &Schema,
        required: &BTreeSet<String>,
        transport: Option<&dyn Transport>,
    ) -> Result<(), Failure> {
        let relaxed = relaxed(schema, required)?;
        // Interrupted after the nullable step: the migration continues.
        if self.issuer() && self.schema == relaxed {
            return Ok(());
        }
        if self.lineage.len() + 2 > MAX_LINEAGE {
            return Err("the domain has too many generations".into());
        }
        let fetched = match transport {
            Some(transport) => self.fetch_generation(path, vault, schema, transport).await,
            None => Err(ServiceError::NetworkRequired("upgrade").into()),
        };
        match fetched {
            Ok(()) => Ok(()),
            // A member waits for an issuer to migrate; the application is ahead.
            Err(_) if !self.issuer() => Ok(()),
            Err(_) => {
                let publisher = *self.publisher.as_bytes();
                match rocky_core::classify(self.domain()?.logical(), &logical(&relaxed, publisher)?)
                {
                    Ok(SchemaChange::Additive { required, .. }) if required.is_empty() => {
                        Migration {
                            from: self.schema.version,
                            done: false,
                        }
                        .write(path)?;
                        self.sign_generation(path, vault, &relaxed)
                    }
                    // Only nullability tightens: the installed schema is the
                    // generation the migration runs in.
                    Ok(SchemaChange::Unchanged | SchemaChange::Local) => Ok(()),
                    Ok(SchemaChange::Additive { .. }) | Err(_) => {
                        Err("the schema's required fields cannot be added in one step".into())
                    }
                }
            }
        }
    }
    /// After the migration: signs `schema`, which requires the fields the
    /// installed schema holds nullable.
    pub(crate) fn require(
        &mut self,
        path: &Path,
        vault: &Vault,
        schema: &Schema,
    ) -> Result<(), Failure> {
        if !self.started_requiring(schema)? {
            return Err("only fields the installed schema holds can become required".into());
        }
        self.sign_generation(path, vault, schema)
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
        let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
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
                handler: before.handler,
            },
            after: Generation {
                schema: after.schema,
                handler: after.handler,
            },
            added,
            tables: ItemChanges::between(&before.entities, &after.entities),
            fields: ItemChanges::between(&before.fields, &after.fields),
            time: crate::enrollment::now_micros()?,
        };
        let signed = SignedTransition::sign(&transition, &publisher)?;
        let mut draft = self.clone();
        draft.lineage.push(Step {
            manifest: Some(self.manifest.clone()),
            transition: HEXLOWER.encode(&signed.to_bytes()?),
        });
        draft.schema = schema.clone();
        draft.manifest = HEXLOWER.encode(&SignedManifest::sign(&after, &publisher)?.to_bytes()?);
        for (name, _) in domain.scopes() {
            if !draft.keys.contains_key(name) {
                let key = reader_key(&publisher, self.domain, name);
                let epoch = rocky_core::KeyEpoch::new(1, &key);
                let role = key_role(name, epoch);
                if vault.find(&role)?.is_none() {
                    vault.put(&role, &key)?;
                }
                let mut ids = draft
                    .revocations
                    .statements()
                    .map(|signed| signed.statement.id())
                    .collect::<Result<Vec<_>, _>>()?;
                ids.sort_unstable();
                draft.epoch_statements.insert(role, ids);
                draft.keys.insert(name.to_owned(), vec![epoch]);
            }
        }
        draft.credentials = draft.issue_own(
            vault,
            crate::enrollment::grants_of(&Profile::Issuer, &domain)?,
        )?;
        draft.save(path)?;
        *self = draft;
        Ok(())
    }
    async fn fetch_generation(
        &mut self,
        path: &Path,
        vault: &Vault,
        schema: &Schema,
        transport: &dyn Transport,
    ) -> Result<(), Failure> {
        self.resolve_saved_routes(transport).await;
        let mut failure: Failure = ServiceError::NoRoute {
            peer: "an issuer",
            network: transport.kind().into(),
        }
        .into();
        for (issuer, addresses) in self.issuer_routes(transport, None) {
            let attempt = async {
                let offer = ask(transport, &addresses).await?;
                let draft = self.adopt_offer(offer, Some(schema))?;
                draft
                    .renew_into(path, vault, transport, &[(issuer, addresses.clone())], self)
                    .await
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
    /// Moves this identity to the newer generation `peer` runs, whatever
    /// schema the application declares: its offer is verified, and an
    /// issuer, `peer` first, renews this device into it. Everything is
    /// saved before the replica switches, which resumes after a crash.
    /// Returns whether the generation was newer.
    pub(crate) async fn discover(
        &mut self,
        path: &Path,
        vault: &Vault,
        transport: &dyn Transport,
        peer: rocky_credentials::PublicKey,
        addresses: &[String],
    ) -> Result<bool, Failure> {
        let offer = ask(transport, addresses).await?;
        if offer.manifest == self.manifest {
            return Ok(false);
        }
        let draft = self.adopt_offer(offer, None)?;
        self.resolve_saved_routes(transport).await;
        let routes = self.issuer_routes(transport, Some((peer, addresses)));
        if routes.is_empty() {
            return Err(ServiceError::NoRoute {
                peer: "an issuer",
                network: transport.kind().into(),
            }
            .into());
        }
        let mut failure = None;
        for route in &routes {
            match draft
                .clone()
                .renew_into(path, vault, transport, std::slice::from_ref(route), self)
                .await
            {
                Ok(adopted) => {
                    *self = adopted;
                    return Ok(true);
                }
                Err(error) => failure = Some(error),
            }
        }
        Err(failure.unwrap_or_else(|| "no issuer renewed this device".into()))
    }
    /// Issuers with routes over `transport`, `first` ahead of the others
    /// when it is one.
    fn issuer_routes(
        &self,
        transport: &dyn Transport,
        first: Option<(rocky_credentials::PublicKey, &[String])>,
    ) -> Vec<(rocky_credentials::PublicKey, Vec<String>)> {
        let mut routes = Vec::new();
        if let Some((peer, addresses)) = first
            && self.issuers.contains(&peer)
            && !addresses.is_empty()
        {
            routes.push((peer, addresses.to_vec()));
        }
        for issuer in &self.issuers {
            if routes.iter().any(|(known, _)| known == issuer) {
                continue;
            }
            let addresses = self
                .peers
                .get(issuer)
                .map(|contact| contact.addresses(transport.kind()))
                .unwrap_or_default();
            if !addresses.is_empty() {
                routes.push((*issuer, addresses));
            }
        }
        routes
    }
    /// This draft generation with credentials an issuer of `routes`
    /// renewed into it, saved.
    async fn renew_into(
        mut self,
        path: &Path,
        vault: &Vault,
        transport: &dyn Transport,
        routes: &[(rocky_credentials::PublicKey, Vec<String>)],
        previous: &Self,
    ) -> Result<Self, Failure> {
        let mut failure: Failure = "no issuer renewed this device".into();
        for (issuer, addresses) in routes {
            match self
                .renew_into_generation(vault, transport, *issuer, addresses, previous)
                .await
            {
                Ok(renewed) => {
                    self.adopt(path, vault, &renewed)?;
                    return Ok(self);
                }
                Err(error) => failure = error,
            }
        }
        Err(failure)
    }
    /// The signed steps a device's `reply` to [`OFFER`] and this identity
    /// hold: what a replica that lacks an older generation learns it from.
    /// The replica verifies each and keeps those that lead to a generation
    /// it knows.
    pub(crate) fn older_steps(&self, reply: &[u8]) -> Result<Vec<GenerationStep>, Failure> {
        let offer: Offer = serde_json::from_slice(reply)?;
        if offer.lineage.len() > MAX_LINEAGE {
            return Err("the offered lineage is too long".into());
        }
        // The peer's own manifest and those either side holds, by hash.
        let mut manifests = std::collections::BTreeMap::new();
        for manifest in [&offer.manifest].into_iter().chain(
            offer
                .lineage
                .iter()
                .chain(&self.lineage)
                .filter_map(|step| step.manifest.as_ref()),
        ) {
            let signed = SignedManifest::from_bytes(&HEXLOWER.decode(manifest.as_bytes())?)?;
            manifests.insert(signed.verify(self.publisher)?.hash(), signed);
        }
        offer
            .lineage
            .iter()
            .chain(&self.lineage)
            .map(|step| {
                let transition = step.signed_transition()?;
                let from = transition.verify(self.publisher)?.from;
                Ok(GenerationStep {
                    manifest: manifests.get(&from).cloned(),
                    transition,
                })
            })
            .collect()
    }
    /// The identity at the generation a device's `reply` to [`OFFER`]
    /// names, verified as [`Self::discover`] verifies it; nothing is saved.
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
        if offer.manifest != self.manifest
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
                manifest.hash() != transition.from
                    || manifest.schema != transition.before.schema
                    || manifest.handler != transition.before.handler
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
        let ends = transitions.last().is_none_or(|last| {
            last.to == manifest.hash()
                && last.after.schema == manifest.schema
                && last.after.handler == manifest.handler
        });
        if !ends
            || transitions
                .windows(2)
                .any(|pair| pair[0].to != pair[1].from || pair[0].after != pair[1].before)
        {
            return Err(forged());
        }
        Ok(())
    }
    /// Holds the manifests among `steps` of the generations `held` names,
    /// which the replica now holds. Returns whether any changed.
    pub(crate) fn hold_manifests(
        &mut self,
        steps: &[GenerationStep],
        held: &BTreeSet<Commitment>,
    ) -> Result<bool, Failure> {
        let mut changed = false;
        for step in &mut self.lineage {
            let from = step.verified(self.publisher)?.from;
            if step.manifest.is_some() || !held.contains(&from) {
                continue;
            }
            for candidate in steps.iter().filter_map(|step| step.manifest.as_ref()) {
                if candidate.verify(self.publisher)?.hash() == from {
                    step.manifest = Some(HEXLOWER.encode(&candidate.to_bytes()?));
                    changed = true;
                    break;
                }
            }
        }
        Ok(changed)
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
/// publisher key, so issuers that sign the same schema apart hold the same
/// key. Revocation rotates it to random later epochs as any other.
fn reader_key(publisher: &PrivateKey, domain: rocky_core::DomainId, table: &str) -> [u8; 32] {
    let mut input = publisher.as_bytes().to_vec();
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

/// `schema` with each `table.field` of `required` nullable: the generation
/// the migration runs in before they become required.
pub(crate) fn relaxed(schema: &Schema, required: &BTreeSet<String>) -> Result<Schema, Failure> {
    let mut relaxed = schema.clone();
    let mut found = 0;
    for table in &mut relaxed.tables {
        for column in &mut table.columns {
            if required.contains(&format!("{}.{}", table.name, column.name)) {
                column.not_null = false;
                found += 1;
            }
        }
    }
    if found != required.len() {
        return Err("a required field is not a column of the schema".into());
    }
    Ok(relaxed)
}
