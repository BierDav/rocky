use crate::{
    Access, Failure, Route,
    access::HeldPartition,
    domain::Domain,
    groups::{GroupPartition, GroupRecord, Member, secret_role},
    membership::Membership,
    peer::{PeerReply, PeerRequest, ReplyFrame},
    registry::{self, Changes, Registry, RegistryFull},
    schema::Schema,
    secrets::{Vault, write_private},
    service::ServiceError,
    transport::Transport,
};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use rocky_core::{Commitment, DomainId, MeshId, ResourceGrant, Right, RowSelection, commitment};
use rocky_credentials::{
    Authority, EpochRequest, IdentityKey, IdentityProvider, MAX_LIVE_REVOCATIONS,
    REVOCATION_SET_FULL, RenewRequest, Renewal, Revocation,
};
use rocky_secure::{
    CredentialChain, KeyMaterial, PrivateKey, PublicKey, RevocationSet, SecureConfig,
    SignedCapability, SignedManifest, SignedRevocation,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

mod rotation_epoch;
pub(crate) use rotation_epoch::RotationEpoch;

pub const VERSION: u32 = 48;
pub(crate) const REPLICA_FILE: &str = "rocky.sqlite";
pub(crate) const AUTHORITY_IDENTITY_KEY: &str = "identity";
pub(crate) const AUTHORITY_REVOKED_KEY: &str = "revoked";
pub(crate) const AUTHORITY_BOOTSTRAP_KEY: &str = "bootstrap";
const CONTACT_VERSION: u32 = 5;
pub const MAX_CONTACTS: usize = 64;
pub const MAX_DIALED: usize = 16;
const MAX_ISSUERS: usize = crate::registry::MAX_ADMINS;
const MAX_ROUTES: usize = 4;
pub(crate) const DEFAULT_TTL: u64 = 180 * 24 * 60 * 60;
pub(crate) const MAX_EPOCHS: usize = 256;
pub(crate) const SKEW: u64 = 300;
type GroupDelivery = (GroupRecord, Vec<Vec<u8>>);
pub(crate) fn naming_for(vault: &Vault, issuer: bool) -> Result<Option<String>, Failure> {
    if !issuer {
        return Ok(None);
    }
    Ok(Some(
        HEXLOWER.encode(
            &vault
                .find(crate::groups::NAMING_ROLE)?
                .ok_or("this issuer holds no partition naming root")?,
        ),
    ))
}
pub(crate) fn hold_naming(vault: &Vault, naming: Option<&str>) -> Result<(), Failure> {
    let root = hex32(naming.ok_or("an issuer enrollment carries the partition naming root")?)?;
    if vault
        .find(crate::groups::NAMING_ROLE)?
        .is_some_and(|held| held != root)
    {
        return Err("a different partition naming root is held".into());
    }
    vault.put(crate::groups::NAMING_ROLE, &root)?;
    Ok(())
}
#[derive(Debug)]
pub(crate) struct Revoked(pub SignedRevocation);
impl std::fmt::Display for Revoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("this device's person was removed or the device was unlinked")
    }
}
impl std::error::Error for Revoked {}

#[derive(Debug)]
pub struct PublisherRequired(pub &'static str);
impl std::fmt::Display for PublisherRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} needs the owner's root key", self.0)
    }
}
impl std::error::Error for PublisherRequired {}

pub(crate) fn root_signer<'a>(
    root: Option<&'a dyn IdentityProvider>,
    publisher: PublicKey,
    action: &'static str,
) -> Result<&'a dyn IdentityProvider, Failure> {
    match root {
        Some(root) if root.public_key()? == publisher => Ok(root),
        _ => Err(Box::new(PublisherRequired(action))),
    }
}

#[derive(Debug)]
pub(crate) struct RevocationSetFull;
impl std::fmt::Display for RevocationSetFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REVOCATION_SET_FULL)
    }
}
impl std::error::Error for RevocationSetFull {}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Notice {
    version: u32,
    statement: SignedRevocation,
}
const NOTICE_VERSION: u32 = 3;

enum Recorded {
    Absent,
    Revoked(Box<SignedRevocation>),
}
impl Recorded {
    fn parse(settings: &Settings, bytes: &[u8]) -> Result<Self, Failure> {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        if value["version"].as_u64() != Some(u64::from(NOTICE_VERSION)) {
            return Err("unsupported device revocation notice".into());
        }
        let notice: Notice = serde_json::from_value(value)?;
        let set = RevocationSet::try_from(vec![notice.statement.clone()])?;
        if settings.revoking(&set)?.is_none() {
            return Err("invalid device revocation notice".into());
        }
        Ok(Self::Revoked(Box::new(notice.statement)))
    }
    fn encode(settings: &Settings, statement: SignedRevocation) -> Result<Vec<u8>, Failure> {
        let set = RevocationSet::try_from(vec![statement.clone()])?;
        if settings.revoking(&set)?.is_none() {
            return Err("the statement does not revoke this device".into());
        }
        Ok(serde_json::to_vec(&Notice {
            version: NOTICE_VERSION,
            statement,
        })?)
    }
    async fn read(
        settings: &Settings,
        replica: &rocky_secure::EncryptedReplica<rocky_core::TypedRows>,
    ) -> Result<Self, Failure> {
        let Some((bytes, _)) = replica.get_authority_state(AUTHORITY_REVOKED_KEY).await? else {
            return Ok(Self::Absent);
        };
        Self::parse(settings, &bytes)
    }
    fn statement(&self) -> Option<&SignedRevocation> {
        match self {
            Self::Absent => None,
            Self::Revoked(statement) => Some(statement),
        }
    }
}

