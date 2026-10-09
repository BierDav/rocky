//! The SDK's enrollment plumbing over `rocky-credentials`: domain
//! creation, invitations with a grant profile, supplied credentials,
//! renewal by any issuer, and revocation with key rotation. A device that
//! holds the domain's publisher key is an issuer; every device signs with
//! its own node key. See `docs/runtime/sdk-enrollment.md`.
use crate::{
    Failure,
    domain::Domain,
    groups::{GroupPartition, GroupRecord, Holder, Member, secret_role},
    schema::Schema,
    secrets::{Vault, write_private},
    service::ServiceError,
    transport::{Protocol, Stream, Transport},
};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use rand::RngCore;
use rocky_core::{Commitment, DomainId, MeshId, ResourceGrant, Right, RowSelection, commitment};
use rocky_credentials::{
    Admission, AuthorRange, Authority, Credentials, Invitation, PairRequest, Profile,
    PseudonymRequest, RenewRequest, Renewal, device_chain, open_enrollment, revoked_subject,
    seal_enrollment,
};
use rocky_secure::{
    CredentialChain, KeyMaterial, PolicyStatement, PrivateKey, PublicKey, SecureConfig,
    SignedCapability, SignedManifest, SignedPolicy,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Current `identity.json` format, including group secrets.
pub const VERSION: u32 = 9;
/// Current invitation format, with an explicit custody routing flag.
const INVITE_VERSION: u32 = 4;
const CONTACT_VERSION: u32 = 2;
/// Peers one device syncs with and serves.
pub const MAX_PEERS: usize = 16;
const MAX_ISSUERS: usize = 8;
const MAX_ROUTES: usize = 4;
const MAX_ADDRESS: usize = 2048;
/// Default lifetime of issued credentials.
pub const DEFAULT_TTL: u64 = 30 * 24 * 60 * 60;
const INVITATION_TTL: u64 = 600;
const MAX_DEVICES: usize = 64;
/// Key epochs kept per collection: one per revocation that rotated it.
const MAX_EPOCHS: usize = 256;
const MAX_MESSAGE: usize = 256 * 1024;
/// Accepted clock difference between devices.
const SKEW: u64 = 300;
const TIMEOUT: Duration = Duration::from_secs(15);

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
    pub policy: SignedPolicy,
    pub credentials: CredentialChain,
    pub ttl: u64,
    /// What this device was enrolled as.
    pub profile: Profile,
    /// The key epochs held of each collection this device reads, ascending.
    pub keys: BTreeMap<String, Vec<u64>>,
    /// Group partitions known here, by group address.
    pub groups: BTreeMap<String, GroupRecord>,
    /// Present on a device that holds the publisher key.
    pub authority: Option<Issuing>,
    /// The issuers that renew this device, by node key. Their routes are
    /// in `peers`.
    pub issuers: Vec<PublicKey>,
    /// A fixed issuer whose pairing/renewal responses this custody node relays.
    pub enrollment_upstream: Option<PublicKey>,
    /// The nodes this device syncs with and serves, by node key, with the
    /// routes last learned for each.
    pub peers: BTreeMap<PublicKey, Contact>,
    pub enrollment_routes: BTreeMap<PublicKey, Contact>,
    pub logged_out: bool,
    /// The generations the domain left, oldest first. Empty until its
    /// first upgrade; see `docs/runtime/generations.md`.
    pub lineage: Vec<crate::generation::Step>,
    /// The author numbers this device assigns to pseudonyms, if any.
    pub minting: Option<Minting>,
    /// This device's pseudonym chain for each collection it writes.
    pub pseudonyms: BTreeMap<String, CredentialChain>,
    /// Assignments survive lost answers and retries of the same key.
    pub author_assignments: BTreeMap<PublicKey, AssignedAuthor>,
    /// Retained across removal/replacement of a supplied author range.
    pub author_high_water: u32,
    /// An issuer's assignment key, separate from every node/user identity.
    pub author_issuer: Option<PublicKey>,
}
/// An assigned signing key's stable author and authenticated device/table.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignedAuthor {
    pub node: PublicKey,
    pub table: Commitment,
    pub author: u32,
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
/// What an issuer keeps: its invitation, the devices it knows and a
/// revocation it has begun.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Issuing {
    pub invitation: Option<Invitation>,
    pub devices: BTreeMap<PublicKey, Device>,
    /// Saved before the rotation's keys are made, so an interrupted
    /// revocation completes at the next open.
    pub rotation: Option<Rotation>,
    /// Devices granted partitions here, by node key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub holders: BTreeMap<PublicKey, Holder>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Rotation {
    /// The policy and key epoch the revocation creates.
    pub epoch: u64,
    pub node: PublicKey,
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
    fn learn(&mut self, routes: Vec<Route>) {
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
/// A device an issuer enrolled or renewed.
#[derive(Clone, Serialize, Deserialize)]
pub struct Device {
    pub user: PublicKey,
    /// The pairing nonce, empty for a device first seen at a renewal.
    pub nonce: String,
    pub enrolled: u64,
    /// The last user capability issued to it.
    pub capability: Commitment,
    pub revoked: bool,
    pub profile: Profile,
    /// The author numbers handed to it, kept for a retried enrollment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authors: Option<AuthorRange>,
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
    policy: SignedPolicy,
    capability: SignedCapability,
    profile: Profile,
    keys: Keys,
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
type Keys = BTreeMap<String, BTreeMap<u64, String>>;
/// What a renewal seals to its device: reader keys and the contacts of the
/// other issuers the answering issuer reaches.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sealed {
    keys: Keys,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    issuers: Vec<String>,
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
    pub keys: Vec<(String, u64, [u8; 32])>,
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
/// What a renewal brought: the policy, this device's chain and reader
/// keys it did not hold.
pub struct Renewed {
    pub policy: SignedPolicy,
    pub chain: CredentialChain,
    pub keys: Vec<(String, u64, [u8; 32])>,
    /// What the grants amount to when they changed, as supplied ones may.
    pub profile: Option<Profile>,
    /// Contacts of further issuers the answering issuer reaches.
    pub issuers: Vec<String>,
}

pub fn now() -> Result<u64, Failure> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn random<const N: usize>() -> [u8; N] {
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
pub(crate) fn key_role(collection: &str, epoch: u64) -> String {
    if epoch == 1 {
        format!("reader.{collection}")
    } else {
        format!("reader.{collection}.{epoch}")
    }
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
/// The profile a set of grants amounts to: a reader or writer, or raw grants.
fn profile_of(grants: &[ResourceGrant], domain: &Domain) -> Profile {
    Profile::of(grants, &domain.resources())
}

impl Settings {
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
        if settings.peers.len() > MAX_PEERS
            || settings.issuers.len() > MAX_ISSUERS
            || settings.enrollment_routes.len() > MAX_ISSUERS
            || settings
                .enrollment_routes
                .keys()
                .any(|node| !settings.issuers.contains(node))
        {
            return Err("identity holds too many peers".into());
        }
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
    /// The node keys whose sync sessions this device serves: its
    /// configured peers and, on an issuer, the devices it knows and did
    /// not revoke. Each request is still authorized by credentials.
    pub fn admitted(&self) -> Vec<PublicKey> {
        let mut admitted: BTreeSet<PublicKey> = self.peers.keys().copied().collect();
        if let Some(authority) = &self.authority {
            for (node, device) in &authority.devices {
                if device.revoked {
                    admitted.remove(node);
                } else {
                    admitted.insert(*node);
                }
            }
        }
        admitted.into_iter().collect()
    }
    /// The peers to sync with, issuers first.
    pub fn targets(&self) -> Vec<(PublicKey, Contact)> {
        let admitted = self.admitted();
        let mut targets: Vec<_> = self
            .peers
            .iter()
            .filter(|(node, contact)| admitted.contains(node) && !contact.routes.is_empty())
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
        let revoked = self.authority.as_ref().is_some_and(|authority| {
            authority
                .devices
                .get(&record.node)
                .is_some_and(|device| device.revoked)
        });
        if revoked {
            return Err("the contact names a revoked device".into());
        }
        if !self.peers.contains_key(&record.node) && self.peers.len() >= MAX_PEERS {
            return Err("peer capacity reached".into());
        }
        if record.issuer && !self.issuers.contains(&record.node) {
            if self.issuers.len() >= MAX_ISSUERS {
                return Err("issuer capacity reached".into());
            }
            self.issuers.push(record.node);
        }
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
    /// Forgets a peer. A member keeps its last issuer, which renews it.
    pub fn remove_peer(&mut self, node: PublicKey) -> Result<(), Failure> {
        if !self.issuer() && self.issuers == [node] {
            return Err(
                "the last issuer stays configured; add another issuer's contact first".into(),
            );
        }
        self.peers.remove(&node).ok_or("unknown peer")?;
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
                keys.push(KeyMaterial {
                    scope,
                    generation: *epoch,
                    key: vault.get(&key_role(name, *epoch))?,
                });
            }
        }
        Ok(SecureConfig {
            publisher: self.publisher,
            manifest: self.manifest()?,
            policy: self.policy.clone(),
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
        ]
        .into_iter()
        .map(String::from)
        .collect();
        for (name, epochs) in &self.keys {
            roles.extend(epochs.iter().map(|epoch| key_role(name, *epoch)));
        }
        if let Ok(domain) = self.domain() {
            roles.extend(domain.scopes().map(|(name, _)| pseudonym_role(name)));
        }
        for record in self.groups.values() {
            roles.extend(
                record
                    .epochs
                    .iter()
                    .map(|epoch| secret_role(&record.address, *epoch)),
            );
        }
        // A rotation may have made keys the identity does not list yet.
        if let Some(Rotation { epoch, .. }) =
            self.authority.as_ref().and_then(|a| a.rotation.as_ref())
        {
            roles.extend(self.keys.keys().map(|name| key_role(name, *epoch)));
            roles.extend(
                self.groups
                    .values()
                    .map(|record| secret_role(&record.address, *epoch)),
            );
        }
        roles
    }
    /// The stable key of this node's routes.
    pub fn transport_key(vault: &Vault) -> Result<[u8; 32], Failure> {
        Ok(vault.get("transport")?)
    }
    /// Whether another holder of the publisher key is known.
    pub fn another_issuer(&self) -> bool {
        !self.issuers.is_empty()
            || self.authority.as_ref().is_some_and(|authority| {
                authority
                    .devices
                    .values()
                    .any(|device| device.profile.issues() && !device.revoked)
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
        let mut keys = BTreeMap::new();
        for name in schema.names() {
            vault.create(&key_role(name, 1))?;
            keys.insert(name.to_owned(), vec![1]);
        }
        let (mesh, domain) = (MeshId(random()), DomainId(random()));
        let bound = Domain::new(schema, *publisher.public_key().as_bytes(), mesh, domain)?;
        let manifest = SignedManifest::sign(&bound.manifest(), &publisher)?;
        let policy = SignedPolicy::sign(
            PolicyStatement {
                manifest: bound.manifest().hash(),
                epoch: 1,
                revoked: BTreeSet::new(),
            },
            &publisher,
        )?;
        let mut settings = Self {
            version: VERSION,
            installation,
            schema: schema.clone(),
            mesh,
            domain,
            publisher: publisher.public_key(),
            manifest: HEXLOWER.encode(&manifest.to_bytes()?),
            policy,
            credentials: CredentialChain(Vec::new()),
            ttl,
            profile: Profile::Issuer,
            keys,
            authority: Some(Issuing::default()),
            issuers: Vec::new(),
            enrollment_upstream: None,
            peers: BTreeMap::new(),
            enrollment_routes: BTreeMap::new(),
            logged_out: false,
            lineage: Vec::new(),
            minting: Some(Minting::fresh(AuthorRange::ALL)),
            pseudonyms: BTreeMap::new(),
            author_assignments: BTreeMap::new(),
            author_high_water: 0,
            author_issuer: None,
            groups: BTreeMap::new(),
        };
        let grants = Profile::Issuer.grants(&bound.resources())?;
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
            self.policy.statement.epoch,
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
            &self.policy.statement,
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
            if let Some(epoch) = self.keys.get(name).and_then(|epochs| epochs.iter().max()) {
                keys.entry(name.to_owned()).or_default().insert(
                    *epoch,
                    HEXLOWER.encode(&vault.get(&key_role(name, *epoch))?),
                );
            }
        }
        Ok(keys)
    }

    /// Opens a new invitation for `profile`, replacing an earlier one.
    pub async fn invite(
        &mut self,
        path: &Path,
        vault: &Vault,
        transport: &dyn Transport,
        profile: Profile,
    ) -> Result<(String, u64), Failure> {
        profile.grants(&self.domain()?.resources())?;
        if self.issuing("invites devices")?.devices.len() >= MAX_DEVICES {
            return Err("device capacity reached".into());
        }
        let address = transport.address().await?;
        // Nothing was sealed under an unused invitation token.
        let token = vault.replace("invitation")?;
        let expires = now()? + INVITATION_TTL;
        self.issuing("invites devices")?.invitation =
            Some(Invitation::open(&token, profile.clone(), expires));
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

    /// Answers one pairing request. The invitation's attempts and the
    /// enrolled device are saved before the answer leaves.
    pub fn pair(&mut self, path: &Path, vault: &Vault, request: &[u8]) -> Result<Vec<u8>, Failure> {
        self.issuing("enrolls devices")?;
        let mut draft = self.clone();
        let reply = draft.claim(vault, request);
        draft.save(path)?;
        *self = draft;
        reply
    }
    fn claim(&mut self, vault: &Vault, request: &[u8]) -> Result<Vec<u8>, Failure> {
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
        let known = authority.devices.get(&request.node);
        match admission {
            Admission::First if known.is_some() || authority.devices.len() >= MAX_DEVICES => {
                return Err("device already enrolled or capacity reached".into());
            }
            Admission::Retry
                if !known.is_some_and(|device| {
                    device.nonce == request.nonce && device.user == request.user
                }) =>
            {
                return Err("invitation already used".into());
            }
            _ => {}
        }
        active.claim(&request);
        let profile = active.profile.clone();
        let grants = profile.grants(&domain.resources())?;
        // A retry carves nothing again: the first answer's range is kept.
        let authors = match (profile.issues(), admission) {
            (true, Admission::First) => self.minting.as_mut().and_then(Minting::split),
            (true, Admission::Retry) => self
                .authority
                .as_ref()
                .and_then(|authority| authority.devices.get(&request.node))
                .and_then(|device| device.authors),
            _ => None,
        };
        let issued = self.issue(&publisher, &domain, request.user, grants.clone(), authors)?;
        if profile.issues() {
            self.learn_joined_issuer(request.node, &request.routes);
        }
        self.issuing("enrolls devices")?.devices.insert(
            request.node,
            Device {
                user: request.user,
                nonce: request.nonce.clone(),
                enrolled: time,
                capability: issued.id()?,
                revoked: false,
                profile: profile.clone(),
                authors,
            },
        );
        let enrollment = Enrollment {
            mesh: self.mesh,
            domain: self.domain,
            manifest: self.manifest.clone(),
            policy: self.policy.clone(),
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
        Ok(seal_enrollment(
            &token,
            &request,
            &serde_json::to_vec(&enrollment)?,
        )?)
    }

    /// Issues `profile`, raw grants included, to the device whose public
    /// keys `device` names: credentials and reader keys it installs with
    /// `Database::install`, on `terms`. The device is then known
    /// like an enrolled one; save the identity before handing them out.
    /// The result carries reader keys: a secret.
    pub fn grant(
        &mut self,
        vault: &Vault,
        device: DeviceKeys,
        profile: Profile,
        terms: Terms,
        contact: Option<String>,
    ) -> Result<Supplied, Failure> {
        let ttl = terms.ttl(self.ttl)?;
        if profile.issues() {
            return Err("the publisher key travels only in an issuer invitation".into());
        }
        let domain = self.domain()?;
        let grants = profile.grants(&domain.resources())?;
        let devices = &self.issuing("grants credentials")?.devices;
        let known = devices.get(&device.node);
        match known {
            Some(known) if known.revoked => return Err("device was revoked".into()),
            Some(known) if known.user != device.user => {
                return Err("this node is known with another user key".into());
            }
            None if devices.len() >= MAX_DEVICES => return Err("device capacity reached".into()),
            _ => {}
        }
        // Granting again with as many numbers keeps the device's range.
        let kept = known.and_then(|known| known.authors).filter(|range| {
            terms
                .authors
                .is_some_and(|count| range.last - range.first + 1 == count)
        });
        let authors = match (kept, terms.authors) {
            (Some(range), _) => Some(range),
            (None, Some(count)) => Some(self.minting()?.carve(count)?),
            (None, None) => None,
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
            self.policy.statement.epoch,
            (time, ttl, SKEW),
        )?;
        let mut keys = Vec::new();
        for (name, epochs) in self.keys_for(vault, &domain, &grants)? {
            for (epoch, key) in epochs {
                keys.push((name.clone(), epoch, hex32(&key)?));
            }
        }
        self.issuing("grants credentials")?.devices.insert(
            device.node,
            Device {
                user: device.user,
                nonce: String::new(),
                enrolled: time,
                capability: capability.id()?,
                revoked: false,
                profile,
                authors,
            },
        );
        Ok(Supplied {
            mesh: self.mesh,
            domain: self.domain,
            credentials: Credentials {
                publisher: self.publisher,
                manifest: self.manifest()?,
                policy: self.policy.clone(),
                chain: CredentialChain(vec![capability]),
            },
            keys,
            issuers: contact
                .into_iter()
                .chain(self.issuer_contacts(&[device.node])?)
                .collect(),
        })
    }
    /// Delegates `profile`, which must narrow this device's own grants, to
    /// the device whose public keys `device` names: a link signed by this
    /// device's user key, with the reader keys the grants read. Any device
    /// whose capability allows a further delegation does this; the
    /// delegate renews by being delegated to again. `authors` carves fresh
    /// numbers from this device's range each time.
    pub fn delegate(
        &mut self,
        vault: &Vault,
        device: DeviceKeys,
        profile: Profile,
        terms: Terms,
    ) -> Result<Supplied, Failure> {
        let ttl = terms.ttl(self.ttl)?;
        if profile.issues() {
            return Err("the publisher key travels only in an issuer invitation".into());
        }
        let domain = self.domain()?;
        let grants = profile.grants(&domain.resources())?;
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
        Ok(Supplied {
            mesh: self.mesh,
            domain: self.domain,
            credentials: Credentials {
                publisher: self.publisher,
                manifest: self.manifest()?,
                policy: self.policy.clone(),
                chain,
            },
            keys,
            issuers: Vec::new(),
        })
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
        if credentials.policy.statement.epoch < self.policy.statement.epoch {
            return Err("the credentials carry an older policy".into());
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
        credentials
            .policy
            .verify(self.publisher, domain.manifest().hash())?;
        let (grants, _) = chain.verify_selectors(
            self.publisher,
            domain.manifest().hash(),
            node.public_key(),
            &credentials.policy.statement,
            now()?,
        )?;
        let mut keys = Vec::new();
        for (name, epoch, key) in &supplied.keys {
            let scope = domain
                .scopes()
                .find(|(collection, _)| collection == name)
                .map(|(_, scope)| scope);
            if !scope.is_some_and(|scope| reads(&grants, scope)) || *epoch == 0 {
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
            policy: credentials.policy.clone(),
            chain,
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
    /// Assigns a pseudonym to the device that sent `request`: its write
    /// grants on the table, an earlier number it held in this device's
    /// range, or the next free one. Save the identity before answering.
    pub fn assign_pseudonym(
        &mut self,
        vault: &Vault,
        request: &PseudonymRequest,
    ) -> Result<CredentialChain, Failure> {
        let domain = self.domain()?;
        let manifest = domain.manifest().hash();
        let (grants, partitions) = request.verify_selectors(
            self.publisher,
            manifest,
            &self.policy.statement,
            (now()?, SKEW),
        )?;
        let range = self.minting()?.range;
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
            let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
            let root = self.issue(
                &publisher,
                &domain,
                key.public_key(),
                self.grants().to_vec(),
                Some(range),
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
        let recorded = self.author_assignments.get(&request.key);
        if recorded.is_some_and(|held| held.node != request.node || held.table != request.table) {
            return Err("a pseudonym key is already assigned to another device or table".into());
        }
        let kept = recorded
            .filter(|held| range.contains(held.author))
            .map(|held| held.author)
            .or_else(|| request.previous_author(self.publisher, manifest, range));
        let author = match kept {
            Some(author) => {
                if self
                    .author_assignments
                    .iter()
                    .any(|(key, held)| *key != request.key && held.author == author)
                {
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
        self.minting()?.next = self.minting()?.next.max(author + 1);
        self.author_assignments.insert(
            request.key,
            AssignedAuthor {
                node: request.node,
                table: request.table,
                author,
            },
        );
        Ok(assigned)
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
            &self.policy.statement,
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
            .filter(|(_, chain)| {
                chain.0.iter().all(|credential| {
                    credential.capability.manifest == manifest
                        && credential.capability.epoch == self.policy.statement.epoch
                        && credential.capability.expires > time
                })
            })
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
            if self.peers.len() >= MAX_PEERS {
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
            {
                let _ = self.add_peer(vault, uri);
            }
        }
    }
    /// Keeps the routes an issuer enrolled by this device announced, so
    /// both sync and either renews the other's devices.
    fn learn_joined_issuer(&mut self, node: PublicKey, routes: &[(String, String)]) {
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
        let full = (!self.peers.contains_key(&node) && self.peers.len() >= MAX_PEERS)
            || (!self.issuers.contains(&node) && self.issuers.len() >= MAX_ISSUERS);
        if contact.routes.is_empty() || full {
            return;
        }
        if !self.issuers.contains(&node) {
            self.issuers.push(node);
        }
        self.peers.entry(node).or_default().learn(contact.routes);
    }
    fn revoked(&self, node: &PublicKey) -> bool {
        self.policy
            .statement
            .revoked
            .contains(&revoked_subject(node))
    }
    /// Forgets the peers and issuers the current policy revoked, such as a
    /// lost issuer another one revoked.
    fn forget_revoked(&mut self) {
        let gone: Vec<PublicKey> = self
            .peers
            .keys()
            .chain(&self.issuers)
            .filter(|node| self.revoked(node) && self.enrollment_upstream != Some(**node))
            .copied()
            .collect();
        for node in gone {
            self.peers.remove(&node);
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
            policy: credentials.policy.clone(),
            credentials: credentials.chain.clone(),
            ttl: DEFAULT_TTL,
            profile: Profile::Issuer,
            keys: BTreeMap::new(),
            authority: None,
            issuers: Vec::new(),
            enrollment_upstream: None,
            peers: BTreeMap::new(),
            enrollment_routes: BTreeMap::new(),
            logged_out: false,
            lineage: Vec::new(),
            minting: None,
            pseudonyms: BTreeMap::new(),
            author_assignments: BTreeMap::new(),
            author_high_water: 0,
            author_issuer: None,
            groups: BTreeMap::new(),
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
        credentials
            .policy
            .verify(credentials.publisher, domain.manifest().hash())?;
        let (grants, _) = settings.credentials.verify_selectors(
            settings.publisher,
            domain.manifest().hash(),
            node.public_key(),
            &settings.policy.statement,
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
            if !readable.contains_key(name.as_str()) || *epoch == 0 {
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
        epoch: u64,
        key: &[u8; 32],
    ) -> Result<bool, Failure> {
        let epochs = self.keys.entry(name.to_owned()).or_default();
        if epochs.contains(&epoch) {
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
        // A joining issuer tells the enrolling one how to reach it, so both
        // issuers sync and renew through each other.
        if invite.profile.issues()
            && let Ok(address) = transport.address().await
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
            policy: enrollment.policy,
            credentials: CredentialChain(Vec::new()),
            ttl: enrollment.ttl,
            profile: enrollment.profile,
            keys: BTreeMap::new(),
            authority: None,
            issuers: vec![enrollment.issuer],
            enrollment_upstream: None,
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
            author_assignments: BTreeMap::new(),
            author_high_water: 0,
            author_issuer: None,
            groups: BTreeMap::new(),
        };
        let domain = settings.domain()?;
        if settings.manifest()?.verify(settings.publisher)?.hash() != domain.manifest().hash() {
            return Err("the domain's manifest differs from this application schema".into());
        }
        settings
            .policy
            .verify(settings.publisher, domain.manifest().hash())?;
        // The device receives what the invitation said, no more and no less.
        let grants = invite.profile.grants(&domain.resources())?;
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
            if settings.credentials.0[0].capability.authors()? != enrollment.authors {
                return Err("the enrollment's author range differs from its capability".into());
            }
            settings.minting = enrollment.authors.map(Minting::fresh);
        }
        settings.learn_issuers(&vault, &enrollment.issuers);
        settings.save(path)?;
        std::fs::remove_file(Intent::path(path, false))?;
        Ok(settings)
    }
}
impl Settings {
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
            if learn
                && let Some(transport) = transport
                && let Ok(renewed) = self.renew_remotely(vault, transport, first).await
            {
                return Ok(renewed);
            }
            return Ok(Renewed {
                policy: self.policy.clone(),
                chain: self.renew_locally(vault)?,
                keys: Vec::new(),
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
        let reply = exchange(transport, addresses, Protocol::Renew, &request.to_bytes()?).await?;
        let renewal = Renewal::from_bytes(&reply)?;
        let sealed = renewal.open(self.publisher, &request, &secret)?;
        let domain = self.domain()?;
        renewal
            .policy
            .verify(self.publisher, domain.manifest().hash())?;
        if renewal.policy.statement.epoch < self.policy.statement.epoch {
            return Err("renewal carries an older policy".into());
        }
        if renewal.capability.capability.subject != user.public_key()
            || renewal.capability.capability.grants != self.grants()
        {
            return Err("renewal names another user or other grants".into());
        }
        let mut next = self.clone();
        next.policy = renewal.policy.clone();
        let chain = next.chain(renewal.capability, &user, &node, &domain)?;
        let Sealed {
            keys: received,
            issuers,
        } = Sealed::parse(&sealed)?;
        let mut keys = Vec::new();
        for (name, scope) in domain.scopes() {
            if !reads(self.grants(), scope) {
                continue;
            }
            let held = self.keys.get(name);
            for (epoch, key) in received.get(name).into_iter().flatten() {
                if *epoch != 0 && !held.is_some_and(|held| held.contains(epoch)) {
                    keys.push((name.to_owned(), *epoch, hex32(key)?));
                }
            }
        }
        Ok(Renewed {
            policy: renewal.policy,
            chain,
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
        renewal
            .policy
            .verify(self.publisher, domain.manifest().hash())?;
        let grants = self.profile.grants(&domain.resources())?;
        if renewal.capability.capability.subject != user.public_key()
            || renewal.capability.capability.grants != grants
        {
            return Err("the renewal names another user or other grants".into());
        }
        let mut next = self.clone();
        next.policy = renewal.policy.clone();
        let chain = next.chain(renewal.capability, &user, &node, &domain)?;
        let Sealed {
            keys: received,
            issuers,
        } = Sealed::parse(&sealed)?;
        let mut keys = Vec::new();
        for (name, scope) in domain.scopes() {
            if !reads(&grants, scope) {
                continue;
            }
            let held = self.keys.get(name);
            for (epoch, key) in received.get(name).into_iter().flatten() {
                if *epoch != 0 && !held.is_some_and(|held| held.contains(epoch)) {
                    keys.push((name.to_owned(), *epoch, hex32(key)?));
                }
            }
        }
        Ok(Renewed {
            policy: renewal.policy,
            chain,
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
    ) -> Result<Vec<(String, u64, [u8; 32])>, Failure> {
        let mut draft = self.clone();
        let mut held = Vec::new();
        for (name, epoch, key) in &renewed.keys {
            if draft.hold(vault, name, *epoch, key)? {
                held.push((name.clone(), *epoch, *key));
            }
        }
        draft.policy = renewed.policy.clone();
        draft.credentials = renewed.chain.clone();
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
        draft.save(path)?;
        *self = draft;
        Ok(held)
    }
    /// Answers a renewal. The request proves a device the publisher once
    /// issued a chain; it receives the same grants under the current
    /// policy and the reader keys those grants read. A revoked device or
    /// user is refused by any issuer, whoever enrolled it.
    pub fn renew(
        &mut self,
        path: &Path,
        vault: &Vault,
        request: &[u8],
    ) -> Result<Vec<u8>, Failure> {
        self.issuing("renews devices")?;
        let request = RenewRequest::from_bytes(request)?;
        let me = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        let domain = self.domain()?;
        let time = now()?;
        let known = self
            .issuing("renews devices")?
            .devices
            .get(&request.node)
            .cloned();
        if known.as_ref().is_some_and(|device| device.revoked) {
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
                    .map(|device| device.profile.clone())
                    .unwrap_or_else(|| profile_of(&prior.grants, &domain));
                prior.grants = profile.grants(&domain.resources())?;
                prior.manifest = current;
                prior
            }
        };
        let prior = &prior;
        let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
        let authority = Authority {
            publisher: &publisher,
            manifest: current,
        };
        let sealed = Sealed {
            keys: self.keys_for(vault, &domain, &prior.grants)?,
            issuers: self.issuer_contacts(&[request.node])?,
        };
        let renewal = Renewal::seal(
            &authority,
            &request,
            prior,
            self.policy.clone(),
            (time, self.ttl, SKEW),
            &serde_json::to_vec(&sealed)?,
        )?;
        let device = Device {
            user: prior.subject,
            nonce: String::new(),
            enrolled: time,
            capability: renewal.capability.id()?,
            revoked: false,
            profile: profile_of(&prior.grants, &domain),
            authors: None,
        };
        let devices = &mut self.issuing("renews devices")?.devices;
        let room = devices.len() < MAX_DEVICES;
        match devices.get_mut(&request.node) {
            Some(known) => known.capability = device.capability,
            // A device another issuer enrolled is listed once it renews here.
            None if room => {
                devices.insert(request.node, device);
            }
            None => {}
        }
        self.save(path)?;
        Ok(renewal.to_bytes()?)
    }
    /// Whether the policy or this issuer revoked a device or its user.
    pub(crate) fn revoked_device(&self, node: &PublicKey) -> bool {
        let revoked = &self.policy.statement.revoked;
        revoked.contains(&revoked_subject(node))
            || self.authority.as_ref().is_some_and(|authority| {
                authority
                    .devices
                    .get(node)
                    .is_some_and(|device| device.revoked)
                    || authority
                        .holders
                        .get(node)
                        .is_some_and(|holder| revoked.contains(&revoked_subject(&holder.user)))
            })
    }
    /// Records partitions granted or delegated to a device against its node
    /// and user, before the chain leaves this issuer, so that revoking it
    /// rotates them. Refuses a revoked device or user.
    pub(crate) fn record_holder(
        &mut self,
        device: &DeviceKeys,
        grants: &[rocky_credentials::PartitionGrant],
    ) -> Result<(), Failure> {
        if self.revoked_device(&device.node)
            || self
                .policy
                .statement
                .revoked
                .contains(&revoked_subject(&device.user))
        {
            return Err("the device or its user was revoked".into());
        }
        let issuing = self.issuing("grants partitions")?;
        if issuing
            .devices
            .get(&device.node)
            .is_some_and(|known| known.user != device.user)
        {
            return Err("this node is known with another user key".into());
        }
        let holders = &mut issuing.holders;
        if !holders.contains_key(&device.node) && holders.len() >= MAX_DEVICES {
            return Err("partition holder capacity reached".into());
        }
        holders
            .entry(device.node)
            .or_insert_with(|| Holder {
                user: device.user,
                partitions: Vec::new(),
            })
            .record(device.user, grants)
    }
    /// Begins revoking a device: the intent is all that changes. `rotate`
    /// completes it, now or at the next open.
    pub fn begin_revocation(&mut self, node: PublicKey) -> Result<(), Failure> {
        let epoch = self.policy.statement.epoch + 1;
        // Another issuer is revoked like any device; the publisher key it
        // holds is not taken away (`docs/runtime/sdk-enrollment.md`).
        let own = self
            .credentials
            .0
            .last()
            .map(|leaf| leaf.capability.subject);
        if own == Some(node) {
            return Err("an issuer cannot revoke itself".into());
        }
        if self.revoked_device(&node) {
            return Err("device is already revoked".into());
        }
        let authority = self.issuing("revokes devices")?;
        if authority.rotation.is_some() {
            return Err("another revocation is unfinished".into());
        }
        authority.rotation = Some(Rotation { epoch, node });
        Ok(())
    }
    /// Completes a begun revocation: a new policy epoch that ends every
    /// earlier chain and names the device, and a fresh key epoch for every
    /// collection it could read. Earlier keys stay; history is not sealed
    /// again. Repeating it after an interruption gives the same result.
    /// Returns the new keys.
    pub fn rotate(&mut self, vault: &Vault) -> Result<Vec<(String, u64, [u8; 32])>, Failure> {
        let Some(Rotation { epoch, node }) = self
            .authority
            .as_ref()
            .and_then(|authority| authority.rotation.clone())
        else {
            return Ok(Vec::new());
        };
        let domain = self.domain()?;
        let mut revoked = vec![revoked_subject(&node)];
        let mut readable: Option<Profile> = None;
        let issuing = self.issuing("revokes devices")?;
        if let Some(device) = issuing.devices.get_mut(&node) {
            device.revoked = true;
            revoked.extend([revoked_subject(&device.user), device.capability]);
            readable = Some(device.profile.clone());
        }
        // A device known through a partition grant loses its user subject
        // too, so a fresh node key under it cannot renew.
        let holder = issuing.holders.get(&node).cloned();
        if let Some(holder) = &holder {
            revoked.push(revoked_subject(&holder.user));
        }
        revoked.extend(
            self.author_assignments
                .iter()
                .filter(|(_, assigned)| assigned.node == node)
                .map(|(key, _)| revoked_subject(key)),
        );
        // A device this issuer never saw may have read anything.
        let mut keys = Vec::new();
        let names: Vec<String> = self.keys.keys().cloned().collect();
        for name in names {
            if readable
                .as_ref()
                .is_some_and(|profile| !profile.reads(&name))
            {
                continue;
            }
            // The intent was saved first: a key missing here was never
            // used, so making it is not replacing one.
            let role = key_role(&name, epoch);
            let key = match vault.find(&role)? {
                Some(key) => key,
                None => vault.create(&role)?,
            };
            let epochs = self.keys.entry(name.clone()).or_default();
            if !epochs.contains(&epoch) {
                if epochs.len() >= MAX_EPOCHS {
                    return Err("key epoch capacity reached".into());
                }
                epochs.push(epoch);
                epochs.sort_unstable();
            }
            keys.push((name, epoch, key));
        }
        // The group secret is the unit: one new secret per group partition
        // the device could read, so every member table advances together.
        // Its profile's tables and its recorded partition grants count; a
        // device enrolled elsewhere may have read any known partition.
        // Even an address held here without a key rotates: the new secret
        // does not depend on the old one.
        let mut rotated = Vec::new();
        for partition in self.partitions(&domain)? {
            let read = match &readable {
                None => true,
                Some(profile) => {
                    partition
                        .members
                        .iter()
                        .any(|member| profile.reads(&member.table))
                        || holder
                            .as_ref()
                            .is_some_and(|holder| holder.selects(&partition))
                }
            };
            if read {
                partition.record.admit(epoch)?;
                rotated.push(partition.record);
            }
        }
        // Capacity was checked for all before any secret is made; a secret
        // found here was made by an interrupted attempt of this intent.
        for mut record in rotated {
            let role = secret_role(&record.address, epoch);
            if vault.find(&role)?.is_none() {
                vault.create(&role)?;
            }
            record.hold(epoch)?;
            self.groups
                .insert(crate::groups::record_key(&record.address), record);
        }
        let publisher = PrivateKey::from_bytes(&vault.get("publisher")?);
        let authority = Authority {
            publisher: &publisher,
            manifest: domain.manifest().hash(),
        };
        if self.policy.statement.epoch < epoch {
            self.policy = authority.revoke(&self.policy.statement, revoked)?;
        }
        self.credentials = self.renew_locally(vault)?;
        let own_authors = self.pseudonyms.keys().cloned().collect::<Vec<_>>();
        for collection in own_authors {
            let request = self.pseudonym_request(vault, &collection)?;
            let chain = self.assign_pseudonym(vault, &request)?;
            self.hold_pseudonym(vault, chain)?;
        }
        self.peers.remove(&node);
        self.issuers.retain(|issuer| *issuer != node);
        self.enrollment_routes.remove(&node);
        self.issuing("revokes devices")?.rotation = None;
        Ok(keys)
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
    let mut stream = dial(transport, addresses, protocol).await?;
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
