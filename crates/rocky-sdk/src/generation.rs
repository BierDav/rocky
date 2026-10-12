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

/// The schema generation of a lineage ending at `schema`, and the nullable
/// fields its transitions added to collections that already existed.
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
        if let SchemaChange::Additive { fields, .. } = change {
            generation += 1;
            added.extend(fields);
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
            self.sign_generation(path, vault, schema)
        } else {
            let transport = transport.ok_or(ServiceError::NetworkRequired("upgrade"))?;
            self.fetch_generation(path, vault, schema, transport).await
        }
    }
    fn sign_generation(
        &mut self,
        path: &Path,
        vault: &Vault,
        schema: &Schema,
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
            // Devices one generation behind keep syncing until they upgrade.
            horizon: before.schema,
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
        for issuer in self.issuers.clone() {
            let addresses = self
                .peers
                .get(&issuer)
                .map(|contact| contact.addresses(transport.kind()))
                .unwrap_or_default();
            if addresses.is_empty() {
                continue;
            }
            let attempt = async {
                let reply = exchange(transport, &addresses, Protocol::Renew, OFFER).await?;
                let offer: Offer = serde_json::from_slice(&reply)?;
                let mut draft = self.adopt_offer(offer, schema)?;
                let renewed = draft
                    .renew_into_generation(vault, transport, issuer, &addresses, self)
                    .await?;
                draft.adopt(path, vault, &renewed)?;
                Ok::<_, Failure>(draft)
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
    /// The identity at an issuer's generation, checked: it is the
    /// application's schema, signed by the publisher, and its lineage
    /// passes through this device's generation.
    fn adopt_offer(&self, offer: Offer, schema: &Schema) -> Result<Self, Failure> {
        if offer.schema != *schema {
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
            return Err("the issuer's generations do not include this device's".into());
        }
        let mut draft = self.clone();
        draft.lineage = offer.lineage;
        draft.schema = offer.schema;
        draft.manifest = offer.manifest;
        let manifest = draft.manifest()?.verify(self.publisher)?;
        if manifest.hash() != draft.domain()?.manifest().hash() {
            return Err("the issuer's manifest differs from this application schema".into());
        }
        draft.learn_revocations(&offer.revocations)?;
        for step in &draft.lineage {
            step.signed()?.transition.verify(self.publisher)?;
        }
        Ok(draft)
    }
}