pub(crate) struct RevocationNotice {
    replica: rocky_secure::EncryptedReplica<rocky_core::TypedRows>,
    recorded: tokio::sync::Mutex<Option<Recorded>>,
}
impl RevocationNotice {
    pub(crate) fn open(replica: rocky_secure::EncryptedReplica<rocky_core::TypedRows>) -> Self {
        Self {
            replica,
            recorded: tokio::sync::Mutex::new(None),
        }
    }
    pub(crate) fn encode(
        settings: &Settings,
        statement: SignedRevocation,
    ) -> Result<Vec<u8>, Failure> {
        Recorded::encode(settings, statement)
    }
    pub(crate) async fn refresh(&self, settings: &Settings) {
        let mut recorded = self.recorded.lock().await;
        *recorded = Recorded::read(settings, &self.replica).await.ok();
    }
    async fn known(
        &self,
        recorded: &mut Option<Recorded>,
        settings: &Settings,
    ) -> Result<Option<SignedRevocation>, Failure> {
        let known = match recorded {
            Some(known) => known,
            None => recorded.insert(Recorded::read(settings, &self.replica).await?),
        };
        Ok(known.statement().cloned())
    }
    async fn write(
        &self,
        recorded: &mut Option<Recorded>,
        settings: &Settings,
        statement: SignedRevocation,
    ) -> Result<(), Failure> {
        let bytes = Recorded::encode(settings, statement)?;
        let written = self
            .replica
            .put_authority_state(AUTHORITY_REVOKED_KEY, &bytes, now()?)
            .await
            .map_err(Failure::from);
        *recorded = Recorded::read(settings, &self.replica).await.ok();
        written
    }
    pub(crate) async fn revoked(
        &self,
        settings: &Settings,
    ) -> Result<Option<SignedRevocation>, Failure> {
        let mut recorded = self.recorded.lock().await;
        self.known(&mut recorded, settings).await
    }
    pub(crate) async fn record_unless_revoked(
        &self,
        settings: &Settings,
        statement: SignedRevocation,
    ) -> Result<bool, Failure> {
        let mut recorded = self.recorded.lock().await;
        if self.known(&mut recorded, settings).await?.is_some() {
            return Ok(false);
        }
        self.write(&mut recorded, settings, statement).await?;
        self.replica.forget_everything().await?;
        Ok(true)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(skip)]
    pub(crate) delivery_nodes: BTreeMap<rocky_credentials::PeerId, PublicKey>,
    pub(crate) contact_refs: BTreeMap<rocky_credentials::PeerId, StoredContact>,
    pub(crate) removed_peers: Vec<rocky_credentials::PeerId>,
    #[serde(skip)]
    pub(crate) resolved_refs: BTreeSet<rocky_credentials::PeerId>,
    pub version: u32,
    pub installation: String,
    pub schema: Schema,
    pub mesh: MeshId,
    pub domain: DomainId,
    pub publisher: PublicKey,
    pub identity: PublicKey,
    pub genesis: String,
    pub generation: u32,
    pub engine: rocky_core::EngineIdentity,
    pub pending: Option<crate::tip_schemas::Pending>,
    pub(crate) bootstrap: Option<crate::tip_schemas::Bootstrap>,
    pub revocations: RevocationSet,
    pub credentials: CredentialChain,
    pub ttl: u64,
    pub access: Access,
    pub keys: BTreeMap<String, Vec<rocky_core::KeyEpoch>>,
    pub groups: BTreeMap<String, GroupRecord>,
    pub epoch_statements: BTreeMap<String, Vec<Commitment>>,
    pub authority: Option<Issuing>,
    pub device_revocations:
        BTreeMap<rocky_credentials::PeerId, crate::membership::DeviceRevocationReceipt>,
    #[serde(skip)]
    pub issuers: Vec<PublicKey>,
    #[serde(skip)]
    pub peers: BTreeMap<PublicKey, Contact>,
    #[serde(skip)]
    pub contact_meta: BTreeMap<PublicKey, crate::mesh::ContactMeta>,
    pub logged_out: bool,
    pub deliveries: BTreeMap<String, crate::delivery::Pending>,
}
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issuing {
    pub rotation: Option<Rotation>,
    pub tombstones: BTreeSet<rocky_credentials::PeerId>,
    pub heard: BTreeMap<PublicKey, u64>,
    pub trusted_nodes:
        BTreeMap<PublicKey, BTreeMap<PublicKey, crate::membership::AdminObservation>>,
    pub seen: BTreeMap<PublicKey, crate::membership::Seen>,
    pub rights_change: Option<RightsChange>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RightsChange {
    pub publication: Option<Commitment>,
    pub version: u32,
    pub people: BTreeSet<PublicKey>,
    pub exposed: Option<Exposure>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rotation {
    pub identities: Vec<PublicKey>,
    pub nodes: Vec<PublicKey>,
    pub statement: Revocation,
    pub signed: Option<SignedRevocation>,
    pub statements: Vec<Commitment>,
    pub keys: BTreeMap<String, RotationEpoch>,
    pub groups: BTreeMap<String, RotationEpoch>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Revoke {
    Identity(PublicKey),
    Admin(PublicKey, rocky_credentials::RevocationMode),
    Node {
        owner: PublicKey,
        node: PublicKey,
    },
    Rights {
        people: BTreeSet<PublicKey>,
        exposed: Exposure,
    },
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exposure {
    pub keys: BTreeSet<String>,
    pub groups: BTreeSet<String>,
}
pub(crate) fn changes_rights_only(statement: &Revocation) -> bool {
    statement.identities.is_empty()
        && statement.nodes.is_empty()
        && !statement.capabilities.is_empty()
}
enum ReadBound {
    Unbounded,
    Recorded(Revoking),
}
impl ReadBound {
    fn of(registry: &Registry, domain: &Domain, identity: Option<&PublicKey>) -> Self {
        identity
            .filter(|identity| {
                registry
                    .get(identity)
                    .is_some_and(|record| record.admitted.is_some() || !record.grants.is_empty())
            })
            .and_then(|identity| Revoking::of(registry, domain, identity))
            .map_or(Self::Unbounded, Self::Recorded)
    }
    fn reads(&self, read: impl FnOnce(&Revoking) -> bool) -> bool {
        match self {
            Self::Unbounded => true,
            Self::Recorded(record) => read(record),
        }
    }
}
struct Revoking {
    capability: Option<Commitment>,
    access: Option<Access>,
    administers: bool,
    partitions: Vec<rocky_credentials::PartitionGrant>,
}
impl Revoking {
    fn of(registry: &Registry, domain: &Domain, identity: &PublicKey) -> Option<Self> {
        let record = registry.get(identity)?;
        Some(Self {
            capability: record.capability().map(|capability| capability.id),
            access: record.access().cloned(),
            administers: registry.admins.contains_key(identity),
            partitions: record.partition_grants(domain),
        })
    }
    fn reads(&self, domain: &Domain, table: &str) -> bool {
        self.administers
            || self
                .access
                .as_ref()
                .is_some_and(|access| domain.reads_table(access, table))
    }
    fn selects(&self, partition: &GroupPartition) -> bool {
        partition.members.iter().any(|member| {
            self.partitions.iter().any(|grant| {
                grant
                    .resolve(member.scope, &member.tuple)
                    .ok()
                    .flatten()
                    .is_some()
            })
        })
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub routes: Vec<Route>,
}
impl Contact {
    pub(crate) fn checked(routes: Vec<Route>) -> Result<Self, Failure> {
        if routes.len() > MAX_ROUTES {
            return Err("invalid contact routes".into());
        }
        let mut contact = Self::default();
        contact.learn(routes);
        Ok(contact)
    }
    pub(crate) fn learn(&mut self, routes: Vec<Route>) {
        for route in routes {
            self.routes.retain(|known| known.scheme() != route.scheme());
            self.routes.push(route);
        }
        let excess = self.routes.len().saturating_sub(MAX_ROUTES);
        self.routes.drain(..excess);
    }
    pub fn addresses(&self, transport: &dyn Transport) -> Vec<String> {
        self.routes
            .iter()
            .filter(|route| transport.claims(route.as_str()))
            .map(|route| route.as_str().to_owned())
            .collect()
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContactRecord {
    v: u32,
    pub(crate) publisher: PublicKey,
    pub(crate) database: rocky_credentials::DatabaseContext,
    pub(crate) node: PublicKey,
    pub(crate) routes: Vec<Route>,
    issuer: bool,
}
impl ContactRecord {
    pub(crate) fn parse(uri: &str) -> Result<Self, Failure> {
        let bytes = BASE64URL_NOPAD.decode(
            uri.trim()
                .strip_prefix("rocky://peer/")
                .ok_or("invalid contact URI")?
                .as_bytes(),
        )?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        if value["v"].as_u64() != Some(u64::from(CONTACT_VERSION)) {
            return Err("unsupported contact version".into());
        }
        let record: Self = serde_json::from_value(value)?;
        if record.database.publisher != record.publisher {
            return Err("contact database publisher mismatch".into());
        }
        Ok(record)
    }
}
pub(crate) struct EnrollingIssuer {
    pub(crate) publisher: PublicKey,
    pub(crate) database: rocky_credentials::DatabaseContext,
    pub(crate) routes: Vec<Route>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Enrollment {
    mesh: MeshId,
    domain: DomainId,
    record: crate::tip_schemas::Record,
    genesis: String,
    bootstrap: crate::tip_schemas::Bootstrap,
    revocations: RevocationSet,
    capability: SignedCapability,
    access: Access,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    partitions: BTreeSet<HeldPartition>,
    keys: Keys,
    groups: Vec<GroupDelivery>,
    epoch_statements: BTreeMap<String, Vec<Commitment>>,
    naming: Option<String>,
    issuer: PublicKey,
    chain: CredentialChain,
    ttl: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    issuers: Vec<String>,
}
impl Enrollment {
    pub(crate) fn issuer(&self) -> PublicKey {
        self.issuer
    }
}
type Keys = BTreeMap<String, Vec<(rocky_core::KeyEpoch, String)>>;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sealed {
    keys: Keys,
    groups: Vec<GroupDelivery>,
    epoch_statements: BTreeMap<String, Vec<Commitment>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    issuers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    naming: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trusted: Option<SignedCapability>,
}
impl Sealed {
    fn parse(bytes: &[u8]) -> Result<Self, Failure> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Intent {
    purpose: Commitment,
    pub(crate) installation: String,
    nonce: String,
    identity: PublicKey,
    minted: bool,
}
pub(crate) struct Prepared {
    pub(crate) intent: Intent,
    pub(crate) vault: Vault,
    pub(crate) provider: Arc<dyn IdentityProvider>,
    pub(crate) minted: Option<IdentityKey>,
}
const MINTED_ROLE: &str = "identity.minted";
impl Intent {
    pub(crate) fn path(identity: &Path) -> std::path::PathBuf {
        identity
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("identity.join.json")
    }
    pub(crate) fn resume(
        path: &Path,
        purpose: Commitment,
        store: &std::sync::Arc<dyn crate::SecretStore>,
        provider: Option<&Arc<dyn IdentityProvider>>,
    ) -> Result<Prepared, Failure> {
        if rocky_platform::fs::exists(path) {
            let previous: Self = serde_json::from_slice(&rocky_platform::fs::read(path)?)?;
            let vault = Vault::new(store.clone(), &previous.installation);
            let resumed = match (previous.purpose == purpose, provider, previous.minted) {
                (true, Some(provider), false) => {
                    if provider.public_key()? != previous.identity {
                        return Err("unfinished enrollment needs its original identity; resume with the supplied provider".into());
                    }
                    Some((provider.clone(), None))
                }
                (true, None, true) => {
                    let minted = IdentityKey::from_secret(vault.get(MINTED_ROLE)?);
                    (minted.person_key() == previous.identity).then(|| {
                        let provider: Arc<dyn IdentityProvider> =
                            Arc::new(IdentityKey::from_secret(minted.secret()));
                        (provider, Some(minted))
                    })
                }
                _ => None,
            };
            if let Some((provider, minted)) = resumed {
                return Ok(Prepared {
                    intent: previous,
                    vault,
                    provider,
                    minted,
                });
            }
            rocky_platform::fs::remove_file(path)?;
            vault.forget(
                ["node", "transport", MINTED_ROLE]
                    .into_iter()
                    .map(String::from),
            )?;
        }
        let minted = provider.is_none().then(IdentityKey::generate);
        let provider: Arc<dyn IdentityProvider> = match (provider, &minted) {
            (Some(provider), _) => provider.clone(),
            (None, Some(minted)) => Arc::new(IdentityKey::from_secret(minted.secret())),
            (None, None) => return Err("an installation needs an identity".into()),
        };
        let intent = Self {
            purpose,
            installation: HEXLOWER.encode(&random::<16>()),
            nonce: HEXLOWER.encode(&random::<32>()),
            identity: provider.public_key()?,
            minted: minted.is_some(),
        };
        let vault = Vault::new(store.clone(), &intent.installation);
        for role in ["node", "transport"] {
            vault.create(role)?;
        }
        if let Some(minted) = &minted {
            vault.put(MINTED_ROLE, &minted.secret())?;
        }
        write_private(path, &serde_json::to_vec(&intent)?)?;
        Ok(Prepared {
            intent,
            vault,
            provider,
            minted,
        })
    }
    pub(crate) fn finish(&self, path: &Path, vault: &Vault) -> Result<(), Failure> {
        rocky_platform::fs::remove_file(Self::path(path))?;
        if self.minted {
            vault.forget([MINTED_ROLE.to_owned()])?;
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NodeKeys {
    pub(crate) identity: PublicKey,
    pub(crate) node: PublicKey,
}
pub struct Renewed {
    pub contacted: Option<PublicKey>,
    pub revocations: RevocationSet,
    pub chain: CredentialChain,
    pub keys: Vec<(String, rocky_core::KeyEpoch, [u8; 32])>,
    pub epoch_statements: BTreeMap<String, Vec<Commitment>>,
    pub groups: Vec<GroupDelivery>,
    pub access: Option<Access>,
    pub issuers: Vec<String>,
    pub naming: Option<String>,
    pub admin_grant: Option<SignedCapability>,
}

pub fn now() -> Result<u64, Failure> {
    Ok(rocky_platform::time::since_epoch()?.as_secs())
}
pub(crate) fn random<const N: usize>() -> [u8; N] {
    rocky_platform::entropy::random()
}
pub(crate) fn hex32(text: &str) -> Result<[u8; 32], Failure> {
    HEXLOWER
        .decode(text.as_bytes())?
        .try_into()
        .map_err(|_| "expected 32 bytes".into())
}
pub(crate) fn key_role(collection: &str, epoch: rocky_core::KeyEpoch) -> String {
    format!(
        "reader.{collection}.{}.{}",
        epoch.generation,
        HEXLOWER.encode(&epoch.secret_hash)
    )
}
pub(crate) fn context(schema: &Schema, database: rocky_credentials::DatabaseContext) -> Commitment {
    commitment(
        "rocky.sdk.enrollment-context.v2",
        &[
            schema.application.as_bytes(),
            database.publisher.as_bytes(),
            &database.scope.mesh.0,
            &database.scope.app.0,
            &database.scope.domain.0,
        ]
        .concat(),
    )
}
pub(crate) fn reads(grants: &[ResourceGrant], scope: rocky_core::ResourceScope) -> bool {
    grants.iter().any(|grant| {
        grant.scope == scope
            && grant.rows == RowSelection::All
            && grant.rights.contains(&Right::Read)
    })
}
impl Settings {
    pub(crate) fn database_context(&self) -> Result<rocky_credentials::DatabaseContext, Failure> {
        Ok(rocky_credentials::DatabaseContext {
            publisher: self.publisher,
            scope: self.domain()?.scope,
        })
    }
    pub(crate) fn revoking(
        &self,
        set: &RevocationSet,
    ) -> Result<Option<SignedRevocation>, Failure> {
        set.verify_in(self.database_context()?)?;
        let ending = set
            .statements()
            .filter(|signed| !changes_rights_only(&signed.statement))
            .cloned()
            .collect::<Vec<_>>();
        Ok(RevocationSet::try_from(ending)?
            .revoking_holder(&self.credentials)?
            .cloned())
    }

    pub(crate) fn revocation_reason(
        &self,
        statement: &SignedRevocation,
        node: Option<rocky_credentials::PeerId>,
    ) -> Result<crate::RevocationReason, Failure> {
        let revoked = &statement.statement;
        Ok(if node.is_some_and(|node| revoked.nodes.contains(&node)) {
            crate::RevocationReason::Node
        } else if revoked.identities.contains(&self.identity) {
            crate::RevocationReason::Identity
        } else if let Some(origin) = self.link_origin()?.filter(|origin| {
            statement.root()
                && revoked.mode == rocky_credentials::RevocationMode::Compromised
                && revoked.identities.contains(origin)
        }) {
            crate::RevocationReason::Cascade {
                admin: crate::PersonId::new(origin),
            }
        } else {
            crate::RevocationReason::Capability
        })
    }

    pub(crate) fn learn_revocations(
        &mut self,
        set: &RevocationSet,
    ) -> Result<Vec<SignedRevocation>, Failure> {
        set.verify_in(self.database_context()?)?;
        let app = self.domain()?.manifest().app;
        if let Some(certificate) = self.credentials.certificate() {
            self.revocations.link(certificate.clone())?;
        }
        let added = self.revocations.merge(set, self.publisher, app, now()?)?;
        if let Some(authority) = &mut self.authority {
            for signed in self.revocations.statements() {
                let statement = &signed.statement;
                authority.tombstones.extend(
                    statement.nodes.iter().copied().chain(
                        statement
                            .identities
                            .iter()
                            .map(|key| rocky_credentials::PeerId::of(*key)),
                    ),
                );
            }
        }
        Ok(added)
    }
    pub(crate) fn revocation_status(&self) -> Result<crate::RevocationStatus, Failure> {
        let time = now()?;
        Ok(crate::RevocationStatus {
            digest: HEXLOWER.encode(&self.revocations.digest(time)?),
            statements: self.revocations.live(time) as u64,
        })
    }
    pub(crate) fn revokes_new_authors(&self, set: &RevocationSet) -> Result<bool, Failure> {
        let time = now()?;
        let mut learned = self.clone();
        learned.learn_revocations(set)?;
        Ok(learned.revocations.digest(time)? != self.revocations.digest(time)?)
    }
    pub(crate) fn lacks(&self, set: &RevocationSet) -> Result<bool, Failure> {
        let time = now()?;
        if set
            .certificate_ids()
            .any(|id| !self.revocations.holds_certificate(id))
        {
            return Ok(true);
        }
        for signed in set.statements() {
            if signed.live(time) && self.revocations.get(&signed.id()?).is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Failure> {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        if value["version"].as_u64() != Some(u64::from(VERSION)) {
            return Err("unsupported identity version; reinstall the domain".into());
        }
        let settings: Self = serde_json::from_value(value)?;
        for contact in settings.peers.values() {
            Contact::checked(contact.routes.clone())?;
        }
        if settings.contact_meta.len() > MAX_CONTACTS
            || settings.contact_meta.iter().any(|(node, meta)| {
                !settings.peers.contains_key(node)
                    || !matches!(
                        meta.source.as_str(),
                        "application" | "enrollment" | "handover" | "request" | "discovery"
                    )
            })
        {
            return Err("invalid contact metadata".into());
        }
        if settings.peers.len() > MAX_CONTACTS || settings.issuers.len() > MAX_ISSUERS {
            return Err("identity holds too many peers".into());
        }
        for epochs in settings
            .keys
            .values()
            .chain(settings.groups.values().map(|group| &group.epochs))
        {
            if epochs.len() > MAX_EPOCHS
                || epochs.iter().any(|epoch| !epoch.valid())
                || epochs.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err("invalid retained key epochs".into());
            }
        }
        for ids in settings.epoch_statements.values() {
            if ids.len() > MAX_LIVE_REVOCATIONS || ids.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err("invalid epoch revocation IDs".into());
            }
        }
        settings.validate_saved_contacts()?;
        settings.check_deliveries()?;
        Ok(settings)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Failure> {
        let saved = self.with_saved_contacts();
        saved.validate_saved_contacts()?;
        Ok(serde_json::to_vec(&saved)?)
    }
    pub fn exists(path: &Path) -> Result<bool, Failure> {
        Ok(rocky_secure::read_authority_entry(path, AUTHORITY_IDENTITY_KEY)?.is_some())
    }
    pub fn load(path: &Path) -> Result<Self, Failure> {
        let Some((bytes, _)) = rocky_secure::read_authority_entry(path, AUTHORITY_IDENTITY_KEY)?
        else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no identity in replica",
            )
            .into());
        };
        Self::from_bytes(&bytes)
    }
    pub async fn save(&self, path: &Path) -> Result<(), Failure> {
        let bytes = self.to_bytes()?;
        rocky_secure::write_authority_entry(path, AUTHORITY_IDENTITY_KEY, &bytes, now()?).await?;
        Ok(())
    }
    pub fn vault(&self, store: &std::sync::Arc<dyn crate::SecretStore>) -> Vault {
        Vault::new(store.clone(), &self.installation)
    }
    pub fn issuer(&self) -> bool {
        self.authority.is_some()
    }
    pub(crate) fn stores_only(&self) -> bool {
        self.access == Access::Store && !self.issuer() && self.keys.is_empty()
    }
    pub(crate) fn renews_from_issuer(&self) -> bool {
        !self.issuer() || !self.issuers.is_empty()
    }
    pub fn domain(&self) -> Result<Domain, Failure> {
        let publisher = *self.publisher.as_bytes();
        Domain::at(
            &self.schema,
            publisher,
            self.mesh,
            self.domain,
            self.generation,
            self.engine,
        )
    }

    pub fn manifest(&self) -> Result<SignedManifest, Failure> {
        Ok(SignedManifest::from_bytes(
            &HEXLOWER.decode(self.genesis.as_bytes())?,
        )?)
    }
    fn grants(&self, domain: &Domain) -> Vec<ResourceGrant> {
        self.held(domain)
            .map_or_else(Vec::new, |held| domain.expand(&held))
    }
    fn held(&self, domain: &Domain) -> Option<rocky_credentials::Granted> {
        let held = self.credentials.held()?;
        let extended = held.extended().ok()?;
        Some(rocky_credentials::Granted {
            grants: held.grants.clone(),
            partitions: domain.effective_partitions(&extended.partitions),
            whole: extended.whole,
            role: extended.role,
        })
    }
    fn selected(
        &self,
        domain: &Domain,
        scope: &rocky_core::ResourceScope,
        tuple: &rocky_core::PartitionTuple,
    ) -> Vec<ResourceGrant> {
        self.held(domain)
            .map_or_else(Vec::new, |held| domain.expand_partitions(&held))
            .into_iter()
            .filter(|grant| grant.table == scope.table)
            .filter_map(|grant| grant.resolve(*scope, tuple).ok().flatten())
            .collect()
    }
    pub(crate) fn reads_partition(&self, domain: &Domain, member: &Member) -> bool {
        self.credentials
            .grants()
            .iter()
            .chain(&self.selected(domain, &member.scope, &member.tuple))
            .any(|grant| {
                grant.scope == member.scope
                    && grant.rows == RowSelection::All
                    && grant.rights.contains(&Right::Read)
            })
    }
    pub(crate) fn owns_partition(&self, domain: &Domain, member: &Member) -> bool {
        let selected = self.selected(domain, &member.scope, &member.tuple);
        [Right::Read, Right::Insert, Right::Update, Right::Delete]
            .iter()
            .all(|right| {
                self.credentials
                    .grants()
                    .iter()
                    .chain(&selected)
                    .any(|grant| {
                        grant.scope == member.scope
                            && grant.rows == RowSelection::All
                            && grant.rights.contains(right)
                    })
            })
    }
    pub(crate) fn partitions(&self, domain: &Domain) -> Result<Vec<GroupPartition>, Failure> {
        self.groups
            .values()
            .map(|record| GroupPartition::of(domain, record))
            .collect()
    }
    pub fn reads_whole(&self, domain: &Domain, scope: &rocky_core::ResourceScope) -> bool {
        let concrete = self.grants(domain).iter().any(|grant| {
            grant.scope == *scope
                && grant.rows == RowSelection::All
                && grant.rights.contains(&Right::Read)
        });
        concrete
            || self.partitions(domain).is_ok_and(|partitions| {
                partitions
                    .iter()
                    .flat_map(|partition| &partition.members)
                    .any(|member| member.scope == *scope && self.reads_partition(domain, member))
            })
    }
    pub fn granted(&self, domain: &Domain) -> Vec<rocky_core::ResourceScope> {
        let partitions = self.partitions(domain).unwrap_or_default();
        let grants = self.grants(domain);
        domain
            .scopes()
            .map(|(_, scope)| scope)
            .filter(|scope| grants.iter().any(|grant| grant.scope == *scope))
            .chain(
                partitions
                    .iter()
                    .flat_map(|partition| &partition.members)
                    .filter(|member| {
                        grants.iter().any(|grant| grant.scope == member.scope)
                            || !self
                                .selected(domain, &member.scope, &member.tuple)
                                .is_empty()
                    })
                    .map(|member| member.scope),
            )
            .collect()
    }
    pub fn kept(&self) -> Vec<PublicKey> {
        self.peers.keys().chain(&self.issuers).copied().collect()
    }
    pub fn targets(&self) -> Vec<(PublicKey, Contact)> {
        let mut targets: Vec<_> = self
            .peers
            .iter()
            .filter(|(node, contact)| !contact.routes.is_empty() && !self.revoked(node))
            .map(|(node, contact)| (*node, contact.clone()))
            .collect();
        targets.sort_by_key(|(node, _)| !self.issuers.contains(node));
        targets
    }
    pub async fn contact(
        &self,
        vault: &Vault,
        transport: &dyn Transport,
    ) -> Result<(String, PublicKey, Vec<Route>), Failure> {
        let node = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        let routes = match transport.address().await? {
            Some(address) => vec![Route::try_from(address)?],
            None => Vec::new(),
        };
        let record = ContactRecord {
            v: CONTACT_VERSION,
            publisher: self.publisher,
            database: self.database_context()?,
            node,
            routes: routes.clone(),
            issuer: self.issuer(),
        };
        let uri = format!(
            "rocky://peer/{}",
            BASE64URL_NOPAD.encode(&serde_json::to_vec(&record)?)
        );
        Ok((uri, node, routes))
    }
    pub fn add_peer(&mut self, vault: &Vault, uri: &str) -> Result<PublicKey, Failure> {
        let record = ContactRecord::parse(uri)?;
        if record.database != self.database_context()? {
            return Err("the contact belongs to another domain".into());
        }
        if record.node == PrivateKey::from_bytes(&vault.get("node")?).public_key() {
            return Err("the contact names this device".into());
        }
        let learned = Contact::checked(record.routes)?;
        self.removed_peers
            .retain(|id| *id != rocky_credentials::PeerId::of(record.node));
        self.bind_saved_contact(record.node);
        if self.revoked(&record.node) {
            return Err("the contact names a revoked device".into());
        }
        if !self.peers.contains_key(&record.node) && self.contact_count() >= MAX_CONTACTS {
            return Err("contact capacity reached".into());
        }
        if record.issuer && !self.issuers.contains(&record.node) {
            if self.issuers.len() >= MAX_ISSUERS {
                return Err("issuer capacity reached".into());
            }
            self.issuers.push(record.node);
        }
        self.contact_meta.insert(
            record.node,
            crate::mesh::ContactMeta {
                source: "application".into(),
                learned: now()?,
                trusted: record.issuer,
                local: false,
            },
        );
        self.peers
            .entry(record.node)
            .or_default()
            .learn(learned.routes);
        Ok(record.node)
    }
    pub(crate) fn regard_as_issuers(&mut self, nodes: &BTreeSet<PublicKey>) -> bool {
        let mut changed = false;
        for node in nodes {
            if self.peers.contains_key(node)
                && !self.issuers.contains(node)
                && self.issuers.len() < MAX_ISSUERS
            {
                self.issuers.push(*node);
                if let Some(meta) = self.contact_meta.get_mut(node) {
                    meta.trusted = true;
                }
                changed = true;
            }
        }
        changed
    }
    pub(crate) fn forget_peer(&mut self, node: PublicKey) {
        let id = rocky_credentials::PeerId::of(node);
        self.contact_refs.remove(&id);
        self.resolved_refs.remove(&id);
        self.peers.remove(&node);
        self.contact_meta.remove(&node);
        self.issuers.retain(|issuer| *issuer != node);
    }
    pub fn remove_peer(&mut self, node: PublicKey) {
        self.forget_peer(node);
        let id = rocky_credentials::PeerId::of(node);
        if !self.removed_peers.contains(&id) {
            if self.removed_peers.len() >= MAX_CONTACTS {
                self.removed_peers.remove(0);
            }
            self.removed_peers.push(id);
        }
    }
    pub(crate) fn peer_removed(&self, node: &PublicKey) -> bool {
        self.removed_peers
            .contains(&rocky_credentials::PeerId::of(*node))
    }
    pub fn expires(&self) -> u64 {
        self.credentials.expires()
    }
    pub fn config(&self, vault: &Vault, domain: &Domain) -> Result<SecureConfig, Failure> {
        let mut keys = Vec::new();
        for (name, scope) in domain.scopes() {
            for epoch in self.keys.get(name).into_iter().flatten() {
                let key = vault.get(&key_role(name, *epoch))?;
                if rocky_core::KeyEpoch::new(epoch.generation, &key) != *epoch {
                    return Err("reader key epoch identity mismatch".into());
                }
                keys.push(KeyMaterial::at(scope, *epoch, key));
            }
        }
        let mut revocations = self.revocations.clone();
        if let Some(certificate) = self.credentials.certificate() {
            revocations.link(certificate.clone())?;
        }
        Ok(SecureConfig {
            publisher: self.publisher,
            manifest: self.manifest()?,
            revocations,
            node: PrivateKey::from_bytes(&vault.get("node")?),
            credentials: self.credentials.clone(),
            keys,
        })
    }
    pub fn open_config(
        &self,
        vault: &Vault,
        domain: &Domain,
    ) -> Result<rocky_secure::OpenConfig, Failure> {
        Ok(self
            .config(vault, domain)?
            .with_table_scopes(domain.scopes().map(|(_, scope)| scope).collect()))
    }
    pub fn roles(&self) -> Vec<String> {
        let mut roles: Vec<String> = [
            "node",
            "transport",
            "invitation",
            "author-issuer",
            "tip-nodes",
            "discovery",
            crate::database::incarnation::ROLE,
        ]
        .into_iter()
        .map(String::from)
        .collect();
        for (name, epochs) in &self.keys {
            roles.extend(epochs.iter().map(|epoch| key_role(name, *epoch)));
            roles.extend(
                epochs
                    .iter()
                    .map(|epoch| format!("rotation.reader.{name}.{}", epoch.generation)),
            );
        }
        for record in self.groups.values() {
            roles.push(format!(
                "prepared.partition.{}",
                crate::groups::record_key(&record.address)
            ));
            roles.extend(record.epochs.iter().map(|epoch| {
                format!(
                    "rotation.partition.{}.{}",
                    crate::groups::record_key(&record.address),
                    epoch.generation
                )
            }));
            roles.extend(
                record
                    .epochs
                    .iter()
                    .map(|epoch| secret_role(&record.address, *epoch)),
            );
        }
        let rotation_with_unlisted_keys = self.authority.as_ref().and_then(|a| a.rotation.as_ref());
        if let Some(rotation) = rotation_with_unlisted_keys {
            roles.extend(
                rotation
                    .keys
                    .iter()
                    .map(|(name, epoch)| format!("rotation.reader.{name}.{epoch}")),
            );
            roles.extend(rotation.groups.iter().filter_map(|(address, epoch)| {
                self.groups.get(address).map(|record| {
                    format!(
                        "rotation.partition.{}.{epoch}",
                        crate::groups::record_key(&record.address)
                    )
                })
            }));
        }
        roles
    }
    pub fn route_secret(vault: &Vault) -> Result<[u8; 32], Failure> {
        Ok(vault.get("transport")?)
    }
    pub(crate) fn renewal_routes(&self) -> Vec<crate::RenewalRoute> {
        let routes = |contact: &Contact| contact.routes.clone();
        let mut candidates: Vec<_> = self
            .issuers
            .iter()
            .map(|node| crate::RenewalRoute {
                target: node.to_hex(),
                routes: self.peers.get(node).map(routes).unwrap_or_default(),
            })
            .collect();
        candidates.extend(
            self.contact_refs
                .iter()
                .filter(|(id, entry)| !self.resolved_refs.contains(id) && entry.issuer)
                .map(|(id, entry)| crate::RenewalRoute {
                    target: HEXLOWER.encode(id.as_bytes()),
                    routes: routes(&entry.contact),
                }),
        );
        candidates.retain(|route| !route.routes.is_empty());
        candidates
    }
    pub fn another_issuer(&self, registry: &Registry, membership: &Membership) -> bool {
        let own = self.identity;
        !self.issuers.is_empty()
            || membership.nodes(&own).count() > 1
            || registry.records.iter().any(|(identity, record)| {
                *identity != own && !record.revoked && registry.admins.contains_key(identity)
            })
    }

    pub fn create(
        schema: &Schema,
        store: &std::sync::Arc<dyn crate::SecretStore>,
        ttl: u64,
        identity: Arc<dyn IdentityProvider>,
        root: &dyn IdentityProvider,
    ) -> Result<Self, Failure> {
        let installation = HEXLOWER.encode(&random::<16>());
        let vault = Vault::new(store.clone(), &installation);
        let publisher = root.public_key()?;
        if identity.public_key()? == publisher {
            return Err("a device's identity is never the root key".into());
        }
        vault.create("node")?;
        vault.create("transport")?;
        vault.create(crate::groups::NAMING_ROLE)?;
        let (mesh, domain) = (MeshId(random()), DomainId(random()));
        let bound = Domain::new(schema, *publisher.as_bytes(), mesh, domain)?;
        let manifest = SignedManifest::sign(&bound.manifest(), root)?;
        let mut keys = BTreeMap::new();
        for (name, _) in bound.scopes() {
            let key = random::<32>();
            let epoch = rocky_core::KeyEpoch::new(1, &key);
            vault.put(&key_role(name, epoch), &key)?;
            keys.insert(name.to_owned(), vec![epoch]);
        }
        let grant =
            Authority::owner(root).issue_owner(identity.public_key()?, bound.scope, now()?)?;
        let mut settings = Self::installed(
            installation,
            (publisher, identity.public_key()?),
            (mesh, domain, schema.clone()),
            HEXLOWER.encode(&manifest.to_bytes()?),
            ttl,
        );
        settings.keys = keys;
        settings.authority = Some(Issuing::default());
        let links = CredentialChain(vec![grant]);
        settings.credentials =
            crate::identities::with_identity(identity, || settings.certify(&vault, links, &bound))?;
        let node = Settings::node_key(&vault)?;
        let credentials = settings.credentials.clone();
        let links = credentials.capabilities();
        crate::mesh::create_root(
            &vault,
            (
                rocky_credentials::Signer::node(&node, &credentials)?,
                &links,
            ),
            bound.scope,
        )?;
        Ok(settings)
    }
    #[cfg(test)]
    pub(crate) fn create_owned(
        schema: &Schema,
        store: &std::sync::Arc<dyn crate::SecretStore>,
        ttl: u64,
    ) -> Result<Self, Failure> {
        let root = rocky_credentials::RootKey::generate();
        Self::create(schema, store, ttl, Arc::new(root.identity()), &root)
    }
    pub(crate) fn installed(
        installation: String,
        (publisher, identity): (PublicKey, PublicKey),
        (mesh, domain, schema): (MeshId, DomainId, Schema),
        manifest: String,
        ttl: u64,
    ) -> Self {
        Self {
            version: VERSION,
            installation,
            schema: schema.clone(),
            mesh,
            domain,
            publisher,
            identity,
            generation: schema.version,
            engine: rocky_core::EngineIdentity::current(),
            genesis: manifest,
            pending: None,
            bootstrap: None,
            revocations: RevocationSet::new(),
            credentials: CredentialChain(Vec::new()),
            ttl,
            access: Access::Full,
            keys: BTreeMap::new(),
            authority: None,
            device_revocations: BTreeMap::new(),
            issuers: Vec::new(),
            delivery_nodes: BTreeMap::new(),
            contact_refs: BTreeMap::new(),
            removed_peers: Vec::new(),
            resolved_refs: BTreeSet::new(),
            contact_meta: BTreeMap::new(),
            peers: BTreeMap::new(),
            logged_out: false,
            deliveries: BTreeMap::new(),
            groups: BTreeMap::new(),
            epoch_statements: BTreeMap::new(),
        }
    }
    pub(crate) fn validity(&self) -> Result<rocky_credentials::Validity, Failure> {
        Ok(rocky_credentials::Validity {
            issued_at: now()?,
            lifetime: std::time::Duration::from_secs(self.ttl),
            clock_skew: std::time::Duration::from_secs(SKEW),
        })
    }
    pub fn make_admin(
        &self,
        registry: &Registry,
        identity: PublicKey,
        root: Option<&dyn IdentityProvider>,
    ) -> Result<(SignedCapability, Changes), Failure> {
        let root = root_signer(root, self.publisher, "granting admin")?;
        if self.tombstoned(&identity) || self.revocations.revokes_trusted(&identity) {
            return Err("identity was revoked".into());
        }
        let record = registry
            .get(&identity)
            .filter(|record| !record.revoked)
            .ok_or("this identity is not recorded")?;
        record.capability().ok_or("this identity is not recorded")?;
        let live =
            registry.admin_identities(self.database_context()?, &self.revocations, now()?)?;
        if !live.contains(&identity) && live.len() >= crate::registry::MAX_ADMINS {
            return Err(format!(
                "a domain has at most {} admins",
                crate::registry::MAX_ADMINS
            )
            .into());
        }
        let grant = Authority::owner(root).issue_trusted(identity, self.domain()?.scope, now()?)?;
        let mut changes = Changes::default();
        changes.admin(
            &identity,
            &crate::registry::AdminRecord {
                grant: grant.clone(),
                granted: now()?,
            },
        )?;
        Ok((grant, changes))
    }
    pub fn install_admin_grant(
        &mut self,
        vault: &Vault,
        link: SignedCapability,
    ) -> Result<(), Failure> {
        let identity = CredentialChain(vec![link.clone()]).trusted_identity(
            self.publisher,
            self.domain()?.scope,
            &self.revocations,
            now()?,
        )?;
        if identity != self.identity {
            return Err("the admin grant names another identity".into());
        }
        self.authority.get_or_insert_with(Issuing::default);
        self.access = Access::Full;
        let domain = self.domain()?;
        self.credentials = self.certify(vault, CredentialChain(vec![link]), &domain)?;
        Ok(())
    }
    pub(crate) fn owns(&self) -> bool {
        self.admin_links()
            .and_then(|chain| chain.0.first()?.capability.extended().ok())
            .is_some_and(|extended| extended.owner)
    }
    pub(crate) fn admin_links(&self) -> Option<CredentialChain> {
        let chain = self.identity_links();
        chain
            .trusted_identity(
                self.publisher,
                self.domain().ok()?.scope,
                &self.revocations,
                now().ok()?,
            )
            .ok()
            .filter(|identity| *identity == self.identity)
            .map(|_| chain)
    }
    pub(crate) fn link_origin(&self) -> Result<Option<PublicKey>, Failure> {
        Ok(match self.identity_links().0.last() {
            Some(link) => link
                .capability
                .extended()?
                .origin
                .map(|origin| origin.identity),
            None => None,
        })
    }
    fn issuing(&mut self, action: &str) -> Result<&mut Issuing, Failure> {
        self.authority
            .as_mut()
            .ok_or_else(|| format!("only an issuer {action}").into())
    }
    fn keys_for(
        &self,
        vault: &Vault,
        domain: &Domain,
        grants: &[ResourceGrant],
    ) -> Result<Keys, Failure> {
        let mut keys = Keys::new();
        for (name, scope) in domain.scopes() {
            if !reads(grants, scope) {
                continue;
            }
            if let Some(epochs) = self.keys.get(name) {
                let generation = epochs.iter().map(|epoch| epoch.generation).max();
                for epoch in epochs
                    .iter()
                    .filter(|epoch| Some(epoch.generation) == generation)
                {
                    keys.entry(name.to_owned()).or_default().push((
                        *epoch,
                        HEXLOWER.encode(&vault.get(&key_role(name, *epoch))?),
                    ));
                }
            }
        }
        Ok(keys)
    }

    pub(crate) fn groups_for(
        &self,
        vault: &Vault,
        node: PublicKey,
        capability: &rocky_credentials::Capability,
        all: bool,
    ) -> Result<Vec<GroupDelivery>, Failure> {
        let domain = self.domain()?;
        let extended = capability.extended()?;
        let every_partition = extended.whole.is_some_and(|whole| whole.reads())
            || reads(&capability.grants, domain.registry());
        let grants = domain.expand_partitions(&rocky_credentials::Granted {
            partitions: extended.partitions,
            whole: extended.whole,
            ..rocky_credentials::Granted::default()
        });
        let sender = PrivateKey::from_bytes(&vault.get("node")?);
        let mut deliveries = Vec::new();
        for partition in self.partitions(&domain)? {
            let Some(current) = partition.record.current() else {
                continue;
            };
            if !partition.members.iter().all(|member| {
                grants.iter().any(|grant| {
                    let names_partitions_only = grant.selector.0.values().all(|selection| {
                        matches!(selection, rocky_core::PartitionSelection::Only(_))
                    });
                    let selected = every_partition || names_partitions_only;
                    selected
                        && grant
                            .resolve(member.scope, &member.tuple)
                            .ok()
                            .flatten()
                            .is_some_and(|grant| {
                                grant.rows == RowSelection::All
                                    && grant.rights.contains(&Right::Read)
                            })
                })
            }) {
                continue;
            }
            let mut record = partition.record.clone();
            if !all {
                record
                    .epochs
                    .retain(|epoch| epoch.generation == current.generation);
            }
            let mut envelopes = Vec::new();
            for epoch in &record.epochs {
                let role = secret_role(&record.address, *epoch);
                envelopes.push(
                    rocky_credentials::GroupSecretEnvelope::seal_recorded(
                        &sender,
                        rocky_credentials::GroupSecrets {
                            group: record.address,
                            epoch: epoch.generation,
                            secret: vault.get(&role)?,
                            name: record.name,
                        },
                        node,
                        self.epoch_statements
                            .get(&role)
                            .cloned()
                            .unwrap_or_default(),
                    )?
                    .to_bytes()?,
                );
            }
            deliveries.push((record, envelopes));
        }
        Ok(deliveries)
    }
    pub(crate) fn adopt_groups(
        &mut self,
        vault: &Vault,
        deliveries: &[(GroupRecord, Vec<Vec<u8>>)],
    ) -> Result<(), Failure> {
        let domain = self.domain()?;
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        for (offered, envelopes) in deliveries {
            let partition = GroupPartition::of(&domain, offered)?;
            if partition
                .members
                .iter()
                .any(|member| !self.reads_partition(&domain, member))
            {
                return Err("group renewal requires read permission on every member".into());
            }
            let address = crate::groups::record_key(&offered.address);
            let mut record = self
                .groups
                .get(&address)
                .cloned()
                .unwrap_or_else(|| GroupRecord {
                    epochs: Vec::new(),
                    ..offered.clone()
                });
            for bytes in envelopes {
                let envelope = rocky_credentials::GroupSecretEnvelope::from_bytes(bytes)?;
                let epoch = envelope.epoch();
                if !offered.epochs.contains(&epoch) {
                    return Err("group renewal epoch mismatch".into());
                }
                let carrying_issuer = envelope.issuer();
                let (secret, name) =
                    envelope.open(&node, carrying_issuer, offered.address, epoch)?;
                if name != offered.name {
                    return Err("group renewal names the partition differently".into());
                }
                record.admit(epoch)?;
                let role = secret_role(&record.address, epoch);
                if vault.find(&role)?.is_some_and(|held| held != secret) {
                    return Err("group secret differs from retained epoch".into());
                }
                vault.put(&role, &secret)?;
                record.hold(epoch)?;
                if self
                    .epoch_statements
                    .get(&role)
                    .is_some_and(|held| held != envelope.statements())
                {
                    return Err("group epoch revocation IDs differ from retained metadata".into());
                }
                self.epoch_statements
                    .insert(role, envelope.statements().to_vec());
            }
            self.groups.insert(address, record);
        }
        Ok(())
    }
    pub(crate) fn recover_groups(
        &mut self,
        vault: &Vault,
        domain: &Domain,
        retained: &[(
            rocky_core::ResourceScope,
            rocky_core::PartitionTuple,
            rocky_core::PartitionName,
            Vec<rocky_core::KeyEpoch>,
        )],
    ) -> Result<bool, Failure> {
        let logical = domain.logical();
        let mut recovered = false;
        for (scope, tuple, name, epochs) in retained {
            let Some(table) = logical
                .partition_groups()?
                .into_iter()
                .flat_map(|group| group.tables)
                .find(|table| logical.table_id(table) == scope.table)
            else {
                continue;
            };
            let values = table
                .partition_by
                .iter()
                .map(|name| {
                    let column = table.column(name).ok_or("unknown partition column")?;
                    Ok(tuple
                        .values()
                        .get(&logical.field_id(table, column))
                        .ok_or("missing partition column")?
                        .value()?)
                })
                .collect::<Result<Vec<_>, Failure>>()?;
            let resolved =
                GroupPartition::resolve(domain, None, Some(&table.sql_name), values, |_| {
                    Ok(*name)
                })?;
            if resolved.members.iter().all(|member| member.scope != *scope)
                || resolved
                    .members
                    .iter()
                    .any(|member| !self.reads_partition(domain, member))
            {
                continue;
            }
            let address = crate::groups::record_key(&resolved.record.address);
            let mut record = self
                .groups
                .get(&address)
                .cloned()
                .unwrap_or(resolved.record);
            let mut changed = false;
            for epoch in epochs {
                if record.epochs.contains(epoch) {
                    continue;
                }
                let held = vault.find(&secret_role(&record.address, *epoch))?;
                if held.is_some_and(|secret| {
                    rocky_core::KeyEpoch::new(epoch.generation, &secret) == *epoch
                }) {
                    record.hold(*epoch)?;
                    changed = true;
                }
            }
            if changed {
                self.groups.insert(address, record);
                recovered = true;
            }
        }
        Ok(recovered)
    }
    fn statements_for(&self, keys: &Keys) -> BTreeMap<String, Vec<Commitment>> {
        keys.iter()
            .flat_map(|(name, epochs)| epochs.iter().map(move |(epoch, _)| key_role(name, *epoch)))
            .map(|role| {
                let ids = self
                    .epoch_statements
                    .get(&role)
                    .cloned()
                    .unwrap_or_default();
                (role, ids)
            })
            .collect()
    }
    pub(crate) fn enrollment_for(
        &mut self,
        vault: &Vault,
        device: NodeKeys,
        (access, named, label): (Access, &[crate::Partition], Option<Vec<u8>>),
        invitation: Option<crate::InvitationId>,
        registry: &Registry,
    ) -> Result<(Enrollment, Changes), Failure> {
        if label
            .as_ref()
            .is_some_and(|label| label.len() > registry::MAX_LABEL)
        {
            return Err("device label exceeds 256 bytes".into());
        }
        self.issuing("admits devices")?;
        if self.revoked_node(&device, registry) {
            return Err("the node or its identity was revoked".into());
        }
        if registry.full_for(&device.identity) {
            return Err(Box::new(RegistryFull));
        }
        let known = registry.get(&device.identity);
        let domain = self.domain()?;
        let issuer = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        let node = Self::node_key(vault)?;
        let links = self.issuer_links();
        let time = now()?;
        let other_access_replaces_record = known
            .and_then(registry::IdentityRecord::access)
            .is_some_and(|known| *known != access);
        let targets: Vec<_> = named.iter().map(crate::Partition::target).collect();
        let mut partitions = domain.partition_targets(&targets)?;
        if let Some(record) = known.filter(|_| !other_access_replaces_record) {
            partitions.extend(record.held(&domain)?);
        }
        let granted = domain.compile(&access, &partitions)?;
        let grants = domain.expand(&granted);
        let authority = self.authority(&node)?;
        let issued = authority.issue_person(device.identity, granted.clone(), self.validity()?)?;
        let mut changes = Changes::default();
        changes.admitted(
            &device.identity,
            &registry::Admitted {
                access: access.clone(),
                invited_by: self.identity,
                origin: issued
                    .capability
                    .extended()?
                    .origin
                    .map(|origin| origin.identity),
                joined: time,
                invitation,
            },
            other_access_replaces_record,
        )?;
        for partition in named {
            changes.grant(
                &device.identity,
                &crate::partitions::StoredPartition::of(partition),
                known,
            )?;
        }
        if let Some(label) = &label {
            if known.is_none_or(|record| record.label.is_none()) {
                changes.label(&device.identity, label)?;
            }
            changes.device_label(&device.identity, &device.node, label)?;
        }
        changes.capability(
            &device.identity,
            registry::Capability {
                id: issued.id()?,
                expires: issued.capability.expires,
            },
            known,
        )?;
        let keys = self.keys_for(vault, &domain, &grants)?;
        let enrollment = Enrollment {
            groups: self.groups_for(vault, device.node, &issued.capability, false)?,
            naming: None,
            epoch_statements: self.statements_for(&keys),
            mesh: self.mesh,
            domain: self.domain,
            record: crate::tip_schemas::Record::new(&self.schema, self.generation, self.engine),
            genesis: self.genesis.clone(),
            bootstrap: self.schema_bootstrap()?,
            revocations: self.revocations.clone(),
            capability: issued,
            keys,
            issuer,
            chain: links.clone(),
            ttl: self.ttl,
            access,
            partitions,
            issuers: self.issuer_contacts(&[device.node])?,
        };
        Ok((enrollment, changes))
    }
    pub(crate) fn own_record(&self, registry: &Registry) -> Result<Changes, Failure> {
        let own = self.identity;
        let known = registry.get(&own);
        let links = self.identity_links();
        let root = links.0.last().ok_or("missing credentials")?;
        let mut changes = Changes::default();
        if self.owns() {
            changes.admitted(
                &own,
                &registry::Admitted {
                    access: self.access.clone(),
                    invited_by: own,
                    origin: self.link_origin()?,
                    joined: now()?,
                    invitation: None,
                },
                false,
            )?;
        }
        changes.capability(
            &own,
            registry::Capability {
                id: root.id()?,
                expires: root.capability.expires,
            },
            known,
        )?;
        if let Some(links) = self.admin_links()
            && !registry.admins.contains_key(&self.identity)
        {
            let grant = links.0.first().ok_or("missing admin grant")?.clone();
            changes.admin(
                &self.identity,
                &registry::AdminRecord {
                    granted: grant.capability.not_before,
                    grant,
                },
            )?;
        }
        for (other, record) in &registry.records {
            let tombstone_unwritten =
                !record.revoked && record.admitted.is_some() && self.tombstoned(other);
            if tombstone_unwritten {
                changes.revoked(other)?;
            }
        }
        Ok(changes)
    }

    pub(crate) fn issuer_contacts(&self, exclude: &[PublicKey]) -> Result<Vec<String>, Failure> {
        let mut contacts = Vec::new();
        for node in &self.issuers {
            let Some(contact) = self.peers.get(node) else {
                continue;
            };
            if contact.routes.is_empty() || exclude.contains(node) || self.revoked(node) {
                continue;
            }
            let record = ContactRecord {
                v: CONTACT_VERSION,
                publisher: self.publisher,
                database: self.database_context()?,
                node: *node,
                routes: contact.routes.clone(),
                issuer: true,
            };
            contacts.push(format!(
                "rocky://peer/{}",
                BASE64URL_NOPAD.encode(&serde_json::to_vec(&record)?)
            ));
        }
        Ok(contacts)
    }
    pub(crate) fn learn_issuers(&mut self, vault: &Vault, contacts: &[String]) {
        for uri in contacts {
            let Ok(record) = ContactRecord::parse(uri) else {
                continue;
            };
            if record.issuer
                && !self.peers.contains_key(&record.node)
                && !self.revoked(&record.node)
                && self.add_peer(vault, uri).is_ok()
                && let Some(meta) = self.contact_meta.get_mut(&record.node)
            {
                meta.source = "enrollment".into();
            }
        }
    }
    fn revoked(&self, node: &PublicKey) -> bool {
        self.revocations.revokes(node)
    }
    pub(crate) fn released_key_roles(&self, after: &Self) -> Vec<String> {
        self.keys
            .iter()
            .filter(|(name, _)| !after.keys.contains_key(*name))
            .flat_map(|(name, epochs)| epochs.iter().map(|epoch| key_role(name, *epoch)))
            .collect()
    }
    fn release_unreadable_keys(&mut self) -> Result<(), Failure> {
        let Some(capability) = self.credentials.held() else {
            return Ok(());
        };
        let domain = self.domain()?;
        let (_, granted) = held(capability, &domain)?;
        let unreadable: Vec<String> = domain
            .scopes()
            .filter(|(_, scope)| !reads(&granted, *scope))
            .map(|(name, _)| name.to_owned())
            .collect();
        for name in unreadable {
            self.keys.remove(&name);
        }
        Ok(())
    }
    pub(crate) fn forget_revoked(&mut self) {
        self.contact_refs
            .retain(|id, _| !self.revocations.revokes_reference(id));
        let gone: Vec<PublicKey> = self
            .peers
            .keys()
            .chain(&self.issuers)
            .filter(|node| self.revoked(node))
            .copied()
            .collect();
        for node in gone {
            self.peers.remove(&node);
            self.contact_meta.remove(&node);
            self.issuers.retain(|issuer| *issuer != node);
        }
    }
    pub(crate) fn hold(
        &mut self,
        vault: &Vault,
        name: &str,
        epoch: rocky_core::KeyEpoch,
        key: &[u8; 32],
    ) -> Result<bool, Failure> {
        if !epoch.valid() || rocky_core::KeyEpoch::new(epoch.generation, key) != epoch {
            return Err("reader key epoch identity mismatch".into());
        }
        let epochs = self.keys.entry(name.to_owned()).or_default();
        if epochs.contains(&epoch) {
            if vault.get(&key_role(name, epoch))? != *key {
                return Err("reader key differs from retained epoch".into());
            }
            return Ok(false);
        }
        if epochs.len() >= MAX_EPOCHS {
            return Err("key epoch capacity reached".into());
        }
        vault.put(&key_role(name, epoch), key)?;
        epochs.push(epoch);
        epochs.sort_unstable();
        Ok(true)
    }
}

fn widen_to_current_generation(
    prior: &mut rocky_credentials::Capability,
    known: Option<&registry::IdentityRecord>,
    domain: &Domain,
) -> Result<(), Failure> {
    let mut extended = prior.extended()?;
    if extended.issues {
        return Ok(());
    }
    let partitions = match known.filter(|record| record.admitted.is_some()) {
        Some(record) => record.held(domain)?,
        None => domain.held_partitions(&domain.effective_partitions(&extended.partitions)),
    };
    let access = known
        .and_then(registry::IdentityRecord::access)
        .cloned()
        .or_else(|| domain.access_of(prior))
        .ok_or("the renewed identity holds no recorded access")?;
    let granted = domain.compile(&access, &partitions)?;
    prior.grants = granted.grants;
    extended.partitions = granted.partitions;
    extended.whole = granted.whole;
    extended.role = granted.role;
    prior.extension = rocky_credentials::Extension::new(extended)?;
    Ok(())
}

fn refuse_oversized_revocation(
    statement: &Revocation,
    links: &CredentialChain,
) -> Result<(), Failure> {
    if rocky_credentials::encode(statement)?.len()
        + rocky_credentials::encode(links)?.len()
        + rocky_credentials::SIGNATURE_OVERHEAD
        > rocky_credentials::MAX_REVOCATION_BYTES
    {
        return Err("the revocation names too many keys".into());
    }
    Ok(())
}

impl Settings {
    pub(crate) fn install_enrollment(
        schema: Option<&Schema>,
        (installation, vault): (String, &Vault),
        issuing: EnrollingIssuer,
        enrollment: Enrollment,
        provider: Arc<dyn IdentityProvider>,
    ) -> Result<Self, Failure> {
        enrollment.record.bytes()?;
        if enrollment.mesh != issuing.database.scope.mesh
            || enrollment.domain != issuing.database.scope.domain
        {
            return Err("enrollment belongs to another database".into());
        }
        match schema {
            Some(schema)
                if context(schema, issuing.database)
                    != context(&enrollment.record.schema, issuing.database) =>
            {
                return Err("application mismatch".into());
            }
            None if enrollment.access != Access::Store => {
                return Err(crate::StoreOnlyRefusal::ReadableAccess.into());
            }
            _ => {}
        }
        enrollment.revocations.verify_in(issuing.database)?;
        let identity = provider.public_key()?;
        let issuer = Contact {
            routes: issuing.routes,
        };
        let mut settings = Settings {
            version: VERSION,
            installation,
            schema: enrollment.record.schema.clone(),
            generation: enrollment.record.generation,
            engine: enrollment.record.engine,
            pending: None,
            bootstrap: Some(enrollment.bootstrap),
            mesh: enrollment.mesh,
            domain: enrollment.domain,
            publisher: issuing.publisher,
            identity,
            genesis: enrollment.genesis,
            revocations: RevocationSet::new(),
            credentials: CredentialChain(Vec::new()),
            ttl: enrollment.ttl,
            access: enrollment.access.clone(),
            keys: BTreeMap::new(),
            authority: None,
            device_revocations: BTreeMap::new(),
            issuers: vec![enrollment.issuer],
            delivery_nodes: BTreeMap::new(),
            contact_refs: BTreeMap::new(),
            removed_peers: Vec::new(),
            resolved_refs: BTreeSet::new(),
            contact_meta: BTreeMap::new(),
            peers: BTreeMap::from([(enrollment.issuer, issuer)]),
            logged_out: false,
            deliveries: BTreeMap::new(),
            groups: BTreeMap::new(),
            epoch_statements: BTreeMap::new(),
        };
        settings.verify_schema()?;
        let domain = settings.domain()?;
        settings.epoch_statements = enrollment.epoch_statements.clone();
        settings.learn_revocations(&enrollment.revocations)?;
        let expected = domain.compile(&settings.access, &enrollment.partitions)?;
        let grants = domain.expand(&expected);
        let signer = if enrollment.chain.0.is_empty() {
            settings.publisher
        } else {
            enrollment.chain.issuer(settings.publisher, None)?
        };
        let capability = &enrollment.capability.capability;
        let extended = capability.extended()?;
        let issued = rocky_credentials::Granted {
            grants: capability.grants.clone(),
            partitions: extended.partitions,
            whole: extended.whole,
            role: extended.role,
        };
        if enrollment.capability.issuer != signer
            || capability.subject != identity
            || extended.issues
            || issued != expected
        {
            return Err("the enrollment differs from its invitation".into());
        }
        let mut links = enrollment.chain.clone();
        links.0.push(enrollment.capability.clone());
        settings.credentials =
            crate::identities::with_identity(provider, || settings.certify(vault, links, &domain))?;
        for (name, scope) in domain.scopes() {
            if !reads(&grants, scope) {
                continue;
            }
            let epochs = enrollment.keys.get(name).ok_or("missing reader key")?;
            for (epoch, key) in epochs {
                settings.hold(vault, name, *epoch, &hex32(key)?)?;
            }
            if !settings.keys.contains_key(name) {
                return Err("missing reader key".into());
            }
        }
        settings.adopt_groups(vault, &enrollment.groups)?;
        settings.learn_issuers(vault, &enrollment.issuers);
        Ok(settings)
    }
}
impl Settings {
    pub(crate) fn answer_epochs(&self, vault: &Vault, bytes: &[u8]) -> Result<Vec<u8>, Failure> {
        if !self.issuer() {
            return Err("only an issuer delivers epochs".into());
        }
        let request = RenewRequest::from_bytes(bytes)?;
        request.chain.verify_database(self.database_context()?)?;
        if self.tombstoned(&request.node) {
            return Err("device was revoked".into());
        }
        let domain = self.domain()?;
        let prior = request.verify(
            PrivateKey::from_bytes(&vault.get("node")?).public_key(),
            self.publisher,
            (now()?, SKEW),
        )?;
        let administers = prior.extended()?.issues;
        if self.tombstoned(&prior.subject) {
            return Err("device was revoked".into());
        }
        let (prior, granted) = held(prior, &domain)?;
        let prior = &prior;
        let all = reads(&granted, domain.registry());
        let mut keys = Keys::new();
        for (name, scope) in domain.scopes() {
            if reads(&granted, scope) {
                let epochs = self.keys.get(name).ok_or("missing epoch collection")?;
                let generation = epochs
                    .iter()
                    .max()
                    .ok_or("empty epoch collection")?
                    .generation;
                for epoch in epochs
                    .iter()
                    .filter(|epoch| all || epoch.generation == generation)
                {
                    keys.entry(name.to_owned()).or_default().push((
                        *epoch,
                        HEXLOWER.encode(&vault.get(&key_role(name, *epoch))?),
                    ));
                }
            }
        }
        let sealed = Sealed {
            epoch_statements: self.statements_for(&keys),
            keys,
            groups: self.groups_for(vault, request.node, prior, all)?,
            issuers: self.issuer_contacts(&[request.node])?,
            naming: naming_for(vault, administers)?,
            trusted: None,
        };
        let node = Self::node_key(vault)?;
        Ok(Renewal::seal_current(
            &self.authority(&node)?,
            self.publisher,
            &request,
            self.revocations.clone(),
            now()?,
            &serde_json::to_vec(&sealed)?,
            &domain.whole_tables,
        )?
        .to_bytes()?)
    }
    pub(crate) fn epoch_sources(&self) -> BTreeSet<PublicKey> {
        self.issuers.iter().copied().collect()
    }
    pub(crate) async fn request_epochs(
        &self,
        vault: &Vault,
        transport: &dyn Transport,
        first: Option<PublicKey>,
    ) -> Result<Renewed, Failure> {
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let mut failure: Failure = ServiceError::no_route("an issuer", transport).into();
        let sources = self.epoch_sources();
        for issuer in sources
            .iter()
            .filter(|issuer| first.is_none_or(|first| **issuer == first))
        {
            let addresses = self
                .peers
                .get(issuer)
                .map(|contact| contact.addresses(transport))
                .unwrap_or_default();
            if addresses.is_empty() {
                continue;
            }
            let (request, secret) =
                RenewRequest::sign(&node, *issuer, self.credentials.clone(), now()?)?;
            let message = PeerRequest::Epochs(Box::new(EpochRequest {
                request: request.clone(),
                revocations: self.revocations.clone(),
            }));
            let bytes = match exchange(transport, &addresses, self.domain()?.scope, &message).await
            {
                Ok(PeerReply::Epochs(bytes)) => bytes,
                Ok(_) => return Err("epochs reply kind".into()),
                Err(error) => {
                    failure = error;
                    continue;
                }
            };
            let reply = Renewal::from_bytes(&bytes)?;
            let payload = reply.open(self.publisher, &request, &secret)?;
            if reply.capability.id()?
                != self
                    .identity_links()
                    .0
                    .last()
                    .ok_or("empty epoch request chain")?
                    .id()?
            {
                return Err("epoch delivery changed credentials".into());
            }
            let Sealed {
                keys: delivered,
                groups,
                epoch_statements,
                issuers,
                naming,
                ..
            } = Sealed::parse(&payload)?;
            let mut keys = Vec::new();
            let domain = self.domain()?;
            for (name, epochs) in delivered {
                let scope = domain
                    .scopes()
                    .find(|(collection, _)| *collection == name)
                    .map(|(_, scope)| scope)
                    .ok_or("unknown epoch collection")?;
                if !reads(&self.grants(&domain), scope) {
                    return Err("epoch delivery exceeds read grants".into());
                }
                for (epoch, key) in epochs {
                    if !self
                        .keys
                        .get(&name)
                        .is_some_and(|held| held.contains(&epoch))
                    {
                        keys.push((name.clone(), epoch, hex32(&key)?));
                    }
                }
            }
            return Ok(Renewed {
                contacted: Some(*issuer),
                revocations: reply.revocations,
                chain: self.credentials.clone(),
                keys,
                groups,
                epoch_statements,
                access: None,
                issuers,
                naming,
                admin_grant: None,
            });
        }
        Err(failure)
    }
    pub async fn request_renewal(
        &self,
        vault: &Vault,
        transport: Option<&dyn Transport>,
        learn: bool,
        first: Option<PublicKey>,
    ) -> Result<Renewed, Failure> {
        if self.issuer() {
            if (learn || self.admin_links().is_none())
                && let Some(transport) = transport
            {
                match self.renew_remotely(vault, transport, first).await {
                    Ok(renewed) => return Ok(renewed),
                    Err(error) if error.is::<Revoked>() => return Err(error),
                    Err(_) => {}
                }
            }
            return Ok(Renewed {
                contacted: None,
                revocations: self.revocations.clone(),
                chain: self.renew_locally(vault)?,
                keys: Vec::new(),
                epoch_statements: BTreeMap::new(),
                groups: Vec::new(),
                access: None,
                issuers: Vec::new(),
                naming: None,
                admin_grant: None,
            });
        }
        let transport = transport.ok_or(ServiceError::NetworkRequired("renew"))?;
        self.renew_remotely(vault, transport, first).await
    }
    async fn renew_remotely(
        &self,
        vault: &Vault,
        transport: &dyn Transport,
        first: Option<PublicKey>,
    ) -> Result<Renewed, Failure> {
        let mut failure: Failure = ServiceError::no_route("an issuer", transport).into();
        let mut issuers = self.issuers.clone();
        issuers.sort_by_key(|issuer| Some(*issuer) != first);
        for issuer in &issuers {
            let addresses = self
                .peers
                .get(issuer)
                .map(|contact| contact.addresses(transport))
                .unwrap_or_default();
            if addresses.is_empty() {
                continue;
            }
            match self.renew_with(vault, transport, *issuer, &addresses).await {
                Ok(renewed) => return Ok(renewed),
                Err(error) if error.is::<Revoked>() => return Err(error),
                Err(error) => failure = error,
            }
        }
        Err(failure)
    }
    async fn renew_with(
        &self,
        vault: &Vault,
        transport: &dyn Transport,
        issuer: PublicKey,
        addresses: &[String],
    ) -> Result<Renewed, Failure> {
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let (request, secret) =
            RenewRequest::sign(&node, issuer, self.credentials.clone(), now()?)?;
        let reply = match exchange(
            transport,
            addresses,
            self.domain()?.scope,
            &PeerRequest::Renew(Box::new(request.clone())),
        )
        .await
        {
            Ok(PeerReply::Renew(reply)) => reply,
            Ok(_) => return Err("renew reply kind".into()),
            Err(error) => {
                if let Ok(PeerReply::Policy(set)) = exchange(
                    transport,
                    addresses,
                    self.domain()?.scope,
                    &PeerRequest::Policy,
                )
                .await
                    && let Some(statement) = self.revoking(&set)?
                {
                    return Err(Box::new(Revoked(statement)));
                }
                return Err(error);
            }
        };
        let renewal = Renewal::from_bytes(&reply)?;
        let sealed = renewal.open(self.publisher, &request, &secret)?;
        let domain = self.domain()?;
        if !self.renews_identity(&renewal.capability.capability, &domain)? {
            return Err("renewal names another identity or other grants".into());
        }
        let mut next = self.clone();
        next.learn_revocations(&renewal.revocations)?;
        let chain = next.certify(vault, renewal.chain(), &domain)?;
        let Sealed {
            keys: received,
            groups,
            epoch_statements,
            issuers,
            naming,
            trusted,
        } = Sealed::parse(&sealed)?;
        let widened_grants = match trusted {
            Some(_) => domain.expand(&domain.administer()),
            None => domain.expand(&crate::access::granted_of(
                chain.held().ok_or("missing identity capability")?,
            )?),
        };
        let mut keys = Vec::new();
        for (name, scope) in domain.scopes() {
            if !reads(&widened_grants, scope) {
                continue;
            }
            let held = self.keys.get(name);
            for (epoch, key) in received.get(name).into_iter().flatten() {
                if epoch.valid() && !held.is_some_and(|held| held.contains(epoch)) {
                    keys.push((name.to_owned(), *epoch, hex32(key)?));
                }
            }
        }
        let access = domain.access_of(&renewal.capability.capability);
        Ok(Renewed {
            contacted: Some(issuer),
            revocations: renewal.revocations,
            chain,
            groups,
            epoch_statements,
            keys,
            access,
            issuers,
            naming,
            admin_grant: trusted,
        })
    }
    pub(crate) fn access_widens(&self, domain: &Domain) -> Result<bool, Failure> {
        let Some(held) = self.held(domain) else {
            return Ok(false);
        };
        let Ok(wanted) = domain.compile(&self.access, &domain.held_partitions(&held.partitions))
        else {
            return Ok(false);
        };
        let (held_grants, wanted_grants) = (domain.expand(&held), domain.expand(&wanted));
        Ok(
            self.credentials.partitions()?.len() != held.partitions.len()
                || wanted.whole != held.whole
                || wanted.role != held.role
                || domain.scopes().any(|(name, scope)| {
                    reads(&wanted_grants, scope) && !self.keys.contains_key(name)
                })
                || (held_grants
                    .iter()
                    .all(|grant| wanted_grants.iter().any(|wide| wide.includes(grant)))
                    && wanted_grants
                        .iter()
                        .any(|wide| !held_grants.iter().any(|grant| grant.includes(wide)))),
        )
    }
    fn renew_locally(&self, vault: &Vault) -> Result<CredentialChain, Failure> {
        self.issue_own(vault)
    }
    pub(crate) fn issue_own(&self, vault: &Vault) -> Result<CredentialChain, Failure> {
        self.certify(vault, self.identity_links(), &self.domain()?)
    }
    fn renews_identity(
        &self,
        renewed: &rocky_credentials::Capability,
        domain: &Domain,
    ) -> Result<bool, Failure> {
        let links = self.identity_links();
        let held = &links
            .0
            .last()
            .ok_or("missing identity credential")?
            .capability;
        Ok(renewed.subject == self.identity
            && domain.access_of(renewed).is_some()
            && renewed.extended()?.issues == held.extended()?.issues)
    }
    pub(crate) fn adopt_into(
        &self,
        vault: &Vault,
        renewed: &Renewed,
    ) -> Result<(Self, Vec<NewKey>, bool), Failure> {
        let mut draft = self.clone();
        let mut held = Vec::new();
        for (name, epoch, key) in &renewed.keys {
            if draft.hold(vault, name, *epoch, key)? {
                held.push((name.clone(), *epoch, *key));
            }
        }
        for (role, ids) in &renewed.epoch_statements {
            if ids.len() > MAX_LIVE_REVOCATIONS || ids.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err("invalid epoch revocation IDs".into());
            }
            if draft
                .epoch_statements
                .get(role)
                .is_some_and(|held| held != ids)
            {
                return Err("epoch revocation IDs differ from retained metadata".into());
            }
            draft.epoch_statements.insert(role.clone(), ids.clone());
        }
        draft.learn_revocations(&renewed.revocations)?;
        draft.credentials = renewed.chain.clone();
        draft.release_unreadable_keys()?;
        draft.adopt_groups(vault, &renewed.groups)?;
        draft.forget_revoked();
        draft.learn_issuers(vault, &renewed.issuers);
        if let Some(access) = &renewed.access {
            draft.access = access.clone();
        }
        if draft.issuer() && renewed.naming.is_some() {
            hold_naming(vault, renewed.naming.as_deref())?;
        }
        let changed = !draft.persisted_eq(self);
        Ok((draft, held, changed))
    }
    pub async fn adopt(
        &mut self,
        path: &Path,
        vault: &Vault,
        renewed: &Renewed,
    ) -> Result<(Vec<NewKey>, bool), Failure> {
        let (draft, held, changed) = self.adopt_into(vault, renewed)?;
        let released = self.released_key_roles(&draft);
        if changed {
            draft.save(path).await?;
        }
        *self = draft;
        vault.forget(released)?;
        Ok((held, changed))
    }
    pub fn renew(
        &self,
        vault: &Vault,
        request: &[u8],
        registry: &Registry,
    ) -> Result<(Vec<u8>, Changes), Failure> {
        if !self.issuer() {
            return Err("only an issuer renews devices".into());
        }
        let request = RenewRequest::from_bytes(request)?;
        let me = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        let domain = self.domain()?;
        let time = now()?;
        if self.tombstoned(&request.node) {
            return Err("device was revoked".into());
        }
        request.chain.verify_database(self.database_context()?)?;
        let mut prior = request.verify(me, self.publisher, (time, SKEW))?.clone();
        let known = registry.get(&prior.subject);
        if self.revoked_identity(&prior.subject, registry) {
            return Err("device was revoked".into());
        }
        widen_to_current_generation(&mut prior, known, &domain)?;
        let (held_by, granted) = held(&prior, &domain)?;
        let prior = &prior;
        if self.tombstoned(&prior.subject) {
            return Err("device was revoked".into());
        }
        if registry.full_for(&prior.subject) {
            return Err(Box::new(RegistryFull));
        }
        let node = Self::node_key(vault)?;
        let authority = self.authority(&node)?;
        let trusted = registry
            .admins
            .get(&prior.subject)
            .map(|record| record.grant.clone());
        let readable = match trusted {
            Some(_) => domain.expand(&domain.administer()),
            None => granted,
        };
        let keys = self.keys_for(vault, &domain, &readable)?;
        let sealed = Sealed {
            groups: self.groups_for(vault, request.node, &held_by, false)?,
            epoch_statements: self.statements_for(&keys),
            keys,
            issuers: self.issuer_contacts(&[request.node])?,
            naming: None,
            trusted,
        };
        let renewal = self.unrevoked_renewal(&authority, &request, prior, &sealed, time)?;
        let mut changes = Changes::default();
        changes.admitted(
            &prior.subject,
            &registry::Admitted {
                access: known
                    .and_then(registry::IdentityRecord::access)
                    .cloned()
                    .or_else(|| domain.access_of(prior))
                    .ok_or("the renewed identity holds no recorded access")?,
                invited_by: self.identity,
                origin: prior.extended()?.origin.map(|origin| origin.identity),
                joined: time,
                invitation: None,
            },
            false,
        )?;
        changes.capability(
            &prior.subject,
            registry::Capability {
                id: renewal.capability.id()?,
                expires: renewal.capability.capability.expires,
            },
            known,
        )?;
        Ok((renewal.to_bytes()?, changes))
    }
    fn unrevoked_renewal(
        &self,
        authority: &Authority<'_>,
        request: &RenewRequest,
        prior: &rocky_credentials::Capability,
        sealed: &Sealed,
        time: u64,
    ) -> Result<Renewal, Failure> {
        let payload = serde_json::to_vec(sealed)?;
        for issued_at in time..time.saturating_add(SKEW) {
            let renewal = Renewal::seal(
                authority,
                request,
                prior,
                self.revocations.clone(),
                rocky_credentials::Validity {
                    issued_at,
                    lifetime: std::time::Duration::from_secs(self.ttl),
                    clock_skew: std::time::Duration::from_secs(SKEW),
                },
                &payload,
            )?;
            if !self
                .revocations
                .revokes_capability(&renewal.capability.id()?)
            {
                return Ok(renewal);
            }
        }
        Err("every renewal within the clock skew names a revoked capability".into())
    }
    pub(crate) fn tombstoned(&self, key: &PublicKey) -> bool {
        self.revocations.revokes(key)
            || self.authority.as_ref().is_some_and(|authority| {
                authority
                    .tombstones
                    .contains(&rocky_credentials::PeerId::of(*key))
            })
    }
    pub(crate) fn revoked_identity(&self, identity: &PublicKey, registry: &Registry) -> bool {
        self.tombstoned(identity) || registry.revoked(identity)
    }
    pub(crate) fn revoked_node(&self, keys: &NodeKeys, registry: &Registry) -> bool {
        self.tombstoned(&keys.node) || self.revoked_identity(&keys.identity, registry)
    }
    #[cfg(test)]
    pub fn begin_revocation(
        &mut self,
        target: Revoke,
        (registry, membership): (&Registry, &Membership),
        root: Option<&dyn IdentityProvider>,
    ) -> Result<(), Failure> {
        self.begin_revocation_cut(target, (registry, membership), root, Vec::new())
    }
    fn reads_group(domain: &Domain, record: &Revoking, partition: &GroupPartition) -> bool {
        record.selects(partition)
            || partition
                .members
                .iter()
                .any(|member| record.reads(domain, &member.table))
    }
    pub(crate) fn exposure(
        &self,
        registry: &Registry,
        people: &BTreeSet<PublicKey>,
    ) -> Result<Exposure, Failure> {
        let domain = self.domain()?;
        let records: Vec<Revoking> = people
            .iter()
            .filter_map(|person| Revoking::of(registry, &domain, person))
            .collect();
        Ok(Exposure {
            keys: self
                .keys
                .keys()
                .filter(|name| records.iter().any(|record| record.reads(&domain, name)))
                .cloned()
                .collect(),
            groups: self
                .partitions(&domain)?
                .into_iter()
                .filter(|partition| {
                    records
                        .iter()
                        .any(|record| Self::reads_group(&domain, record, partition))
                })
                .map(|partition| crate::groups::record_key(&partition.record.address))
                .collect(),
        })
    }
    pub(crate) fn change_rights_after(&mut self, change: RightsChange) -> Result<(), Failure> {
        let issuing = self.issuing("changes rights")?;
        if issuing.rights_change.is_some() {
            return Err("an earlier rights change is unfinished".into());
        }
        issuing.rights_change = Some(change);
        Ok(())
    }
    pub(crate) fn begin_rights_change(
        &mut self,
        vault: &Vault,
        publication: Option<Commitment>,
        published: bool,
        (registry, membership): (&Registry, &Membership),
    ) -> Result<bool, Failure> {
        let issuing = self.issuing("changes rights")?;
        if issuing
            .rights_change
            .as_ref()
            .is_none_or(|change| change.publication != publication)
        {
            return Ok(false);
        }
        let Some(change) = issuing.rights_change.take() else {
            return Ok(false);
        };
        if !published {
            return Ok(false);
        }
        let people = change
            .people
            .into_iter()
            .filter(|person| !self.revoked_identity(person, registry));
        let Some(exposed) = change.exposed else {
            let nodes: Vec<PublicKey> = people
                .flat_map(|person| {
                    membership
                        .nodes(&person)
                        .map(|linked| linked.node)
                        .collect::<Vec<_>>()
                })
                .filter(|node| !self.tombstoned(node))
                .collect();
            self.queue_rights_notices(vault, &nodes)?;
            return Ok(false);
        };
        let people: BTreeSet<PublicKey> = people
            .filter(|person| {
                !registry.admins.contains_key(person)
                    && registry
                        .get(person)
                        .is_some_and(|record| !record.capabilities.is_empty())
            })
            .collect();
        if people.is_empty() {
            return Ok(false);
        }
        self.begin_revocation_cut(
            Revoke::Rights { people, exposed },
            (registry, membership),
            None,
            Vec::new(),
        )?;
        Ok(true)
    }
    pub fn begin_revocation_cut(
        &mut self,
        target: Revoke,
        (registry, membership): (&Registry, &Membership),
        root: Option<&dyn IdentityProvider>,
        cut: Vec<rocky_credentials::Cut>,
    ) -> Result<(), Failure> {
        let own = self.identity;
        let admin = match &target {
            Revoke::Identity(identity) | Revoke::Admin(identity, _) => {
                registry.admins.contains_key(identity)
            }
            Revoke::Node { owner, .. } => {
                if *owner == own || *owner == self.publisher || registry.admins.contains_key(owner)
                {
                    return Err("an admin's own devices are unlinked with its identity".into());
                }
                false
            }
            Revoke::Rights { people: owners, .. } => {
                if owners.is_empty() {
                    return Err("a rights change names no person".into());
                }
                if owners.iter().any(|owner| {
                    *owner == own || *owner == self.publisher || registry.admins.contains_key(owner)
                }) {
                    return Err("an admin's rights change only through the root".into());
                }
                false
            }
        };
        let root = if admin {
            Some(root_signer(root, self.publisher, "revoking an admin")?)
        } else {
            None
        };
        let mode = match &target {
            Revoke::Admin(_, mode) if admin => *mode,
            Revoke::Admin(_, _) => return Err("identity is not an admin".into()),
            Revoke::Identity(_) if admin => {
                return Err("revoking an admin requires a revocation mode".into());
            }
            _ => rocky_credentials::RevocationMode::Lost,
        };
        let identities: Vec<PublicKey> = match &target {
            &(Revoke::Identity(identity) | Revoke::Admin(identity, _)) => {
                if identity == own {
                    return Err("an issuer cannot revoke its own identity".into());
                }
                if identity == self.publisher {
                    return Err("the root is never revoked".into());
                }
                if self.tombstoned(&identity)
                    && !(admin
                        && mode == rocky_credentials::RevocationMode::Compromised
                        && !self.revocations.revokes_origin(&identity))
                {
                    return Err("identity is already revoked".into());
                }
                let mut identities = BTreeSet::from([identity]);
                if admin && mode == rocky_credentials::RevocationMode::Compromised {
                    identities.extend(registry.enrolled_by(identity));
                }
                identities.into_iter().collect()
            }
            Revoke::Node { node, .. } => {
                if self.tombstoned(node) {
                    return Err("the device is already unlinked".into());
                }
                Vec::new()
            }
            Revoke::Rights { people: owners, .. } => {
                if owners
                    .iter()
                    .any(|owner| self.revoked_identity(owner, registry))
                {
                    return Err("the person was removed".into());
                }
                Vec::new()
            }
        };
        let nodes: Vec<PublicKey> = match &target {
            Revoke::Node { node, .. } => vec![*node],
            Revoke::Rights { .. } => Vec::new(),
            _ => identities
                .iter()
                .flat_map(|identity| membership.nodes(identity))
                .map(|linked| linked.node)
                .collect(),
        };
        if self.issuing("revokes devices")?.rotation.is_some() {
            return Err("another revocation is unfinished".into());
        }
        let time = now()?;
        if self.revocations.live(time) >= MAX_LIVE_REVOCATIONS {
            return Err(Box::new(RevocationSetFull));
        }
        let domain = self.domain()?;
        let records: Vec<Revoking> = match &target {
            Revoke::Node { owner, .. } => {
                Revoking::of(registry, &domain, owner).into_iter().collect()
            }
            Revoke::Rights { people: owners, .. } => owners
                .iter()
                .filter_map(|owner| Revoking::of(registry, &domain, owner))
                .collect(),
            _ => identities
                .iter()
                .filter_map(|identity| Revoking::of(registry, &domain, identity))
                .collect(),
        };
        let issuer = if root.is_some() { self.publisher } else { own };
        let statement = match &target {
            &(Revoke::Identity(identity) | Revoke::Admin(identity, _)) => Revocation {
                app: domain.manifest().app,
                issuer,
                time,
                mode,
                nodes: BTreeSet::new(),
                identities: [identity].into(),
                capabilities: records
                    .iter()
                    .filter_map(|record| record.capability)
                    .chain(
                        registry
                            .admins
                            .get(&identity)
                            .and_then(|record| record.grant.id().ok()),
                    )
                    .collect(),
                cut: if root.is_some() { cut } else { Vec::new() },
            },
            Revoke::Node { node, .. } => Revocation {
                app: domain.manifest().app,
                issuer,
                time,
                mode,
                nodes: [rocky_credentials::PeerId::of(*node)].into(),
                identities: BTreeSet::new(),
                capabilities: BTreeSet::new(),
                cut: Vec::new(),
            },
            Revoke::Rights { people: owners, .. } => Revocation {
                app: domain.manifest().app,
                issuer,
                time,
                mode,
                nodes: BTreeSet::new(),
                identities: BTreeSet::new(),
                capabilities: owners
                    .iter()
                    .filter_map(|owner| registry.get(owner))
                    .flat_map(|record| record.capabilities.iter().map(|capability| capability.id))
                    .collect(),
                cut: Vec::new(),
            },
        };
        let exposed = match &target {
            Revoke::Rights { exposed, .. } => exposed.clone(),
            _ => Exposure::default(),
        };
        if statement.capabilities.is_empty() && matches!(target, Revoke::Rights { .. }) {
            return Err("the person holds no recorded capability".into());
        }
        let bounded_by_records = !records.is_empty();
        let mut keys = BTreeMap::new();
        for (name, epochs) in &self.keys {
            if bounded_by_records
                && !exposed.keys.contains(name)
                && !records.iter().any(|record| record.reads(&domain, name))
            {
                continue;
            }
            if epochs.len() >= MAX_EPOCHS {
                return Err("key epoch capacity reached".into());
            }
            keys.insert(
                name.clone(),
                RotationEpoch::after(epochs.iter().max().copied()).ok_or("key epoch overflow")?,
            );
        }
        let groups = self.next_group_epochs(&domain, &records, &exposed)?;
        let links = match root {
            Some(_) => CredentialChain::default(),
            None => self.issuer_links(),
        };
        refuse_oversized_revocation(&statement, &links)?;
        let database = self.database_context()?;
        let signed = root
            .map(|root| Authority::owner(root).revoke_in(statement.clone(), database))
            .transpose()?;
        self.issuing("revokes devices")?.rotation = Some(Rotation {
            signed,
            identities,
            nodes,
            statements: {
                let mut ids = self
                    .revocations
                    .statements()
                    .map(|signed| signed.statement.id())
                    .collect::<Result<Vec<_>, _>>()?;
                ids.push(statement.id()?);
                ids.sort_unstable();
                ids.dedup();
                ids
            },
            statement,
            keys,
            groups,
        });
        Ok(())
    }
    fn next_group_epochs(
        &self,
        domain: &Domain,
        records: &[Revoking],
        exposed: &Exposure,
    ) -> Result<BTreeMap<String, RotationEpoch>, Failure> {
        let bounded_by_records = !records.is_empty();
        let mut groups = BTreeMap::new();
        for partition in self.partitions(domain)? {
            let read = !bounded_by_records
                || exposed
                    .groups
                    .contains(&crate::groups::record_key(&partition.record.address))
                || records
                    .iter()
                    .any(|record| Self::reads_group(domain, record, &partition));
            if let (true, Some(held)) = (read, partition.record.current()) {
                let target =
                    RotationEpoch::after(Some(held)).ok_or("group secret epoch overflow")?;
                if partition.record.epochs.len() >= crate::groups::MAX_GROUP_EPOCHS {
                    return Err("group secret epoch capacity reached".into());
                }
                groups.insert(crate::groups::record_key(&partition.record.address), target);
            }
        }
        Ok(groups)
    }
    pub(crate) fn begin_exposure_rotation(&mut self, registry: &Registry) -> Result<bool, Failure> {
        if !self.issuer()
            || self
                .authority
                .as_ref()
                .is_some_and(|a| a.rotation.is_some())
        {
            return Ok(false);
        }
        let domain = self.domain()?;
        let mut keys = BTreeMap::new();
        let mut groups = BTreeMap::new();
        let mut trigger = None;
        for signed in self.revocations.statements() {
            let id = signed.statement.id()?;
            let read_bounds: Vec<ReadBound> = signed
                .statement
                .identities
                .iter()
                .map(|identity| ReadBound::of(registry, &domain, Some(identity)))
                .chain(signed.statement.nodes.iter().map(|node| {
                    ReadBound::of(
                        registry,
                        &domain,
                        self.revocations
                            .certificates()
                            .find(|certificate| {
                                rocky_credentials::PeerId::of(certificate.capability.subject)
                                    == *node
                            })
                            .map(|certificate| &certificate.issuer),
                    )
                }))
                .collect();
            for (name, epochs) in &self.keys {
                let Some(current) = epochs.iter().max() else {
                    continue;
                };
                let covered = self
                    .epoch_statements
                    .get(&key_role(name, *current))
                    .is_some_and(|ids| ids.contains(&id));
                let read = read_bounds
                    .iter()
                    .any(|bound| bound.reads(|record| record.reads(&domain, name)));
                if !covered && read {
                    if epochs.len() >= MAX_EPOCHS {
                        return Err("key epoch capacity reached".into());
                    }
                    keys.insert(
                        name.clone(),
                        RotationEpoch::after(Some(*current)).ok_or("key epoch overflow")?,
                    );
                    trigger = Some(signed.statement.clone());
                }
            }
            for partition in self.partitions(&domain)? {
                let Some(current) = partition.record.current() else {
                    continue;
                };
                let covered = self
                    .epoch_statements
                    .get(&secret_role(&partition.record.address, current))
                    .is_some_and(|ids| ids.contains(&id));
                let read = read_bounds.iter().any(|bound| {
                    bound.reads(|record| {
                        partition
                            .members
                            .iter()
                            .any(|member| record.reads(&domain, &member.table))
                            || record.selects(&partition)
                    })
                });
                if !covered && read {
                    if partition.record.epochs.len() >= crate::groups::MAX_GROUP_EPOCHS {
                        return Err("group secret epoch capacity reached".into());
                    }
                    groups.insert(
                        crate::groups::record_key(&partition.record.address),
                        RotationEpoch::after(Some(current)).ok_or("group epoch overflow")?,
                    );
                    trigger = Some(signed.statement.clone());
                }
            }
        }
        let Some(statement) = trigger else {
            return Ok(false);
        };
        if statement.identities.is_empty() && statement.nodes.is_empty() {
            return Err("rotation statement names no identity".into());
        }
        let mut statements = self
            .revocations
            .statements()
            .map(|signed| signed.statement.id())
            .collect::<Result<Vec<_>, _>>()?;
        statements.sort_unstable();
        self.issuing("rotates exposed scopes")?.rotation = Some(Rotation {
            identities: statement
                .identities
                .iter()
                .filter(|identity| registry.admitted(identity))
                .copied()
                .collect(),
            nodes: Vec::new(),
            statement,
            signed: None,
            statements,
            keys,
            groups,
        });
        Ok(true)
    }
    pub fn rotate(&mut self, vault: &Vault) -> Result<Option<Vec<NewKey>>, Failure> {
        let Some(Rotation {
            nodes,
            identities: _,
            statement,
            signed,
            statements: ids,
            keys: targets,
            groups,
        }) = self
            .authority
            .as_ref()
            .and_then(|authority| authority.rotation.clone())
        else {
            return Ok(None);
        };
        let node = Self::node_key(vault)?;
        let credentials = self.credentials.clone();
        let links = credentials.capabilities();
        let signer = rocky_credentials::Signer::node(&node, &credentials)?;
        crate::mesh::mint_value(
            vault,
            (signer, &links),
            (self.publisher, self.domain()?.scope),
            &self.revocations,
            &statement,
        )?;
        fault::check(fault::Stage::Begun)?;
        let mut keys = Vec::new();
        for (name, epoch) in targets {
            let role = format!("rotation.reader.{name}.{epoch}");
            let key = match vault.find(&role)? {
                Some(key) => key,
                None => vault.create(&role)?,
            };
            let epoch = rocky_core::KeyEpoch::new(epoch.generation(), &key);
            vault.put(&key_role(&name, epoch), &key)?;
            self.epoch_statements
                .insert(key_role(&name, epoch), ids.clone());
            let epochs = self.keys.entry(name.clone()).or_default();
            if !epochs.contains(&epoch) {
                epochs.push(epoch);
                epochs.sort_unstable();
            }
            keys.push((name, epoch, key));
            fault::check(fault::Stage::Key)?;
        }
        for (address, epoch) in groups {
            let mut record = self
                .groups
                .get(&address)
                .cloned()
                .ok_or("a rotated group partition is unknown")?;
            let role = format!(
                "rotation.partition.{}.{epoch}",
                crate::groups::record_key(&record.address)
            );
            let secret = match vault.find(&role)? {
                Some(secret) => secret,
                None => vault.create(&role)?,
            };
            let epoch = rocky_core::KeyEpoch::new(epoch.generation(), &secret);
            vault.put(&secret_role(&record.address, epoch), &secret)?;
            self.epoch_statements
                .insert(secret_role(&record.address, epoch), ids.clone());
            record.hold(epoch)?;
            self.groups.insert(address, record);
            fault::check(fault::Stage::Key)?;
        }
        let signed = match (self.revocations.get(&statement.id()?), signed) {
            (Some(held), _) => held.clone(),
            (None, Some(signed)) => signed,
            (None, None) => self.authority(&node)?.revoke(statement)?,
        };
        self.learn_revocations(&RevocationSet::try_from(vec![signed])?)?;
        fault::check(fault::Stage::Signed)?;
        self.credentials = self.renew_locally(vault)?;
        fault::check(fault::Stage::Renewed)?;
        for node in nodes {
            self.forget_peer(node);
        }
        Ok(Some(keys))
    }
    pub(crate) fn complete_revocation(
        &mut self,
        vault: &Vault,
        domain: &Domain,
        (registry, membership): (&Registry, &Membership),
    ) -> Result<(), Failure> {
        let Some(identities) = self
            .issuing("revokes devices")?
            .rotation
            .as_ref()
            .map(|rotation| rotation.identities.clone())
        else {
            return Ok(());
        };
        if identities
            .iter()
            .any(|identity| !registry.revoked(identity))
        {
            return Err("revocation tombstone is not committed".into());
        }
        self.queue_rotation(vault, domain, (registry, membership))?;
        fault::check(fault::Stage::Clearing)?;
        if let Some(rotation) = self.issuing("revokes devices")?.rotation.take() {
            for receipt in self.device_revocations.values_mut() {
                if rotation.statements.contains(&receipt.statement) {
                    receipt.local = true;
                }
            }
        }
        Ok(())
    }
}

fn held(
    capability: &rocky_credentials::Capability,
    domain: &Domain,
) -> Result<(rocky_credentials::Capability, Vec<ResourceGrant>), Failure> {
    let extended = capability.extended()?;
    if !extended.issues {
        let granted = rocky_credentials::Granted {
            grants: capability.grants.clone(),
            partitions: extended.partitions,
            whole: extended.whole,
            role: extended.role,
        };
        return Ok((capability.clone(), domain.expand(&granted)));
    }
    let granted = domain.administer();
    let grants = domain.expand(&granted);
    let mut held = capability.clone();
    held.grants = granted.grants;
    held.extension = rocky_credentials::Extension::new(rocky_credentials::Extended {
        whole: granted.whole,
        ..rocky_credentials::Extended::default()
    })?;
    Ok((held, grants))
}

pub type NewKey = (String, rocky_core::KeyEpoch, [u8; 32]);

pub(crate) mod fault {
    use crate::Failure;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Stage {
        Begun,
        Key,
        Table,
        Signed,
        Renewed,
        Tombstone,
        Committed,
        Clearing,
    }
    #[cfg(test)]
    thread_local! {
        pub static STOP: std::cell::Cell<Option<(Stage, usize)>> = const { std::cell::Cell::new(None) };
    }
    #[cfg(test)]
    pub fn check(stage: Stage) -> Result<(), Failure> {
        STOP.with(|stop| match stop.get() {
            Some((at, 0)) if at == stage => {
                stop.set(None);
                Err(format!("injected stop at {stage:?}").into())
            }
            Some((at, passes)) if at == stage => {
                stop.set(Some((at, passes - 1)));
                Ok(())
            }
            _ => Ok(()),
        })
    }
    #[cfg(not(test))]
    pub fn check(_: Stage) -> Result<(), Failure> {
        Ok(())
    }
}

pub(crate) async fn exchange(
    transport: &dyn Transport,
    addresses: &[String],
    scope: rocky_core::Scope,
    request: &PeerRequest,
) -> Result<PeerReply, Failure> {
    let bytes = request.to_bytes()?;
    let reply = transport
        .request(addresses, scope, &bytes)
        .await
        .map_err(ServiceError::dial_failed)?;
    let reply = ReplyFrame::from_bytes(&reply)?;
    let reply = reply
        .result
        .map_err(|error| -> Failure { Box::new(error) })?;
    if !request.accepts(&reply) {
        return Err("peer RPC reply kind mismatch".into());
    }
    Ok(reply)
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredContact {
    pub(crate) contact: Contact,
    meta: Option<crate::mesh::ContactMeta>,
    issuer: bool,
}
impl Settings {
    pub(crate) fn contact_count(&self) -> usize {
        let mut ids: BTreeSet<_> = self
            .contact_refs
            .keys()
            .filter(|id| !self.resolved_refs.contains(id))
            .copied()
            .collect();
        ids.extend(
            self.peers
                .keys()
                .map(|key| rocky_credentials::PeerId::of(*key)),
        );
        ids.len()
    }
    fn with_saved_contacts(&self) -> Self {
        let mut saved = self.clone();
        saved.contact_refs = self.saved_contacts();
        saved
    }
    fn saved_contacts(&self) -> BTreeMap<rocky_credentials::PeerId, StoredContact> {
        let mut saved = self.contact_refs.clone();
        saved.retain(|id, _| !self.resolved_refs.contains(id));
        for (node, contact) in &self.peers {
            saved.insert(
                rocky_credentials::PeerId::of(*node),
                StoredContact {
                    contact: contact.clone(),
                    meta: self.contact_meta.get(node).cloned(),
                    issuer: self.issuers.contains(node),
                },
            );
        }
        saved
    }
    pub(crate) fn persisted_eq(&self, other: &Self) -> bool {
        let Self {
            delivery_nodes: _,
            contact_refs: _,
            removed_peers,
            resolved_refs: _,
            version,
            installation,
            schema,
            mesh,
            domain,
            publisher,
            identity,
            genesis,
            generation,
            engine,
            pending,
            bootstrap,
            revocations,
            credentials,
            ttl,
            access,
            keys,
            groups,
            epoch_statements,
            authority,
            device_revocations,
            issuers: _,
            peers: _,
            contact_meta: _,
            logged_out,
            deliveries,
        } = self;
        *version == other.version
            && *installation == other.installation
            && *schema == other.schema
            && *mesh == other.mesh
            && *domain == other.domain
            && *publisher == other.publisher
            && *identity == other.identity
            && *genesis == other.genesis
            && *generation == other.generation
            && *engine == other.engine
            && *pending == other.pending
            && *bootstrap == other.bootstrap
            && *revocations == other.revocations
            && *credentials == other.credentials
            && *ttl == other.ttl
            && *access == other.access
            && *keys == other.keys
            && *groups == other.groups
            && *epoch_statements == other.epoch_statements
            && *authority == other.authority
            && *device_revocations == other.device_revocations
            && *logged_out == other.logged_out
            && *deliveries == other.deliveries
            && *removed_peers == other.removed_peers
            && self.saved_contacts() == other.saved_contacts()
    }
    fn validate_saved_contacts(&self) -> Result<(), Failure> {
        if self.contact_refs.len() > MAX_CONTACTS
            || self
                .contact_refs
                .values()
                .filter(|entry| entry.issuer)
                .count()
                > MAX_ISSUERS
        {
            return Err("identity holds too many contacts".into());
        }
        for entry in self.contact_refs.values() {
            Contact::checked(entry.contact.routes.clone())?;
            if entry.meta.as_ref().is_some_and(|meta| {
                !matches!(
                    meta.source.as_str(),
                    "application" | "enrollment" | "handover" | "request" | "discovery"
                )
            }) {
                return Err("invalid contact metadata".into());
            }
        }
        Ok(())
    }
    pub(crate) fn bind_saved_contact(&mut self, node: PublicKey) {
        let id = rocky_credentials::PeerId::of(node);
        if self.resolved_refs.insert(id)
            && let Some(entry) = self.contact_refs.get(&id)
        {
            self.peers.insert(node, entry.contact.clone());
            if let Some(meta) = &entry.meta {
                self.contact_meta.insert(node, meta.clone());
            }
            if entry.issuer && !self.issuers.contains(&node) {
                self.issuers.push(node);
            }
        }
    }
    pub(crate) async fn resolve_saved_route(
        &self,
        transport: &dyn Transport,
        id: rocky_credentials::PeerId,
    ) -> Result<Option<PublicKey>, Failure> {
        let Some(entry) = self.contact_refs.get(&id) else {
            return Ok(None);
        };
        let addresses = entry.contact.addresses(transport);
        if addresses.is_empty() {
            return Ok(None);
        }
        let request = rocky_credentials::PeerResolutionRequest::fresh();
        let PeerReply::Resolve(reply) = exchange(
            transport,
            &addresses,
            self.domain()?.scope,
            &PeerRequest::Resolve(request.clone()),
        )
        .await?
        else {
            return Err("resolution reply kind".into());
        };
        Ok(Some(reply.verify(&request, id)?))
    }
    pub(crate) fn unresolved_contacts(&self) -> Vec<(rocky_credentials::PeerId, Contact)> {
        self.contact_refs
            .iter()
            .filter(|(id, _)| !self.resolved_refs.contains(id))
            .map(|(id, entry)| (*id, entry.contact.clone()))
            .collect()
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests;
