//! Root escrow of the data keys an installation holds: every collection
//! key epoch, group secret epoch (with its partition's naming secret) and
//! the partition naming root is sealed to the domain's offline root as it
//! is stored, in the identity record that backups carry. The root alone
//! then recovers a backup's data, with every trusted node and device lost
//! (`Configuration::recover`). Records are opaque: see
//! `rocky_credentials::RootEscrow`.
use crate::{Failure, enrollment::Settings, groups::GroupRecord};
use data_encoding::HEXLOWER;
use rocky_core::{Commitment, KeyEpoch, PartitionName, PartitionValue};
use rocky_credentials::{PublicKey, RootEscrow, RootKey, escrow_tag};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    pub fn group(record: &GroupRecord, epoch: KeyEpoch, secret: [u8; 32]) -> Self {
        Self::Group {
            group: record.group.clone(),
            values: record.values.clone(),
            address: record.address,
            name: record.name,
            epoch,
            secret,
        }
    }
    fn secret(&self) -> &[u8; 32] {
        match self {
            Self::Reader { key, .. } => key,
            Self::Group { secret, .. } => secret,
            Self::Naming { root } => root,
        }
    }
}
impl Settings {
    /// Seals `item` to the root unless a record of its key is held.
    pub(crate) fn escrow(&mut self, item: &Escrowed) -> Result<(), Failure> {
        let tag = escrow_tag(self.publisher, item.secret());
        let id = HEXLOWER.encode(&tag);
        if !self.escrow.contains_key(&id) {
            let record = RootEscrow::seal(self.publisher, tag, &serde_json::to_vec(item)?)?;
            self.escrow.insert(id, HEXLOWER.encode(&record.to_bytes()?));
        }
        Ok(())
    }
}

/// Keys a root opened from an identity record's escrow. Recovery reads
/// native archives only.
#[cfg_attr(target_family = "wasm", allow(dead_code))]
#[derive(Default)]
pub(crate) struct Recovered {
    pub readers: BTreeMap<(String, KeyEpoch), [u8; 32]>,
    /// Group secrets by group address and epoch, with the naming secret.
    pub groups: BTreeMap<(Commitment, KeyEpoch), (PartitionName, [u8; 32])>,
    pub naming: Option<[u8; 32]>,
}
#[cfg_attr(target_family = "wasm", allow(dead_code))]
impl Recovered {
    /// Opens every record of `archived` with `root`, and checks they hold
    /// exactly the keys the record lists: a missing, extra or inconsistent
    /// record fails the recovery and is named.
    pub fn open(archived: &Settings, root: &RootKey) -> Result<Self, Failure> {
        let publisher: PublicKey = root.public_key();
        if publisher != archived.publisher {
            return Err("the root key is not this domain's root".into());
        }
        let mut recovered = Self::default();
        for (id, bytes) in &archived.escrow {
            let record = RootEscrow::from_bytes(&HEXLOWER.decode(bytes.as_bytes())?)?;
            let item: Escrowed = serde_json::from_slice(&record.open(root)?)?;
            let tag = escrow_tag(publisher, item.secret());
            if record.tag() != tag || HEXLOWER.encode(&tag) != *id {
                return Err("an escrow record's tag does not match its key".into());
            }
            match item {
                Escrowed::Reader {
                    collection,
                    epoch,
                    key,
                } => {
                    if KeyEpoch::new(epoch.generation, &key) != epoch {
                        return Err("an escrowed key does not match its epoch".into());
                    }
                    recovered.readers.insert((collection, epoch), key);
                }
                Escrowed::Group {
                    group,
                    values,
                    address,
                    name,
                    epoch,
                    secret,
                } => {
                    let held = archived
                        .groups
                        .get(&crate::groups::record_key(&address))
                        .filter(|record| {
                            record.group == group && record.values == values && record.name == name
                        });
                    if held.is_none() || KeyEpoch::new(epoch.generation, &secret) != epoch {
                        return Err(
                            "an escrowed group secret does not match the partition it names".into(),
                        );
                    }
                    recovered.groups.insert((address, epoch), (name, secret));
                }
                Escrowed::Naming { root } => recovered.naming = Some(root),
            }
        }
        let mut listed = 0;
        for (collection, epochs) in &archived.keys {
            for epoch in epochs {
                if !recovered
                    .readers
                    .contains_key(&(collection.clone(), *epoch))
                {
                    return Err(format!(
                        "key epoch {} of {collection} has no root escrow record",
                        epoch.generation
                    )
                    .into());
                }
                listed += 1;
            }
        }
        for record in archived.groups.values() {
            for epoch in &record.epochs {
                if !recovered.groups.contains_key(&(record.address, *epoch)) {
                    return Err(format!(
                        "secret epoch {} of a {} partition has no root escrow record",
                        epoch.generation, record.group
                    )
                    .into());
                }
                listed += 1;
            }
        }
        if listed != recovered.readers.len() + recovered.groups.len() {
            return Err("an escrow record holds a key the archived device does not list".into());
        }
        Ok(recovered)
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
    fn hold(settings: &mut Settings, generation: u64, key: [u8; 32]) -> Result<(), Failure> {
        let epoch = KeyEpoch::new(generation, &key);
        settings.keys.entry("notes".into()).or_default().push(epoch);
        settings.escrow(&Escrowed::Reader {
            collection: "notes".into(),
            epoch,
            key,
        })
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn recovery_opens_exactly_the_listed_keys_and_names_a_missing_one() -> Result<(), Failure> {
        let root = RootKey::generate();
        let mut listed = settings(&root);
        hold(&mut listed, 1, [3; 32])?;
        hold(&mut listed, 2, [4; 32])?;
        // Escrowing a held key again adds nothing.
        let before = listed.escrow.clone();
        listed.escrow(&Escrowed::Reader {
            collection: "notes".into(),
            epoch: KeyEpoch::new(2, &[4; 32]),
            key: [4; 32],
        })?;
        assert_eq!(listed.escrow, before);
        // A record of a key the archive does not list fails.
        let mut extra = listed.clone();
        extra.keys.get_mut("notes").unwrap().pop();
        let error = Recovered::open(&extra, &root).err().unwrap().to_string();
        assert!(error.contains("does not list"), "{error}");
        let recovered = Recovered::open(&listed, &root)?;
        assert_eq!(recovered.readers.len(), 2);
        assert!(recovered.readers.values().any(|key| *key == [4; 32]));
        // A key without a record fails, naming its collection.
        let missing = KeyEpoch::new(3, &[5; 32]);
        listed.keys.get_mut("notes").unwrap().push(missing);
        let error = Recovered::open(&listed, &root).err().unwrap().to_string();
        assert!(error.contains("key epoch 3 of notes"), "{error}");
        // Another root opens nothing.
        assert!(Recovered::open(&listed, &RootKey::generate()).is_err());
        Ok(())
    }
}
