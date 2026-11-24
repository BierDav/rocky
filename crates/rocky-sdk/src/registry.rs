use crate::{Failure, domain::REGISTRY};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use rocky_core::{Commitment, SqlValue, TypedRows};
use rocky_credentials::PartitionGrant;
use rocky_secure::{EncryptedReplica, PublicKey, SecureSql, SqlOptions};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;

pub const MAX_IDENTITIES: usize = 256;
pub const MAX_LABEL: usize = 256;
const MAX_GRANTS: usize = 256;
pub const MAX_TRUSTED: usize = 32;

#[derive(Debug)]
pub struct RegistryFull;
impl std::fmt::Display for RegistryFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "registry_full: the domain's issuer registry holds {MAX_IDENTITIES} identities"
        )
    }
}
impl std::error::Error for RegistryFull {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Created,
    Paired,
    Renewed,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admitted {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<crate::Access>,
    pub issuer: PublicKey,
    pub origin: Option<PublicKey>,
    pub enrolled: u64,
    pub invitation: Option<crate::InvitationId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    #[serde(with = "hex32")]
    pub id: Commitment,
    pub expires: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub grant: PartitionGrant,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedRecord {
    pub grant: rocky_credentials::SignedCapability,
    pub granted: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Label {
    label: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tombstone {}

#[derive(Clone, Debug, Default)]
pub struct IdentityRecord {
    pub admitted: Option<Admitted>,
    pub revoked: bool,
    pub capabilities: Vec<Capability>,
    pub grants: Vec<Grant>,
    pub label: Option<Vec<u8>>,
}
impl IdentityRecord {
    pub fn capability(&self) -> Option<Capability> {
        self.capabilities
            .iter()
            .max_by_key(|capability| (capability.expires, capability.id))
            .copied()
    }
    pub fn access(&self) -> Option<&crate::Access> {
        self.admitted
            .as_ref()
            .and_then(|admitted| admitted.access.as_ref())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub records: BTreeMap<PublicKey, IdentityRecord>,
    pub trusted: BTreeMap<PublicKey, TrustedRecord>,
}
impl Registry {
    pub fn get(&self, identity: &PublicKey) -> Option<&IdentityRecord> {
        self.records.get(identity)
    }
    pub fn identities(&self) -> usize {
        self.records
            .values()
            .filter(|record| record.admitted.is_some())
            .count()
    }
    pub fn admitted(&self, identity: &PublicKey) -> bool {
        self.get(identity)
            .is_some_and(|record| record.admitted.is_some())
    }
    pub(crate) fn full_for(&self, identity: &PublicKey) -> bool {
        !self.admitted(identity) && self.identities() >= MAX_IDENTITIES
    }
    pub fn revoked(&self, identity: &PublicKey) -> bool {
        self.get(identity).is_some_and(|record| record.revoked)
    }
    pub fn enrolled_by(&self, origin: PublicKey) -> std::collections::BTreeSet<PublicKey> {
        self.records
            .iter()
            .filter(|(_, record)| {
                record
                    .admitted
                    .as_ref()
                    .is_some_and(|admitted| admitted.origin == Some(origin))
            })
            .map(|(identity, _)| *identity)
            .filter(|identity| !self.trusted.contains_key(identity))
            .collect()
    }
    pub fn trusted_identities(
        &self,
        database: rocky_credentials::DatabaseContext,
        revocations: &rocky_credentials::RevocationSet,
        time: u64,
    ) -> Result<Vec<PublicKey>, Failure> {
        revocations.verify_in(database)?;
        let mut trusted = Vec::new();
        for (identity, record) in &self.trusted {
            let chain = rocky_credentials::CredentialChain(vec![record.grant.clone()]);
            if chain.database_context(database.publisher)? != database {
                return Err("trusted grant belongs to another database".into());
            }
            let subject = chain.trusted_identity(
                database.publisher,
                database.scope,
                &rocky_credentials::RevocationSet::new(),
                record.grant.capability.not_before,
            )?;
            if subject != *identity {
                return Err("trusted grant names another identity".into());
            }
            if record.grant.capability.not_before <= time
                && time < record.grant.capability.expires
                && !revocations.revokes_trusted(&record.grant.capability.subject)
            {
                trusted.push(*identity);
            }
        }
        Ok(trusted)
    }

    fn apply(&mut self, identity: &str, entry: &str, value: &str) -> Result<(), Failure> {
        let identity: PublicKey = identity.parse()?;
        if entry == TRUSTED {
            self.trusted.insert(identity, parse(value)?);
            return Ok(());
        }
        let record = self.records.entry(identity).or_default();
        match entry.split_once('/').unwrap_or((entry, "")) {
            (IDENTITY, "") => record.admitted = Some(parse(value)?),
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
    pub fn merged(
        rows: &mut BTreeMap<(String, String), String>,
        changes: &Changes,
    ) -> Result<Self, Failure> {
        for change in &changes.0 {
            let key = (change.identity.clone(), change.entry.clone());
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
        for ((identity, entry), value) in rows.iter() {
            registry.apply(identity, entry, value)?;
        }
        Ok(registry)
    }
}
fn parse<T: DeserializeOwned>(value: &str) -> Result<T, Failure> {
    Ok(serde_json::from_str(value)?)
}
pub(crate) mod hex32 {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Keep,
    Replace,
    Delete,
}
#[derive(Clone, Debug)]
struct Change {
    identity: String,
    entry: String,
    value: String,
    mode: Mode,
}
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
        identity: &PublicKey,
        entry: String,
        value: &impl Serialize,
        mode: Mode,
    ) -> Result<(), Failure> {
        self.0.push(Change {
            identity: identity.to_hex(),
            entry,
            value: serde_json::to_string(value)?,
            mode,
        });
        Ok(())
    }
    pub fn admitted(
        &mut self,
        identity: &PublicKey,
        admitted: &Admitted,
        replace: bool,
    ) -> Result<(), Failure> {
        let mode = if replace { Mode::Replace } else { Mode::Keep };
        self.push(identity, IDENTITY.into(), admitted, mode)
    }
    pub fn revoked(&mut self, identity: &PublicKey) -> Result<(), Failure> {
        self.push(identity, "revoked".into(), &Tombstone {}, Mode::Keep)
    }
    pub fn capability(
        &mut self,
        identity: &PublicKey,
        capability: Capability,
        prior: Option<&IdentityRecord>,
    ) -> Result<(), Failure> {
        for old in prior.into_iter().flat_map(|record| &record.capabilities) {
            if old.expires < capability.expires && old.id != capability.id {
                self.push(identity, capability_entry(old), old, Mode::Delete)?;
            }
        }
        self.push(
            identity,
            capability_entry(&capability),
            &capability,
            Mode::Keep,
        )
    }
    pub fn grant(
        &mut self,
        identity: &PublicKey,
        grant: &Grant,
        prior: Option<&IdentityRecord>,
    ) -> Result<(), Failure> {
        let known = prior.map_or(0, |record| record.grants.len());
        if known >= MAX_GRANTS && !prior.is_some_and(|record| record.grants.contains(grant)) {
            return Err("partition grant capacity of this identity reached".into());
        }
        let digest =
            rocky_core::commitment("rocky.sdk.registry-grant.v1", &serde_json::to_vec(grant)?);
        self.push(
            identity,
            format!("grant/{}", HEXLOWER.encode(&digest)),
            grant,
            Mode::Keep,
        )
    }

    pub fn trusted(&mut self, identity: &PublicKey, record: &TrustedRecord) -> Result<(), Failure> {
        self.push(identity, TRUSTED.into(), record, Mode::Replace)
    }
    pub fn label(&mut self, identity: &PublicKey, label: &[u8]) -> Result<(), Failure> {
        if label.len() > MAX_LABEL {
            return Err(format!("an identity label holds at most {MAX_LABEL} bytes").into());
        }
        let label = Label {
            label: BASE64URL_NOPAD.encode(label),
        };
        self.push(identity, "label".into(), &label, Mode::Replace)
    }
}
const IDENTITY: &str = "identity";
const TRUSTED: &str = "trusted";
fn capability_entry(capability: &Capability) -> String {
    format!("capability/{}", HEXLOWER.encode(&capability.id))
}

fn text(value: &SqlValue) -> Result<&str, Failure> {
    match value {
        SqlValue::Text(text) => Ok(text),
        _ => Err("registry rows hold text".into()),
    }
}
async fn count(sql: &SecureSql<TypedRows>, entry: &str) -> Result<usize, Failure> {
    let count = format!("SELECT COUNT(*) FROM {REGISTRY} WHERE entry = ?");
    match sql
        .run(&count, vec![SqlValue::Text(entry.into())])
        .await?
        .rows
        .first()
        .map(Vec::as_slice)
    {
        Some([SqlValue::Integer(count)]) => Ok(usize::try_from(*count)?),
        _ => Err("invalid registry count".into()),
    }
}
pub async fn read(
    replica: &EncryptedReplica<TypedRows>,
    scope: rocky_core::ResourceScope,
) -> Result<Registry, Failure> {
    let session = replica.session([scope].into()).await?;
    let sql = session.sql(SqlOptions::default()).await?;
    let rows = sql
        .run(
            &format!("SELECT identity, entry, value FROM {REGISTRY}"),
            Vec::new(),
        )
        .await?;
    let mut registry = Registry::default();
    for row in &rows.rows {
        let [identity, entry, value] = row.as_slice() else {
            return Err("invalid registry row".into());
        };
        registry.apply(text(identity)?, text(entry)?, text(value)?)?;
    }
    Ok(registry)
}
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
                SqlValue::Text(change.identity.clone()),
                SqlValue::Text(change.entry.clone()),
            ];
            let select =
                format!("SELECT id, value FROM {REGISTRY} WHERE identity = ? AND entry = ?");
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
                    if change.entry == IDENTITY && count(&sql, IDENTITY).await? >= MAX_IDENTITIES {
                        return Err(Box::new(RegistryFull) as Failure);
                    }
                    let id = sql.next_id(REGISTRY).await?;
                    let mut values = key;
                    values.extend([
                        SqlValue::Text(change.value.clone()),
                        id.sql(),
                    ]);
                    let conflict = match change.mode {
                        Mode::Replace => "DO UPDATE SET value = excluded.value",
                        _ => "DO NOTHING",
                    };
                    let insert = format!(
                        "INSERT INTO {REGISTRY}(identity, entry, value, id) VALUES(?, ?, ?, ?) ON CONFLICT(identity, entry) {conflict}"
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

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_credentials::{Authority, DatabaseContext, PrivateKey, RevocationSet};

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn foreign_or_misnamed_root_grants_cannot_make_registry_identities_trusted() {
        let root = PrivateKey::new();
        let identity = PrivateKey::new().public_key();
        let database = DatabaseContext {
            publisher: root.public_key(),
            scope: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
        };
        let record = TrustedRecord {
            grant: Authority::owner(&root)
                .issue_trusted(
                    identity,
                    database.scope,
                    100 + rocky_credentials::LIFETIME_SKEW,
                )
                .unwrap(),
            granted: 100,
        };
        let registry = Registry {
            trusted: [(identity, record.clone())].into(),
            ..Default::default()
        };
        let empty = RevocationSet::new();
        assert!(
            registry
                .trusted_identities(database, &empty, 99)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            registry.trusted_identities(database, &empty, 100).unwrap(),
            [identity]
        );
        for foreign in [
            DatabaseContext {
                publisher: PrivateKey::new().public_key(),
                ..database
            },
            DatabaseContext {
                scope: rocky_core::Scope {
                    domain: rocky_core::DomainId([9; 16]),
                    ..database.scope
                },
                ..database
            },
        ] {
            assert!(registry.trusted_identities(foreign, &empty, 100).is_err());
        }
        let misnamed = Registry {
            trusted: [(PrivateKey::new().public_key(), record)].into(),
            ..Default::default()
        };
        assert!(misnamed.trusted_identities(database, &empty, 100).is_err());
        assert_eq!(
            registry.trusted_identities(database, &empty, 100).unwrap(),
            [identity]
        );
    }
}
