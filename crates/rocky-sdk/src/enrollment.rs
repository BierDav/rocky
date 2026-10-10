//! The SDK's enrollment plumbing over `rocky-credentials`: domain
//! creation, invitations with a grant profile, supplied credentials,
//! renewal by any issuer, and revocation with key rotation. A device that
//! holds the domain's publisher key is an issuer; every device signs with
//! its own node key. See `docs/runtime/sdk-enrollment.md`.
use crate::{
    Failure,
    domain::Domain,
    groups::{GroupPartition, GroupRecord, Member, secret_role},
    registry::{self, Changes, Registry, RegistryFull},
    schema::Schema,
    secrets::{Vault, write_private},
    service::ServiceError,
    transport::{Protocol, Stream, Transport},
};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use rand::RngCore;
use rocky_core::{Commitment, DomainId, MeshId, ResourceGrant, Right, RowSelection, commitment};
use rocky_credentials::{
    Admission, AuthorRange, Authority, Credentials, Invitation, MAX_LIVE_REVOCATIONS, PairRequest,
    Profile, PseudonymRequest, REVOCATION_SET_FULL, RenewRequest, Renewal, Revocation,
    device_chain, open_enrollment, seal_enrollment,
};
use rocky_secure::{
    CredentialChain, KeyMaterial, PrivateKey, PublicKey, RevocationSet, SecureConfig,
    SignedCapability, SignedManifest, SignedRevocation,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Current `identity.json` format: issuer records live in the registry,
/// revocations are a set of statements, envelopes wait in `deliveries`.
pub const VERSION: u32 = 13;
/// Current invitation format, with an explicit custody routing flag.
const INVITE_VERSION: u32 = 4;
const CONTACT_VERSION: u32 = 2;
/// Contacts one device keeps: routes to peers, which grant nothing.
pub const MAX_CONTACTS: usize = 64;
/// Contacts one sync pass dials.
pub const MAX_DIALED: usize = 16;
const MAX_ISSUERS: usize = 8;
const MAX_ROUTES: usize = 4;
const MAX_ADDRESS: usize = 2048;
/// Default lifetime of issued credentials.
pub const DEFAULT_TTL: u64 = 30 * 24 * 60 * 60;
const INVITATION_TTL: u64 = 600;
/// Key epochs kept per collection: one per revocation that rotated it.
const MAX_EPOCHS: usize = 256;
/// A renewal answer carries the whole revocation set, hexadecimal in JSON.
const MAX_MESSAGE: usize = 3 * rocky_credentials::MAX_REVOCATION_SET_BYTES;
/// Accepted clock difference between devices.
const SKEW: u64 = 300;
/// Prefixes a pseudonym request a member sends an issuer over the renewal
/// protocol; the answer is the assigned chain.
pub(crate) const AUTHOR: &[u8] = b"rocky.sdk.author.v1\n";
type GroupDelivery = (GroupRecord, Vec<Vec<u8>>);
pub(crate) const EPOCHS: &[u8] = b"rocky.sdk.epochs.v1\n";
pub(crate) const POLICY: &[u8] = b"rocky.sdk.policy.v1\n";
const TIMEOUT: Duration = Duration::from_secs(15);

/// The statement that revokes this device.
#[derive(Debug)]
pub(crate) struct Revoked(pub SignedRevocation);
impl std::fmt::Display for Revoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("this device has been revoked")
    }
}
impl std::error::Error for Revoked {}

/// A revocation refused because 1,024 live statements are held.
#[derive(Debug)]
pub(crate) struct RevocationSetFull;
impl std::fmt::Display for RevocationSetFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REVOCATION_SET_FULL)
    }
}
impl std::error::Error for RevocationSetFull {}

/// `revoked.json`: the statement that revoked this device.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Notice {
    version: u32,
    statement: SignedRevocation,
}
const NOTICE_VERSION: u32 = 2;

/// The persisted identity of one installation. It holds no secret; keys
/// are in the `SecretStore` under `installation`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: u32,
    pub installation: String,
    pub schema: Schema,
    pub mesh: MeshId,
    pub domain: DomainId,
    /// The pinned trust anchor: the domain's publisher key.
    pub publisher: PublicKey,
    pub manifest: String,
    /// Every revocation statement this device verified.
    pub revocations: RevocationSet,
    pub credentials: CredentialChain,
    pub ttl: u64,
    /// What this device was enrolled as.
    pub profile: Profile,
    /// The key epochs held of each collection this device reads, ascending.
    pub keys: BTreeMap<String, Vec<rocky_core::KeyEpoch>>,
    /// Group partitions known here, by group address.
    pub groups: BTreeMap<String, GroupRecord>,
    /// Revocation statement IDs held when each epoch was sealed, by secret role.
    pub epoch_statements: BTreeMap<String, Vec<Commitment>>,
    /// Present on a device that holds the publisher key.
    pub authority: Option<Issuing>,
    /// The issuers that renew this device, by node key. Their routes are
    /// in `peers`.
    pub issuers: Vec<PublicKey>,
    /// A fixed issuer whose pairing/renewal responses this custody node relays.
    pub enrollment_upstream: Option<PublicKey>,
    /// This device's contacts, by node key, with the routes last learned
    /// for each. A contact is a route that grants nothing: any admitted
    /// peer is served, and sync dials contacts.
    pub peers: BTreeMap<PublicKey, Contact>,
    pub contact_meta: BTreeMap<PublicKey, crate::mesh::ContactMeta>,
    pub enrollment_routes: BTreeMap<PublicKey, Contact>,
    pub logged_out: bool,
    /// The generations the domain left, oldest first. Empty until its
    /// first upgrade; see `docs/runtime/generations.md`.
    pub lineage: Vec<crate::generation::Step>,
    /// The author numbers this device assigns to pseudonyms, if any.
    pub minting: Option<Minting>,
    /// This device's pseudonym chain for each collection it writes.
    pub pseudonyms: BTreeMap<String, CredentialChain>,
    /// Retained across removal/replacement of a supplied author range.
    pub author_high_water: u32,
    /// An issuer's assignment key, separate from every node/user identity.
    pub author_issuer: Option<PublicKey>,
    /// Envelopes this device sends, by delivery digest, until delivered.
    pub deliveries: BTreeMap<String, crate::delivery::Pending>,
}
/// An assigner's author numbers: its range and the next free one. It is
/// saved before any number it hands out leaves the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Minting {
    pub range: AuthorRange,
    pub next: u32,
}
impl Minting {
    fn fresh(range: AuthorRange) -> Self {
        Self {
            range,
            next: range.first,
        }
    }
    /// The next `count` free numbers, for a recipient that assigns them.
    fn carve(&mut self, count: u32) -> Result<AuthorRange, Failure> {
        let last = self
            .next
            .checked_add(
                count
                    .checked_sub(1)
                    .ok_or("assign at least one author number")?,
            )
            .filter(|last| *last <= self.range.last)
            .ok_or("not enough free author numbers")?;
        let carved = AuthorRange::new(self.next, last)?;
        self.next = last + 1;
        Ok(carved)
    }
    /// The upper half of the free numbers, handed to another issuer; this
    /// device keeps the lower half. None when fewer than two are free.
    fn split(&mut self) -> Option<AuthorRange> {
        let free = self.range.last.checked_sub(self.next)?;
        if free == 0 {
            return None;
        }
        let first = self.next + free / 2 + 1;
        let upper = AuthorRange::new(first, self.range.last).ok()?;
        self.range = AuthorRange::new(self.range.first, first - 1).ok()?;
        Some(upper)
    }
}
/// What an issuer keeps besides the registry: its invitation, the device
/// that claimed it, a revocation it has begun and the keys revoked for good.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issuing {
    pub invitation: Option<Invitation>,
    /// The device that consumed the invitation, so its retry is answered
    /// alike. Replaced with the invitation.
    pub claimed: Option<Claim>,
    /// Saved before the rotation's keys are made, so an interrupted
    /// revocation completes at the next open.
    pub rotation: Option<Rotation>,
    /// Every node and user key a statement this issuer learned revoked.
    /// Kept after the statement leaves the set: no renewal for them.
    pub tombstones: BTreeSet<PublicKey>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub node: PublicKey,
    pub user: PublicKey,
    /// The author numbers handed to an issuer profile.
    pub authors: Option<AuthorRange>,
}
/// A begun revocation: its statement and the key epoch each collection and
/// group partition rotates to. Signing is deterministic: the statement keeps
/// its ID however often an interrupted rotation signs it again.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rotation {
    pub node: PublicKey,
    pub statement: Revocation,
    pub statements: Vec<Commitment>,
    pub keys: BTreeMap<String, u64>,
    pub groups: BTreeMap<String, u64>,
}
/// What the registry recorded of a device a revocation acts on.
struct Revoking {
    user: Option<PublicKey>,
    capability: Option<Commitment>,
    /// Absent on a device recorded only by partition grants.
    profile: Option<Profile>,
    partitions: Vec<rocky_credentials::PartitionGrant>,
    pseudonyms: Vec<PublicKey>,
}
impl Revoking {
    fn of(record: &registry::Record) -> Self {
        Self {
            user: record.device.as_ref().map(|device| device.user),
            capability: record.capability().map(|capability| capability.id),
            profile: record.profile().cloned(),
            partitions: record
                .grants
                .iter()
                .map(|grant| grant.grant.clone())
                .collect(),
            pseudonyms: record
                .pseudonyms
                .iter()
                .map(|pseudonym| pseudonym.key)
                .collect(),
        }
    }
    fn selects(&self, partition: &GroupPartition) -> bool {
        partition.members.iter().any(|member| {
            self.partitions.iter().any(|grant| {
                grant
                    .resolve(member.scope.domain, member.scope.table, &member.tuple)
                    .ok()
                    .flatten()
                    .is_some()
            })
        })
    }
}
/// One way to reach a node: an address in a transport kind's format. A
/// hint only; whoever answers must still prove the node key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub transport: String,
    pub address: String,
}
/// The routes known for one node, at most one per transport kind.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub routes: Vec<Route>,
}
impl Contact {
    fn checked(routes: Vec<Route>) -> Result<Self, Failure> {
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
    /// Replaces the routes of the kinds `routes` name and keeps the others.
    pub(crate) fn learn(&mut self, routes: Vec<Route>) {
        for route in routes {
            self.routes
                .retain(|known| known.transport != route.transport);
            self.routes.push(route);
        }
        let excess = self.routes.len().saturating_sub(MAX_ROUTES);
        self.routes.drain(..excess);
    }
    /// The addresses to try over a transport of `kind`.
    pub fn addresses(&self, kind: &str) -> Vec<String> {
        self.routes
            .iter()
            .filter(|route| route.transport == kind)
            .map(|route| route.address.clone())
            .collect()
    }
}
/// What a contact URI carries: a node of the publisher's domain and its
/// routes. It grants nothing; see `docs/runtime/sdk-enrollment.md`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContactRecord {
    v: u32,
    publisher: PublicKey,
    node: PublicKey,
    routes: Vec<Route>,
    /// A hint that the node renews credentials. Its answers are believed
    /// only when the publisher key signed them.
    issuer: bool,
    enrollment_only: bool,
}
impl ContactRecord {
    fn parse(uri: &str) -> Result<Self, Failure> {
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
        if record.enrollment_only && !record.issuer {
            return Err("invalid enrollment-only contact".into());
        }
        Ok(record)
    }
}
/// What an invitation URI carries.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Invite {
    v: u32,
    routes: Vec<Route>,
    token: String,
    publisher: PublicKey,
    expires: u64,
    pub(crate) profile: Profile,
    via_custody: bool,
}
/// Sealed under a key derived from the invitation token.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Enrollment {
    mesh: MeshId,
    domain: DomainId,
    manifest: String,
    revocations: RevocationSet,
    capability: SignedCapability,
    profile: Profile,
    keys: Keys,
    groups: Vec<GroupDelivery>,
    epoch_statements: BTreeMap<String, Vec<Commitment>>,
    /// The enrolling issuer's node key, which sync sessions pin.
    issuer: PublicKey,
    ttl: u64,
    /// The domain's publisher key, for an issuer profile only.
    #[serde(default)]
    publisher_key: Option<String>,
    /// The generations the domain left, so a new device admits history
    /// and peers of earlier ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    lineage: Vec<crate::generation::Step>,
    /// For an issuer profile: the author numbers it assigns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authors: Option<AuthorRange>,
    /// Contacts of the other issuers the enrolling issuer reaches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    issuers: Vec<String>,
}
/// Reader keys in transit: collection, key epoch, hexadecimal key.
type Keys = BTreeMap<String, Vec<(rocky_core::KeyEpoch, String)>>;
/// What a renewal seals to its device: reader keys and the contacts of the
/// other issuers the answering issuer reaches.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sealed {
    keys: Keys,
    groups: Vec<GroupDelivery>,
    epoch_statements: BTreeMap<String, Vec<Commitment>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    issuers: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EpochRequest {
    pub request: Vec<u8>,
    pub revocations: RevocationSet,
}
impl Sealed {
    fn parse(bytes: &[u8]) -> Result<Self, Failure> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

impl Invite {
    pub(crate) fn parse(uri: &str) -> Result<Self, Failure> {
        let bytes = BASE64URL_NOPAD.decode(
            uri.trim()
                .strip_prefix("rocky://join/")
                .ok_or("invalid invitation URI")?
                .as_bytes(),
        )?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        if value["v"].as_u64() != Some(u64::from(INVITE_VERSION)) {
            return Err("unsupported invitation version".into());
        }
        let invite: Self = serde_json::from_value(value)?;
        Contact::checked(invite.routes.clone())?;
        Ok(invite)
    }
}
/// A pending join or installation, persisted before anything is sent, so
/// a retry is the same device.
#[derive(Serialize, Deserialize)]
struct Intent {
    /// The invitation a join answers; absent for supplied credentials.
    invitation: Option<Commitment>,
    installation: String,
    nonce: String,
}
impl Intent {
    fn path(identity: &Path, supplied: bool) -> std::path::PathBuf {
        identity.with_extension(if supplied {
            "install.json"
        } else {
            "join.json"
        })
    }
    /// The saved intent, or a new one whose device keys now exist.
    fn resume(
        path: &Path,
        invitation: Option<Commitment>,
        store: &std::sync::Arc<dyn crate::SecretStore>,
    ) -> Result<(Self, Vault), Failure> {
        if path.exists() {
            let previous: Self = serde_json::from_slice(&std::fs::read(path)?)?;
            if previous.invitation != invitation {
                return Err("a different join is unresolved; retry its invitation".into());
            }
            let vault = Vault::new(store.clone(), &previous.installation);
            return Ok((previous, vault));
        }
        let intent = Self {
            invitation,
            installation: HEXLOWER.encode(&random::<16>()),
            nonce: HEXLOWER.encode(&random::<32>()),
        };
        let vault = Vault::new(store.clone(), &intent.installation);
        for role in ["user", "node", "transport"] {
            vault.create(role)?;
        }
        write_private(path, &serde_json::to_vec(&intent)?)?;
        Ok((intent, vault))
    }
}
/// The public halves of a device's keys, for an issuer outside the SDK.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceKeys {
    /// The subject of the user capability to issue.
    pub user: PublicKey,
    /// The key that signs the device's operations.
    pub node: PublicKey,
}
/// Credentials and reader keys supplied to a device from outside the SDK.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Supplied {
    pub mesh: MeshId,
    pub domain: DomainId,
    /// The chain ends at the device's node key, or at its user key; the
    /// device then delegates to its own node.
    pub credentials: Credentials,
    /// Collection, key epoch and key of every collection the grants read.
    pub keys: Vec<(String, rocky_core::KeyEpoch, [u8; 32])>,
    pub epoch_statements: BTreeMap<String, Vec<Commitment>>,
    /// Contacts of the issuers that renew this device.
    pub issuers: Vec<String>,
}
/// The lifetime, further delegations and author numbers of granted or
/// delegated credentials. Absent values are this device's lifetime, 1 (the
/// recipient reaches only its own node key) and none.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Terms {
    pub ttl: Option<u64>,
    pub delegations: Option<u8>,
    /// How many of this device's free author numbers the recipient may
    /// assign to pseudonyms itself.
    pub authors: Option<u32>,
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
/// What a renewal brought: the issuer's revocations, this device's chain
/// and reader keys it did not hold.
pub struct Renewed {
    pub revocations: RevocationSet,
    pub chain: CredentialChain,
    pub keys: Vec<(String, rocky_core::KeyEpoch, [u8; 32])>,
    pub epoch_statements: BTreeMap<String, Vec<Commitment>>,
    pub groups: Vec<GroupDelivery>,
    /// What the grants amount to when they changed, as supplied ones may.
    pub profile: Option<Profile>,
    /// Contacts of further issuers the answering issuer reaches.
    pub issuers: Vec<String>,
}

