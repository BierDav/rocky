//! The replicated issuer registry: one record per device in the reserved
//! collection, read and written only by issuers. Each entry is its own row,
//! so records merge by row identity. See
//! `docs/decisions/0048-replicated-issuer-registry.md`.
use crate::{Failure, domain::REGISTRY};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use rocky_core::{Commitment, SqlValue, TypedRows};
use rocky_credentials::{PartitionGrant, Profile};
use rocky_secure::{EncryptedReplica, PublicKey, SecureSql, SqlOptions};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;

/// Records with a `device` entry one domain holds.
pub const MAX_DEVICES: usize = 256;
/// Bytes of an application's opaque device label.
pub const MAX_LABEL: usize = 256;
/// Partition grants recorded per device.
const MAX_GRANTS: usize = 256;

/// The named refusal of a write that would add the 257th device.
#[derive(Debug)]
pub struct RegistryFull;
impl std::fmt::Display for RegistryFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "registry_full: the domain's issuer registry holds {MAX_DEVICES} devices"
        )
    }
}
impl std::error::Error for RegistryFull {}

/// How the device's record began.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Created,
    Paired,
    Granted,
    Delegated,
    Partition,
    Renewed,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub user: PublicKey,
    pub kind: Kind,
    /// Absent when only partition grants of the device are known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
    /// The issuer that wrote the record first.
    pub issuer: PublicKey,
    pub enrolled: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub authors: rocky_credentials::AuthorRange,
    #[serde(with = "hex32")]
    pub id: Commitment,
    pub expires: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub grant: PartitionGrant,
    pub delegations: u8,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Label {
    label: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tombstone {}

/// One device's record: its rows merged by the registry's rules.
#[derive(Clone, Debug, Default)]
pub struct Record {
    pub device: Option<Device>,
    pub revoked: bool,
    /// Every recorded capability; the latest expiry is the current one.
    pub capabilities: Vec<Capability>,
    pub grants: Vec<Grant>,
    pub label: Option<Vec<u8>>,
}
impl Record {
    /// The capability with the latest expiry.
    pub fn capability(&self) -> Option<Capability> {
        self.capabilities
            .iter()
            .max_by_key(|capability| (capability.expires, capability.id))
            .copied()
    }
    pub fn profile(&self) -> Option<&Profile> {
        self.device
            .as_ref()
            .and_then(|device| device.profile.as_ref())
    }
}

/// Every record an issuer has received, by node key.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub records: BTreeMap<PublicKey, Record>,
}
impl Registry {
    pub fn get(&self, node: &PublicKey) -> Option<&Record> {
        self.records.get(node)
    }
    /// The range already granted to this principal, if it has a recorded node.
    pub fn authors(&self, user: PublicKey) -> Option<rocky_credentials::AuthorRange> {
        self.records
            .values()
            .filter(|record| {
                record
                    .device
                    .as_ref()
                    .is_some_and(|device| device.user == user)
            })
            .filter_map(Record::capability)
            .map(|capability| capability.authors)
            .next()
    }
    /// Records holding a `device` entry: what the bound counts.
    pub fn devices(&self) -> usize {
        self.records
            .values()
            .filter(|record| record.device.is_some())
            .count()
    }
    pub fn revoked(&self, node: &PublicKey) -> bool {
        self.get(node).is_some_and(|record| record.revoked)
    }

