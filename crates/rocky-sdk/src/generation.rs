//! Schema and handler generations of an SDK domain. A changed application
//! schema either rebuilds local tables, or moves the domain to the next
//! signed generation: an issuer signs it, a member fetches it from an
//! issuer with credentials for it. See `docs/runtime/generations.md`.
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
use rocky_core::{Generation, GenerationTransition, SchemaChange};
use rocky_credentials::{Profile, SignedTransition};
use rocky_secure::{GenerationStep, PrivateKey, RevocationSet, SignedManifest, Upgrade};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

/// Asks an issuer, on the renewal protocol, for the domain's generations.
pub(crate) const OFFER: &[u8] = b"rocky.sdk.generations.v1\n";
/// Earlier generations a domain keeps.
const MAX_LINEAGE: usize = 32;

/// A generation a domain left: its schema, signed manifest and the signed
/// transition to the next. All of it is public.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub schema: Schema,
    pub manifest: String,
    pub transition: String,
}
impl Step {
    fn signed(&self) -> Result<GenerationStep, Failure> {
        Ok(GenerationStep {
            manifest: SignedManifest::from_bytes(&HEXLOWER.decode(self.manifest.as_bytes())?)?,
            transition: SignedTransition::from_bytes(
                &HEXLOWER.decode(self.transition.as_bytes())?,
            )?,
        })
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

/// The schema generation of a lineage ending at `schema`, and the fields
/// its transitions added to, or required of, collections that already
/// existed: rows written before them may lack them.
pub(crate) fn lineage(
    steps: &[Step],
    schema: &Schema,
    publisher: [u8; 32],
) -> Result<(u32, BTreeSet<String>), Failure> {
    let (mut generation, mut added) = (1, BTreeSet::new());
    let schemas: Vec<&Schema> = steps
        .iter()
        .map(|step| &step.schema)
        .chain([schema])
        .collect();
    for pair in schemas.windows(2) {
        let change =
            rocky_core::classify(&logical(pair[0], publisher)?, &logical(pair[1], publisher)?)?;
        if change.replicated() {
            generation += 1;
            added.extend(change.added());
        }
    }
    Ok((generation, added))
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
        let Some(step) = self.lineage.last() else {
            return Ok(None);
        };
        Ok(Some(step.signed()?.transition.verify(self.publisher)?))
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
        if let SchemaChange::Breaking { required, .. } = &change {
            return self
                .evolve_breaking(path, vault, schema, required, transport)
                .await;
        }
        // Grants issued before would silently change meaning.
        if let Some(table) = self.schema.tables.iter().find(|table| {
            self.schema.granted_like.get(&table.name) != schema.granted_like.get(&table.name)
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
            self.sign_generation(path, vault, schema, false)
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
    /// left defined. Versions are local and never signed, so they decide
    /// nothing here.
    pub(crate) fn application(&self, declared: &Schema) -> Result<crate::DeclaredSchema, Failure> {
        if *declared == self.schema {
            return Ok(crate::DeclaredSchema::Current);
        }
        let publisher = *self.publisher.as_bytes();
        let declared = logical(declared, publisher)?;
        for step in &self.lineage {
            if matches!(
                rocky_core::classify(&logical(&step.schema, publisher)?, &declared),
                Ok(SchemaChange::Unchanged | SchemaChange::Local)
            ) {
                return Ok(crate::DeclaredSchema::Behind);
            }
        }
        Ok(crate::DeclaredSchema::Ahead)
    }
    /// Whether this issuer started a cut to `schema`: the installed schema
    /// holds nullable the fields `schema` requires.
    pub(crate) fn started_cut(&self, schema: &Schema) -> Result<bool, Failure> {
        if !self.issuer() {
            return Ok(false);
        }
        let publisher = *self.publisher.as_bytes();
        Ok(matches!(
            rocky_core::classify(self.domain()?.logical(), &logical(schema, publisher)?),
            Ok(SchemaChange::Breaking { tables, fields, .. }) if tables.is_empty() && fields.is_empty()
        ))
    }
    /// Moves to a schema that requires fields rows of the installed one
    /// lack. A member, or an issuer another issuer already moved, fetches
    /// the generation and imports a checkpoint of it: its own pending work
    /// of earlier generations is dropped. Otherwise this issuer cuts: it
    /// signs the schema with those fields nullable here, and the open
    /// migrates and calls [`Self::cut`].
    async fn evolve_breaking(
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
        // Intent first: a device that adopts the cut must import it.
        let intent = path.with_file_name("bootstrap.json");
        let held = intent.exists();
        if !held {
            rocky_credentials::write_private(&intent, b"true")?;
        }
        let fetched = match transport {
            Some(transport) => self.fetch_generation(path, vault, schema, transport).await,
            None => Err(ServiceError::NetworkRequired("upgrade").into()),
        };
        if fetched.is_err() && !held {
            std::fs::remove_file(&intent)?;
        }
        match fetched {
            Ok(()) => Ok(()),
            // A member waits for an issuer to cut; the application is ahead.
            Err(_) if !self.issuer() => Ok(()),
            Err(_) => {
                let publisher = *self.publisher.as_bytes();
                match rocky_core::classify(self.domain()?.logical(), &logical(&relaxed, publisher)?)
                {
                    Ok(SchemaChange::Additive { .. }) => {
                        self.sign_generation(path, vault, &relaxed, false)
                    }
                    // Only nullability tightens: the installed schema is the
                    // generation the migration runs in.
                    Ok(SchemaChange::Unchanged | SchemaChange::Local) => Ok(()),
                    Ok(SchemaChange::Breaking { .. }) | Err(_) => {
                        Err("the schema's required fields cannot be added in one step".into())
                    }
                }
            }
        }
    }
    /// After the migration: signs `schema`, which requires the fields the
    /// installed schema holds nullable, as a cut admitting no earlier
    /// generation.
    pub(crate) fn cut(
        &mut self,
        path: &Path,
        vault: &Vault,
        schema: &Schema,
    ) -> Result<(), Failure> {
        let publisher = *self.publisher.as_bytes();
        match rocky_core::classify(self.domain()?.logical(), &logical(schema, publisher)?)? {
            SchemaChange::Breaking { tables, fields, .. }
                if tables.is_empty() && fields.is_empty() =>
            {
                self.sign_generation(path, vault, schema, true)
            }
            _ => Err("a cut only requires fields the installed schema holds".into()),
        }
    }
    fn sign_generation(
        &mut self,
        path: &Path,
        vault: &Vault,
        schema: &Schema,
        breaking: bool,
    ) -> Result<(), Failure> {
        let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
        let before = self.domain()?.manifest();
        let mut draft = self.clone();
        draft.lineage.push(Step {
            schema: self.schema.clone(),
            manifest: self.manifest.clone(),
            transition: String::new(),
        });
        draft.schema = schema.clone();
        let domain = draft.domain()?;
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
            // Every earlier generation keeps syncing; after a cut, none do.
            cut: breaking,
            time: crate::enrollment::now()?,
        };
        let signed = SignedTransition::sign(&transition, &publisher)?;
        if let Some(step) = draft.lineage.last_mut() {
            step.transition = HEXLOWER.encode(&signed.to_bytes()?);
        }
        draft.manifest = HEXLOWER.encode(&SignedManifest::sign(&after, &publisher)?.to_bytes()?);
        for (name, _) in domain.scopes() {
            if !draft.keys.contains_key(name) {
                let key = match vault.find(&format!("generation.reader.{name}"))? {
                    Some(key) => key,
                    None => vault.create(&format!("generation.reader.{name}"))?,
                };
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
        // A cut drops this device's unsynced work: only an application
        // that declares it adopts it, at open.
        if draft.cuts_after(&self.manifest)? {
            return Err(
                "the newer generation is a cut; open the application that declares it".into(),
            );
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
    /// Whether a transition after `manifest` in this lineage is a cut.
    fn cuts_after(&self, manifest: &str) -> Result<bool, Failure> {
        let mut after = false;
        for step in &self.lineage {
            if after && step.signed()?.transition.verify(self.publisher)?.breaking() {
                return Ok(true);
            }
            if step.manifest == manifest {
                after = true;
                if step.signed()?.transition.verify(self.publisher)?.breaking() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
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
        offer
            .lineage
            .iter()
            .chain(&self.lineage)
            .map(Step::signed)
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
        if offer.lineage.len() > MAX_LINEAGE
            || !offer
                .lineage
                .iter()
                .any(|step| step.manifest == self.manifest)
        {
            return Err("the offered generations do not include this device's".into());
        }
        let mut draft = self.clone();
        draft.lineage = offer.lineage;
        draft.schema = offer.schema;
        draft.manifest = offer.manifest;
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
                .any(|(theirs, ours)| theirs.manifest != ours.manifest)
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
            .map(|index| Ok(draft.lineage[index].signed()?.transition))
            .collect::<Result<_, Failure>>()?;
        Ok(Some((draft, transitions)))
    }
    /// Checks the lineage and current generation as [`Self::adopt_offer`]
    /// describes.
    pub(crate) fn verify_lineage(&self) -> Result<(), Failure> {
        let forged = || -> Failure { "the offered generation is not the publisher's".into() };
        let mut manifests = Vec::with_capacity(self.lineage.len() + 1);
        for (index, step) in self.lineage.iter().enumerate() {
            let signed = step.signed()?;
            let manifest = signed.manifest.verify(self.publisher)?;
            let mut earlier = self.clone();
            earlier.lineage.truncate(index);
            earlier.schema = step.schema.clone();
            if earlier.domain()?.manifest().hash() != manifest.hash() {
                return Err(forged());
            }
            manifests.push((manifest.hash(), Some(signed.transition)));
        }
        let manifest = self.manifest()?.verify(self.publisher)?;
        if manifest.hash() != self.domain()?.manifest().hash() {
            return Err(forged());
        }
        manifests.push((manifest.hash(), None));
        for pair in manifests.windows(2) {
            let (from, Some(transition)) = &pair[0] else {
                return Err(forged());
            };
            let transition = transition.verify(self.publisher)?;
            if transition.from != *from || transition.to != pair[1].0 {
                return Err(forged());
            }
        }
        Ok(())
    }
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
        let Some(earlier) = other
            .iter()
            .find(|other| other.manifest == step.manifest && other.transition != step.transition)
        else {
            continue;
        };
        let current = step.signed()?.transition.verify(publisher)?;
        let candidate = earlier.signed()?.transition.verify(publisher)?;
        if candidate.same_step(&current) && candidate.time < current.time {
            step.transition = earlier.transition.clone();
            changed.push(index);
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
/// a cut migrates in before it requires them.
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
