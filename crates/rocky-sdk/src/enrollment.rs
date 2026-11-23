use crate::{
    Failure,
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
    Authority, Credentials, EpochRequest, IdentityKey, IdentityProvider, MAX_LIVE_REVOCATIONS,
    Profile, REVOCATION_SET_FULL, RenewRequest, Renewal, Revocation,
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

pub const VERSION: u32 = 39;
pub(crate) const REPLICA_FILE: &str = "rocky.sqlite";
pub(crate) const AUTHORITY_IDENTITY_KEY: &str = "identity";
pub(crate) const AUTHORITY_REVOKED_KEY: &str = "revoked";
pub(crate) const AUTHORITY_BOOTSTRAP_KEY: &str = "bootstrap";
const PROVISION_VERSION: u32 = 1;
const CONTACT_VERSION: u32 = 3;
pub const MAX_CONTACTS: usize = 64;
pub const MAX_DIALED: usize = 16;
const MAX_ISSUERS: usize = crate::registry::MAX_TRUSTED;
const MAX_ROUTES: usize = 4;
const MAX_ADDRESS: usize = 2048;
pub const DEFAULT_TTL: u64 = 30 * 24 * 60 * 60;
pub(crate) const MAX_EPOCHS: usize = 256;
pub(crate) const SKEW: u64 = 300;
type GroupDelivery = (GroupRecord, Vec<Vec<u8>>);
pub(crate) fn naming_for(vault: &Vault, profile: &Profile) -> Result<Option<String>, Failure> {
    if !profile.issues() {
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
        f.write_str("this device has been revoked")
    }
}
impl std::error::Error for Revoked {}

#[derive(Debug)]
pub struct PublisherRequired(pub &'static str);
impl std::fmt::Display for PublisherRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "publisher_required: {} needs the domain's root key",
            self.0
        )
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
        Ok(true)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(skip)]
    pub(crate) delivery_nodes: BTreeMap<rocky_credentials::PeerId, PublicKey>,
    pub(crate) contact_refs: BTreeMap<rocky_credentials::PeerId, StoredContact>,
    #[serde(skip)]
    pub(crate) resolved_refs: BTreeSet<rocky_credentials::PeerId>,
    pub version: u32,
    pub installation: String,
    pub schema: Schema,
    pub mesh: MeshId,
    pub domain: DomainId,
    pub publisher: PublicKey,
    pub identity: PublicKey,
    pub single_use: bool,
    pub genesis: String,
    pub generation: u32,
    pub engine: rocky_core::EngineIdentity,
    pub pending: Option<crate::tip_schemas::Pending>,
    pub pending_follow: Option<String>,
    pub(crate) bootstrap: Option<crate::tip_schemas::Bootstrap>,
    pub revocations: RevocationSet,
    pub credentials: CredentialChain,
    pub ttl: u64,
    pub profile: Profile,
    pub keys: BTreeMap<String, Vec<rocky_core::KeyEpoch>>,
    pub groups: BTreeMap<String, GroupRecord>,
    pub epoch_statements: BTreeMap<String, Vec<Commitment>>,
    pub authority: Option<Issuing>,
    pub device_revocations: BTreeMap<rocky_credentials::PeerId, crate::DeviceRevocationReceipt>,
    #[serde(skip)]
    pub issuers: Vec<PublicKey>,
    #[serde(skip)]
    pub enrollment_upstream: Option<PublicKey>,
    #[serde(skip)]
    pub peers: BTreeMap<PublicKey, Contact>,
    #[serde(skip)]
    pub contact_meta: BTreeMap<PublicKey, crate::mesh::ContactMeta>,
    #[serde(skip)]
    pub enrollment_routes: BTreeMap<PublicKey, Contact>,
    pub logged_out: bool,
    pub deliveries: BTreeMap<String, crate::delivery::Pending>,
    pub claims: Option<crate::claims::Holding>,
    pub revocations_pushed: Option<Commitment>,
}
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issuing {
    pub rotation: Option<Rotation>,
    pub tombstones: BTreeSet<rocky_credentials::PeerId>,
    pub heard: BTreeMap<PublicKey, u64>,
    pub trusted_nodes:
        BTreeMap<PublicKey, BTreeMap<PublicKey, crate::membership::TrustedObservation>>,
    pub stale_after: Option<u64>,
    pub seen: BTreeMap<PublicKey, crate::membership::Seen>,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Revoke {
    Identity(PublicKey),
    TrustedIdentity(PublicKey, rocky_credentials::RevocationMode),
    Claims(Commitment),
}
enum ReadBound {
    Unbounded,
    Recorded(Revoking),
}
impl ReadBound {
    fn of(record: Option<&crate::registry::IdentityRecord>) -> Self {
        record
            .filter(|record| record.admitted.is_some() || !record.grants.is_empty())
            .map_or(Self::Unbounded, |record| {
                Self::Recorded(Revoking::of(record))
            })
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
    profile: Option<Profile>,
    partitions: Vec<rocky_credentials::PartitionGrant>,
}
impl Revoking {
    fn of(record: &registry::IdentityRecord) -> Self {
        Self {
            capability: record.capability().map(|capability| capability.id),
            profile: record.profile().cloned(),
            partitions: record
                .grants
                .iter()
                .map(|grant| grant.grant.clone())
                .collect(),
        }
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub transport: String,
    pub address: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub routes: Vec<Route>,
}
impl Contact {
    pub(crate) fn checked(routes: Vec<Route>) -> Result<Self, Failure> {
        let valid = |route: &Route| {
            (1..=32).contains(&route.transport.len())
                && (1..=MAX_ADDRESS).contains(&route.address.len())
        };
        if routes.len() > MAX_ROUTES || !routes.iter().all(valid) {
            return Err("invalid contact routes".into());
        }
        let mut contact = Self::default();
        contact.learn(routes);
        Ok(contact)
    }
    pub(crate) fn learn(&mut self, routes: Vec<Route>) {
        for route in routes {
            self.routes
                .retain(|known| known.transport != route.transport);
            self.routes.push(route);
        }
        let excess = self.routes.len().saturating_sub(MAX_ROUTES);
        self.routes.drain(..excess);
    }
    pub fn addresses(&self, kind: &str) -> Vec<String> {
        self.routes
            .iter()
            .filter(|route| route.transport == kind)
            .map(|route| route.address.clone())
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
    enrollment_only: bool,
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
        if record.enrollment_only && !record.issuer {
            return Err("invalid enrollment-only contact".into());
        }
        Ok(record)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProvisioningBundle {
    v: u32,
    routes: Vec<Route>,
    publisher: PublicKey,
    database: rocky_credentials::DatabaseContext,
    via_custody: bool,
    node_key: String,
    identity_key: String,
    enrollment: Box<Enrollment>,
}
pub(crate) struct EnrollingIssuer {
    pub(crate) publisher: PublicKey,
    pub(crate) database: rocky_credentials::DatabaseContext,
    pub(crate) routes: Vec<Route>,
    pub(crate) via_custody: bool,
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
    profile: Profile,
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
    claims: Option<CredentialChain>,
}
impl Sealed {
    fn parse(bytes: &[u8]) -> Result<Self, Failure> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

impl ProvisioningBundle {
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn profile(&self) -> &Profile {
        &self.enrollment.profile
    }
    pub(crate) fn parse(uri: &str) -> Result<Self, Failure> {
        let bytes = BASE64URL_NOPAD.decode(
            uri.trim()
                .strip_prefix("rocky://provision/")
                .ok_or("invalid provisioning URI")?
                .as_bytes(),
        )?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        if value["v"].as_u64() != Some(u64::from(PROVISION_VERSION)) {
            return Err("unsupported provisioning version".into());
        }
        let bundle: Self = serde_json::from_value(value)?;
        if bundle.database.publisher != bundle.publisher {
            return Err("provisioning database publisher mismatch".into());
        }
        Contact::checked(bundle.routes.clone())?;
        Ok(bundle)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Intent {
    invitation: Option<Commitment>,
    pub(crate) installation: String,
    nonce: String,
    pub(crate) single_use: bool,
    identity: PublicKey,
}
impl Intent {
    pub(crate) fn path(identity: &Path, supplied: bool) -> std::path::PathBuf {
        identity
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(if supplied {
                "identity.install.json"
            } else {
                "identity.join.json"
            })
    }
    pub(crate) fn resume(
        path: &Path,
        invitation: Option<Commitment>,
        store: &std::sync::Arc<dyn crate::SecretStore>,
        provider: Option<&Arc<dyn IdentityProvider>>,
        single_use: bool,
    ) -> Result<(Self, Vault), Failure> {
        let identity = provider
            .ok_or("supply an identity provider to prepare an installation")?
            .public_key()?;
        if rocky_platform::fs::exists(path) {
            let previous: Self = serde_json::from_slice(&rocky_platform::fs::read(path)?)?;
            let vault = Vault::new(store.clone(), &previous.installation);
            if previous.invitation == invitation {
                if previous.identity != identity {
                    return Err("unfinished enrollment needs its original identity; resume with the pending admission or the supplied provider".into());
                }
                return Ok((previous, vault));
            }
            rocky_platform::fs::remove_file(path)?;
            vault.forget(["node", "transport"].into_iter().map(String::from))?;
        }
        let intent = Self {
            invitation,
            installation: HEXLOWER.encode(&random::<16>()),
            nonce: HEXLOWER.encode(&random::<32>()),
            single_use,
            identity,
        };
        let vault = Vault::new(store.clone(), &intent.installation);
        for role in ["node", "transport"] {
            vault.create(role)?;
        }
        write_private(path, &serde_json::to_vec(&intent)?)?;
        Ok((intent, vault))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeKeys {
    pub identity: PublicKey,
    pub node: PublicKey,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Supplied {
    pub mesh: MeshId,
    pub domain: DomainId,
    pub manifest: SignedManifest,
    pub credentials: Credentials,
    pub keys: Vec<(String, rocky_core::KeyEpoch, [u8; 32])>,
    pub epoch_statements: BTreeMap<String, Vec<Commitment>>,
    pub issuers: Vec<String>,
}
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Terms {
    pub ttl: Option<u64>,
    pub delegations: Option<u8>,
}
impl Terms {
    fn ttl(&self, default: u64) -> Result<u64, Failure> {
        let ttl = self.ttl.unwrap_or(default);
        if !(60..=366 * 24 * 60 * 60).contains(&ttl) {
            return Err("credential lifetime must be between a minute and a year".into());
        }
        Ok(ttl)
    }
}
pub struct Renewed {
    pub contacted: Option<PublicKey>,
    pub revocations: RevocationSet,
    pub chain: CredentialChain,
    pub keys: Vec<(String, rocky_core::KeyEpoch, [u8; 32])>,
    pub epoch_statements: BTreeMap<String, Vec<Commitment>>,
    pub groups: Vec<GroupDelivery>,
    pub profile: Option<Profile>,
    pub issuers: Vec<String>,
    pub claims: Option<CredentialChain>,
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
fn profile_of(grants: &[ResourceGrant], domain: &Domain) -> Profile {
    let registry = domain.registry();
    if grants
        .iter()
        .any(|grant| grant.scope == registry && grant.rights.contains(&Right::Insert))
    {
        return Profile::Issuer;
    }
    let named: Vec<ResourceGrant> = grants
        .iter()
        .filter(|grant| grant.scope != registry)
        .cloned()
        .collect();
    Profile::of(&named, &domain.resources())
}
pub(crate) fn grants_of(profile: &Profile, domain: &Domain) -> Result<Vec<ResourceGrant>, Failure> {
    let mut grants = domain.effective(profile).grants(&domain.resources())?;
    let registry = domain.registry_resource();
    match profile {
        Profile::Issuer => grants.extend(Profile::Issuer.grants(&registry)?),
        Profile::Custody { .. } => grants.extend(
            Profile::Custody {
                scopes: [crate::domain::REGISTRY.to_owned()].into(),
            }
            .grants(&registry)?,
        ),
        _ => {}
    }
    Ok(grants)
}
fn profile_grants(profile: &Profile, domain: &Domain) -> Result<Vec<ResourceGrant>, Failure> {
    if matches!(profile, Profile::Grants { grants } if grants.is_empty()) {
        return Ok(Vec::new());
    }
    grants_of(profile, domain)
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
        Ok(set.revoking_holder(&self.credentials)?.cloned())
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
        } else if let Some(recovery) = revoked.grants().filter(|_| statement.root()) {
            crate::RevocationReason::Recovery {
                recovered: recovery.keep.to_hex(),
            }
        } else if revoked.reset().is_some() && statement.root() {
            crate::RevocationReason::Reset {
                reset: statement.id()?,
            }
        } else if let Some(origin) = self.link_origin()?.filter(|origin| {
            statement.root()
                && revoked.mode == rocky_credentials::RevocationMode::Compromised
                && revoked.identities.contains(origin)
        }) {
            crate::RevocationReason::Cascade {
                trusted: origin.to_hex(),
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
    fn tombstone_held_statements(&mut self) -> Result<(), Failure> {
        self.learn_revocations(&RevocationSet::new()).map(drop)
    }
    pub(crate) fn revocation_status(&self) -> Result<crate::RevocationStatus, Failure> {
        let time = now()?;
        Ok(crate::RevocationStatus {
            digest: HEXLOWER.encode(&self.revocations.digest(time)?),
            statements: self.revocations.live(time) as u64,
            unpushed: self.unpushed_revocations(time)?.is_some(),
        })
    }
    pub(crate) fn unpushed_revocations(
        &self,
        time: u64,
    ) -> Result<Option<(RevocationSet, Commitment)>, Failure> {
        if self.revocations.live(time) == 0 {
            return Ok(None);
        }
        let digest = self.revocations.digest(time)?;
        Ok((self.revocations_pushed != Some(digest))
            .then(|| (self.revocations.pruned(time), digest)))
    }
    pub(crate) fn lacks(&self, set: &RevocationSet) -> Result<bool, Failure> {
        let time = now()?;
        let known: BTreeSet<_> = self
            .revocations
            .certificates()
            .map(SignedCapability::id)
            .collect::<Result<_, _>>()?;
        for certificate in set.certificates() {
            if !known.contains(&certificate.id()?) {
                return Ok(true);
            }
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
        for contact in settings
            .peers
            .values()
            .chain(settings.enrollment_routes.values())
        {
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
        if settings.peers.len() > MAX_CONTACTS
            || settings.issuers.len() > MAX_ISSUERS
            || settings.enrollment_routes.len() > MAX_ISSUERS
            || settings
                .enrollment_routes
                .keys()
                .any(|node| !settings.issuers.contains(node))
        {
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
        match self.credentials.held() {
            Some(held) => domain.families.for_capability(held).map_or_else(
                |_| held.grants.clone(),
                |families| families.resources(&held.grants),
            ),
            None => Vec::new(),
        }
    }
    fn selected(
        &self,
        domain: &Domain,
        scope: &rocky_core::ResourceScope,
        tuple: &rocky_core::PartitionTuple,
    ) -> Vec<ResourceGrant> {
        self.credentials
            .held()
            .and_then(|held| {
                Some((
                    domain.families.for_capability(held).ok()?,
                    held.extended().ok()?.partitions,
                ))
            })
            .into_iter()
            .flat_map(|(families, grants)| {
                families.partitions_for(&grants, scope.domain, scope.table)
            })
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
        self.peers
            .keys()
            .chain(&self.issuers)
            .chain(&self.enrollment_upstream)
            .copied()
            .collect()
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
        let routes = vec![Route {
            transport: transport.kind().into(),
            address: transport.address().await?,
        }];
        Contact::checked(routes.clone())?;
        let record = ContactRecord {
            v: CONTACT_VERSION,
            publisher: self.publisher,
            database: self.database_context()?,
            node,
            routes: routes.clone(),
            issuer: self.issuer(),
            enrollment_only: false,
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
        self.peers.entry(record.node).or_default();
        if record.enrollment_only {
            self.enrollment_routes
                .entry(record.node)
                .or_default()
                .learn(learned.routes);
        } else {
            self.peers
                .entry(record.node)
                .or_default()
                .learn(learned.routes);
        }
        Ok(record.node)
    }
    pub fn remove_peer(&mut self, node: PublicKey) -> Result<(), Failure> {
        let saved = self
            .contact_refs
            .remove(&rocky_credentials::PeerId::of(node));
        self.resolved_refs
            .remove(&rocky_credentials::PeerId::of(node));
        if self.peers.remove(&node).is_none() && saved.is_none() {
            return Err("unknown peer".into());
        }
        self.contact_meta.remove(&node);
        self.issuers.retain(|issuer| *issuer != node);
        self.enrollment_routes.remove(&node);
        Ok(())
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
    pub fn roles(&self) -> Vec<String> {
        let mut roles: Vec<String> = [
            "node",
            "transport",
            "invitation",
            "author-issuer",
            "tip-nodes",
            "discovery",
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
        let routes = |contact: &Contact| {
            contact
                .routes
                .iter()
                .map(|route| crate::Route {
                    transport: route.transport.clone(),
                    address: route.address.clone(),
                })
                .collect::<Vec<_>>()
        };
        let mut candidates: Vec<_> = self
            .issuers
            .iter()
            .map(|node| crate::RenewalRoute {
                target: node.to_hex(),
                routes: self.peers.get(node).map(routes).unwrap_or_default(),
                enrollment_routes: self
                    .enrollment_routes
                    .get(node)
                    .map(routes)
                    .unwrap_or_default(),
            })
            .collect();
        candidates.extend(
            self.contact_refs
                .iter()
                .filter(|(id, entry)| !self.resolved_refs.contains(id) && entry.issuer)
                .map(|(id, entry)| crate::RenewalRoute {
                    target: HEXLOWER.encode(id.as_bytes()),
                    routes: routes(&entry.contact),
                    enrollment_routes: entry.enrollment.as_ref().map(routes).unwrap_or_default(),
                }),
        );
        candidates.retain(|route| !route.routes.is_empty() || !route.enrollment_routes.is_empty());
        candidates
    }
    pub fn another_issuer(&self, registry: &Registry, membership: &Membership) -> bool {
        let own = self.identity;
        !self.issuers.is_empty()
            || membership.nodes(&own).count() > 1
            || registry.records.iter().any(|(identity, record)| {
                *identity != own && !record.revoked && record.profile().is_some_and(Profile::issues)
            })
    }

    pub fn create(
        schema: &Schema,
        store: &std::sync::Arc<dyn crate::SecretStore>,
        ttl: u64,
        provider: Option<Arc<dyn IdentityProvider>>,
        root: &dyn IdentityProvider,
    ) -> Result<Self, Failure> {
        let installation = HEXLOWER.encode(&random::<16>());
        let vault = Vault::new(store.clone(), &installation);
        let single_use = false;
        let identity: Arc<dyn IdentityProvider> = match provider {
            Some(provider) => provider,
            None => root
                .owner_identity()
                .ok_or("an application root signer needs an explicit owner identity")?,
        };
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
            Authority::owner(root).issue_trusted(identity.public_key()?, bound.scope, now()?)?;
        let mut settings = Self::installed(
            (installation, single_use),
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
    pub(crate) fn recover(
        store: &std::sync::Arc<dyn crate::SecretStore>,
        (archived, predecessor): (&Self, PublicKey),
        recovered: &crate::escrow::Recovered,
        provider: Option<Arc<dyn IdentityProvider>>,
        root: &dyn IdentityProvider,
    ) -> Result<Self, Failure> {
        let publisher = root.public_key()?;
        if publisher != archived.publisher {
            return Err("the root key is not this domain's root".into());
        }
        let installation = HEXLOWER.encode(&random::<16>());
        let vault = Vault::new(store.clone(), &installation);
        let (single_use, identity): (bool, Arc<dyn IdentityProvider>) = match provider {
            Some(provider) => (false, provider),
            None => (true, Arc::new(crate::IdentityKey::generate())),
        };
        if identity.public_key()? == publisher {
            return Err("a device's identity is never the root key".into());
        }
        vault.create("node")?;
        vault.create("transport")?;
        match recovered.naming {
            Some(naming) => vault.put(crate::groups::NAMING_ROLE, &naming)?,
            None => drop(vault.create(crate::groups::NAMING_ROLE)?),
        }
        let bound = archived.domain()?;
        let mut keys: BTreeMap<String, Vec<rocky_core::KeyEpoch>> = BTreeMap::new();
        for ((name, epoch), key) in &recovered.readers {
            vault.put(&key_role(name, *epoch), key)?;
            keys.entry(name.clone()).or_default().push(*epoch);
        }
        for (name, _) in bound.scopes() {
            if !keys.contains_key(name) {
                let key = random::<32>();
                let epoch = rocky_core::KeyEpoch::new(1, &key);
                vault.put(&key_role(name, epoch), &key)?;
                keys.insert(name.to_owned(), vec![epoch]);
            }
        }
        for ((address, epoch), secret) in &recovered.secrets {
            vault.put(&secret_role(address, *epoch), secret)?;
        }
        let groups = escrowed_groups(&archived.groups, &recovered.groups)?;
        let grant = Authority::owner(root).issue_trusted(
            identity.public_key()?,
            bound.scope,
            after_recovery(&archived.revocations, now()? + AFTER_SAME_SECOND_GRANTS),
        )?;
        let mut settings = Self::installed(
            (installation, single_use),
            (publisher, identity.public_key()?),
            (archived.mesh, archived.domain, archived.schema.clone()),
            archived.genesis.clone(),
            archived.ttl,
        );
        settings.generation = archived.generation;
        settings.engine = archived.engine;
        settings.pending = archived.pending.clone();
        settings.pending_follow = archived.pending_follow.clone();
        settings.learn_revocations(&archived.revocations)?;
        settings.keys = keys;
        settings.groups = groups;
        settings.epoch_statements = archived.epoch_statements.clone();
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
        let node = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        settings.learn_contacts(archived, &[node, predecessor]);
        Ok(settings)
    }
    pub(crate) fn installed(
        (installation, single_use): (String, bool),
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
            single_use,
            generation: schema.version,
            engine: rocky_core::EngineIdentity::current(),
            genesis: manifest,
            pending: None,
            pending_follow: None,
            bootstrap: None,
            revocations: RevocationSet::new(),
            credentials: CredentialChain(Vec::new()),
            ttl,
            profile: Profile::Issuer,
            keys: BTreeMap::new(),
            authority: None,
            device_revocations: BTreeMap::new(),
            issuers: Vec::new(),
            enrollment_upstream: None,
            delivery_nodes: BTreeMap::new(),
            contact_refs: BTreeMap::new(),
            resolved_refs: BTreeSet::new(),
            contact_meta: BTreeMap::new(),
            peers: BTreeMap::new(),
            enrollment_routes: BTreeMap::new(),
            logged_out: false,
            deliveries: BTreeMap::new(),
            claims: None,
            revocations_pushed: None,
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
    pub fn trust(
        &self,
        registry: &Registry,
        identity: PublicKey,
        root: Option<&dyn IdentityProvider>,
    ) -> Result<(SignedCapability, Changes), Failure> {
        let root = root_signer(root, self.publisher, "granting trust")?;
        if self.tombstoned(&identity) || self.revocations.revokes_trusted(&identity) {
            return Err("identity was revoked".into());
        }
        let record = registry
            .get(&identity)
            .filter(|record| !record.revoked)
            .ok_or("this identity is not recorded")?;
        record.capability().ok_or("this identity is not recorded")?;
        if !record.profile().is_some_and(Profile::issues) {
            return Err("only an identity enrolled with an issuer invitation is trusted".into());
        }
        let live =
            registry.trusted_identities(self.database_context()?, &self.revocations, now()?)?;
        if !live.contains(&identity) && live.len() >= crate::registry::MAX_TRUSTED {
            return Err(format!(
                "a domain has at most {} trusted identities",
                crate::registry::MAX_TRUSTED
            )
            .into());
        }
        let grant = Authority::owner(root).issue_trusted(
            identity,
            self.domain()?.scope,
            after_recovery(&self.revocations, now()?),
        )?;
        let mut changes = Changes::default();
        changes.trusted(
            &identity,
            &crate::registry::TrustedRecord {
                grant: grant.clone(),
                granted: now()?,
            },
        )?;
        Ok((grant, changes))
    }
    pub(crate) fn revoke_before_recovery(
        &mut self,
        root: Option<&dyn IdentityProvider>,
        mut cut: Vec<rocky_credentials::Cut>,
    ) -> Result<bool, Failure> {
        let root = root_signer(root, self.publisher, "recovering a domain")?;
        let links = self.identity_links();
        let grant = &links
            .0
            .first()
            .filter(|_| links.0.len() == 1)
            .ok_or("a recovered identity holds one trusted grant")?
            .capability;
        let recovery = rocky_credentials::RootRecovery {
            since: grant.not_before,
            keep: self.identity,
        };
        if self.revocations.recoveries().contains(&recovery) {
            return Ok(false);
        }
        cut.sort();
        let mut parts: Vec<Vec<rocky_credentials::Cut>> = vec![Vec::new()];
        for chain in cut {
            let last = parts.last_mut().ok_or("no statement")?;
            let heads: usize = last.iter().map(|held| held.statements.len()).sum();
            if last.len() == rocky_credentials::MAX_CUT_LANES
                || heads + chain.statements.len() > rocky_credentials::MAX_CUT_HEADS
            {
                parts.push(Vec::new());
            }
            parts.last_mut().ok_or("no statement")?.push(chain);
        }
        let (app, time) = (self.domain()?.manifest().app, now()?);
        let database = self.database_context()?;
        let statements = parts
            .into_iter()
            .map(|cut| {
                Authority::owner(root).revoke_in(
                    Revocation {
                        app,
                        issuer: self.publisher,
                        time,
                        nodes: BTreeSet::new(),
                        identities: BTreeSet::new(),
                        capabilities: BTreeSet::new(),
                        cut,
                        mode: rocky_credentials::RevocationMode::Compromised,
                        recovery: Some(rocky_credentials::RootDecision::Grants(recovery)),
                    },
                    database,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.learn_revocations(&RevocationSet::try_from(statements)?)?;
        Ok(true)
    }
    pub fn install_trust(&mut self, vault: &Vault, link: SignedCapability) -> Result<(), Failure> {
        let identity = CredentialChain(vec![link.clone()]).trusted_identity(
            self.publisher,
            self.domain()?.scope,
            &self.revocations,
            now()?,
        )?;
        if identity != self.identity {
            return Err("the trusted grant names another identity".into());
        }
        if !self.issuer() {
            return Err("only an issuer identity becomes trusted".into());
        }
        let domain = self.domain()?;
        self.credentials = self.certify(vault, CredentialChain(vec![link]), &domain)?;
        Ok(())
    }
    pub(crate) fn trusted_links(&self) -> Option<CredentialChain> {
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
    fn issue(
        &self,
        authority: &Authority<'_>,
        domain: &Domain,
        identity: PublicKey,
        grants: Vec<ResourceGrant>,
        issues: bool,
    ) -> Result<SignedCapability, Failure> {
        if issues {
            return Ok(authority.issue_issuer(identity, self.validity()?)?);
        }
        let partitions = domain
            .families
            .compact_partitions(&whole_partitions(domain, &grants));
        let grants = domain.families.compact_resources(&grants);
        let issue = if domain.families.is_empty() {
            Authority::issue_partition_terms
        } else {
            Authority::issue_family_terms
        };
        Ok(issue(
            authority,
            identity,
            grants,
            partitions,
            rocky_credentials::DelegationTerms { delegations: 1 },
            self.validity()?,
        )?)
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
    pub(crate) fn issuing_origin(&self) -> Result<Option<PublicKey>, Failure> {
        Ok(match self.identity_links().0.last() {
            Some(link) if link.capability.extended()?.trusted.is_some() => {
                Some(link.capability.subject)
            }
            _ => self.link_origin()?,
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
        let grants = domain
            .families
            .for_capability(capability)?
            .partitions(&capability.extended()?.partitions);
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
                    let selected =
                        reads(&capability.grants, domain.registry()) || names_partitions_only;
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
    fn statements_for_keys(&self, keys: &[NewKey]) -> BTreeMap<String, Vec<Commitment>> {
        keys.iter()
            .map(|(name, epoch, _)| {
                let role = key_role(name, *epoch);
                let ids = self
                    .epoch_statements
                    .get(&role)
                    .cloned()
                    .unwrap_or_default();
                (role, ids)
            })
            .collect()
    }
    pub(crate) fn provision(
        &mut self,
        vault: &Vault,
        routes: Vec<Route>,
        via_custody: bool,
        admission: (Profile, Option<Vec<u8>>),
        registry: &Registry,
    ) -> Result<((String, PublicKey, PublicKey), Changes), Failure> {
        let provisioned_node = PrivateKey::from_bytes(&rocky_platform::entropy::random());
        let provisioned_identity = IdentityKey::from_secret(rocky_platform::entropy::random());
        let device = NodeKeys {
            identity: provisioned_identity.public_key(),
            node: provisioned_node.public_key(),
        };
        let (enrollment, changes) =
            self.enrollment_for(vault, device, admission, None, registry)?;
        let bundle = ProvisioningBundle {
            v: PROVISION_VERSION,
            routes,
            publisher: self.publisher,
            database: self.database_context()?,
            via_custody,
            node_key: HEXLOWER.encode(provisioned_node.as_bytes()),
            identity_key: HEXLOWER.encode(&provisioned_identity.secret()),
            enrollment: Box::new(enrollment),
        };
        let uri = format!(
            "rocky://provision/{}",
            BASE64URL_NOPAD.encode(&serde_json::to_vec(&bundle)?)
        );
        Ok(((uri, device.node, device.identity), changes))
    }
    pub(crate) fn enrollment_for(
        &mut self,
        vault: &Vault,
        device: NodeKeys,
        (profile, label): (Profile, Option<Vec<u8>>),
        invitation: Option<crate::InvitationId>,
        registry: &Registry,
    ) -> Result<(Enrollment, Changes), Failure> {
        if label
            .as_ref()
            .is_some_and(|label| label.len() > registry::MAX_LABEL)
        {
            return Err("device label exceeds 256 bytes".into());
        }
        self.check_profile(&profile)?;
        self.issuing("provisions devices")?;
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
        let grants = grants_of(&profile, &domain)?;
        let authority = self.authority(&node)?;
        let issued = self.issue(
            &authority,
            &domain,
            device.identity,
            grants.clone(),
            profile.issues(),
        )?;
        let mut changes = Changes::default();
        let other_profile_replaces_record = known
            .and_then(registry::IdentityRecord::profile)
            .is_some_and(|known| *known != profile);
        changes.admitted(
            &device.identity,
            &registry::Admitted {
                kind: registry::Kind::Paired,
                profile: Some(profile.clone()),
                issuer: self.identity,
                origin: issued
                    .capability
                    .extended()?
                    .origin
                    .map(|origin| origin.identity),
                enrolled: time,
                invitation,
                single_use: false,
            },
            other_profile_replaces_record,
        )?;
        if let Some(label) = &label {
            changes.label(&device.identity, label)?;
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
        let database = self.database_context()?;
        crate::reset::admit_after_resets(&mut self.revocations, &node, database, device.node)?;
        let enrollment = Enrollment {
            groups: self.groups_for(vault, device.node, &issued.capability, false)?,
            naming: naming_for(vault, &profile)?,
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
            profile,
            issuers: self.issuer_contacts(&[device.node])?,
        };
        Ok((enrollment, changes))
    }
    pub(crate) fn check_profile(&self, profile: &Profile) -> Result<(), Failure> {
        let domain = self.domain()?;
        profile.grants(&domain.resources())?;
        grants_of(profile, &domain)?;
        Ok(())
    }

    pub fn grant(
        &mut self,
        vault: &Vault,
        device: NodeKeys,
        (profile, terms): (Profile, Terms),
        contact: Option<String>,
        registry: &Registry,
    ) -> Result<(Supplied, Changes), Failure> {
        let ttl = terms.ttl(self.ttl)?;
        if profile.issues() {
            return Err("issuer authority travels only in an issuer invitation".into());
        }
        let domain = self.domain()?;
        profile.grants(&domain.resources())?;
        let grants = grants_of(&profile, &domain)?;
        self.issuing("grants credentials")?;
        let known = registry.get(&device.identity);
        if self.revoked_node(&device, registry) {
            return Err("device was revoked".into());
        }
        if registry.full_for(&device.identity) {
            return Err(Box::new(RegistryFull));
        }
        let node = Self::node_key(vault)?;
        let authority = self.authority(&node)?;
        let time = now()?;
        let issue = if domain.families.is_empty() {
            Authority::issue_partition_terms
        } else {
            Authority::issue_family_terms
        };
        let capability = issue(
            &authority,
            device.identity,
            domain.families.compact_resources(&grants),
            domain
                .families
                .compact_partitions(&whole_partitions(&domain, &grants)),
            rocky_credentials::DelegationTerms {
                delegations: terms.delegations.unwrap_or(1),
            },
            rocky_credentials::Validity {
                issued_at: time,
                lifetime: std::time::Duration::from_secs(ttl),
                clock_skew: std::time::Duration::from_secs(SKEW),
            },
        )?;
        let mut keys = Vec::new();
        for (name, epochs) in self.keys_for(vault, &domain, &grants)? {
            for (epoch, key) in epochs {
                keys.push((name.clone(), epoch, hex32(&key)?));
            }
        }
        let mut changes = Changes::default();
        let other_profile_replaces_record = known
            .and_then(registry::IdentityRecord::profile)
            .is_some_and(|known| *known != profile);
        changes.admitted(
            &device.identity,
            &registry::Admitted {
                kind: registry::Kind::Granted,
                profile: Some(profile),
                issuer: self.identity,
                origin: capability
                    .capability
                    .extended()?
                    .origin
                    .map(|origin| origin.identity),
                enrolled: time,
                invitation: None,
                single_use: false,
            },
            other_profile_replaces_record,
        )?;
        changes.capability(
            &device.identity,
            registry::Capability {
                id: capability.id()?,
                expires: capability.capability.expires,
            },
            known,
        )?;
        let database = self.database_context()?;
        crate::reset::admit_after_resets(&mut self.revocations, &node, database, device.node)?;
        let supplied = Supplied {
            epoch_statements: self.statements_for_keys(&keys),
            mesh: self.mesh,
            domain: self.domain,
            manifest: self.manifest()?,
            credentials: Credentials::of(
                self.publisher,
                self.revocations.clone(),
                &authority.chain_of(capability),
            ),
            keys,
            issuers: contact
                .into_iter()
                .chain(self.issuer_contacts(&[device.node])?)
                .collect(),
        };
        Ok((supplied, changes))
    }
    pub fn delegate(
        &mut self,
        vault: &Vault,
        device: NodeKeys,
        (profile, terms): (Profile, Terms),
        registry: Option<&Registry>,
    ) -> Result<(Supplied, Changes), Failure> {
        let ttl = terms.ttl(self.ttl)?;
        if profile.issues() {
            return Err("issuer authority travels only in an issuer invitation".into());
        }
        if self.tombstoned(&device.node) || self.tombstoned(&device.identity) {
            return Err("device was revoked".into());
        }
        let domain = self.domain()?;
        let grants = grants_of(&profile, &domain)?;
        if let Some(registry) = registry {
            if self.revoked_node(&device, registry) {
                return Err("device was revoked".into());
            }
            if registry.full_for(&device.identity) {
                return Err(Box::new(RegistryFull));
            }
        }
        let node = Self::node_key(vault)?;
        let family_context = if domain.families.is_empty() {
            rocky_credentials::GrantFamilies::default()
        } else {
            domain.families.clone()
        };
        let chain = self.credentials.delegable().delegate_with_families(
            rocky_credentials::Signer::node(&node, &self.credentials)?,
            rocky_credentials::Delegation {
                subject: device.identity,
                kind: rocky_core::PrincipalKind::User,
                grants: domain.families.compact_resources(&grants),
                partitions: domain
                    .families
                    .compact_partitions(&whole_partitions(&domain, &grants)),
                terms: rocky_credentials::DelegationTerms {
                    delegations: terms.delegations.unwrap_or(1),
                },
                validity: rocky_credentials::Validity {
                    issued_at: now()?,
                    lifetime: std::time::Duration::from_secs(ttl),
                    clock_skew: std::time::Duration::from_secs(SKEW),
                },
            },
            &family_context,
        )?;
        let mut keys = Vec::new();
        for (name, epochs) in self.keys_for(vault, &domain, &grants)? {
            for (epoch, key) in epochs {
                keys.push((name.clone(), epoch, hex32(&key)?));
            }
        }
        let mut changes = Changes::default();
        if let Some(registry) = registry {
            let known = registry.get(&device.identity);
            let leaf = chain.0.last().ok_or("empty delegation")?;
            changes.admitted(
                &device.identity,
                &registry::Admitted {
                    kind: registry::Kind::Delegated,
                    profile: Some(profile),
                    issuer: self.identity,
                    origin: self.issuing_origin()?,
                    enrolled: now()?,
                    invitation: None,
                    single_use: false,
                },
                false,
            )?;
            changes.capability(
                &device.identity,
                registry::Capability {
                    id: leaf.id()?,
                    expires: chain.expires(),
                },
                known,
            )?;
        }
        let database = self.database_context()?;
        crate::reset::admit_after_resets(&mut self.revocations, &node, database, device.node)?;
        let supplied = Supplied {
            epoch_statements: self.statements_for_keys(&keys),
            mesh: self.mesh,
            domain: self.domain,
            manifest: self.manifest()?,
            credentials: Credentials::of(self.publisher, self.revocations.clone(), &chain),
            keys,
            issuers: Vec::new(),
        };
        Ok((supplied, changes))
    }
    pub fn supplied_renewal(&self, vault: &Vault, supplied: &Supplied) -> Result<Renewed, Failure> {
        if self.issuer() {
            return Err("an issuer renews its own credentials".into());
        }
        let credentials = &supplied.credentials;
        let domain = self.domain()?;
        if supplied.mesh != self.mesh
            || supplied.domain != self.domain
            || credentials.publisher != self.publisher
        {
            return Err("the credentials belong to another domain".into());
        }
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let chain = self.own_chain(vault, &domain, credentials)?;
        let mut revocations = self.revocations.clone();
        revocations.merge(
            &credentials.revocations,
            self.publisher,
            domain.manifest().app,
            now()?,
        )?;
        let (grants, _) = domain
            .verified(&chain, self.publisher, &revocations, now()?)?
            .selectors(node.public_key(), &revocations, now()?)?;
        let mut keys = Vec::new();
        for (name, epoch, key) in &supplied.keys {
            let scope = domain
                .scopes()
                .find(|(collection, _)| collection == name)
                .map(|(_, scope)| scope);
            if !scope.is_some_and(|scope| reads(&grants, scope)) || !epoch.valid() {
                return Err(
                    "a supplied key is for a collection the credentials do not read".into(),
                );
            }
            keys.push((name.clone(), *epoch, *key));
        }
        let missing = domain.scopes().any(|(name, scope)| {
            reads(&grants, scope)
                && !self.keys.contains_key(name)
                && !keys.iter().any(|(held, _, _)| held == name)
        });
        if missing {
            return Err("missing reader key".into());
        }
        Ok(Renewed {
            contacted: None,
            revocations: credentials.revocations.clone(),
            chain,
            epoch_statements: supplied.epoch_statements.clone(),
            groups: Vec::new(),
            keys,
            profile: Some(profile_of(&grants, &domain)),
            issuers: Vec::new(),
            claims: None,
        })
    }
    fn own_chain(
        &self,
        vault: &Vault,
        domain: &Domain,
        credentials: &Credentials,
    ) -> Result<CredentialChain, Failure> {
        credentials
            .revocations
            .verify_in(self.database_context()?)?;
        let mut chain = credentials.chain();
        if chain.database_context(self.publisher)? != self.database_context()? {
            return Err("supplied credentials belong to another database".into());
        }
        let node = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        if chain
            .0
            .last()
            .is_some_and(|last| last.capability.kind == rocky_core::PrincipalKind::Node)
            && chain.principal() == Some(self.identity)
        {
            domain
                .verified(&chain, self.publisher, &credentials.revocations, now()?)?
                .selectors(node, &credentials.revocations, now()?)?;
            chain.0.pop();
        }
        if chain
            .0
            .last()
            .is_some_and(|last| last.capability.subject == self.identity)
        {
            let mut certifying = self.clone();
            certifying.learn_revocations(&credentials.revocations)?;
            chain = certifying.certify(vault, chain, domain)?;
        }
        Ok(chain)
    }
    pub(crate) fn own_record(&self, registry: &Registry) -> Result<Changes, Failure> {
        let own = self.identity;
        let known = registry.get(&own);
        let links = self.identity_links();
        let root = links.0.last().ok_or("missing credentials")?;
        let mut changes = Changes::default();
        changes.admitted(
            &own,
            &registry::Admitted {
                kind: registry::Kind::Created,
                profile: Some(self.profile.clone()),
                issuer: own,
                origin: self.link_origin()?,
                enrolled: now()?,
                invitation: None,
                single_use: self.single_use,
            },
            false,
        )?;
        changes.capability(
            &own,
            registry::Capability {
                id: root.id()?,
                expires: root.capability.expires,
            },
            known,
        )?;
        if let Some(links) = self.trusted_links()
            && !registry.trusted.contains_key(&self.identity)
        {
            let grant = links.0.first().ok_or("missing trusted grant")?.clone();
            changes.trusted(
                &self.identity,
                &registry::TrustedRecord {
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

    pub fn provision_publisher(provisioning: &str) -> Result<PublicKey, Failure> {
        Ok(ProvisioningBundle::parse(provisioning)?.publisher)
    }
    pub fn learn_contacts(&mut self, other: &Settings, exclude: &[PublicKey]) {
        for (node, contact) in &other.peers {
            if exclude.contains(node) || self.peers.contains_key(node) {
                continue;
            }
            if self.contact_count() >= MAX_CONTACTS {
                break;
            }
            self.peers.insert(*node, contact.clone());
            if other.issuers.contains(node)
                && !self.issuers.contains(node)
                && self.issuers.len() < MAX_ISSUERS
            {
                self.issuers.push(*node);
            }
        }
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
                enrollment_only: false,
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
    pub(crate) fn forget_revoked(&mut self) {
        self.contact_refs
            .retain(|id, entry| entry.upstream || !self.revocations.revokes_reference(id));
        let gone: Vec<PublicKey> = self
            .peers
            .keys()
            .chain(&self.issuers)
            .filter(|node| self.revoked(node) && self.enrollment_upstream != Some(**node))
            .copied()
            .collect();
        for node in gone {
            self.peers.remove(&node);
            self.contact_meta.remove(&node);
            self.issuers.retain(|issuer| *issuer != node);
            self.enrollment_routes.remove(&node);
        }
    }
    pub fn prepare_install(
        path: &Path,
        store: &std::sync::Arc<dyn crate::SecretStore>,
        provider: Option<Arc<dyn IdentityProvider>>,
    ) -> Result<NodeKeys, Failure> {
        let (_, vault) = Intent::resume(
            &Intent::path(path, true),
            None,
            store,
            provider.as_ref(),
            false,
        )?;
        Ok(NodeKeys {
            identity: provider
                .as_ref()
                .ok_or("supply the identity for this installation")?
                .public_key()?,
            node: PrivateKey::from_bytes(&vault.get("node")?).public_key(),
        })
    }
    pub async fn install(
        path: &Path,
        schema: &Schema,
        store: &std::sync::Arc<dyn crate::SecretStore>,
        supplied: &Supplied,
        provider: Option<Arc<dyn IdentityProvider>>,
    ) -> Result<Self, Failure> {
        let intent_path = Intent::path(path, true);
        if !rocky_platform::fs::exists(&intent_path) {
            return Err("no device keys in this directory: prepare the device first".into());
        }
        let (intent, vault) = Intent::resume(&intent_path, None, store, provider.as_ref(), false)?;
        let identity = provider
            .as_ref()
            .ok_or("supply the identity for this installation")?
            .public_key()?;
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let credentials = &supplied.credentials;
        let mut settings = Self {
            version: VERSION,
            installation: intent.installation,
            schema: schema.clone(),
            mesh: supplied.mesh,
            domain: supplied.domain,
            publisher: credentials.publisher,
            identity,
            single_use: intent.single_use,
            genesis: HEXLOWER.encode(&supplied.manifest.to_bytes()?),
            generation: schema.version,
            engine: rocky_core::EngineIdentity::current(),
            pending: None,
            pending_follow: None,
            bootstrap: None,
            revocations: RevocationSet::new(),
            credentials: credentials.chain(),
            ttl: DEFAULT_TTL,
            profile: Profile::Issuer,
            keys: BTreeMap::new(),
            authority: None,
            device_revocations: BTreeMap::new(),
            issuers: Vec::new(),
            enrollment_upstream: None,
            delivery_nodes: BTreeMap::new(),
            contact_refs: BTreeMap::new(),
            resolved_refs: BTreeSet::new(),
            contact_meta: BTreeMap::new(),
            peers: BTreeMap::new(),
            enrollment_routes: BTreeMap::new(),
            logged_out: false,
            deliveries: BTreeMap::new(),
            claims: None,
            revocations_pushed: None,
            groups: BTreeMap::new(),
            epoch_statements: BTreeMap::new(),
        };
        let domain = settings.domain()?;
        if supplied
            .manifest
            .verify_historic(credentials.publisher)?
            .hash()
            != domain.manifest().hash()
        {
            return Err("the domain's manifest differs from this application schema".into());
        }
        settings.credentials = crate::identities::with_identity(
            provider.ok_or("supply the identity for this installation")?,
            || settings.own_chain(&vault, &domain, credentials),
        )?;
        settings.epoch_statements = supplied.epoch_statements.clone();
        settings.learn_revocations(&credentials.revocations)?;
        let (grants, _) = domain
            .verified(
                &settings.credentials,
                settings.publisher,
                &settings.revocations,
                now()?,
            )?
            .selectors(node.public_key(), &settings.revocations, now()?)?;
        settings.ttl = settings
            .credentials
            .expires()
            .saturating_sub(now()?)
            .clamp(60, DEFAULT_TTL);
        settings.profile = profile_of(&grants, &domain);
        let readable: BTreeMap<&str, _> = domain
            .scopes()
            .filter(|(_, scope)| reads(&grants, *scope))
            .collect();
        for (name, epoch, key) in &supplied.keys {
            if !readable.contains_key(name.as_str()) || !epoch.valid() {
                return Err(
                    "a supplied key is for a collection the credentials do not read".into(),
                );
            }
            settings.hold(&vault, name, *epoch, key)?;
        }
        if readable
            .keys()
            .any(|name| !settings.keys.contains_key(*name))
        {
            return Err("missing reader key".into());
        }
        for contact in &supplied.issuers {
            let node = settings.add_peer(&vault, contact)?;
            if !settings.issuers.contains(&node) {
                settings.issuers.push(node);
            }
        }
        settings.save(path).await?;
        Ok(settings)
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
    if prior.extended()?.issues {
        return Ok(());
    }
    let profile = known
        .and_then(registry::IdentityRecord::profile)
        .cloned()
        .unwrap_or_else(|| profile_of(&prior.grants, domain));
    let widened = profile_grants(&profile, domain)?;
    if prior
        .grants
        .iter()
        .all(|grant| widened.iter().any(|wide| wide.includes(grant)))
    {
        prior.grants = widened;
    }
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

const AFTER_SAME_SECOND_GRANTS: u64 = 1;

fn escrowed_groups(
    archived: &BTreeMap<String, GroupRecord>,
    escrowed: &BTreeMap<String, GroupRecord>,
) -> Result<BTreeMap<String, GroupRecord>, Failure> {
    let mut groups = archived.clone();
    for record in groups.values_mut() {
        record.epochs.clear();
    }
    for (key, record) in escrowed {
        match groups.get_mut(key) {
            Some(named) if named.name != record.name => {
                return Err("an escrowed partition differs from the one the backup names".into());
            }
            Some(named) => named.epochs = record.epochs.clone(),
            None => drop(groups.insert(key.clone(), record.clone())),
        }
    }
    Ok(groups)
}

fn after_recovery(revocations: &RevocationSet, time: u64) -> u64 {
    revocations.recovery().map_or(time, |recovery| {
        time.max(recovery.since + rocky_credentials::LIFETIME_SKEW + 1)
    })
}
impl Settings {
    pub(crate) async fn install_provisioned(
        store: &Arc<dyn crate::SecretStore>,
        schema: &Schema,
        provisioning: &str,
        single_use: bool,
    ) -> Result<(Self, Vault, Option<String>), Failure> {
        let bundle = ProvisioningBundle::parse(provisioning)?;
        let installation = HEXLOWER.encode(&rocky_platform::entropy::random::<32>());
        let vault = Vault::new(store.clone(), &installation);
        vault.put("node", &hex32(&bundle.node_key)?)?;
        vault.put("transport", &rocky_platform::entropy::random::<32>())?;
        let identity_key = IdentityKey::from_secret(hex32(&bundle.identity_key)?);
        let phrase = (!single_use).then(|| identity_key.phrase());
        let settings = Self::install_enrollment(
            schema,
            (installation, &vault),
            EnrollingIssuer {
                publisher: bundle.publisher,
                database: bundle.database,
                routes: bundle.routes,
                via_custody: bundle.via_custody,
            },
            *bundle.enrollment,
            (Arc::new(identity_key), single_use),
        )?;
        Ok((settings, vault, phrase))
    }
    pub(crate) fn install_enrollment(
        schema: &Schema,
        (installation, vault): (String, &Vault),
        issuing: EnrollingIssuer,
        enrollment: Enrollment,
        (provider, single_use): (Arc<dyn IdentityProvider>, bool),
    ) -> Result<Self, Failure> {
        enrollment.record.bytes()?;
        if enrollment.mesh != issuing.database.scope.mesh
            || enrollment.domain != issuing.database.scope.domain
        {
            return Err("enrollment belongs to another database".into());
        }
        if context(schema, issuing.database) != context(&enrollment.record.schema, issuing.database)
        {
            return Err("application mismatch".into());
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
            pending_follow: None,
            bootstrap: Some(enrollment.bootstrap),
            mesh: enrollment.mesh,
            domain: enrollment.domain,
            publisher: issuing.publisher,
            identity,
            single_use,
            genesis: enrollment.genesis,
            revocations: RevocationSet::new(),
            credentials: CredentialChain(Vec::new()),
            ttl: enrollment.ttl,
            profile: enrollment.profile,
            keys: BTreeMap::new(),
            authority: None,
            device_revocations: BTreeMap::new(),
            issuers: vec![enrollment.issuer],
            enrollment_upstream: None,
            delivery_nodes: BTreeMap::new(),
            contact_refs: BTreeMap::new(),
            resolved_refs: BTreeSet::new(),
            contact_meta: BTreeMap::new(),
            peers: BTreeMap::from([(
                enrollment.issuer,
                if issuing.via_custody {
                    Contact::default()
                } else {
                    issuer.clone()
                },
            )]),
            enrollment_routes: if issuing.via_custody {
                BTreeMap::from([(enrollment.issuer, issuer)])
            } else {
                BTreeMap::new()
            },
            logged_out: false,
            deliveries: BTreeMap::new(),
            claims: None,
            revocations_pushed: None,
            groups: BTreeMap::new(),
            epoch_statements: BTreeMap::new(),
        };
        settings.verify_schema()?;
        let domain = settings.domain()?;
        settings.epoch_statements = enrollment.epoch_statements.clone();
        settings.learn_revocations(&enrollment.revocations)?;
        let grants = grants_of(&settings.profile, &domain)?;
        let signer = if enrollment.chain.0.is_empty() {
            settings.publisher
        } else {
            enrollment.chain.issuer(settings.publisher, None)?
        };
        let issues = enrollment.capability.capability.extended()?.issues;
        let granted = domain
            .families
            .for_capability(&enrollment.capability.capability)?
            .resources(&enrollment.capability.capability.grants);
        if enrollment.capability.issuer != signer
            || enrollment.capability.capability.subject != identity
            || issues != settings.profile.issues()
            || (!issues
                && (granted.len() != grants.len()
                    || granted.iter().any(|grant| !grants.contains(grant))
                    || grants.iter().any(|grant| !granted.contains(grant))))
        {
            return Err("the enrollment differs from its provisioning bundle".into());
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
        if settings.profile.issues() {
            settings.authority = Some(Issuing::default());
            hold_naming(vault, enrollment.naming.as_deref())?;
            settings.tombstone_held_statements()?;
        }
        settings.adopt_groups(vault, &enrollment.groups)?;
        settings.learn_issuers(vault, &enrollment.issuers);
        Ok(settings)
    }
}
impl Settings {
    pub(crate) fn answer_epochs(
        &self,
        vault: &Vault,
        bytes: &[u8],
        registry: &Registry,
    ) -> Result<Vec<u8>, Failure> {
        if !self.issuer() {
            return Err("only an issuer delivers epochs".into());
        }
        let request = RenewRequest::from_bytes(bytes)?;
        request.chain.verify_database(self.database_context()?)?;
        if self.tombstoned(&request.node) {
            return Err("device was revoked".into());
        }
        let domain = self.domain()?;
        let prior = request.verify_with_families(
            PrivateKey::from_bytes(&vault.get("node")?).public_key(),
            self.publisher,
            (now()?, SKEW),
            &domain.families,
        )?;
        if self.revoked_identity(&prior.subject, registry) {
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
            claims: None,
        };
        let node = Self::node_key(vault)?;
        Ok(Renewal::seal_current_with_families(
            &self.authority(&node)?,
            self.publisher,
            &request,
            self.revocations.clone(),
            now()?,
            &serde_json::to_vec(&sealed)?,
            &domain.families,
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
        let mut failure: Failure = ServiceError::NoRoute {
            peer: "an issuer",
            network: transport.kind().into(),
        }
        .into();
        let sources = self.epoch_sources();
        for issuer in sources
            .iter()
            .filter(|issuer| first.is_none_or(|first| **issuer == first))
        {
            let addresses = self
                .enrollment_routes
                .get(issuer)
                .or_else(|| self.peers.get(issuer))
                .map(|contact| contact.addresses(transport.kind()))
                .unwrap_or_default();
            if addresses.is_empty() {
                continue;
            }
            let (request, secret) =
                RenewRequest::sign(&node, *issuer, self.credentials.clone(), now()?)?;
            let message = PeerRequest::Epochs(EpochRequest {
                request: request.clone(),
                revocations: self.revocations.clone(),
            });
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
                claims: _,
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
                profile: None,
                issuers,
                claims: None,
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
            if (learn || self.trusted_links().is_none())
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
                profile: None,
                issuers: Vec::new(),
                claims: None,
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
        let mut failure: Failure = ServiceError::NoRoute {
            peer: "an issuer",
            network: transport.kind().into(),
        }
        .into();
        let mut issuers = self.issuers.clone();
        issuers.sort_by_key(|issuer| Some(*issuer) != first);
        for issuer in &issuers {
            let addresses = self
                .enrollment_routes
                .get(issuer)
                .or_else(|| self.peers.get(issuer))
                .map(|contact| contact.addresses(transport.kind()))
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
            &PeerRequest::Renew(request.clone()),
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
        let widened_grants = domain
            .families
            .for_capability(chain.held().ok_or("missing identity capability")?)?
            .resources(chain.grants());
        let Sealed {
            keys: received,
            groups,
            epoch_statements,
            issuers,
            claims,
        } = Sealed::parse(&sealed)?;
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
        Ok(Renewed {
            contacted: Some(issuer),
            revocations: renewal.revocations,
            chain,
            groups,
            epoch_statements,
            keys,
            profile: None,
            issuers,
            claims,
        })
    }
    pub(crate) fn profile_widens(&self, domain: &Domain) -> Result<bool, Failure> {
        let held = self.grants(domain);
        let wanted = profile_grants(&self.profile, domain)?;
        let partitions = self.credentials.partitions()?;
        let held_partitions = match self.credentials.held() {
            Some(held) => domain
                .families
                .for_capability(held)?
                .partitions(&partitions),
            None => Vec::new(),
        };
        Ok(domain.effective_partitions(&partitions) != held_partitions
            || domain
                .scopes()
                .any(|(name, scope)| reads(&wanted, scope) && !self.keys.contains_key(name))
            || (held
                .iter()
                .all(|grant| wanted.iter().any(|wide| wide.includes(grant)))
                && wanted
                    .iter()
                    .any(|wide| !held.iter().any(|grant| grant.includes(wide)))))
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
        let renewed_grants = domain
            .families
            .for_capability(renewed)?
            .resources(&renewed.grants);
        let held_grants = domain
            .families
            .for_capability(held)?
            .resources(&held.grants);
        let profile_grants = profile_grants(&self.profile, domain)?;
        let same = |other: &[ResourceGrant]| {
            renewed_grants.len() == other.len()
                && renewed_grants.iter().all(|grant| other.contains(grant))
        };
        Ok(renewed.subject == self.identity
            && (same(&held_grants) || same(&profile_grants))
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
        if let Some(claims) = &renewed.claims {
            draft.hold_claims(claims.clone(), renewed.contacted)?;
        }
        draft.adopt_groups(vault, &renewed.groups)?;
        draft.forget_revoked();
        draft.learn_issuers(vault, &renewed.issuers);
        if let Some(profile) = &renewed.profile {
            draft.profile = profile.clone();
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
        if changed {
            draft.save(path).await?;
        }
        *self = draft;
        Ok((held, changed))
    }
    pub fn renew(
        &self,
        vault: &Vault,
        request: &[u8],
        registry: &Registry,
        claims: Option<CredentialChain>,
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
        let mut prior = request
            .verify_with_families(me, self.publisher, (time, SKEW), &domain.families)?
            .clone();
        let known = registry.get(&prior.subject);
        if self.revoked_identity(&prior.subject, registry) {
            return Err("device was revoked".into());
        }
        widen_to_current_generation(&mut prior, known, &domain)?;
        let mut extended = prior.extended()?;
        extended.partitions = domain.effective_partitions(&extended.partitions);
        extended.partitions = domain.families.compact_partitions(&extended.partitions);
        extended.families = !extended.issues && !domain.families.is_empty();
        prior.grants = domain.families.compact_resources(&prior.grants);
        prior.extension = rocky_credentials::Extension::new(extended)?;
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
        let sealed = Sealed {
            groups: self.groups_for(vault, request.node, &held_by, false)?,
            epoch_statements: self.statements_for(&self.keys_for(vault, &domain, &granted)?),
            keys: self.keys_for(vault, &domain, &granted)?,
            issuers: self.issuer_contacts(&[request.node])?,
            claims,
        };
        let renewal = Renewal::seal(
            &authority,
            &request,
            prior,
            self.revocations.clone(),
            rocky_credentials::Validity {
                issued_at: time,
                lifetime: std::time::Duration::from_secs(self.ttl),
                clock_skew: std::time::Duration::from_secs(SKEW),
            },
            &serde_json::to_vec(&sealed)?,
        )?;
        let mut changes = Changes::default();
        changes.admitted(
            &prior.subject,
            &registry::Admitted {
                kind: registry::Kind::Renewed,
                profile: Some(profile_of(&granted, &domain)),
                issuer: self.identity,
                origin: prior.extended()?.origin.map(|origin| origin.identity),
                enrolled: time,
                invitation: None,
                single_use: false,
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
    pub(crate) fn tombstoned(&self, key: &PublicKey) -> bool {
        self.revocations.unfenced().revokes(key)
            || self.signed_out(key)
            || self.authority.as_ref().is_some_and(|authority| {
                authority
                    .tombstones
                    .contains(&rocky_credentials::PeerId::of(*key))
            })
    }
    pub(crate) fn signed_out(&self, node: &PublicKey) -> bool {
        self.revocations
            .reset_ends(&rocky_credentials::PeerId::of(*node))
            && (self
                .revocations
                .certificates()
                .any(|certificate| certificate.capability.subject == *node)
                || self
                    .authority
                    .as_ref()
                    .is_some_and(|authority| authority.seen.contains_key(node)))
    }
    pub(crate) fn revoked_identity(&self, identity: &PublicKey, registry: &Registry) -> bool {
        self.tombstoned(identity) || registry.revoked(identity)
    }
    pub(crate) fn revoked_node(&self, keys: &NodeKeys, registry: &Registry) -> bool {
        self.tombstoned(&keys.node) || self.revoked_identity(&keys.identity, registry)
    }
    pub(crate) fn holder_changes(
        &self,
        device: &NodeKeys,
        grants: &[rocky_credentials::PartitionGrant],
        delegations: u8,
        registry: &Registry,
    ) -> Result<Changes, Failure> {
        if self.revoked_node(device, registry) {
            return Err("the node or its identity was revoked".into());
        }
        let known = registry.get(&device.identity);
        if registry.full_for(&device.identity) {
            return Err(Box::new(RegistryFull));
        }
        let mut changes = Changes::default();
        changes.admitted(
            &device.identity,
            &registry::Admitted {
                kind: registry::Kind::Partition,
                profile: None,
                issuer: self.identity,
                origin: self.issuing_origin()?,
                enrolled: now()?,
                invitation: None,
                single_use: false,
            },
            false,
        )?;
        for grant in grants {
            changes.grant(
                &device.identity,
                &registry::Grant {
                    grant: grant.clone(),
                    delegations,
                },
                known,
            )?;
        }
        Ok(changes)
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
    pub fn begin_revocation_cut(
        &mut self,
        target: Revoke,
        (registry, membership): (&Registry, &Membership),
        root: Option<&dyn IdentityProvider>,
        cut: Vec<rocky_credentials::Cut>,
    ) -> Result<(), Failure> {
        let own = self.identity;
        let trusted = match target {
            Revoke::Identity(identity) | Revoke::TrustedIdentity(identity, _) => {
                registry.trusted.contains_key(&identity)
            }
            Revoke::Claims(_) => false,
        };
        let root = if trusted {
            Some(root_signer(
                root,
                self.publisher,
                "revoking a trusted identity",
            )?)
        } else {
            None
        };
        let mode = match target {
            Revoke::TrustedIdentity(_, mode) if trusted => mode,
            Revoke::TrustedIdentity(_, _) => return Err("identity is not trusted".into()),
            Revoke::Identity(_) if trusted => {
                return Err("revoking a trusted identity requires a revocation mode".into());
            }
            _ => rocky_credentials::RevocationMode::Lost,
        };
        let identities: Vec<PublicKey> = match target {
            Revoke::Identity(identity) | Revoke::TrustedIdentity(identity, _) => {
                if identity == own {
                    return Err("an issuer cannot revoke its own identity".into());
                }
                if identity == self.publisher {
                    return Err("the root is never revoked".into());
                }
                if self.tombstoned(&identity)
                    && !(trusted
                        && mode == rocky_credentials::RevocationMode::Compromised
                        && !self.revocations.revokes_origin(&identity))
                {
                    return Err("identity is already revoked".into());
                }
                let mut identities = BTreeSet::from([identity]);
                if trusted && mode == rocky_credentials::RevocationMode::Compromised {
                    identities.extend(registry.enrolled_by(identity));
                }
                identities.into_iter().collect()
            }
            Revoke::Claims(_) => Vec::new(),
        };
        let nodes: Vec<PublicKey> = identities
            .iter()
            .flat_map(|identity| membership.nodes(identity))
            .map(|linked| linked.node)
            .collect();
        if self.issuing("revokes devices")?.rotation.is_some() {
            return Err("another revocation is unfinished".into());
        }
        let time = now()?;
        if self.revocations.live(time) >= MAX_LIVE_REVOCATIONS {
            return Err(Box::new(RevocationSetFull));
        }
        let domain = self.domain()?;
        let records: Vec<Revoking> = identities
            .iter()
            .filter_map(|identity| registry.get(identity).map(Revoking::of))
            .collect();
        let issuer = if root.is_some() { self.publisher } else { own };
        let statement = match target {
            Revoke::Identity(identity) | Revoke::TrustedIdentity(identity, _) => Revocation {
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
                            .trusted
                            .get(&identity)
                            .and_then(|record| record.grant.id().ok()),
                    )
                    .collect(),
                cut: if root.is_some() { cut } else { Vec::new() },
                recovery: None,
            },
            Revoke::Claims(capability) => Revocation {
                app: domain.manifest().app,
                issuer: own,
                time,
                mode: rocky_credentials::RevocationMode::Lost,
                nodes: BTreeSet::new(),
                identities: BTreeSet::new(),
                capabilities: [capability].into(),
                cut: Default::default(),
                recovery: None,
            },
        };
        let claim_right_reads_nothing = matches!(target, Revoke::Claims(_));
        let bounded_by_records = !records.is_empty();
        let readable: Vec<Profile> = records
            .iter()
            .filter_map(|record| record.profile.as_ref())
            .map(|profile| domain.effective(profile))
            .collect();
        let mut keys = BTreeMap::new();
        for (name, epochs) in &self.keys {
            if claim_right_reads_nothing
                || (bounded_by_records && !readable.iter().any(|profile| profile.reads(name)))
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
        let groups = if claim_right_reads_nothing {
            BTreeMap::new()
        } else {
            self.next_group_epochs(&domain, &records, &readable)?
        };
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
        readable: &[Profile],
    ) -> Result<BTreeMap<String, RotationEpoch>, Failure> {
        let bounded_by_records = !records.is_empty();
        let mut groups = BTreeMap::new();
        for partition in self.partitions(domain)? {
            let read = !bounded_by_records
                || readable.iter().any(|profile| {
                    partition
                        .members
                        .iter()
                        .any(|member| profile.reads(&member.table))
                })
                || records.iter().any(|record| record.selects(&partition));
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
                .map(|identity| ReadBound::of(registry.get(identity)))
                .chain(signed.statement.nodes.iter().map(|node| {
                    ReadBound::of(
                        self.revocations
                            .certificates()
                            .find(|certificate| {
                                rocky_credentials::PeerId::of(certificate.capability.subject)
                                    == *node
                            })
                            .and_then(|certificate| registry.get(&certificate.issuer)),
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
                let read = read_bounds.iter().any(|bound| {
                    bound.reads(|record| {
                        record
                            .profile
                            .as_ref()
                            .is_some_and(|profile| domain.effective(profile).reads(name))
                    })
                });
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
                        record.profile.as_ref().is_some_and(|profile| {
                            let profile = domain.effective(profile);
                            partition
                                .members
                                .iter()
                                .any(|member| profile.reads(&member.table))
                        }) || record.selects(&partition)
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
            let _ = self.remove_peer(node);
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
    if !capability.extended()?.issues {
        return Ok((
            capability.clone(),
            domain
                .families
                .for_capability(capability)?
                .resources(&capability.grants),
        ));
    }
    let grants = grants_of(&Profile::Issuer, domain)?;
    let mut held = capability.clone();
    held.grants = domain.families.compact_resources(&grants);
    held.extension = rocky_credentials::Extension::new(rocky_credentials::Extended {
        partitions: domain
            .families
            .compact_partitions(&whole_partitions(domain, &grants)),
        families: !domain.families.is_empty(),
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
        .map_err(|error| -> Failure {
            if error.is::<rocky_core::net::ScopeRefusal>() {
                error
            } else {
                ServiceError::Unreachable(error.to_string()).into()
            }
        })?;
    let reply = ReplyFrame::from_bytes(&reply)?;
    let reply = reply
        .result
        .map_err(|error| -> Failure { Box::new(error) })?;
    if !request.accepts(&reply) {
        return Err("peer RPC reply kind mismatch".into());
    }
    Ok(reply)
}
pub fn route_enrollment(uri: &str, custody_contact: &str) -> Result<String, Failure> {
    fn contact(uri: &str) -> Result<ContactRecord, Failure> {
        let record = ContactRecord::parse(uri)?;
        Contact::checked(record.routes.clone())?;
        Ok(record)
    }
    let relay = contact(custody_contact)?;
    if crate::invitation::is_uri(uri) {
        crate::invitation::routed_through(uri, relay)
    } else if uri.trim().starts_with("rocky://provision/") {
        let mut bundle = ProvisioningBundle::parse(uri)?;
        if bundle.publisher != relay.publisher {
            return Err("relay belongs to another domain".into());
        }
        bundle.routes = relay.routes;
        bundle.via_custody = true;
        Ok(format!(
            "rocky://provision/{}",
            BASE64URL_NOPAD.encode(&serde_json::to_vec(&bundle)?)
        ))
    } else {
        let mut issuer = contact(uri)?;
        if !issuer.issuer {
            return Err("renewal routing needs an issuer contact".into());
        }
        if issuer.publisher != relay.publisher {
            return Err("relay belongs to another domain".into());
        }
        issuer.routes = relay.routes;
        issuer.enrollment_only = true;
        Ok(format!(
            "rocky://peer/{}",
            BASE64URL_NOPAD.encode(&serde_json::to_vec(&issuer)?)
        ))
    }
}

pub(crate) fn whole_partitions(
    domain: &Domain,
    grants: &[ResourceGrant],
) -> Vec<rocky_credentials::PartitionGrant> {
    let logical = domain.logical();
    grants
        .iter()
        .filter_map(|grant| {
            let table = logical.tables.iter().find(|table| {
                table.class == rocky_core::TableClass::Replayed
                    && logical.table_id(table) == grant.scope.table
            })?;
            if table.partition_by.is_empty() {
                return None;
            }
            Some(rocky_credentials::PartitionGrant {
                domain: grant.scope.domain,
                table: grant.scope.table,
                selector: rocky_core::PartitionSelector(
                    table
                        .partition_by
                        .iter()
                        .filter_map(|name| table.column(name))
                        .map(|column| {
                            (
                                logical.field_id(table, column),
                                rocky_core::PartitionSelection::Any,
                            )
                        })
                        .collect(),
                ),
                rows: grant.rows.clone(),
                rights: grant.rights.clone(),
                fields: grant.fields.clone(),
            })
        })
        .collect()
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredContact {
    pub(crate) contact: Contact,
    meta: Option<crate::mesh::ContactMeta>,
    issuer: bool,
    enrollment: Option<Contact>,
    upstream: bool,
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
                    enrollment: self.enrollment_routes.get(node).cloned(),
                    upstream: self.enrollment_upstream == Some(*node),
                },
            );
        }
        saved
    }
    pub(crate) fn persisted_eq(&self, other: &Self) -> bool {
        let Self {
            delivery_nodes: _,
            contact_refs: _,
            resolved_refs: _,
            version,
            installation,
            schema,
            mesh,
            domain,
            publisher,
            identity,
            single_use,
            genesis,
            generation,
            engine,
            pending,
            pending_follow,
            bootstrap,
            revocations,
            credentials,
            ttl,
            profile,
            keys,
            groups,
            epoch_statements,
            authority,
            device_revocations,
            issuers: _,
            enrollment_upstream: _,
            peers: _,
            contact_meta: _,
            enrollment_routes: _,
            logged_out,
            deliveries,
            claims,
            revocations_pushed,
        } = self;
        *version == other.version
            && *installation == other.installation
            && *schema == other.schema
            && *mesh == other.mesh
            && *domain == other.domain
            && *publisher == other.publisher
            && *identity == other.identity
            && *single_use == other.single_use
            && *genesis == other.genesis
            && *generation == other.generation
            && *engine == other.engine
            && *pending == other.pending
            && *pending_follow == other.pending_follow
            && *bootstrap == other.bootstrap
            && *revocations == other.revocations
            && *credentials == other.credentials
            && *ttl == other.ttl
            && *profile == other.profile
            && *keys == other.keys
            && *groups == other.groups
            && *epoch_statements == other.epoch_statements
            && *authority == other.authority
            && *device_revocations == other.device_revocations
            && *logged_out == other.logged_out
            && *deliveries == other.deliveries
            && *claims == other.claims
            && *revocations_pushed == other.revocations_pushed
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
            || self
                .contact_refs
                .values()
                .filter(|entry| entry.upstream)
                .count()
                > 1
        {
            return Err("identity holds too many contacts".into());
        }
        for entry in self.contact_refs.values() {
            Contact::checked(entry.contact.routes.clone())?;
            if let Some(contact) = &entry.enrollment {
                Contact::checked(contact.routes.clone())?;
                if !entry.issuer {
                    return Err("enrollment route requires an issuer".into());
                }
            }
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
            if let Some(contact) = &entry.enrollment {
                self.enrollment_routes.insert(node, contact.clone());
            }
            if entry.upstream {
                self.enrollment_upstream = Some(node);
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
        let contact = if entry.contact.routes.is_empty() {
            match &entry.enrollment {
                Some(contact) => contact,
                None => return Ok(None),
            }
        } else {
            &entry.contact
        };
        let addresses = contact.addresses(transport.kind());
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
    pub(crate) fn has_enrollment_upstream(&self) -> bool {
        self.enrollment_upstream.is_some() || self.contact_refs.values().any(|entry| entry.upstream)
    }
    pub(crate) async fn resolve_saved_upstream(
        &self,
        transport: &dyn Transport,
    ) -> Result<Option<PublicKey>, Failure> {
        let Some((id, _)) = self.contact_refs.iter().find(|(_, entry)| entry.upstream) else {
            return Ok(None);
        };
        self.resolve_saved_route(transport, *id).await
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