    fn apply(&mut self, node: &str, entry: &str, value: &str) -> Result<(), Failure> {
        let node: PublicKey = node.parse()?;
        let record = self.records.entry(node).or_default();
        match entry.split_once('/').unwrap_or((entry, "")) {
            ("device", "") => record.device = Some(parse(value)?),
            ("revoked", "") => {
                parse::<Tombstone>(value)?;
                record.revoked = true;
            }
            ("label", "") => {
                let label: Label = parse(value)?;
                record.label = Some(BASE64URL_NOPAD.decode(label.label.as_bytes())?);
            }
            ("capability", _) => record.capabilities.push(parse(value)?),
            ("grant", _) => record.grants.push(parse(value)?),
            _ => return Err(format!("unknown registry entry {entry}").into()),
        }
        Ok(())
    }
}
#[cfg(test)]
impl Registry {
    /// Applies `changes` as `write` would, over rows held in memory.
    pub fn merged(
        rows: &mut BTreeMap<(String, String), String>,
        changes: &Changes,
    ) -> Result<Self, Failure> {
        for change in &changes.0 {
            let key = (change.node.clone(), change.entry.clone());
            match (rows.contains_key(&key), change.mode) {
                (true, Mode::Keep) | (false, Mode::Delete) => {}
                (_, Mode::Delete) => {
                    rows.remove(&key);
                }
                _ => {
                    rows.insert(key, change.value.clone());
                }
            }
        }
        let mut registry = Self::default();
        for ((node, entry), value) in rows.iter() {
            registry.apply(node, entry, value)?;
        }
        Ok(registry)
    }
}
fn parse<T: DeserializeOwned>(value: &str) -> Result<T, Failure> {
    Ok(serde_json::from_str(value)?)
}
mod hex32 {
    use data_encoding::HEXLOWER;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(value: &[u8; 32], to: S) -> Result<S::Ok, S::Error> {
        to.serialize_str(&HEXLOWER.encode(value))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(from)?;
        HEXLOWER
            .decode(text.as_bytes())
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| D::Error::custom("expected 32 hexadecimal bytes"))
    }
}

/// How a change meets a row that exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Insert-only entries keep the row they find.
    Keep,
    Replace,
    Delete,
}
#[derive(Clone, Debug)]
struct Change {
    node: String,
    entry: String,
    value: String,
    mode: Mode,
}
/// The rows one answer commits, in one transaction.
#[derive(Clone, Debug, Default)]
pub struct Changes(Vec<Change>);
impl Changes {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn extend(&mut self, other: Changes) {
        self.0.extend(other.0);
    }
    fn push(
        &mut self,
        node: &PublicKey,
        entry: String,
        value: &impl Serialize,
        mode: Mode,
    ) -> Result<(), Failure> {
        self.0.push(Change {
            node: node.to_hex(),
            entry,
            value: serde_json::to_string(value)?,
            mode,
        });
        Ok(())
    }
    /// The device entry, kept when one exists unless `replace`.
    pub fn device(
        &mut self,
        node: &PublicKey,
        device: &Device,
        replace: bool,
    ) -> Result<(), Failure> {
        let mode = if replace { Mode::Replace } else { Mode::Keep };
        self.push(node, "device".into(), device, mode)
    }
    pub fn revoked(&mut self, node: &PublicKey) -> Result<(), Failure> {
        self.push(node, "revoked".into(), &Tombstone {}, Mode::Keep)
    }
    /// A new capability; those of `prior` with an earlier expiry go.
    pub fn capability(
        &mut self,
        node: &PublicKey,
        capability: Capability,
        prior: Option<&Record>,
    ) -> Result<(), Failure> {
        for old in prior.into_iter().flat_map(|record| &record.capabilities) {
            if old.expires < capability.expires && old.id != capability.id {
                self.push(node, capability_entry(old), old, Mode::Delete)?;
            }
        }
        self.push(node, capability_entry(&capability), &capability, Mode::Keep)
    }
    pub fn grant(
        &mut self,
        node: &PublicKey,
        grant: &Grant,
        prior: Option<&Record>,
    ) -> Result<(), Failure> {
        let known = prior.map_or(0, |record| record.grants.len());
        if known >= MAX_GRANTS && !prior.is_some_and(|record| record.grants.contains(grant)) {
            return Err("partition grant capacity of this device reached".into());
        }
        let digest =
            rocky_core::commitment("rocky.sdk.registry-grant.v1", &serde_json::to_vec(grant)?);
        self.push(
            node,
            format!("grant/{}", HEXLOWER.encode(&digest)),
            grant,
            Mode::Keep,
        )
    }

    pub fn label(&mut self, node: &PublicKey, label: &[u8]) -> Result<(), Failure> {
        if label.len() > MAX_LABEL {
            return Err(format!("a device label holds at most {MAX_LABEL} bytes").into());
        }
        let label = Label {
            label: BASE64URL_NOPAD.encode(label),
        };
        self.push(node, "label".into(), &label, Mode::Replace)
    }
}
fn capability_entry(capability: &Capability) -> String {
    format!("capability/{}", HEXLOWER.encode(&capability.id))
}

