//! Root escrow of the data keys an installation holds: every collection
//! key epoch and group secret epoch (with its partition's naming secret)
//! is sealed to the domain's offline root and kept in the replica beside
//! the frames of that epoch, which carry it to every holder of the scope,
//! keyless custody too. An issuer's partition naming root stays local.
//! The root alone then recovers the data of any node's backup, with every
//! trusted node and device lost (`Configuration::recover`). Records are
//! opaque: see `rocky_credentials::RootEscrow`.
use crate::{
    Failure,
    domain::Domain,
    enrollment::{Settings, key_role},
    groups::{GroupPartition, GroupRecord, NAMING_ROLE, record_key, secret_role},
    secrets::Vault,
};
use rocky_core::{Commitment, KeyEpoch, PartitionName, PartitionValue, ResourceScope};
use rocky_credentials::{RootEscrow, RootKey, escrow_tag};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The key epoch a record travels with, or none for the naming root.
pub(crate) type Slot = Option<(ResourceScope, KeyEpoch)>;
/// A store's records and the key epochs of the ciphertext it holds.
pub(crate) type Archived = (Vec<(Slot, RootEscrow)>, BTreeSet<(ResourceScope, KeyEpoch)>);

/// What one record seals.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Escrowed {
    /// A collection's key epoch.
    Reader {
        collection: String,
        epoch: KeyEpoch,
        key: [u8; 32],
    },
    /// A group partition's secret epoch and the partition it keys.
    Group {
        group: String,
        values: Vec<PartitionValue>,
        address: Commitment,
        name: PartitionName,
        epoch: KeyEpoch,
        secret: [u8; 32],
    },
    /// An issuer's partition naming root.
    Naming { root: [u8; 32] },
}
impl Escrowed {
    fn secret(&self) -> &[u8; 32] {
        match self {
            Self::Reader { key, .. } => key,
            Self::Group { secret, .. } => secret,
            Self::Naming { root } => root,
        }
    }
}

/// Seals every key `settings` holds that `held`, the replica's records by
/// slot and tag, lacks: each collection key epoch under its scope, each
/// group secret epoch under every member table's scope, and the naming
/// root of an issuer.
fn seal(
    settings: &Settings,
    vault: &Vault,
    domain: &Domain,
    held: &BTreeSet<(Slot, Commitment)>,
) -> Result<Vec<(Slot, RootEscrow)>, Failure> {
    let mut items = Vec::new();
    for (name, scope) in domain.scopes() {
        for epoch in settings.keys.get(name).into_iter().flatten() {
            let key = vault.get(&key_role(name, *epoch))?;
            let item = Escrowed::Reader {
                collection: name.to_owned(),
                epoch: *epoch,
                key,
            };
            items.push((Some((scope, *epoch)), item));
        }
    }
    for partition in settings.partitions(domain)? {
        let record = &partition.record;
        for epoch in &record.epochs {
            let secret = vault.get(&secret_role(&record.address, *epoch))?;
            for member in &partition.members {
                let item = Escrowed::Group {
                    group: record.group.clone(),
                    values: record.values.clone(),
                    address: record.address,
                    name: record.name,
                    epoch: *epoch,
                    secret,
                };
                items.push((Some((member.scope, *epoch)), item));
            }
        }
    }
    if let Some(root) = vault.find(NAMING_ROLE)? {
        items.push((None, Escrowed::Naming { root }));
    }
    let mut sealed = Vec::new();
    for (slot, item) in items {
        let tag = escrow_tag(settings.publisher, item.secret());
        if !held.contains(&(slot, tag)) {
            let record = RootEscrow::seal(settings.publisher, tag, &serde_json::to_vec(&item)?)?;
            sealed.push((slot, record));
        }
    }
    Ok(sealed)
}

/// Seals into `replica` what it lacks of the keys `settings` holds. The
/// records derive from the vault, so the next call completes one an
/// interruption cut short.
pub(crate) async fn keep(
    replica: &rocky_secure::EncryptedReplica<rocky_core::TypedRows>,
    settings: &Settings,
    vault: &Vault,
    domain: &Domain,
) -> Result<(), Failure> {
    let held = replica.escrowed().await?;
    replica
        .keep_escrow(seal(settings, vault, domain, &held)?)
        .await?;
    Ok(())
}