pub fn now() -> Result<u64, Failure> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
pub(crate) fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes
}
fn hex32(text: &str) -> Result<[u8; 32], Failure> {
    HEXLOWER
        .decode(text.as_bytes())?
        .try_into()
        .map_err(|_| "expected 32 bytes".into())
}
/// The key provider role of a collection's reader key at `epoch`. Epoch 1
/// keeps the name it had before key epochs.
pub(crate) fn key_role(collection: &str, epoch: rocky_core::KeyEpoch) -> String {
    format!(
        "reader.{collection}.{}.{}",
        epoch.generation,
        HEXLOWER.encode(&epoch.secret_hash)
    )
}
/// The key provider role of this device's pseudonym key in `collection`.
fn pseudonym_role(collection: &str) -> String {
    format!("pseudonym.{collection}")
}
/// What a joining device expects to join.
fn context(schema: &Schema) -> Result<Commitment, Failure> {
    Ok(commitment(
        "rocky.sdk.schema.v1",
        &serde_json::to_vec(schema)?,
    ))
}
fn reads(grants: &[ResourceGrant], scope: rocky_core::ResourceScope) -> bool {
    grants.iter().any(|grant| {
        grant.scope == scope
            && grant.rows == RowSelection::All
            && grant.rights.contains(&Right::Read)
    })
}
/// The profile a set of grants amounts to: an issuer, whose grants alone
/// write the registry, a reader, writer or custody, or raw grants. The
/// registry grant the SDK adds is not part of the others.
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
/// The signed grants of `profile`: its grants over the application's
/// collections, and the registry's for an issuer (read and write) or a
/// custody peer (ciphertext only). No other profile names the registry.
pub(crate) fn grants_of(profile: &Profile, domain: &Domain) -> Result<Vec<ResourceGrant>, Failure> {
    let mut grants = profile.grants(&domain.resources())?;
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
/// The command identity a registry record names for `node`.
pub(crate) fn command_identity(node: PublicKey) -> String {
    HEXLOWER.encode(&rocky_secure::secure_node_id(node).0)
}

impl Settings {
    /// The statement of `set` that revokes this device, once the whole set
    /// verifies.
    pub(crate) fn revoking(
        &self,
        set: &RevocationSet,
    ) -> Result<Option<SignedRevocation>, Failure> {
        set.verify(self.publisher, self.domain()?.manifest().app)?;
        Ok(set.revoking(&self.credentials)?.cloned())
    }

    pub(crate) fn revocation(&self, path: &Path) -> Result<Option<SignedRevocation>, Failure> {
        let bytes = match std::fs::read(path.with_file_name("revoked.json")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        if value["version"].as_u64() != Some(u64::from(NOTICE_VERSION)) {
            return Err("unsupported device revocation notice".into());
        }
        let notice: Notice = serde_json::from_value(value)?;
        let set = RevocationSet::try_from(vec![notice.statement.clone()])?;
        if self.revoking(&set)?.is_none() {
            return Err("invalid device revocation notice".into());
        }
        Ok(Some(notice.statement))
    }

    pub(crate) fn record_revocation(
        &self,
        path: &Path,
        statement: SignedRevocation,
    ) -> Result<(), Failure> {
        let set = RevocationSet::try_from(vec![statement.clone()])?;
        if self.revoking(&set)?.is_none() {
            return Err("the statement does not revoke this device".into());
        }
        write_private(
            &path.with_file_name("revoked.json"),
            &serde_json::to_vec(&Notice {
                version: NOTICE_VERSION,
                statement,
            })?,
        )?;
        Ok(())
    }

    /// Merges `set` into this identity's revocations. An issuer also
    /// tombstones every node and user key they revoke, for good; its next
    /// registry commit writes the records' tombstones. Returns the
    /// statements added.
    pub(crate) fn learn_revocations(
        &mut self,
        set: &RevocationSet,
    ) -> Result<Vec<SignedRevocation>, Failure> {
        let app = self.domain()?.manifest().app;
        let added = self.revocations.merge(set, self.publisher, app, now()?)?;
        if let Some(authority) = &mut self.authority {
            for signed in self.revocations.statements() {
                let statement = &signed.statement;
                authority
                    .tombstones
                    .extend(statement.nodes.iter().chain(&statement.users));
            }
        }
        Ok(added)
    }
    /// The digest and live count of the held revocations, as status shows them.
    pub(crate) fn revocation_status(&self) -> Result<serde_json::Value, Failure> {
        let time = now()?;
        Ok(serde_json::json!({
            "digest": HEXLOWER.encode(&self.revocations.digest(time)?),
            "statements": self.revocations.live(time),
        }))
    }
    /// Whether `set` holds a live statement this identity lacks.
    pub(crate) fn lacks(&self, set: &RevocationSet) -> Result<bool, Failure> {
        let time = now()?;
        for signed in set.statements() {
            if signed.statement.live(time) && self.revocations.get(&signed.id()?).is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// Whether a pseudonym chain of this device is still usable: of this
    /// definition, unexpired and ended by no held statement.
    fn current_pseudonym(&self, chain: &CredentialChain, manifest: Commitment, time: u64) -> bool {
        chain.0.iter().all(|credential| {
            credential.capability.manifest == manifest
                && credential.capability.expires > time
                && self.revocations.ends(credential).is_ok_and(|ended| !ended)
        })
    }

    /// Loads only the current identity format, without changing the file or keys.
    pub fn load(path: &Path) -> Result<Self, Failure> {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
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
        settings.check_deliveries()?;
        Ok(settings)
    }
    pub fn save(&self, path: &Path) -> Result<(), Failure> {
        write_private(path, &serde_json::to_vec(self)?)
    }
    pub fn vault(&self, store: &std::sync::Arc<dyn crate::SecretStore>) -> Vault {
        Vault::new(store.clone(), &self.installation)
    }
    /// Whether this device holds the publisher key.
    pub fn issuer(&self) -> bool {
        self.authority.is_some()
    }
    pub fn domain(&self) -> Result<Domain, Failure> {
        let publisher = *self.publisher.as_bytes();
        let (generation, added) =
            crate::generation::lineage(&self.lineage, &self.schema, publisher)?;
        Domain::at(
            &self.schema,
            publisher,
            self.mesh,
            self.domain,
            generation,
            &added,
        )
    }
    pub fn manifest(&self) -> Result<SignedManifest, Failure> {
        Ok(SignedManifest::from_bytes(
            &HEXLOWER.decode(self.manifest.as_bytes())?,
        )?)
    }
    /// The grants of this device's node: its chain's leaf.
    fn grants(&self) -> &[ResourceGrant] {
        self.credentials.grants()
    }
    /// The paired grants this device's selectors resolve on one partition.
    fn selected(
        &self,
        scope: &rocky_core::ResourceScope,
        tuple: &rocky_core::PartitionTuple,
    ) -> Vec<ResourceGrant> {
        self.credentials
            .0
            .last()
            .and_then(|leaf| leaf.capability.extended().ok())
            .map(|extended| {
                extended
                    .partitions
                    .iter()
                    .filter_map(|grant| {
                        grant
                            .resolve(scope.domain, scope.table, tuple)
                            .ok()
                            .flatten()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    /// Whether this device may read a partition in whole, as its key needs.
    pub(crate) fn reads_partition(&self, member: &Member) -> bool {
        self.grants()
            .iter()
            .chain(&self.selected(&member.scope, &member.tuple))
            .any(|grant| {
                grant.scope == member.scope
                    && grant.rows == RowSelection::All
                    && grant.rights.contains(&Right::Read)
            })
    }
    /// The members of every known group partition under `domain`'s schema.
    pub(crate) fn partitions(&self, domain: &Domain) -> Result<Vec<GroupPartition>, Failure> {
        self.groups
            .values()
            .map(|record| GroupPartition::of(domain, record))
            .collect()
    }
    /// Whether this device may read `scope` in whole, as a checkpoint needs.
    pub fn reads_whole(&self, domain: &Domain, scope: &rocky_core::ResourceScope) -> bool {
        let concrete = self.grants().iter().any(|grant| {
            grant.scope == *scope
                && grant.rows == RowSelection::All
                && grant.rights.contains(&Right::Read)
        });
        concrete
            || self.partitions(domain).is_ok_and(|partitions| {
                partitions
                    .iter()
                    .flat_map(|partition| &partition.members)
                    .any(|member| member.scope == *scope && self.reads_partition(member))
            })
    }
    /// The scopes this device's credentials name: what its sessions and
    /// its sync cover.
    pub fn granted(&self, domain: &Domain) -> Vec<rocky_core::ResourceScope> {
        let partitions = self.partitions(domain).unwrap_or_default();
        domain
            .scopes()
            .map(|(_, scope)| scope)
            .filter(|scope| self.grants().iter().any(|grant| grant.scope == *scope))
            .chain(
                partitions
                    .iter()
                    .flat_map(|partition| &partition.members)
                    .filter(|member| {
                        self.grants()
                            .iter()
                            .any(|grant| grant.scope == member.scope)
                            || !self.selected(&member.scope, &member.tuple).is_empty()
                    })
                    .map(|member| member.scope),
            )
            .collect()
    }
    /// The peers whose credentials and frontiers the replica keeps whatever
    /// its bound on unconfigured peers: contacts and issuers.
    pub fn kept(&self) -> Vec<PublicKey> {
        self.peers
            .keys()
            .chain(&self.issuers)
            .chain(&self.enrollment_upstream)
            .copied()
            .collect()
    }
    /// The contacts a sync may dial, issuers first: those with routes and
    /// not revoked.
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
    /// This node's contact URI over `transport`.
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
    /// Configures the peer a contact URI names, or learns its new routes.
    /// A contact that says so is also asked for renewals.
    pub fn add_peer(&mut self, vault: &Vault, uri: &str) -> Result<PublicKey, Failure> {
        let record = ContactRecord::parse(uri)?;
        if record.publisher != self.publisher {
            return Err("the contact belongs to another domain".into());
        }
        if record.node == PrivateKey::from_bytes(&vault.get("node")?).public_key() {
            return Err("the contact names this device".into());
        }
        let learned = Contact::checked(record.routes)?;
        if self.revoked(&record.node) {
            return Err("the contact names a revoked device".into());
        }
        if !self.peers.contains_key(&record.node) && self.peers.len() >= MAX_CONTACTS {
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
    /// Forgets a peer, an issuer included: any issuer renews any device,
    /// and a member left without issuer contact reports it.
    pub fn remove_peer(&mut self, node: PublicKey) -> Result<(), Failure> {
        self.peers.remove(&node).ok_or("unknown peer")?;
        self.contact_meta.remove(&node);
        self.issuers.retain(|issuer| *issuer != node);
        self.enrollment_routes.remove(&node);
        Ok(())
    }
    /// The earliest expiry of the node's credential chain.
    pub fn expires(&self) -> u64 {
        self.credentials.expires()
    }
    /// The open configuration of the secure replica: the chain and every
    /// held collection key epoch. A missing key is an error, never a new
    /// key. Partition keys follow once their addresses are registered.
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
        Ok(SecureConfig {
            publisher: self.publisher,
            manifest: self.manifest()?,
            revocations: self.revocations.clone(),
            node: PrivateKey::from_bytes(&vault.get("node")?),
            credentials: self.credentials.clone(),
            keys,
        })
    }
    /// Every secret role this installation holds.
    pub fn roles(&self) -> Vec<String> {
        let mut roles: Vec<String> = [
            "node",
            "user",
            "transport",
            "publisher",
            "invitation",
            "author-issuer",
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
            roles.push(format!("generation.reader.{name}"));
        }
        if let Ok(domain) = self.domain() {
            roles.extend(domain.scopes().map(|(name, _)| pseudonym_role(name)));
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
        // A rotation may have made keys the identity does not list yet.
        if let Some(rotation) = self.authority.as_ref().and_then(|a| a.rotation.as_ref()) {
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
    /// The stable key of this node's routes.
    pub fn transport_key(vault: &Vault) -> Result<[u8; 32], Failure> {
        Ok(vault.get("transport")?)
    }
    /// Whether this device can reach an issuer to renew: it is one, or a
    /// configured issuer has routes.
    pub fn issuer_contact(&self) -> bool {
        self.issuer()
            || self.issuers.iter().any(|issuer| {
                self.peers
                    .get(issuer)
                    .is_some_and(|contact| !contact.routes.is_empty())
                    || self.enrollment_routes.contains_key(issuer)
            })
    }
    /// Whether another holder of the publisher key is known.
    pub fn another_issuer(&self, registry: &Registry, own: PublicKey) -> bool {
        !self.issuers.is_empty()
            || registry.records.iter().any(|(node, record)| {
                *node != own && !record.revoked && record.profile().is_some_and(Profile::issues)
            })
    }

    /// A new domain whose publisher, user and node keys this device holds.
    pub fn create(
        schema: &Schema,
        store: &std::sync::Arc<dyn crate::SecretStore>,
        ttl: u64,
    ) -> Result<Self, Failure> {
        let installation = HEXLOWER.encode(&random::<16>());
        let vault = Vault::new(store.clone(), &installation);
        let publisher = PrivateKey::from_bytes(&vault.create("publisher")?);
        let user = PrivateKey::from_bytes(&vault.create("user")?);
        let node = PrivateKey::from_bytes(&vault.create("node")?);
        vault.create("transport")?;
        let (mesh, domain) = (MeshId(random()), DomainId(random()));
        let bound = Domain::new(schema, *publisher.public_key().as_bytes(), mesh, domain)?;
        crate::mesh::create_root(&vault, &publisher, bound.scope)?;
        // The registry's key is made like any collection's.
        let mut keys = BTreeMap::new();
        for (name, _) in bound.scopes() {
            let key = random::<32>();
            let epoch = rocky_core::KeyEpoch::new(1, &key);
            vault.put(&key_role(name, epoch), &key)?;
            keys.insert(name.to_owned(), vec![epoch]);
        }
        let manifest = SignedManifest::sign(&bound.manifest(), &publisher)?;
        let mut settings = Self {
            version: VERSION,
            installation,
            schema: schema.clone(),
            mesh,
            domain,
            publisher: publisher.public_key(),
            manifest: HEXLOWER.encode(&manifest.to_bytes()?),
            revocations: RevocationSet::new(),
            credentials: CredentialChain(Vec::new()),
            ttl,
            profile: Profile::Issuer,
            keys,
            authority: Some(Issuing::default()),
            issuers: Vec::new(),
            enrollment_upstream: None,
            contact_meta: BTreeMap::new(),
            peers: BTreeMap::new(),
            enrollment_routes: BTreeMap::new(),
            logged_out: false,
            lineage: Vec::new(),
            minting: Some(Minting::fresh(AuthorRange::ALL)),
            pseudonyms: BTreeMap::new(),
            author_high_water: 0,
            author_issuer: None,
            deliveries: BTreeMap::new(),
            groups: BTreeMap::new(),
            epoch_statements: BTreeMap::new(),
        };
        let grants = grants_of(&Profile::Issuer, &bound)?;
        let capability = settings.issue(
            &publisher,
            &bound,
            user.public_key(),
            grants,
            Some(AuthorRange::ALL),
        )?;
        settings.credentials = settings.chain(capability, &user, &node, &bound)?;
        Ok(settings)
    }
    fn validity(&self) -> Result<(u64, u64, u64), Failure> {
        Ok((now()?, self.ttl, SKEW))
    }
    /// A user capability for `user`, signed by the publisher, assigning
    /// `authors` if given.
    fn issue(
        &self,
        publisher: &PrivateKey,
        domain: &Domain,
        user: PublicKey,
        grants: Vec<ResourceGrant>,
        authors: Option<AuthorRange>,
    ) -> Result<SignedCapability, Failure> {
        let authority = Authority {
            publisher,
            manifest: domain.manifest().hash(),
        };
        let partitions = whole_partitions(domain, &grants);
        Ok(authority.issue_partition_terms(
            user,
            grants,
            partitions,
            (1, authors),
            self.validity()?,
        )?)
    }
    /// The node's chain under a user capability, verified.
    fn chain(
        &self,
        capability: SignedCapability,
        user: &PrivateKey,
        node: &PrivateKey,
        domain: &Domain,
    ) -> Result<CredentialChain, Failure> {
        let chain = device_chain(capability, user, node.public_key())?;
        chain.verify_selectors(
            self.publisher,
            domain.manifest().hash(),
            node.public_key(),
            &self.revocations,
            now()?,
        )?;
        Ok(chain)
    }
    fn issuing(&mut self, action: &str) -> Result<&mut Issuing, Failure> {
        self.authority
            .as_mut()
            .ok_or_else(|| format!("only an issuer {action}").into())
    }
    /// The current reader key of every collection `grants` read whole.
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

    fn groups_for(
        &self,
        vault: &Vault,
        node: PublicKey,
        capability: &rocky_credentials::Capability,
        all: bool,
    ) -> Result<Vec<GroupDelivery>, Failure> {
        let domain = self.domain()?;
        let grants = capability.extended()?.partitions;
        let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
        let mut deliveries = Vec::new();
        for partition in self.partitions(&domain)? {
            let Some(current) = partition.record.current() else {
                continue;
            };
            if !partition.members.iter().all(|member| {
                grants.iter().any(|grant| {
                    grant
                        .resolve(member.scope.domain, member.scope.table, &member.tuple)
                        .ok()
                        .flatten()
                        .is_some_and(|grant| {
                            grant.rows == RowSelection::All && grant.rights.contains(&Right::Read)
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
                        &publisher,
                        domain.manifest().hash(),
                        record.address,
                        epoch.generation,
                        vault.get(&role)?,
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
    fn adopt_groups(
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
                .any(|member| !self.reads_partition(member))
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
                let secret = envelope.open(
                    &node,
                    self.publisher,
                    domain.manifest().hash(),
                    offered.address,
                    epoch,
                )?;
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
    /// Opens a new invitation for `profile`, replacing an earlier one.
    pub async fn invite(
        &mut self,
        path: &Path,
        vault: &Vault,
        transport: &dyn Transport,
        profile: Profile,
        registry: &Registry,
    ) -> Result<(String, u64), Failure> {
        grants_of(&profile, &self.domain()?)?;
        self.issuing("invites devices")?;
        if registry.devices() >= registry::MAX_DEVICES {
            return Err(Box::new(RegistryFull));
        }
        let address = transport.address().await?;
        // Nothing was sealed under an unused invitation token.
        let token = vault.replace("invitation")?;
        let expires = now()? + INVITATION_TTL;
        let issuing = self.issuing("invites devices")?;
        issuing.invitation = Some(Invitation::open(&token, profile.clone(), expires));
        issuing.claimed = None;
        self.save(path)?;
        let invite = Invite {
            v: INVITE_VERSION,
            routes: vec![Route {
                transport: transport.kind().into(),
                address,
            }],
            token: HEXLOWER.encode(&token),
            publisher: self.publisher,
            expires,
            profile,
            via_custody: false,
        };
        let uri = format!(
            "rocky://join/{}",
            BASE64URL_NOPAD.encode(&serde_json::to_vec(&invite)?)
        );
        Ok((uri, expires))
    }

    /// Answers one pairing request with the sealed enrollment and the
    /// registry rows to commit before it leaves. Save the identity first,
    /// even on failure: it counts the invitation's attempts.
    pub fn pair(
        &mut self,
        vault: &Vault,
        request: &[u8],
        registry: &Registry,
    ) -> Result<(Vec<u8>, Changes), Failure> {
        let request = PairRequest::from_bytes(request)?;
        let domain = self.domain()?;
        let token = vault.get("invitation")?;
        let issuer = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
        let expected = context(&self.schema)?;
        let time = now()?;
        let authority = self.issuing("enrolls devices")?;
        let active = authority
            .invitation
            .as_mut()
            .ok_or("no active invitation")?;
        let admission = active.admit(&token, &request, time)?;
        // A mismatch does not consume the invitation.
        if request.context != expected {
            return Err("application schema mismatch".into());
        }
        let claimed = authority
            .claimed
            .as_ref()
            .filter(|claim| claim.node == request.node && claim.user == request.user)
            .cloned();
        match admission {
            Admission::First if registry.get(&request.node).is_some() => {
                return Err("device already enrolled".into());
            }
            // The bound refuses before the invitation is consumed.
            Admission::First if registry.devices() >= registry::MAX_DEVICES => {
                return Err(Box::new(RegistryFull));
            }
            Admission::Retry if claimed.is_none() => {
                return Err("invitation already used".into());
            }
            _ => {}
        }
        active.claim(&request);
        let profile = active.profile.clone();
        let grants = grants_of(&profile, &domain)?;
        // A retry carves nothing again: the first answer's range is kept.
        let authors = match (profile.issues(), &claimed) {
            (true, None) => self.minting.as_mut().and_then(Minting::split),
            (true, Some(claim)) => claim.authors,
            _ => None,
        };
        self.issuing("enrolls devices")?.claimed = Some(Claim {
            node: request.node,
            user: request.user,
            authors,
        });
        let issued = self.issue(&publisher, &domain, request.user, grants.clone(), authors)?;
        self.learn_joined_peer(request.node, &request.routes, profile.issues());
        let mut changes = Changes::default();
        changes.device(
            &request.node,
            &registry::Device {
                user: request.user,
                kind: registry::Kind::Paired,
                profile: Some(profile.clone()),
                issuer,
                command: command_identity(request.node),
                enrolled: time,
            },
            false,
        )?;
        changes.capability(
            &request.node,
            registry::Capability {
                id: issued.id()?,
                expires: issued.capability.expires,
            },
            registry.get(&request.node),
        )?;
        let enrollment = Enrollment {
            groups: self.groups_for(vault, request.node, &issued.capability, false)?,
            epoch_statements: self.statements_for(&self.keys_for(vault, &domain, &grants)?),
            mesh: self.mesh,
            domain: self.domain,
            manifest: self.manifest.clone(),
            revocations: self.revocations.clone(),
            capability: issued,
            keys: self.keys_for(vault, &domain, &grants)?,
            issuer,
            ttl: self.ttl,
            // An issuer device holds the publisher key under its own
            // provider and signs with its own node key.
            publisher_key: profile
                .issues()
                .then(|| HEXLOWER.encode(publisher.as_bytes())),
            profile,
            lineage: self.lineage.clone(),
            authors,
            issuers: self.issuer_contacts(&[request.node])?,
        };
        let sealed = seal_enrollment(&token, &request, &serde_json::to_vec(&enrollment)?)?;
        Ok((sealed, changes))
    }

    /// Issues `profile`, raw grants included, to the device whose public
    /// keys `device` names: credentials and reader keys it installs with
    /// `Database::install`, on `terms`, and the registry rows that record
    /// it. Save the identity and commit the rows before handing them out.
    /// Author numbers are carved afresh each time. The result carries
    /// reader keys: a secret.
    pub fn grant(
        &mut self,
        vault: &Vault,
        device: DeviceKeys,
        (profile, terms): (Profile, Terms),
        contact: Option<String>,
        registry: &Registry,
    ) -> Result<(Supplied, Changes), Failure> {
        let ttl = terms.ttl(self.ttl)?;
        if profile.issues() {
            return Err("the publisher key travels only in an issuer invitation".into());
        }
        let domain = self.domain()?;
        let grants = grants_of(&profile, &domain)?;
        self.issuing("grants credentials")?;
        let known = registry.get(&device.node);
        if self.revoked_device(&device.node, registry) || self.revoked(&device.user) {
            return Err("device was revoked".into());
        }
        match known.and_then(|known| known.device.as_ref()) {
            Some(known) if known.user != device.user => {
                return Err("this node is known with another user key".into());
            }
            None if registry.devices() >= registry::MAX_DEVICES => {
                return Err(Box::new(RegistryFull));
            }
            _ => {}
        }
        let authors = match terms.authors {
            Some(count) => Some(self.minting()?.carve(count)?),
            None => None,
        };
        let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
        let time = now()?;
        let capability = Authority {
            publisher: &publisher,
            manifest: domain.manifest().hash(),
        }
        .issue_partition_terms(
            device.user,
            grants.clone(),
            whole_partitions(&domain, &grants),
            (terms.delegations.unwrap_or(1), authors),
            (time, ttl, SKEW),
        )?;
        let mut keys = Vec::new();
        for (name, epochs) in self.keys_for(vault, &domain, &grants)? {
            for (epoch, key) in epochs {
                keys.push((name.clone(), epoch, hex32(&key)?));
            }
        }
        let issuer = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        let mut changes = Changes::default();
        // A grant of another profile replaces what the record said.
        let replace = known
            .and_then(registry::Record::profile)
            .is_some_and(|known| *known != profile);
        changes.device(
            &device.node,
            &registry::Device {
                user: device.user,
                kind: registry::Kind::Granted,
                profile: Some(profile),
                issuer,
                command: command_identity(device.node),
                enrolled: time,
            },
            replace,
        )?;
        changes.capability(
            &device.node,
            registry::Capability {
                id: capability.id()?,
                expires: capability.capability.expires,
            },
            known,
        )?;
        let supplied = Supplied {
            epoch_statements: self.statements_for_keys(&keys),
            mesh: self.mesh,
            domain: self.domain,
            credentials: Credentials {
                publisher: self.publisher,
                manifest: self.manifest()?,
                revocations: self.revocations.clone(),
                chain: CredentialChain(vec![capability]),
            },
            keys,
            issuers: contact
                .into_iter()
                .chain(self.issuer_contacts(&[device.node])?)
                .collect(),
        };
        Ok((supplied, changes))
    }
    /// Delegates `profile`, which must narrow this device's own grants, to
    /// the device whose public keys `device` names: a link signed by this
    /// device's user key, with the reader keys the grants read. Any device
    /// whose capability allows a further delegation does this; the
    /// delegate renews by being delegated to again. `authors` carves fresh
    /// numbers from this device's range each time. On an issuer the
    /// registry rows recording the delegate come with it.
    pub fn delegate(
        &mut self,
        vault: &Vault,
        device: DeviceKeys,
        (profile, terms): (Profile, Terms),
        registry: Option<&Registry>,
    ) -> Result<(Supplied, Changes), Failure> {
        let ttl = terms.ttl(self.ttl)?;
        if profile.issues() {
            return Err("the publisher key travels only in an issuer invitation".into());
        }
        if self.tombstoned(&device.node) || self.tombstoned(&device.user) {
            return Err("device was revoked".into());
        }
        let domain = self.domain()?;
        let grants = profile.grants(&domain.resources())?;
        if let Some(registry) = registry {
            if self.revoked_device(&device.node, registry) || self.revoked(&device.user) {
                return Err("device was revoked".into());
            }
            if registry
                .get(&device.node)
                .is_none_or(|record| record.device.is_none())
                && registry.devices() >= registry::MAX_DEVICES
            {
                return Err(Box::new(RegistryFull));
            }
        }
        let user = PrivateKey::from_bytes(&vault.get("user")?);
        let authors = match terms.authors {
            Some(count) => Some(self.minting()?.carve(count)?),
            None => None,
        };
        let chain = self.credentials.delegable().delegate(
            &user,
            device.user,
            rocky_core::PrincipalKind::User,
            grants.clone(),
            terms.delegations.unwrap_or(1),
            authors,
            (now()?, ttl, SKEW),
        )?;
        let mut keys = Vec::new();
        for (name, epochs) in self.keys_for(vault, &domain, &grants)? {
            for (epoch, key) in epochs {
                keys.push((name.clone(), epoch, hex32(&key)?));
            }
        }
        let mut changes = Changes::default();
        if let Some(registry) = registry {
            let known = registry.get(&device.node);
            let leaf = chain.0.last().ok_or("empty delegation")?;
            changes.device(
                &device.node,
                &registry::Device {
                    user: device.user,
                    kind: registry::Kind::Delegated,
                    profile: Some(profile),
                    issuer: PrivateKey::from_bytes(&vault.get("node")?).public_key(),
                    command: command_identity(device.node),
                    enrolled: now()?,
                },
                false,
            )?;
            changes.capability(
                &device.node,
                registry::Capability {
                    id: leaf.id()?,
                    expires: chain.expires(),
                },
                known,
            )?;
        }
        let supplied = Supplied {
            epoch_statements: self.statements_for_keys(&keys),
            mesh: self.mesh,
            domain: self.domain,
            credentials: Credentials {
                publisher: self.publisher,
                manifest: self.manifest()?,
                revocations: self.revocations.clone(),
                chain,
            },
            keys,
            issuers: Vec::new(),
        };
        Ok((supplied, changes))
    }
    /// Replaces this device's credentials with supplied ones, such as a
    /// delegator's renewed delegation. They are verified like an
    /// installation's; keys of collections they no longer read are
    /// dropped by the replica. An issuer renews itself instead.
    pub fn supplied_renewal(&self, vault: &Vault, supplied: &Supplied) -> Result<Renewed, Failure> {
        if self.issuer() {
            return Err("an issuer renews its own credentials".into());
        }
        let credentials = &supplied.credentials;
        let domain = self.domain()?;
        if supplied.mesh != self.mesh
            || supplied.domain != self.domain
            || credentials.publisher != self.publisher
            || credentials.manifest.verify(self.publisher)?.hash() != domain.manifest().hash()
        {
            return Err("the credentials belong to another domain or definition".into());
        }
        let user = PrivateKey::from_bytes(&vault.get("user")?);
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let mut chain = credentials.chain.clone();
        if chain
            .0
            .last()
            .is_some_and(|last| last.capability.subject == user.public_key())
        {
            chain = chain.complete(&user, node.public_key())?;
        }
        let mut revocations = self.revocations.clone();
        revocations.merge(
            &credentials.revocations,
            self.publisher,
            domain.manifest().app,
            now()?,
        )?;
        let (grants, _) = chain.verify_selectors(
            self.publisher,
            domain.manifest().hash(),
            node.public_key(),
            &revocations,
            now()?,
        )?;
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
            revocations: credentials.revocations.clone(),
            chain,
            epoch_statements: supplied.epoch_statements.clone(),
            groups: Vec::new(),
            keys,
            profile: Some(profile_of(&grants, &domain)),
            issuers: Vec::new(),
        })
    }
    fn minting(&mut self) -> Result<&mut Minting, Failure> {
        self.minting.as_mut().ok_or_else(|| {
            "this device assigns no author numbers; claim an author range first".into()
        })
    }
    /// The collection a table commitment names, if any.
    pub fn collection_name(&self, table: Commitment) -> Option<String> {
        self.collection_of(table).ok()
    }
    /// The collection whose table `table` names.
    fn collection_of(&self, table: Commitment) -> Result<String, Failure> {
        let domain = self.domain()?;
        let name = domain
            .scopes()
            .find(|(_, scope)| scope.table == table)
            .map(|(name, _)| name.to_owned())
            .ok_or("the pseudonym names no collection of this schema")?;
        Ok(name)
    }
    /// This device's request for a pseudonym in `collection`: its key is
    /// made once and kept, and an earlier pseudonym keeps its number.
    pub fn pseudonym_request(
        &self,
        vault: &Vault,
        collection: &str,
    ) -> Result<PseudonymRequest, Failure> {
        let domain = self.domain()?;
        let (_, scope) = domain
            .scopes()
            .find(|(name, _)| *name == collection)
            .ok_or("unknown collection")?;
        let role = pseudonym_role(collection);
        let key = match vault.find(&role)? {
            Some(key) => key,
            None => vault.create(&role)?,
        };
        Ok(PseudonymRequest::sign(
            &PrivateKey::from_bytes(&vault.get("node")?),
            &PrivateKey::from_bytes(&key),
            self.credentials.clone(),
            scope.table,
            self.pseudonyms.get(collection).cloned(),
            now()?,
        )?)
    }
    /// The collections this device writes but holds no current pseudonym
    /// for, in name order.
    pub fn missing_authors(&self, domain: &Domain) -> Result<Vec<String>, Failure> {
        let grants = self.grants();
        let partitions = self
            .credentials
            .0
            .last()
            .ok_or("empty device credential chain")?
            .capability
            .extended()?
            .partitions;
        let manifest = domain.manifest().hash();
        let time = now()?;
        let current = |collection: &str| {
            self.pseudonyms
                .get(collection)
                .is_some_and(|chain| self.current_pseudonym(chain, manifest, time))
        };
        // The registry's author is installed when an issuer first writes it.
        Ok(domain
            .collections()
            .filter(|(_, scope)| {
                grants.iter().any(|grant| {
                    grant.scope.domain == scope.domain
                        && grant.scope.table == scope.table
                        && [Right::Insert, Right::Update, Right::Delete]
                            .iter()
                            .any(|right| grant.rights.contains(right))
                }) || partitions.iter().any(|grant| {
                    grant.domain == scope.domain
                        && grant.table == scope.table
                        && [Right::Insert, Right::Update, Right::Delete]
                            .iter()
                            .any(|right| grant.rights.contains(right))
                })
            })
            .map(|(name, _)| name)
            .filter(|name| !current(name))
            .map(str::to_owned)
            .collect())
    }
    /// Whether this device holds a current pseudonym for `collection`.
    pub(crate) fn holds_author(&self, domain: &Domain, collection: &str) -> Result<bool, Failure> {
        let manifest = domain.manifest().hash();
        let time = now()?;
        Ok(self
            .pseudonyms
            .get(collection)
            .is_some_and(|chain| self.current_pseudonym(chain, manifest, time)))
    }
    /// The rows of this issuer's own record: its device entry, current
    /// capability and pseudonyms. Rows that exist are kept.
    pub(crate) fn own_record(
        &self,
        vault: &Vault,
        registry: &Registry,
    ) -> Result<Changes, Failure> {
        let domain = self.domain()?;
        let node = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        let known = registry.get(&node);
        let root = self.credentials.0.first().ok_or("missing credentials")?;
        let mut changes = Changes::default();
        changes.device(
            &node,
            &registry::Device {
                user: root.capability.subject,
                kind: registry::Kind::Created,
                profile: Some(self.profile.clone()),
                issuer: node,
                command: command_identity(node),
                enrolled: now()?,
            },
            false,
        )?;
        changes.capability(
            &node,
            registry::Capability {
                id: root.id()?,
                expires: root.capability.expires,
            },
            known,
        )?;
        for (collection, chain) in &self.pseudonyms {
            let Some((_, scope)) = domain.scopes().find(|(name, _)| name == collection) else {
                continue;
            };
            let (Some(leaf), Some(author)) = (
                chain.0.last(),
                chain.authors()?.and_then(|range| range.single()),
            ) else {
                continue;
            };
            let key = leaf.capability.subject;
            if known.is_some_and(|record| record.pseudonyms.iter().any(|held| held.key == key)) {
                continue;
            }
            changes.pseudonym(
                &node,
                &registry::Pseudonym {
                    table: scope.table,
                    key,
                    author,
                    assigned: now()?,
                    closed: false,
                },
            )?;
        }
        // A revocation interrupted before its tombstone was written, or one
        // learned from a statement.
        for (other, record) in &registry.records {
            if !record.revoked && record.device.is_some() && self.revoked_device(other, registry) {
                changes.revoked(other)?;
            }
        }
        Ok(changes)
    }
    /// Asks the issuers this member knows, in turn, to assign the pseudonym
    /// `request` asks for.
    pub async fn request_author(
        &self,
        transport: &dyn Transport,
        request: &PseudonymRequest,
    ) -> Result<CredentialChain, Failure> {
        let mut failure: Failure = ServiceError::NoRoute {
            peer: "an issuer",
            network: transport.kind().into(),
        }
        .into();
        let message = [AUTHOR, &request.to_bytes()?].concat();
        for issuer in &self.issuers {
            let addresses = self
                .enrollment_routes
                .get(issuer)
                .or_else(|| self.peers.get(issuer))
                .map(|contact| contact.addresses(transport.kind()))
                .unwrap_or_default();
            if addresses.is_empty() {
                continue;
            }
            match exchange(transport, &addresses, Protocol::Renew, &message).await {
                Ok(reply) => return Ok(serde_json::from_slice(&reply)?),
                Err(error) => failure = error,
            }
        }
        Err(failure)
    }
    /// Assigns a pseudonym to the device that sent `request`: its write
    /// grants on the table and the number the registry holds for its key,
    /// an earlier number it held in this device's range, or the next free
    /// one. On an issuer the registry row to commit comes with it; save
    /// the identity and commit the row before answering. Without a
    /// registry, as on a non-issuer, nothing records the assignment.
    pub fn assign_pseudonym(
        &mut self,
        vault: &Vault,
        request: &PseudonymRequest,
        registry: Option<&Registry>,
    ) -> Result<(CredentialChain, Changes), Failure> {
        let domain = self.domain()?;
        let manifest = domain.manifest().hash();
        let (grants, partitions) = request.verify_selectors(
            self.publisher,
            manifest,
            &self.revocations,
            (now()?, SKEW),
        )?;
        let range = self.minting()?.range;
        let recorded = registry.and_then(|registry| registry.assignment(&request.key));
        if recorded.is_some_and(|(node, held)| node != request.node || held.table != request.table)
        {
            return Err("a pseudonym key is already assigned to another device or table".into());
        }
        let kept = recorded
            .map(|(_, held)| held.author)
            .or_else(|| request.previous_author(self.publisher, manifest, range));
        let author = match kept {
            Some(author) => {
                if registry.is_some_and(|registry| {
                    registry
                        .holders(author)
                        .iter()
                        .any(|key| *key != request.key)
                }) {
                    return Err("the author number is assigned to another key".into());
                }
                author
            }
            None => {
                let minting = self.minting()?;
                let author = minting.next;
                if !minting.range.contains(author) {
                    return Err("this device's author numbers are used up".into());
                }
                minting.next += 1;
                author
            }
        };
        let (holder, prefix) = if self.issuer() {
            let key = match self.author_issuer {
                Some(public) => {
                    let key = PrivateKey::from_bytes(&vault.get("author-issuer")?);
                    if key.public_key() != public {
                        return Err("the author's issuing key differs from its identity".into());
                    }
                    key
                }
                None => {
                    let bytes = match vault.find("author-issuer")? {
                        Some(bytes) => bytes,
                        None => vault.create("author-issuer")?,
                    };
                    let key = PrivateKey::from_bytes(&bytes);
                    self.author_issuer = Some(key.public_key());
                    key
                }
            };
            // A number another issuer assigned is authorized alone.
            let authority = if range.contains(author) {
                range
            } else {
                AuthorRange::new(author, author)?
            };
            let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
            let root = self.issue(
                &publisher,
                &domain,
                key.public_key(),
                self.grants().to_vec(),
                Some(authority),
            )?;
            (key, CredentialChain(vec![root]))
        } else {
            let prefix = self.credentials.delegable();
            let requested_user = request
                .chain
                .delegable()
                .0
                .last()
                .map(|leaf| leaf.capability.subject)
                .ok_or("missing requesting user")?;
            if prefix
                .0
                .iter()
                .any(|link| link.capability.subject == requested_user)
            {
                return Err("request a pseudonym from an independent assigner; this chain would disclose its device".into());
            }
            (PrivateKey::from_bytes(&vault.get("user")?), prefix)
        };
        let assigned = if partitions.is_empty() {
            prefix.assign(
                &holder,
                request.key,
                author,
                request.node,
                grants,
                request.chain.expires(),
                self.validity()?,
            )?
        } else {
            prefix.assign_partitions(
                &holder,
                request.key,
                author,
                request.node,
                partitions,
                self.validity()?,
            )?
        };
        if range.contains(author) {
            self.minting()?.next = self.minting()?.next.max(author + 1);
        }
        let mut changes = Changes::default();
        if registry.is_some() {
            changes.pseudonym(
                &request.node,
                &registry::Pseudonym {
                    table: request.table,
                    key: request.key,
                    author,
                    assigned: recorded.map_or(now()?, |(_, held)| held.assigned),
                    closed: false,
                },
            )?;
        }
        Ok((assigned, changes))
    }
    /// Keeps a pseudonym assigned to this device, after checking that it
    /// names this device's key for its collection and verifies now.
    pub fn hold_pseudonym(
        &mut self,
        vault: &Vault,
        chain: CredentialChain,
    ) -> Result<(String, u32), Failure> {
        let leaf = chain.0.last().ok_or("empty pseudonym chain")?;
        let table = leaf
            .capability
            .grants
            .first()
            .map(|grant| grant.scope.table)
            .or_else(|| {
                leaf.capability
                    .extended()
                    .ok()?
                    .partitions
                    .first()
                    .map(|grant| grant.table)
            })
            .ok_or("empty pseudonym")?;
        let collection = self.collection_of(table)?;
        let key = vault
            .find(&pseudonym_role(&collection))?
            .map(|key| PrivateKey::from_bytes(&key).public_key())
            .ok_or("this device asked for no pseudonym in that collection")?;
        let (pseudonym, _) = chain.verify_pseudonym_selectors(
            self.publisher,
            self.domain()?.manifest().hash(),
            key,
            &self.revocations,
            now()?,
        )?;
        self.pseudonyms.insert(collection.clone(), chain);
        Ok((collection, pseudonym.author))
    }
    pub(crate) fn row_authors(
        &self,
        vault: &Vault,
    ) -> Result<Vec<(PrivateKey, CredentialChain)>, Failure> {
        let manifest = self.domain()?.manifest().hash();
        let time = now()?;
        self.pseudonyms
            .iter()
            // Retain stale chains for renewal, without admitting their writers.
            .filter(|(_, chain)| self.current_pseudonym(chain, manifest, time))
            .map(|(collection, chain)| {
                Ok((
                    PrivateKey::from_bytes(&vault.get(&pseudonym_role(collection))?),
                    chain.clone(),
                ))
            })
            .collect()
    }
    /// The node key of the device a pseudonym names, on an issuer.
    pub fn pseudonym_owner(
        &self,
        vault: &Vault,
        chain: &CredentialChain,
    ) -> Result<(PublicKey, String, u32), Failure> {
        if !self.issuer() {
            return Err("only an issuer opens a pseudonym's device".into());
        }
        let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
        let manifest = self.domain()?.manifest().hash();
        let node = chain.pseudonym_device(&publisher, manifest)?;
        let leaf = chain.0.last().ok_or("empty pseudonym chain")?;
        let author = leaf
            .capability
            .authors()?
            .and_then(|range| range.single())
            .ok_or("invalid pseudonym")?;
        let table = leaf
            .capability
            .grants
            .first()
            .map(|grant| grant.scope.table)
            .or_else(|| {
                leaf.capability
                    .extended()
                    .ok()?
                    .partitions
                    .first()
                    .map(|grant| grant.table)
            })
            .ok_or("empty pseudonym")?;
        Ok((node, self.collection_of(table)?, author))
    }
    /// Gives an issuer without one the author numbers it assigns. Ranges of issuers must
    /// not overlap; nothing can check that between them.
    pub fn claim_authors(&mut self, range: AuthorRange) -> Result<(), Failure> {
        if !self.issuer() || self.minting.is_some() {
            return Err("only an issuer without author numbers claims a range".into());
        }
        self.minting = Some(Minting::fresh(range));
        Ok(())
    }
    /// The publisher key an invitation names, before anything is stored.
    pub fn invitation_publisher(invitation: &str) -> Result<PublicKey, Failure> {
        Ok(Invite::parse(invitation)?.publisher)
    }
    /// Adds the contacts of another identity of this domain, such as the
    /// one a restored backup carried, without replacing known ones.
    pub fn learn_contacts(&mut self, other: &Settings, exclude: &[PublicKey]) {
        for (node, contact) in &other.peers {
            if exclude.contains(node) || self.peers.contains_key(node) {
                continue;
            }
            if self.peers.len() >= MAX_CONTACTS {
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
    /// Contacts of the issuers this device reaches, except `exclude`, for
    /// a device it enrolls or renews to learn.
    fn issuer_contacts(&self, exclude: &[PublicKey]) -> Result<Vec<String>, Failure> {
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
    /// Adds the issuers another issuer named as peers that also renew this
    /// device. They are hints: a known peer keeps its routes, and a revoked
    /// node, a full list or an unusable contact is skipped.
    fn learn_issuers(&mut self, vault: &Vault, contacts: &[String]) {
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
    /// Keeps the authenticated return route for change-driven delivery.
    fn learn_joined_peer(&mut self, node: PublicKey, routes: &[(String, String)], issuer: bool) {
        let routes = routes
            .iter()
            .map(|(transport, address)| Route {
                transport: transport.clone(),
                address: address.clone(),
            })
            .collect();
        let Ok(contact) = Contact::checked(routes) else {
            return;
        };
        let full = (!self.peers.contains_key(&node) && self.peers.len() >= MAX_CONTACTS)
            || (issuer && !self.issuers.contains(&node) && self.issuers.len() >= MAX_ISSUERS);
        if contact.routes.is_empty() || full {
            return;
        }
        if issuer && !self.issuers.contains(&node) {
            self.issuers.push(node);
        }
        self.peers.entry(node).or_default().learn(contact.routes);
    }
    fn revoked(&self, node: &PublicKey) -> bool {
        self.revocations.revokes(node)
    }
    /// Forgets the peers and issuers a held statement revokes, such as a
    /// lost issuer another one revoked.
    pub(crate) fn forget_revoked(&mut self) {
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
    /// Starts or resumes the join an invitation names: this device's keys
    /// exist once it returns, and nothing was sent.
    pub fn prepare_join(
        path: &Path,
        invitation: &str,
        store: &std::sync::Arc<dyn crate::SecretStore>,
    ) -> Result<Joining, Failure> {
        let invite = Invite::parse(invitation)?;
        let token = hex32(&invite.token)?;
        let digest = commitment("rocky.sdk.invitation.v1", &token);
        let (intent, vault) = Intent::resume(&Intent::path(path, false), Some(digest), store)?;
        Ok(Joining {
            vault,
            invite,
            intent,
            token,
        })
    }
    /// The keys of a device that will be handed credentials from outside
    /// the SDK. Asking again returns the same keys.
    pub fn prepare_install(
        path: &Path,
        store: &std::sync::Arc<dyn crate::SecretStore>,
    ) -> Result<DeviceKeys, Failure> {
        let (_, vault) = Intent::resume(&Intent::path(path, true), None, store)?;
        Ok(DeviceKeys {
            user: PrivateKey::from_bytes(&vault.get("user")?).public_key(),
            node: PrivateKey::from_bytes(&vault.get("node")?).public_key(),
        })
    }
    /// Installs supplied credentials and keys for the device
    /// `prepare_install` made. Everything is verified first; nothing is
    /// trusted because of where it came from.
    pub fn install(
        path: &Path,
        schema: &Schema,
        store: &std::sync::Arc<dyn crate::SecretStore>,
        supplied: &Supplied,
    ) -> Result<Self, Failure> {
        let intent_path = Intent::path(path, true);
        if !intent_path.exists() {
            return Err("no device keys in this directory: prepare the device first".into());
        }
        let (intent, vault) = Intent::resume(&intent_path, None, store)?;
        let user = PrivateKey::from_bytes(&vault.get("user")?);
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let credentials = &supplied.credentials;
        let mut settings = Self {
            version: VERSION,
            installation: intent.installation,
            schema: schema.clone(),
            mesh: supplied.mesh,
            domain: supplied.domain,
            publisher: credentials.publisher,
            manifest: HEXLOWER.encode(&credentials.manifest.to_bytes()?),
            revocations: RevocationSet::new(),
            credentials: credentials.chain.clone(),
            ttl: DEFAULT_TTL,
            profile: Profile::Issuer,
            keys: BTreeMap::new(),
            authority: None,
            issuers: Vec::new(),
            enrollment_upstream: None,
            contact_meta: BTreeMap::new(),
            peers: BTreeMap::new(),
            enrollment_routes: BTreeMap::new(),
            logged_out: false,
            lineage: Vec::new(),
            minting: None,
            pseudonyms: BTreeMap::new(),
            author_high_water: 0,
            author_issuer: None,
            deliveries: BTreeMap::new(),
            groups: BTreeMap::new(),
            epoch_statements: BTreeMap::new(),
        };
        let domain = settings.domain()?;
        if credentials.manifest.verify(credentials.publisher)?.hash() != domain.manifest().hash() {
            return Err("the domain's manifest differs from this application schema".into());
        }
        // A chain that ends at this device's user is completed by the device.
        if let Some(last) = settings.credentials.0.pop() {
            if last.capability.subject == user.public_key() {
                let own = device_chain(last, &user, node.public_key())?;
                settings.credentials.0.extend(own.0);
            } else {
                settings.credentials.0.push(last);
            }
        }
        settings.epoch_statements = supplied.epoch_statements.clone();
        settings.learn_revocations(&credentials.revocations)?;
        let (grants, _) = settings.credentials.verify_selectors(
            settings.publisher,
            domain.manifest().hash(),
            node.public_key(),
            &settings.revocations,
            now()?,
        )?;
        settings.ttl = settings
            .credentials
            .expires()
            .saturating_sub(now()?)
            .clamp(60, DEFAULT_TTL);
        settings.profile = profile_of(&grants, &domain);
        settings.minting = settings
            .credentials
            .delegable()
            .authors()?
            .map(Minting::fresh);
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
        settings.save(path)?;
        std::fs::remove_file(intent_path)?;
        Ok(settings)
    }
    /// Stores a reader key and lists its epoch. A held epoch keeps its key.
    fn hold(
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

/// A join whose keys exist and whose enrollment is still to be fetched.
pub struct Joining {
    pub vault: Vault,
    invite: Invite,
    intent: Intent,
    token: [u8; 32],
}
impl Joining {
    /// Enrolls with the issuer over `transport` and stores what it sealed.
    pub async fn enroll(
        self,
        path: &Path,
        schema: &Schema,
        transport: &dyn Transport,
    ) -> Result<Settings, Failure> {
        let Self {
            vault,
            invite,
            intent,
            token,
        } = self;
        let issuer = Contact {
            routes: invite.routes,
        };
        let addresses = issuer.addresses(transport.kind());
        if addresses.is_empty() {
            return Err(ServiceError::NoRoute {
                peer: "the invitation",
                network: transport.kind().into(),
            }
            .into());
        }
        let user = PrivateKey::from_bytes(&vault.get("user")?);
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let mut request = PairRequest::new(
            &token,
            intent.nonce.clone(),
            node.public_key(),
            user.public_key(),
            context(schema)?,
        );
        // Every joining device supplies a return route for later commits.
        if let Ok(address) = transport.address().await
            && !address.is_empty()
        {
            request = request.with_routes(&token, vec![(transport.kind().into(), address)]);
        }
        let sealed = exchange(transport, &addresses, Protocol::Pair, &request.to_bytes()?).await?;
        let enrollment: Enrollment =
            serde_json::from_slice(&open_enrollment(&token, &request, &sealed)?)?;
        let mut settings = Settings {
            version: VERSION,
            installation: intent.installation.clone(),
            schema: schema.clone(),
            mesh: enrollment.mesh,
            domain: enrollment.domain,
            publisher: invite.publisher,
            manifest: enrollment.manifest,
            revocations: RevocationSet::new(),
            credentials: CredentialChain(Vec::new()),
            ttl: enrollment.ttl,
            profile: enrollment.profile,
            keys: BTreeMap::new(),
            authority: None,
            issuers: vec![enrollment.issuer],
            enrollment_upstream: None,
            contact_meta: BTreeMap::new(),
            peers: BTreeMap::from([(
                enrollment.issuer,
                if invite.via_custody {
                    Contact::default()
                } else {
                    issuer.clone()
                },
            )]),
            enrollment_routes: if invite.via_custody {
                BTreeMap::from([(enrollment.issuer, issuer)])
            } else {
                BTreeMap::new()
            },
            logged_out: false,
            lineage: enrollment.lineage,
            minting: None,
            pseudonyms: BTreeMap::new(),
            author_high_water: 0,
            author_issuer: None,
            deliveries: BTreeMap::new(),
            groups: BTreeMap::new(),
            epoch_statements: BTreeMap::new(),
        };
        let domain = settings.domain()?;
        if settings.manifest()?.verify(settings.publisher)?.hash() != domain.manifest().hash() {
            return Err("the domain's manifest differs from this application schema".into());
        }
        settings.epoch_statements = enrollment.epoch_statements.clone();
        settings.learn_revocations(&enrollment.revocations)?;
        // The device receives what the invitation said, no more and no less.
        let grants = grants_of(&invite.profile, &domain)?;
        if enrollment.capability.issuer != settings.publisher
            || enrollment.capability.capability.subject != user.public_key()
            || settings.profile != invite.profile
            || enrollment.capability.capability.grants != grants
        {
            return Err("the enrollment differs from its invitation".into());
        }
        settings.credentials = settings.chain(enrollment.capability, &user, &node, &domain)?;
        for (name, scope) in domain.scopes() {
            if !reads(&grants, scope) {
                continue;
            }
            let epochs = enrollment.keys.get(name).ok_or("missing reader key")?;
            for (epoch, key) in epochs {
                settings.hold(&vault, name, *epoch, &hex32(key)?)?;
            }
            if !settings.keys.contains_key(name) {
                return Err("missing reader key".into());
            }
        }
        if settings.profile.issues() {
            let key = hex32(
                enrollment
                    .publisher_key
                    .as_deref()
                    .ok_or("missing publisher key")?,
            )?;
            if PrivateKey::from_bytes(&key).public_key() != settings.publisher {
                return Err("the enrollment carries another publisher key".into());
            }
            vault.put("publisher", &key)?;
            settings.authority = Some(Issuing::default());
            // Tombstones start with every statement already held.
            settings.learn_revocations(&RevocationSet::new())?;
            if settings.credentials.0[0].capability.authors()? != enrollment.authors {
                return Err("the enrollment's author range differs from its capability".into());
            }
            settings.minting = enrollment.authors.map(Minting::fresh);
        }
        settings.adopt_groups(&vault, &enrollment.groups)?;
        settings.learn_issuers(&vault, &enrollment.issuers);
        settings.save(path)?;
        std::fs::remove_file(Intent::path(path, false))?;
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
        if self.revoked_device(&request.node, registry) {
            return Err("device was revoked".into());
        }
        let domain = self.domain()?;
        let prior = request.verify(
            PrivateKey::from_bytes(&vault.get("node")?).public_key(),
            self.publisher,
            domain.manifest().hash(),
            (now()?, SKEW),
        )?;
        let all = reads(&prior.grants, domain.registry());
        let mut keys = Keys::new();
        for (name, scope) in domain.scopes() {
            if reads(&prior.grants, scope) {
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
        };
        let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
        Ok(Renewal::seal_current(
            &Authority {
                publisher: &publisher,
                manifest: domain.manifest().hash(),
            },
            &request,
            self.revocations.clone(),
            now()?,
            &serde_json::to_vec(&sealed)?,
        )?
        .to_bytes()?)
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
        for issuer in self
            .issuers
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
            let message = [
                EPOCHS,
                &serde_json::to_vec(&EpochRequest {
                    request: request.to_bytes()?,
                    revocations: self.revocations.clone(),
                })?,
            ]
            .concat();
            let bytes = match exchange(transport, &addresses, Protocol::Renew, &message).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    failure = error;
                    continue;
                }
            };
            let reply = Renewal::from_bytes(&bytes)?;
            let payload = reply.open(self.publisher, &request, &secret)?;
            if reply.capability.id()?
                != self
                    .credentials
                    .0
                    .first()
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
            } = Sealed::parse(&payload)?;
            let mut keys = Vec::new();
            let domain = self.domain()?;
            for (name, epochs) in delivered {
                let scope = domain
                    .scopes()
                    .find(|(collection, _)| *collection == name)
                    .map(|(_, scope)| scope)
                    .ok_or("unknown epoch collection")?;
                if !reads(self.grants(), scope) {
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
                revocations: reply.revocations,
                chain: self.credentials.clone(),
                keys,
                groups,
                epoch_statements,
                profile: None,
                issuers,
            });
        }
        Err(failure)
    }
    /// Renews this device's credentials. A member asks its issuers in
    /// turn, `first` before the others. An issuer signs its own; with
    /// `learn` it first asks the other issuers, which is how it hears of
    /// their revocations.
    pub async fn request_renewal(
        &self,
        vault: &Vault,
        transport: Option<&dyn Transport>,
        learn: bool,
        first: Option<PublicKey>,
    ) -> Result<Renewed, Failure> {
        if self.issuer() {
            if learn && let Some(transport) = transport {
                match self.renew_remotely(vault, transport, first).await {
                    Ok(renewed) => return Ok(renewed),
                    Err(error) if error.is::<Revoked>() => return Err(error),
                    Err(_) => {}
                }
            }
            return Ok(Renewed {
                revocations: self.revocations.clone(),
                chain: self.renew_locally(vault)?,
                keys: Vec::new(),
                epoch_statements: BTreeMap::new(),
                groups: Vec::new(),
                profile: None,
                issuers: Vec::new(),
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
        let user = PrivateKey::from_bytes(&vault.get("user")?);
        let (request, secret) =
            RenewRequest::sign(&node, issuer, self.credentials.clone(), now()?)?;
        let reply = match exchange(transport, addresses, Protocol::Renew, &request.to_bytes()?)
            .await
        {
            Ok(reply) => reply,
            Err(error) => {
                if let Ok(reply) = exchange(transport, addresses, Protocol::Renew, POLICY).await {
                    let set: RevocationSet = serde_json::from_slice(&reply)?;
                    if let Some(statement) = self.revoking(&set)? {
                        return Err(Box::new(Revoked(statement)));
                    }
                }
                return Err(error);
            }
        };
        let renewal = Renewal::from_bytes(&reply)?;
        let sealed = renewal.open(self.publisher, &request, &secret)?;
        let domain = self.domain()?;
        if renewal.capability.capability.subject != user.public_key()
            || renewal.capability.capability.grants != self.grants()
        {
            return Err("renewal names another user or other grants".into());
        }
        let mut next = self.clone();
        next.learn_revocations(&renewal.revocations)?;
        let chain = next.chain(renewal.capability, &user, &node, &domain)?;
        let Sealed {
            keys: received,
            groups,
            epoch_statements,
            issuers,
        } = Sealed::parse(&sealed)?;
        let mut keys = Vec::new();
        for (name, scope) in domain.scopes() {
            if !reads(self.grants(), scope) {
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
            revocations: renewal.revocations,
            chain,
            groups,
            epoch_statements,
            keys,
            profile: None,
            issuers,
        })
    }
    /// An issuer's own chain, issued again with the grants it holds.
    fn renew_locally(&self, vault: &Vault) -> Result<CredentialChain, Failure> {
        self.issue_own(vault, self.grants().to_vec())
    }
    /// An issuer's own chain with `grants`, for the current definition.
    pub(crate) fn issue_own(
        &self,
        vault: &Vault,
        grants: Vec<ResourceGrant>,
    ) -> Result<CredentialChain, Failure> {
        let domain = self.domain()?;
        let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
        let user = PrivateKey::from_bytes(&vault.get("user")?);
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let authors = self.minting.map(|minting| minting.range);
        let capability = self.issue(&publisher, &domain, user.public_key(), grants, authors)?;
        self.chain(capability, &user, &node, &domain)
    }
    /// Renews into this identity's generation from an issuer, presenting
    /// `previous`'s chain. The grants are this device's profile over the
    /// new definition; reader keys of collections it now reads arrive too.
    pub(crate) async fn renew_into_generation(
        &self,
        vault: &Vault,
        transport: &dyn Transport,
        issuer: PublicKey,
        addresses: &[String],
        previous: &Settings,
    ) -> Result<Renewed, Failure> {
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let user = PrivateKey::from_bytes(&vault.get("user")?);
        let (request, secret) =
            RenewRequest::sign(&node, issuer, previous.credentials.clone(), now()?)?;
        let reply = exchange(transport, addresses, Protocol::Renew, &request.to_bytes()?).await?;
        let renewal = Renewal::from_bytes(&reply)?;
        let sealed = renewal.open(self.publisher, &request, &secret)?;
        let domain = self.domain()?;
        let grants = grants_of(&self.profile, &domain)?;
        if renewal.capability.capability.subject != user.public_key()
            || renewal.capability.capability.grants != grants
        {
            return Err("the renewal names another user or other grants".into());
        }
        let mut next = self.clone();
        next.learn_revocations(&renewal.revocations)?;
        let chain = next.chain(renewal.capability, &user, &node, &domain)?;
        let Sealed {
            keys: received,
            groups,
            epoch_statements,
            issuers,
        } = Sealed::parse(&sealed)?;
        let mut keys = Vec::new();
        for (name, scope) in domain.scopes() {
            if !reads(&grants, scope) {
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
            revocations: renewal.revocations,
            chain,
            groups,
            epoch_statements,
            keys,
            profile: None,
            issuers,
        })
    }
    /// Saves what a renewal brought before the replica learns it: the keys
    /// in the provider, then the identity. Returns the keys now listed.
    pub fn adopt(
        &mut self,
        path: &Path,
        vault: &Vault,
        renewed: &Renewed,
    ) -> Result<Vec<(String, rocky_core::KeyEpoch, [u8; 32])>, Failure> {
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
        draft.adopt_groups(vault, &renewed.groups)?;
        draft.forget_revoked();
        draft.learn_issuers(vault, &renewed.issuers);
        // Changing or removing a range must never rewind its allocator.
        if let Some(profile) = &renewed.profile {
            draft.profile = profile.clone();
            draft.author_high_water = draft
                .author_high_water
                .max(draft.minting.map_or(0, |minting| minting.next));
            draft.minting = renewed.chain.delegable().authors()?.map(|range| Minting {
                range,
                next: range.first.max(draft.author_high_water),
            });
        }
        if serde_json::to_vec(&draft)? != serde_json::to_vec(self)? {
            draft.save(path)?;
        }
        *self = draft;
        Ok(held)
    }
    /// Answers a renewal with the registry rows to commit before it
    /// leaves. The request proves a device the publisher once issued a
    /// chain; it receives the same grants, the revocations this issuer
    /// holds and the reader keys those grants read. The record's tombstone
    /// and profile count wherever the device enrolled; without a record the
    /// presented chain is renewed and recorded. A node or user a statement
    /// revoked is refused, also after the statement left the set.
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
        let known = registry.get(&request.node);
        if self.revoked_device(&request.node, registry) {
            return Err("device was revoked".into());
        }
        let current = domain.manifest().hash();
        // A device of an earlier generation is renewed into this one with
        // its profile's grants over the current definition.
        let prior = match request.verify(me, self.publisher, current, (time, SKEW)) {
            Ok(prior) => prior.clone(),
            Err(error) => {
                let earlier = self.lineage.iter().find_map(|step| {
                    let manifest = step.manifest.as_bytes();
                    let hash = HEXLOWER
                        .decode(manifest)
                        .ok()
                        .and_then(|bytes| SignedManifest::from_bytes(&bytes).ok())
                        .and_then(|signed| signed.verify(self.publisher).ok())?
                        .hash();
                    request
                        .verify(me, self.publisher, hash, (time, SKEW))
                        .ok()
                        .cloned()
                });
                let Some(mut prior) = earlier else {
                    return Err(error.into());
                };
                let profile = known
                    .and_then(registry::Record::profile)
                    .cloned()
                    .unwrap_or_else(|| profile_of(&prior.grants, &domain));
                prior.grants = grants_of(&profile, &domain)?;
                prior.manifest = current;
                prior
            }
        };
        let prior = &prior;
        if self.tombstoned(&prior.subject) {
            return Err("device was revoked".into());
        }
        if known.and_then(|known| known.device.as_ref()).is_none()
            && registry.devices() >= registry::MAX_DEVICES
        {
            return Err(Box::new(RegistryFull));
        }
        let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
        let authority = Authority {
            publisher: &publisher,
            manifest: current,
        };
        let sealed = Sealed {
            groups: self.groups_for(vault, request.node, prior, false)?,
            epoch_statements: self.statements_for(&self.keys_for(vault, &domain, &prior.grants)?),
            keys: self.keys_for(vault, &domain, &prior.grants)?,
            issuers: self.issuer_contacts(&[request.node])?,
        };
        let renewal = Renewal::seal(
            &authority,
            &request,
            prior,
            self.revocations.clone(),
            (time, self.ttl, SKEW),
            &serde_json::to_vec(&sealed)?,
        )?;
        let mut changes = Changes::default();
        // A device another issuer enrolled is recorded once it renews here,
        // unless its record already arrived.
        changes.device(
            &request.node,
            &registry::Device {
                user: prior.subject,
                kind: registry::Kind::Renewed,
                profile: Some(profile_of(&prior.grants, &domain)),
                issuer: me,
                command: command_identity(request.node),
                enrolled: time,
            },
            false,
        )?;
        changes.capability(
            &request.node,
            registry::Capability {
                id: renewal.capability.id()?,
                expires: renewal.capability.capability.expires,
            },
            known,
        )?;
        Ok((renewal.to_bytes()?, changes))
    }
    /// Whether a key is revoked for good here: by a held statement or, on
    /// an issuer, a tombstone.
    fn tombstoned(&self, key: &PublicKey) -> bool {
        self.revocations.revokes(key)
            || self
                .authority
                .as_ref()
                .is_some_and(|authority| authority.tombstones.contains(key))
    }
    /// Whether a statement, a tombstone or the registry revoked a device,
    /// or a statement or tombstone its recorded user.
    pub(crate) fn revoked_device(&self, node: &PublicKey, registry: &Registry) -> bool {
        self.tombstoned(node)
            || registry.get(node).is_some_and(|record| {
                record.revoked
                    || record
                        .device
                        .as_ref()
                        .is_some_and(|device| self.tombstoned(&device.user))
            })
    }
    /// The registry rows recording partitions granted or delegated to a
    /// device against its node and user, committed before the chain leaves
    /// this issuer so that revoking it rotates them. Refuses a revoked
    /// device or user.
    pub(crate) fn holder_changes(
        &self,
        vault: &Vault,
        device: &DeviceKeys,
        grants: &[rocky_credentials::PartitionGrant],
        delegations: u8,
        registry: &Registry,
    ) -> Result<Changes, Failure> {
        if self.revoked_device(&device.node, registry) || self.tombstoned(&device.user) {
            return Err("the device or its user was revoked".into());
        }
        let known = registry.get(&device.node);
        match known.and_then(|known| known.device.as_ref()) {
            Some(known) if known.user != device.user => {
                return Err("this node is known with another user key".into());
            }
            None if registry.devices() >= registry::MAX_DEVICES => {
                return Err(Box::new(RegistryFull));
            }
            _ => {}
        }
        let mut changes = Changes::default();
        changes.device(
            &device.node,
            &registry::Device {
                user: device.user,
                kind: registry::Kind::Partition,
                profile: None,
                issuer: PrivateKey::from_bytes(&vault.get("node")?).public_key(),
                command: command_identity(device.node),
                enrolled: now()?,
            },
            false,
        )?;
        for grant in grants {
            changes.grant(
                &device.node,
                &registry::Grant {
                    grant: grant.clone(),
                    delegations,
                },
                known,
            )?;
        }
        Ok(changes)
    }
    /// Begins revoking a device: fixes its statement, from what the registry
    /// recorded of it, and the key epoch each collection and group partition
    /// it could read rotates to, the held maximum plus one. The intent is all
    /// that changes; `rotate` signs and completes it, now or at the next open.
    pub fn begin_revocation(
        &mut self,
        node: PublicKey,
        registry: &Registry,
    ) -> Result<(), Failure> {
        // Another issuer is revoked like any device; the publisher key it
        // holds is not taken away (`docs/runtime/sdk-enrollment.md`).
        let own = self
            .credentials
            .0
            .last()
            .map(|leaf| leaf.capability.subject)
            .ok_or("empty device credential chain")?;
        if own == node {
            return Err("an issuer cannot revoke itself".into());
        }
        if self.revoked_device(&node, registry) {
            return Err("device is already revoked".into());
        }
        if self.issuing("revokes devices")?.rotation.is_some() {
            return Err("another revocation is unfinished".into());
        }
        let time = now()?;
        if self.revocations.live(time) >= MAX_LIVE_REVOCATIONS {
            return Err(Box::new(RevocationSetFull));
        }
        let domain = self.domain()?;
        // A recorded device loses its user too, so a fresh node key under it
        // cannot renew, and its capability and pseudonyms.
        let record = registry.get(&node).map(Revoking::of);
        let statement = Revocation {
            app: domain.manifest().app,
            issuer: own,
            time,
            nodes: [node].into(),
            users: record.iter().filter_map(|record| record.user).collect(),
            capabilities: record
                .iter()
                .filter_map(|record| record.capability)
                .collect(),
            pseudonyms: record
                .iter()
                .flat_map(|record| record.pseudonyms.iter().copied())
                .collect(),
        };
        // Only an absent record uses the full-rotation fallback.
        let readable = record.as_ref().and_then(|record| record.profile.clone());
        let mut keys = BTreeMap::new();
        for (name, epochs) in &self.keys {
            if record.is_some() && !readable.as_ref().is_some_and(|profile| profile.reads(name)) {
                continue;
            }
            if epochs.len() >= MAX_EPOCHS {
                return Err("key epoch capacity reached".into());
            }
            let held = epochs
                .iter()
                .map(|epoch| epoch.generation)
                .max()
                .unwrap_or(0);
            keys.insert(
                name.clone(),
                held.checked_add(1)
                    .filter(|generation| *generation <= i64::MAX as u64)
                    .ok_or("key epoch overflow")?,
            );
        }
        // The group secret is the unit: one new secret per group partition
        // the device could read, so every member table advances together.
        // Its profile's tables and its recorded partition grants count; a
        // node absent from the registry may have read any known partition. A
        // partition whose secret this issuer never held has no epoch to
        // follow, and is left to an issuer that holds it.
        let mut groups = BTreeMap::new();
        for partition in self.partitions(&domain)? {
            let read = record.is_none()
                || readable.as_ref().is_some_and(|profile| {
                    partition
                        .members
                        .iter()
                        .any(|member| profile.reads(&member.table))
                })
                || record
                    .as_ref()
                    .is_some_and(|record| record.selects(&partition));
            if let (true, Some(held)) = (read, partition.record.current()) {
                let target = held
                    .generation
                    .checked_add(1)
                    .filter(|generation| *generation <= i64::MAX as u64)
                    .ok_or("group secret epoch overflow")?;
                if partition.record.epochs.len() >= crate::groups::MAX_GROUP_EPOCHS {
                    return Err("group secret epoch capacity reached".into());
                }
                groups.insert(crate::groups::record_key(&partition.record.address), target);
            }
        }
        // Refused here, before anything is saved, when it is too large.
        if rocky_credentials::encode(&statement)?.len() + rocky_credentials::SIGNATURE_OVERHEAD
            > rocky_credentials::MAX_REVOCATION_BYTES
        {
            return Err("the revocation names too many keys".into());
        }
        self.issuing("revokes devices")?.rotation = Some(Rotation {
            node,
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
    /// Saves a superseding generation only for uncovered revocations of readers.
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
            let records: Vec<_> = signed
                .statement
                .nodes
                .iter()
                .map(|node| {
                    registry
                        .get(node)
                        .filter(|record| record.device.is_some() || !record.grants.is_empty())
                        .map(Revoking::of)
                })
                .collect();
            for (name, epochs) in &self.keys {
                let Some(current) = epochs.iter().max() else {
                    continue;
                };
                let covered = self
                    .epoch_statements
                    .get(&key_role(name, *current))
                    .is_some_and(|ids| ids.contains(&id));
                let read = records.iter().any(|record| {
                    record.as_ref().is_none_or(|record| {
                        record
                            .profile
                            .as_ref()
                            .is_some_and(|profile| profile.reads(name))
                    })
                });
                if !covered && read {
                    if epochs.len() >= MAX_EPOCHS {
                        return Err("key epoch capacity reached".into());
                    }
                    keys.insert(
                        name.clone(),
                        current
                            .generation
                            .checked_add(1)
                            .filter(|generation| *generation <= i64::MAX as u64)
                            .ok_or("key epoch overflow")?,
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
                let read = records.iter().any(|record| {
                    record.as_ref().is_none_or(|record| {
                        record.profile.as_ref().is_some_and(|profile| {
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
                        current
                            .generation
                            .checked_add(1)
                            .filter(|generation| *generation <= i64::MAX as u64)
                            .ok_or("group epoch overflow")?,
                    );
                    trigger = Some(signed.statement.clone());
                }
            }
        }
        let Some(statement) = trigger else {
            return Ok(false);
        };
        let node = statement
            .nodes
            .iter()
            .next()
            .copied()
            .ok_or("rotation statement names no device")?;
        let mut statements = self
            .revocations
            .statements()
            .map(|signed| signed.statement.id())
            .collect::<Result<Vec<_>, _>>()?;
        statements.sort_unstable();
        self.issuing("rotates exposed scopes")?.rotation = Some(Rotation {
            node,
            statement,
            statements,
            keys,
            groups,
        });
        Ok(true)
    }
    /// Prepares a begun revocation: a fresh key epoch for every collection
    /// and group partition it names, the signed statement joins the set and
    /// the issuer renews its own chain. Earlier keys stay; history is not sealed
    /// again. Repeating it after an interruption makes the same keys and
    /// statement. The intent stays until `complete_revocation` observes the
    /// committed tombstone. Returns the new keys, or nothing when no revocation was
    /// begun.
    pub fn rotate(&mut self, vault: &Vault) -> Result<Option<Vec<NewKey>>, Failure> {
        let Some(Rotation {
            node,
            statement,
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
        crate::mesh::mint_value(
            vault,
            self.publisher,
            self.domain()?.scope,
            &self.revocations,
            &statement,
        )?;
        fault::check(fault::Stage::Begun)?;
        let mut keys = Vec::new();
        for (name, epoch) in targets {
            // The intent was saved first: a key missing here was never
            // used, so making it is not replacing one.
            let role = format!("rotation.reader.{name}.{epoch}");
            let key = match vault.find(&role)? {
                Some(key) => key,
                None => vault.create(&role)?,
            };
            let epoch = rocky_core::KeyEpoch::new(epoch, &key);
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
            let epoch = rocky_core::KeyEpoch::new(epoch, &secret);
            vault.put(&secret_role(&record.address, epoch), &secret)?;
            self.epoch_statements
                .insert(secret_role(&record.address, epoch), ids.clone());
            record.hold(epoch)?;
            self.groups.insert(address, record);
            fault::check(fault::Stage::Key)?;
        }
        let domain = self.domain()?;
        let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
        let signed = match self.revocations.get(&statement.id()?) {
            Some(held) => held.clone(),
            None => Authority {
                publisher: &publisher,
                manifest: domain.manifest().hash(),
            }
            .revoke(statement, &PrivateKey::from_bytes(&vault.get("node")?))?,
        };
        self.learn_revocations(&RevocationSet::try_from(vec![signed])?)?;
        fault::check(fault::Stage::Signed)?;
        self.credentials = self.renew_locally(vault)?;
        fault::check(fault::Stage::Renewed)?;
        self.peers.remove(&node);
        self.issuers.retain(|issuer| *issuer != node);
        self.enrollment_routes.remove(&node);
        // Keep the redo intent until the registry commits its tombstone.
        Ok(Some(keys))
    }
    /// Discards the redo intent only after the completion transaction committed.
    /// The rotation's envelopes are saved with it.
    pub(crate) fn complete_revocation(
        &mut self,
        vault: &Vault,
        domain: &Domain,
        registry: &Registry,
    ) -> Result<(), Failure> {
        let Some(node) = self
            .issuing("revokes devices")?
            .rotation
            .as_ref()
            .map(|rotation| rotation.node)
        else {
            return Ok(());
        };
        if !registry.revoked(&node) {
            return Err("revocation tombstone is not committed".into());
        }
        self.queue_rotation(vault, domain, registry)?;
        fault::check(fault::Stage::Clearing)?;
        self.issuing("revokes devices")?.rotation = None;
        Ok(())
    }
}

/// A key `rotate` made: collection, key epoch and key.
pub type NewKey = (String, rocky_core::KeyEpoch, [u8; 32]);

/// Deterministic interruptions of `rotate`, for its fault sweep.
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
        /// The stage, and how many of it pass first, at which `rotate` stops.
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

/// A stream to the first of `addresses` that answers.
pub async fn dial(
    transport: &dyn Transport,
    addresses: &[String],
    protocol: Protocol,
) -> Result<Stream, Failure> {
    let mut failure = None;
    for address in addresses {
        match tokio::time::timeout(TIMEOUT, transport.connect(address, protocol)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => failure = Some(error.to_string()),
            Err(_) => failure = Some("the dial timed out".into()),
        }
    }
    Err(ServiceError::Unreachable(failure.unwrap_or_else(|| "no route".into())).into())
}
/// One request and its answer on a fresh stream: the reply's bytes, or
/// the peer's refusal.
pub async fn exchange(
    transport: &dyn Transport,
    addresses: &[String],
    protocol: Protocol,
    request: &[u8],
) -> Result<Vec<u8>, Failure> {
    let stream = dial(transport, addresses, protocol).await?;
    exchange_stream(stream, request).await
}
pub(crate) async fn exchange_stream(
    mut stream: Stream,
    request: &[u8],
) -> Result<Vec<u8>, Failure> {
    tokio::time::timeout(2 * TIMEOUT, async {
        write_message(&mut stream, request).await?;
        let reply = read_message(&mut stream).await?;
        let _ = stream.shutdown().await;
        let reply: serde_json::Value = serde_json::from_slice(&reply)?;
        // The peer answered with a definite refusal, such as a revoked renewal.
        if let Some(error) = reply.get("error") {
            let message = error.as_str().unwrap_or("peer refused").to_owned();
            if reply["retryable"] == true {
                return Err(ServiceError::Unreachable(message).into());
            }
            let refused: Failure = Box::new(crate::node::Refused(message));
            return Err(refused);
        }
        Ok(HEXLOWER.decode(reply["reply"].as_str().ok_or("missing reply")?.as_bytes())?)
    })
    .await
    .unwrap_or_else(|_| Err(ServiceError::Unreachable("exchange timed out".into()).into()))
}
/// Serves one request of an inbound stream with `answer`.
pub async fn answer(
    mut stream: Stream,
    answer: impl AsyncFnOnce(Vec<u8>) -> Result<Vec<u8>, Failure>,
) -> Result<(), Failure> {
    let request = tokio::time::timeout(TIMEOUT, read_message(&mut stream)).await??;
    let reply = match answer(request).await {
        Ok(reply) => serde_json::json!({"reply": HEXLOWER.encode(&reply)}),
        Err(error) => {
            serde_json::json!({"error": error.to_string(), "retryable": crate::node::error(&error)["retryable"]})
        }
    };
    let reply = serde_json::to_vec(&reply).unwrap_or_default();
    tokio::time::timeout(TIMEOUT, async {
        write_message(&mut stream, &reply).await?;
        stream.shutdown().await?;
        // Wait for the peer to close, so the answer is delivered.
        let mut rest = [0; 1];
        let _ = stream.read(&mut rest).await;
        Ok::<_, Failure>(())
    })
    .await?
}
async fn write_message(stream: &mut Stream, bytes: &[u8]) -> Result<(), Failure> {
    if bytes.len() > MAX_MESSAGE {
        return Err("message quota".into());
    }
    stream.write_u32(u32::try_from(bytes.len())?).await?;
    stream.write_all(bytes).await?;
    stream.flush().await?;
    Ok(())
}
async fn read_message(stream: &mut Stream) -> Result<Vec<u8>, Failure> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_MESSAGE {
        return Err("message quota".into());
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}

/// Replaces an invitation's or issuer contact's routes with a custody peer's
/// routes. Grants, invitation token and issuer identity stay unchanged.
pub fn route_enrollment(uri: &str, custody_contact: &str) -> Result<String, Failure> {
    fn contact(uri: &str) -> Result<ContactRecord, Failure> {
        let record = ContactRecord::parse(uri)?;
        Contact::checked(record.routes.clone())?;
        Ok(record)
    }
    let relay = contact(custody_contact)?;
    if uri.trim().starts_with("rocky://join/") {
        let mut invitation = Invite::parse(uri)?;
        if invitation.publisher != relay.publisher {
            return Err("relay belongs to another domain".into());
        }
        invitation.routes = relay.routes;
        invitation.via_custody = true;
        Ok(format!(
            "rocky://join/{}",
            BASE64URL_NOPAD.encode(&serde_json::to_vec(&invitation)?)
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

/// Every partition of each partitioned table `grants` name, as enrollment
/// grants a profile: an `Any` selector beside the table grant.
fn whole_partitions(
    domain: &Domain,
    grants: &[ResourceGrant],
) -> Vec<rocky_credentials::PartitionGrant> {
    let logical = domain.logical();
    grants
        .iter()
        .filter_map(|grant| {
            let table = logical
                .mergeable()
                .find(|table| logical.table_id(table) == grant.scope.table)?;
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod deadline_tests {
    use super::*;
    use crate::transport::BoxFuture;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    struct Routes {
        attempts: AtomicUsize,
        stream: Mutex<Option<Stream>>,
    }
    impl Transport for Routes {
        fn kind(&self) -> &str {
            "test"
        }
        fn address(&self) -> BoxFuture<'_, Result<String, Failure>> {
            Box::pin(async { Ok("third".into()) })
        }
        fn connect<'a>(
            &'a self,
            address: &'a str,
            _: Protocol,
        ) -> BoxFuture<'a, Result<Stream, Failure>> {
            Box::pin(async move {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                if address != "third" {
                    std::future::pending::<()>().await;
                }
                Ok(self.stream.lock().unwrap().take().unwrap())
            })
        }
        fn accept(&self) -> BoxFuture<'_, Option<(Protocol, Stream)>> {
            Box::pin(async { None })
        }
        fn close(&self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    #[tokio::test(start_paused = true)]
    async fn exchange_tries_later_routes_after_two_full_dial_deadlines() {
        let (client, server) = tokio::io::duplex(1024);
        let mut server: Stream = Box::new(server);
        let transport = Routes {
            attempts: AtomicUsize::new(0),
            stream: Mutex::new(Some(Box::new(client))),
        };
        let answer = tokio::spawn(async move {
            assert_eq!(read_message(&mut server).await.unwrap(), b"request");
            // Use some of the exchange budget, after the two dial budgets.
            tokio::time::sleep(TIMEOUT).await;
            write_message(&mut server, br#"{"reply":"6f6b"}"#)
                .await
                .unwrap();
        });
        let start = tokio::time::Instant::now();
        let reply = exchange(
            &transport,
            &["first".into(), "second".into(), "third".into()],
            Protocol::Pair,
            b"request",
        )
        .await
        .unwrap();
        assert_eq!(reply, b"ok");
        assert_eq!(transport.attempts.load(Ordering::SeqCst), 3);
        assert!(start.elapsed() >= 3 * TIMEOUT);
        answer.await.unwrap();
    }
}