fn text(value: &SqlValue) -> Result<&str, Failure> {
    match value {
        SqlValue::Text(text) => Ok(text),
        _ => Err("registry rows hold text".into()),
    }
}
/// Records with a `device` entry, inside the writing transaction.
async fn devices(sql: &SecureSql<TypedRows>) -> Result<usize, Failure> {
    let count = format!("SELECT COUNT(*) FROM {REGISTRY} WHERE entry = 'device'");
    match sql
        .run(&count, Vec::new())
        .await?
        .rows
        .first()
        .map(Vec::as_slice)
    {
        Some([SqlValue::Integer(count)]) => Ok(usize::try_from(*count)?),
        _ => Err("invalid registry count".into()),
    }
}
/// Every record this issuer holds.
pub async fn read(
    replica: &EncryptedReplica<TypedRows>,
    scope: rocky_core::ResourceScope,
) -> Result<Registry, Failure> {
    let session = replica.session([scope].into()).await?;
    let sql = session.sql(SqlOptions::default()).await?;
    let rows = sql
        .run(
            &format!("SELECT node, entry, value FROM {REGISTRY}"),
            Vec::new(),
        )
        .await?;
    let mut registry = Registry::default();
    for row in &rows.rows {
        let [node, entry, value] = row.as_slice() else {
            return Err("invalid registry row".into());
        };
        registry.apply(text(node)?, text(entry)?, text(value)?)?;
    }
    Ok(registry)
}
/// Commits `changes` as one secure transaction, or nothing. A change that
/// would add a device beyond the bound refuses the whole transaction.
pub async fn write(
    replica: &EncryptedReplica<TypedRows>,
    scope: rocky_core::ResourceScope,
    changes: &Changes,
) -> Result<(), Failure> {
    if changes.is_empty() {
        return Ok(());
    }
    let session = replica.session([scope].into()).await?;
    let sql = session.sql(SqlOptions::default()).await?;
    sql.run("BEGIN IMMEDIATE", Vec::new()).await?;
    let result = async {
        for change in &changes.0 {
            let key = vec![
                SqlValue::Text(change.node.clone()),
                SqlValue::Text(change.entry.clone()),
            ];
            let select = format!("SELECT id, value FROM {REGISTRY} WHERE node = ? AND entry = ?");
            let found = sql.run(&select, key.clone()).await?;
            match (found.rows.first(), change.mode) {
                (None, Mode::Delete) | (Some(_), Mode::Keep) => {}
                (Some(row), Mode::Delete) => {
                    sql.run(
                        &format!("DELETE FROM {REGISTRY} WHERE id = ?"),
                        vec![row[0].clone()],
                    )
                    .await?;
                }
                (Some(row), Mode::Replace) => {
                    if text(&row[1])? != change.value {
                        sql.run(
                            &format!("UPDATE {REGISTRY} SET value = ? WHERE id = ?"),
                            vec![SqlValue::Text(change.value.clone()), row[0].clone()],
                        )
                        .await?;
                    }
                }
                (None, Mode::Keep | Mode::Replace) => {
                    if change.entry == "device" && devices(&sql).await? >= MAX_DEVICES {
                        return Err(Box::new(RegistryFull) as Failure);
                    }
                    let id = sql.next_id(REGISTRY).await?;
                    let mut values = key;
                    values.extend([
                        SqlValue::Text(change.value.clone()),
                        SqlValue::Integer(id.integer()),
                    ]);
                    let insert = format!(
                        "INSERT INTO {REGISTRY}(node, entry, value, id) VALUES(?, ?, ?, ?)"
                    );
                    sql.run(&insert, values).await?;
                }
            }
        }
        if changes.0.iter().any(|change| change.entry == "revoked") {
            crate::enrollment::fault::check(crate::enrollment::fault::Stage::Tombstone)?;
        }
        sql.run("COMMIT", Vec::new()).await?;
        Ok::<_, Failure>(())
    }
    .await;
    if result.is_err() {
        let _ = sql.run("ROLLBACK", Vec::new()).await;
    }
    result
}