/// Keys a root opened from a store's escrow records. Recovery reads native
/// archives only.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
#[derive(Default)]
pub(crate) struct Recovered {
    pub readers: BTreeMap<(String, KeyEpoch), [u8; 32]>,
    /// Group partitions by record key, with the epochs recovered.
    pub groups: BTreeMap<String, GroupRecord>,
    /// Group secrets by group address and epoch.
    pub secrets: BTreeMap<(Commitment, KeyEpoch), [u8; 32]>,
    pub naming: Option<[u8; 32]>,
}
#[cfg_attr(target_family = "wasm", allow(dead_code))]
impl Recovered {
    /// Opens the `records` of a store of `archived`'s domain with `root`.
    /// Anyone can seal to the root, so a record that opens to no key of
    /// its slot is skipped; every key epoch in `held`, those the store
    /// holds ciphertext of, must be opened, or the recovery fails naming it.
    pub fn open(
        archived: &Settings,
        (records, held): Archived,
        root: &RootKey,
    ) -> Result<Self, Failure> {
        let publisher = root.public_key();
        if publisher != archived.publisher {
            return Err("the root key is not this domain's root".into());
        }
        let domain = archived.domain()?;
        let mut recovered = Self::default();
        let mut covered = BTreeSet::new();
        for (slot, record) in records {
            let Some(item) = record
                .open(root)
                .ok()
                .and_then(|payload| serde_json::from_slice::<Escrowed>(&payload).ok())
                .filter(|item| record.tag() == escrow_tag(publisher, item.secret()))
            else {
                continue;
            };
            match (slot, item) {
                (None, Escrowed::Naming { root }) => recovered.naming = Some(root),
                (
                    Some((scope, epoch)),
                    Escrowed::Reader {
                        collection,
                        epoch: sealed,
                        key,
                    },
                ) => {
                    if sealed == epoch
                        && KeyEpoch::new(epoch.generation, &key) == epoch
                        && domain
                            .scopes()
                            .any(|(name, held)| name == collection && held == scope)
                    {
                        recovered.readers.insert((collection, epoch), key);
                        covered.insert((scope, epoch));
                    }
                }
                (Some((scope, epoch)), item) => {
                    covered.extend(recovered.group(&domain, scope, epoch, item)?);
                }
                (None, _) => {}
            }
        }
        if let Some((scope, epoch)) = held.iter().find(|slot| !covered.contains(*slot)) {
            return Err(format!(
                "the backup holds data of key epoch {} of scope {} that no root escrow record \
                 opens",
                epoch.generation,
                rocky_core::scope_label(scope)
            )
            .into());
        }
        Ok(recovered)
    }
    /// Takes a group secret sealed for `scope` at `epoch` if it is that
    /// epoch's and the partition it names has `scope` as a member; returns
    /// the epoch's slots of every member.
    fn group(
        &mut self,
        domain: &Domain,
        scope: ResourceScope,
        epoch: KeyEpoch,
        item: Escrowed,
    ) -> Result<Vec<(ResourceScope, KeyEpoch)>, Failure> {
        let Escrowed::Group {
            group,
            values,
            address,
            name,
            epoch: sealed,
            secret,
        } = item
        else {
            return Ok(Vec::new());
        };
        if sealed != epoch || KeyEpoch::new(epoch.generation, &secret) != epoch {
            return Ok(Vec::new());
        }
        let record = GroupRecord {
            group,
            values,
            address,
            name,
            epochs: Vec::new(),
        };
        let Ok(partition) = GroupPartition::of(domain, &record) else {
            return Ok(Vec::new());
        };
        if !partition.members.iter().any(|member| member.scope == scope) {
            return Ok(Vec::new());
        }
        self.groups
            .entry(record_key(&address))
            .or_insert(record)
            .hold(epoch)?;
        self.secrets.insert((address, epoch), secret);
        Ok(partition
            .members
            .iter()
            .map(|member| (member.scope, epoch))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn settings(root: &RootKey) -> Settings {
        let schema: crate::schema::Schema = serde_json::from_str(
            r#"{"application":"escrow","version":1,"format":6,"tables":[{"name":"notes","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]}]}"#,
        )
        .unwrap();
        Settings::installed(
            ("escrow".into(), true),
            (
                root.public_key(),
                RootKey::generate().public_key(),
                Arc::new(RootKey::generate()),
            ),
            (
                rocky_core::MeshId([1; 16]),
                rocky_core::DomainId([2; 16]),
                schema,
            ),
            String::new(),
            60,
        )
    }
    fn record(root: &RootKey, item: &Escrowed) -> RootEscrow {
        let tag = escrow_tag(root.public_key(), item.secret());
        RootEscrow::seal(root.public_key(), tag, &serde_json::to_vec(item).unwrap()).unwrap()
    }
    fn reader(generation: u64, key: [u8; 32]) -> Escrowed {
        Escrowed::Reader {
            collection: "notes".into(),
            epoch: KeyEpoch::new(generation, &key),
            key,
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn recovery_opens_the_records_of_held_epochs_skips_forgeries_and_names_a_gap()
    -> Result<(), Failure> {
        let root = RootKey::generate();
        let archived = settings(&root);
        let scope = archived
            .domain()?
            .scopes()
            .find(|(name, _)| *name == "notes")
            .map(|(_, scope)| scope)
            .ok_or("scope")?;
        let (one, two) = (KeyEpoch::new(1, &[3; 32]), KeyEpoch::new(2, &[4; 32]));
        let held = BTreeSet::from([(scope, one), (scope, two)]);
        let records = vec![
            (Some((scope, one)), record(&root, &reader(1, [3; 32]))),
            // Sealed to another root, or under the wrong epoch: skipped.
            (
                Some((scope, two)),
                record(&RootKey::generate(), &reader(2, [4; 32])),
            ),
            (Some((scope, two)), record(&root, &reader(1, [9; 32]))),
            (Some((scope, two)), record(&root, &reader(2, [4; 32]))),
            (None, record(&root, &Escrowed::Naming { root: [5; 32] })),
        ];
        let recovered = Recovered::open(&archived, (records.clone(), held.clone()), &root)?;
        assert_eq!(recovered.readers.len(), 2);
        assert_eq!(recovered.readers[&("notes".into(), two)], [4; 32]);
        assert_eq!(recovered.naming, Some([5; 32]));
        // An epoch the store holds ciphertext of without a record fails.
        let mut gap = held;
        gap.insert((scope, KeyEpoch::new(3, &[6; 32])));
        let error = Recovered::open(&archived, (records.clone(), gap), &root)
            .err()
            .ok_or("recovered a gap")?
            .to_string();
        assert!(error.contains("key epoch 3"), "{error}");
        // Another root opens nothing.
        assert!(
            Recovered::open(&archived, (records, BTreeSet::new()), &RootKey::generate()).is_err()
        );
        Ok(())
    }
}
