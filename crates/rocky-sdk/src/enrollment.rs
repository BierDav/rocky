//! The SDK's enrollment plumbing over `rocky-credentials`: domain
//! creation, invitations with a grant profile, supplied credentials,
//! renewal by any issuer, and revocation with key rotation. Permissions
//! belong to identities; an identity with issuer authority is an issuer.
//! The publisher key is the domain's offline root: it signs the genesis,
//! the trusted grants that make trusted nodes and their revocations, and
//! nothing else. No device holds it. Every node signs with its own key
//! under its identity's certificate.
use crate::{
    Failure,
    domain::Domain,
    groups::{GroupPartition, GroupRecord, Member, secret_role},
    join_requests::{Dropped, Note, State},
    membership::Membership,
    registry::{self, Changes, Registry, RegistryFull},
    schema::Schema,
    secrets::{Vault, write_private},
    service::ServiceError,
    transport::{Protocol, Stream, Transport},
};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use rocky_core::{Commitment, DomainId, MeshId, ResourceGrant, Right, RowSelection, commitment};
use rocky_credentials::{
    Admission, AuthorRange, Authority, Credentials, IdentityProvider, Invitation,
    MAX_LIVE_REVOCATIONS, PairRequest, Profile, REVOCATION_SET_FULL, RenewRequest, Renewal,
    Revocation, open_enrollment, seal_enrollment,
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
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Current `identity.json` format: issuer records live in the registry,
/// revocations are a set of statements, envelopes wait in `deliveries`,
/// join requests wait in `Issuing::requests`.
pub const VERSION: u32 = 32;
/// Current invitation format: the issuer's node key, and a profile only
/// when the invited device is admitted on arrival.
const INVITE_VERSION: u32 = 7;
/// Current format of what an issuer seals to a joining device.
const ANSWER_FORMAT: u32 = 5;
const CONTACT_VERSION: u32 = 3;
/// Contacts one device keeps: routes to peers, which grant nothing.
pub const MAX_CONTACTS: usize = 64;
/// Contacts one sync pass dials.
pub const MAX_DIALED: usize = 16;
/// Issuers one device renews with: every trusted node, at most.
const MAX_ISSUERS: usize = crate::registry::MAX_TRUSTED;
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
type GroupDelivery = (GroupRecord, Vec<Vec<u8>>);
pub(crate) const EPOCHS: &[u8] = b"rocky.sdk.epochs.v1\n";
/// The naming root an enrollment of `profile` carries: an issuer's only.
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
/// Stores the naming root an issuer's enrollment carried.
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
pub(crate) const POLICY: &[u8] = b"rocky.sdk.policy.v1\n";
/// A new node of an identity asks a node of the same identity for it.
pub(crate) const UNLINK: &[u8] = b"rocky.sdk.membership-unlink.v1\n";
pub(crate) const LINK: &[u8] = b"rocky.sdk.membership-link.v1\n";
pub(crate) const IDENTITY: &[u8] = b"rocky.sdk.identity.v1\n";
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

/// A root-only action without the root's signer, or with another key:
/// creating the domain, granting or revoking trust, and recovery.
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

/// A join through `Session::invitation()` that the issuer has not admitted
/// yet. Both devices show `code`; ask again with the same directory, keys
/// and nonce once the issuer admitted it, before `expires_epoch_seconds`.
#[derive(Clone)]
pub struct AwaitingAdmission {
    pub code: String,
    pub expires_epoch_seconds: u64,
    pub(crate) identity: Arc<dyn IdentityProvider>,
    pub(crate) phrase: Option<String>,
    installation: String,
    invitation: Commitment,
}
impl AwaitingAdmission {
    pub(crate) fn check_resume(&self, directory: &Path, uri: &str) -> Result<(), Failure> {
        let invite = Invite::parse(uri)?;
        let token = hex32(&invite.token)?;
        let digest = commitment("rocky.sdk.invitation.v1", &token);
        let intent: Intent = serde_json::from_slice(&rocky_platform::fs::read(Intent::path(
            &directory.join("identity.json"),
            false,
        ))?)?;
        if digest != self.invitation
            || intent.invitation != Some(digest)
            || intent.installation != self.installation
            || intent.identity != self.identity.public_key()?
        {
            return Err(
                "the pending admission belongs to another installation or invitation".into(),
            );
        }
        Ok(())
    }
}
impl std::fmt::Debug for AwaitingAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwaitingAdmission")
            .field("code", &self.code)
            .field("expires_epoch_seconds", &self.expires_epoch_seconds)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for AwaitingAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "awaiting_admission: the issuer admits this device after confirming code {}",
            self.code
        )
    }
}
impl std::error::Error for AwaitingAdmission {}
/// The issuer refused this device's join request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinRefused;
impl std::fmt::Display for JoinRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("join_refused: the issuer refused this device's join request")
    }
}
impl std::error::Error for JoinRefused {}
/// The root signer one call carries, checked against the pinned publisher
/// key; never stored.
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
    /// The pinned trust anchor: the domain's publisher key, its offline root.
    pub publisher: PublicKey,
    /// The identity this node belongs to; never the root.
    pub identity: PublicKey,
    /// This identity discarded its key after certifying its only node.
    pub single_use: bool,
    pub genesis: String,
    pub generation: u32,
    /// The replay engine the installed generation's tip names.
    pub engine: rocky_core::EngineIdentity,
    pub pending: Option<crate::tip_schemas::Pending>,
    pub pending_follow: Option<String>,
    pub(crate) bootstrap: Option<crate::tip_schemas::Bootstrap>,
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
    /// Present on a node whose identity holds issuer authority.
    pub authority: Option<Issuing>,
    /// The issuers that renew this device, by node key. Their routes are
    /// in `peers`.
    #[serde(skip)]
    pub issuers: Vec<PublicKey>,
    /// A fixed issuer whose pairing/renewal responses this custody node relays.
    #[serde(skip)]
    pub enrollment_upstream: Option<PublicKey>,
    /// This device's contacts, by node key, with the routes last learned
    /// for each. A contact is a route that grants nothing: any admitted
    /// peer is served, and sync dials contacts.
    #[serde(skip)]
    pub peers: BTreeMap<PublicKey, Contact>,
    #[serde(skip)]
    pub contact_meta: BTreeMap<PublicKey, crate::mesh::ContactMeta>,
    #[serde(skip)]
    pub enrollment_routes: BTreeMap<PublicKey, Contact>,
    pub logged_out: bool,
    /// The author numbers this principal grants to other principals, if any.
    pub minting: Option<Minting>,
    /// Stable outgoing principal ranges, including non-issuer delegations.
    pub granted_authors: BTreeMap<PublicKey, AuthorRange>,
    /// Retained across removal/replacement of a supplied author range.
    pub author_high_water: u32,
    /// Envelopes this device sends, by delivery digest, until delivered.
    pub deliveries: BTreeMap<String, crate::delivery::Pending>,
    /// This identity's counter claim right, which its node presents to the
    /// coordination service.
    pub claims: Option<crate::claims::Holding>,
    /// The digest of the live revocations the coordination service last
    /// acknowledged holding. Until it matches the held set's, any node
    /// holding them pushes them again.
    pub revocations_pushed: Option<Commitment>,
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
    /// Default grants keep half a small free range; explicit counts remain exact.
    fn grant(&mut self, count: Option<u32>) -> Result<AuthorRange, Failure> {
        let available = self.range.last.saturating_add(1).saturating_sub(self.next);
        self.carve(count.unwrap_or(available.div_ceil(2).min(1024)))
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
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issuing {
    pub invitation: Option<Invitation>,
    pub invitation_label: Option<Vec<u8>>,
    /// The device that consumed a pre-approved invitation, so its retry is
    /// answered alike. Replaced with the invitation.
    pub claimed: Option<Claim>,
    /// The devices that answered an approval invitation.
    pub requests: crate::join_requests::Requests,
    /// Saved before the rotation's keys are made, so an interrupted
    /// revocation completes at the next open.
    pub rotation: Option<Rotation>,
    /// Every node and identity a statement this issuer learned revoked.
    /// Kept after the statement leaves the set: no renewal for them.
    pub tombstones: BTreeSet<rocky_credentials::PeerId>,
    /// On a trusted node: when it last heard from each linked trusted
    /// node, saved coarsely so a restart keeps staleness.
    pub heard: BTreeMap<PublicKey, u64>,
    /// Local, durable observations of trusted membership, including our own.
    pub trusted_nodes:
        BTreeMap<PublicKey, BTreeMap<PublicKey, crate::membership::TrustedObservation>>,
    /// Seconds another trusted identity may stay unheard before it is
    /// stale; `None` is `TRUSTED_STALE_AFTER`.
    pub stale_after: Option<u64>,
    /// Nodes this issuer admitted or renewed, by node key, until the
    /// membership view holds identities' links.
    pub seen: BTreeMap<PublicKey, crate::membership::Seen>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub node: PublicKey,
    /// The identity the invitation granted to.
    pub user: PublicKey,
    /// The range reserved for the recipient principal.
    pub authors: Option<AuthorRange>,
}
/// A begun revocation: its statement and the key epoch each collection and
/// group partition rotates to. Signing is deterministic: the statement keeps
/// its ID however often an interrupted rotation signs it again.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rotation {
    /// The recorded identities it revokes, whose registry tombstones
    /// complete it.
    pub identities: Vec<PublicKey>,
    /// Their linked nodes, dropped as peers.
    pub nodes: Vec<PublicKey>,
    pub statement: Revocation,
    /// The root's signature, made when it began: the root is not there to
    /// sign when an interrupted revocation completes.
    pub signed: Option<SignedRevocation>,
    pub statements: Vec<Commitment>,
    pub keys: BTreeMap<String, u64>,
    pub groups: BTreeMap<String, u64>,
}
/// What an issuer's revocation ends: an identity and every node of it, or a
/// claim right. Of a trusted identity only the root revokes, and its
/// revocation also ends every identity the trusted one first enrolled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Revoke {
    Identity(PublicKey),
    /// A claim right, by its grant id: every link of it, whoever signed it.
    Claims(Commitment),
}
/// What the registry recorded of an identity a revocation acts on.
struct Revoking {
    capability: Option<Commitment>,
    /// Absent on an identity recorded only by partition grants.
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
pub(crate) struct ContactRecord {
    v: u32,
    pub(crate) publisher: PublicKey,
    pub(crate) database: rocky_credentials::DatabaseContext,
    pub(crate) node: PublicKey,
    pub(crate) routes: Vec<Route>,
    /// A hint that the node renews credentials. Its answers are believed
    /// only when the publisher key signed them.
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
/// What an invitation URI carries.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Invite {
    v: u32,
    routes: Vec<Route>,
    token: String,
    publisher: PublicKey,
    database: rocky_credentials::DatabaseContext,
    /// The issuing node, whose key the admission code covers.
    node: PublicKey,
    expires: u64,
    /// Absent when the issuer admits each requesting device.
    pub(crate) profile: Option<Profile>,
    via_custody: bool,
}
/// What an issuer seals to a joining device under a key derived from the
/// invitation token.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    format: u32,
    outcome: Outcome,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Outcome {
    Enrolled(Box<Enrollment>),
    /// Held until an issuer admits it, at most until `expires`.
    Awaiting {
        expires: u64,
    },
    Refused,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Enrollment {
    mesh: MeshId,
    domain: DomainId,
    /// The domain's current schema, which the device installs whatever
    /// schema its application declares.
    record: crate::tip_schemas::Record,
    genesis: String,
    bootstrap: crate::tip_schemas::Bootstrap,
    revocations: RevocationSet,
    capability: SignedCapability,
    profile: Profile,
    keys: Keys,
    groups: Vec<GroupDelivery>,
    epoch_statements: BTreeMap<String, Vec<Commitment>>,
    /// The partition naming root, hex, for an issuer only.
    naming: Option<String>,
    /// The enrolling issuer's node key, which sync sessions pin.
    issuer: PublicKey,
    /// The issuer links `capability` follows; empty when the owner signed.
    chain: CredentialChain,
    ttl: u64,
    /// The principal range granted by this enrollment.
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
    /// A fresh claim link, when the answering issuer renews a claim right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claims: Option<CredentialChain>,
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
        if invite.database.publisher != invite.publisher {
            return Err("invitation database publisher mismatch".into());
        }
        Contact::checked(invite.routes.clone())?;
        Ok(invite)
    }
}
/// A pending join or installation, persisted before anything is sent, so
/// a retry is the same device.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Intent {
    /// The invitation a join answers; absent for supplied credentials.
    invitation: Option<Commitment>,
    pub(crate) installation: String,
    nonce: String,
    pub(crate) single_use: bool,
    identity: PublicKey,
}
impl Intent {
    pub(crate) fn path(identity: &Path, supplied: bool) -> std::path::PathBuf {
        identity.with_extension(if supplied {
            "install.json"
        } else {
            "join.json"
        })
    }
    /// The saved intent, or a new one whose device keys now exist. Another
    /// invitation supersedes an unfinished join: its keys may already be
    /// registered under the consumed invitation, so they are discarded.
    /// The provider supplies only its public identity; no signing key is saved.
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
/// The public halves of a node's keys, for an issuer outside the SDK.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeKeys {
    /// The identity the capability to issue names.
    pub identity: PublicKey,
    /// The key that signs the node's operations.
    pub node: PublicKey,
}
/// Credentials and reader keys supplied to a device from outside the SDK.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Supplied {
    pub mesh: MeshId,
    pub domain: DomainId,
    /// The signed manifest of the supplier's generation, which an
    /// installation starts at. The credentials name no generation.
    pub manifest: SignedManifest,
    /// The chain ends at the node's certificate, or at its identity; the
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
    /// certify for its nodes.
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
    /// The issuer that answered, when renewal used a transport.
    pub contacted: Option<PublicKey>,
    pub revocations: RevocationSet,
    pub chain: CredentialChain,
    pub keys: Vec<(String, rocky_core::KeyEpoch, [u8; 32])>,
    pub epoch_statements: BTreeMap<String, Vec<Commitment>>,
    pub groups: Vec<GroupDelivery>,
    /// What the grants amount to when they changed, as supplied ones may.
    pub profile: Option<Profile>,
    /// Contacts of further issuers the answering issuer reaches.
    pub issuers: Vec<String>,
    /// A renewed claim link from the answering issuer.
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
/// The key provider role of a collection's reader key at `epoch`. Epoch 1
/// keeps the name it had before key epochs.
pub(crate) fn key_role(collection: &str, epoch: rocky_core::KeyEpoch) -> String {
    format!(
        "reader.{collection}.{}.{}",
        epoch.generation,
        HEXLOWER.encode(&epoch.secret_hash)
    )
}
/// What a joining device expects to join.
/// What a joining device's application is: its name, whatever version of
/// the schema it declares.
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
/// `grants_of`, or none for a profile that names no collection, as one of
/// partition grants alone does: what a renewal grants over the current
/// generation.
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
    /// The statement of `set` that revokes this device, once the whole set
    /// verifies.
    pub(crate) fn revoking(
        &self,
        set: &RevocationSet,
    ) -> Result<Option<SignedRevocation>, Failure> {
        set.verify_in(self.database_context()?)?;
        Ok(set.revoking_holder(&self.credentials)?.cloned())
    }

    pub(crate) fn revocation(&self, path: &Path) -> Result<Option<SignedRevocation>, Failure> {
        let bytes = match rocky_platform::fs::read(path.with_file_name("revoked.json")) {
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

    /// Why `statement`, which ends this device, does.
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
        } else if let Some(recovery) = revoked.recovery.filter(|_| statement.root()) {
            crate::RevocationReason::Recovery {
                recovered: recovery.keep.to_hex(),
            }
        } else if let Some(origin) = self
            .link_origin()?
            .filter(|origin| statement.root() && revoked.identities.contains(origin))
        {
            crate::RevocationReason::Origin {
                trusted: origin.to_hex(),
            }
        } else {
            crate::RevocationReason::Capability
        })
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
    /// tombstones every node and identity they revoke, for good; its next
    /// registry commit writes the records' tombstones. Returns the
    /// statements added.
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
    /// The digest and live count of the held revocations, and whether the
    /// coordination service has yet to acknowledge them, as status shows them.
    pub(crate) fn revocation_status(&self) -> Result<serde_json::Value, Failure> {
        let time = now()?;
        Ok(serde_json::json!({
            "digest": HEXLOWER.encode(&self.revocations.digest(time)?),
            "statements": self.revocations.live(time),
            "unpushed": self.unpushed_revocations(time)?.is_some(),
        }))
    }
    /// The held revocations and their digest at `time` while the
    /// coordination service has not acknowledged that digest.
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
    /// Whether `set` holds a live statement this identity lacks.
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

    /// Loads only the current identity format, without changing the file or keys.
    pub fn load(path: &Path) -> Result<Self, Failure> {
        let value: serde_json::Value = serde_json::from_slice(&rocky_platform::fs::read(path)?)?;
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
    pub fn save(&self, path: &Path) -> Result<(), Failure> {
        {
            let saved = self.with_saved_contacts();
            saved.validate_saved_contacts()?;
            write_private(path, &serde_json::to_vec(&saved)?)
        }
    }
    pub fn vault(&self, store: &std::sync::Arc<dyn crate::SecretStore>) -> Vault {
        Vault::new(store.clone(), &self.installation)
    }
    /// Whether this node's identity holds issuer authority.
    pub fn issuer(&self) -> bool {
        self.authority.is_some()
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
    /// The grants of this device's node: its chain's leaf.
    fn grants(&self, domain: &Domain) -> Vec<ResourceGrant> {
        match self.credentials.held() {
            Some(held) => domain.families.for_capability(held).map_or_else(
                |_| held.grants.clone(),
                |families| families.resources(&held.grants),
            ),
            None => Vec::new(),
        }
    }
    /// The paired grants this device's selectors resolve on one partition.
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
    /// Whether this device may read a partition in whole, as its key needs.
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
    /// Whether this device reads, inserts, updates and deletes every row
    /// of `member`.
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
    /// The members of every known group partition under `domain`'s schema.
    pub(crate) fn partitions(&self, domain: &Domain) -> Result<Vec<GroupPartition>, Failure> {
        self.groups
            .values()
            .map(|record| GroupPartition::of(domain, record))
            .collect()
    }
    /// Whether this device may read `scope` in whole, as a checkpoint needs.
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
    /// The scopes this device's credentials name: what its sessions and
    /// its sync cover.
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
    /// Configures the peer a contact URI names, or learns its new routes.
    /// A contact that says so is also asked for renewals.
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
    /// Forgets a peer, an issuer included: any issuer renews any device,
    /// and a member left without issuer contact reports it.
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
    /// Every secret role this installation holds.
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
    /// Whether another issuer is known: a contact, another issuer identity,
    /// or another node of this one.
    pub fn another_issuer(&self, registry: &Registry, membership: &Membership) -> bool {
        let own = self.identity;
        !self.issuers.is_empty()
            || membership.nodes(&own).count() > 1
            || registry.records.iter().any(|(identity, record)| {
                *identity != own && !record.revoked && record.profile().is_some_and(Profile::issues)
            })
    }

    /// A new domain whose root `root` signs its genesis and a trusted
    /// grant to the supplied identity, or the root's derived owner identity. The root's key is the publisher key; it is used here
    /// and dropped, never stored.
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
        // The registry's key is made like any collection's.
        let mut keys = BTreeMap::new();
        for (name, _) in bound.scopes() {
            let key = random::<32>();
            let epoch = rocky_core::KeyEpoch::new(1, &key);
            vault.put(&key_role(name, epoch), &key)?;
            keys.insert(name.to_owned(), vec![epoch]);
        }
        let manifest = SignedManifest::sign(&bound.manifest(), root)?;
        let grant = Authority::owner(root).issue_trusted(
            identity.public_key()?,
            bound.scope,
            AuthorRange::DOMAIN,
            now()?,
        )?;
        let mut settings = Self::installed(
            (installation, single_use),
            (publisher, identity.public_key()?),
            (mesh, domain, schema.clone()),
            HEXLOWER.encode(&manifest.to_bytes()?),
            ttl,
        );
        settings.keys = keys;
        settings.authority = Some(Issuing::default());
        settings.minting = Some(settings.minting_of(AuthorRange::DOMAIN));
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
    /// A trusted node the root installs from the identity record of
    /// `archived`, the node `predecessor` of the domain whose backup it
    /// restores, any node, with the keys `recovered` opened from its
    /// store's escrow records. A collection none opened gets a fresh key,
    /// and a store without the naming root a fresh one. Its identity is
    /// fresh, supplied or single-use; the root
    /// grants it trust over a recovery slot of the author numbers only the
    /// root grants (`recovery_authors`).
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
        let authors = recovery_authors(archived)?;
        let installation = HEXLOWER.encode(&random::<16>());
        let vault = Vault::new(store.clone(), &installation);
        let single_use = provider.is_none();
        let identity: Arc<dyn IdentityProvider> =
            provider.unwrap_or_else(|| Arc::new(crate::IdentityKey::generate()));
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
        // Partitions the store named without a key stay known, keyless.
        let mut groups = archived.groups.clone();
        for record in groups.values_mut() {
            record.epochs.clear();
        }
        for (key, record) in &recovered.groups {
            match groups.get_mut(key) {
                Some(named) if named.name != record.name => {
                    return Err(
                        "an escrowed partition differs from the one the backup names".into(),
                    );
                }
                Some(named) => named.epochs = record.epochs.clone(),
                None => drop(groups.insert(key.clone(), record.clone())),
            }
        }
        // A second ahead, so a grant signed in the same second precedes it.
        let grant = Authority::owner(root).issue_trusted(
            identity.public_key()?,
            bound.scope,
            authors,
            after_recovery(&archived.revocations, now()? + 1),
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
        settings.minting = Some(settings.minting_of(authors));
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
    /// A node of `identity` about to be installed in `domain`, holding
    /// nothing yet: no credentials, keys, contacts or authority.
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
            issuers: Vec::new(),
            enrollment_upstream: None,
            delivery_nodes: BTreeMap::new(),
            contact_refs: BTreeMap::new(),
            resolved_refs: BTreeSet::new(),
            contact_meta: BTreeMap::new(),
            peers: BTreeMap::new(),
            enrollment_routes: BTreeMap::new(),
            logged_out: false,
            minting: None,
            granted_authors: BTreeMap::new(),
            author_high_water: 0,
            deliveries: BTreeMap::new(),
            claims: None,
            revocations_pushed: None,
            groups: BTreeMap::new(),
            epoch_statements: BTreeMap::new(),
        }
    }
    pub(crate) fn validity(&self) -> Result<(u64, u64, u64), Failure> {
        Ok((now()?, self.ttl, SKEW))
    }
    /// The root's trusted grant to `identity`, a recorded issuer identity, with the registry row that tells every trusted
    /// node: issuer authority over its recorded author range, tip and
    /// bookmark control, and no expiry. `root` signs and is dropped.
    pub fn trust(
        &self,
        registry: &Registry,
        identity: PublicKey,
        root: Option<&dyn IdentityProvider>,
    ) -> Result<(SignedCapability, Changes), Failure> {
        let root = root_signer(root, self.publisher, "granting trust")?;
        if self.tombstoned(&identity) || self.revocations.revokes_origin(&identity) {
            return Err("identity was revoked".into());
        }
        let record = registry
            .get(&identity)
            .filter(|record| !record.revoked)
            .ok_or("this identity is not recorded")?;
        let authors = record
            .capability()
            .map(|capability| capability.authors)
            .ok_or("this identity is not recorded")?;
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
            authors,
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
    /// A recovered identity's own statements: the root ends every trusted
    /// grant valid before this identity's, with what those identities
    /// first enrolled, wherever a store learns of them. `cut` keeps the
    /// held heads of the chains it ends, split over as many statements as
    /// the cut bounds need. Returns whether they were signed now rather
    /// than held.
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
        let slot = grant
            .authors()?
            .and_then(|authors| authors.recovery_slot())
            .ok_or("not a recovered identity")?;
        let recovery = rocky_credentials::RootRecovery {
            since: grant.not_before,
            keep: self.identity,
            slot,
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
                        recovery: Some(recovery),
                    },
                    database,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.learn_revocations(&RevocationSet::try_from(statements)?)?;
        Ok(true)
    }
    /// Takes `link` as this issuer identity's credentials once it is the
    /// root's current trusted grant to it: its nodes are trusted nodes.
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
    /// The chain this identity signs tip statements through as a trusted
    /// identity, while its grant holds: its root-signed grant.
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
    /// An identity capability `authority` signs for `identity`: issuer
    /// authority with `authors`, or `grants` and their whole partitions.
    fn issue(
        &self,
        authority: &Authority<'_>,
        domain: &Domain,
        identity: PublicKey,
        grants: Vec<ResourceGrant>,
        (issues, authors): (bool, Option<AuthorRange>),
    ) -> Result<SignedCapability, Failure> {
        if issues {
            return Ok(authority.issue_issuer(identity, authors, self.validity()?)?);
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
            (1, authors),
            self.validity()?,
        )?)
    }
    /// The origin this identity's own link names: `None` for a trusted
    /// identity, which the root enrolled.
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
    /// The origin of what this identity enrolls: itself when trusted.
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
        // The envelopes travel inside a message the issuer authenticates.
        let sender = PrivateKey::from_bytes(&vault.get("node")?);
        let mut deliveries = Vec::new();
        for partition in self.partitions(&domain)? {
            let Some(current) = partition.record.current() else {
                continue;
            };
            if !partition.members.iter().all(|member| {
                grants.iter().any(|grant| {
                    // Wildcard members receive keys through explicit sharing, not renewal.
                    let selected = reads(&capability.grants, domain.registry())
                        || grant.selector.0.values().all(|selection| {
                            matches!(selection, rocky_core::PartitionSelection::Only(_))
                        });
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
                        record.address,
                        epoch.generation,
                        vault.get(&role)?,
                        record.name,
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
                // The enrollment or renewal carrying it is the issuer's.
                let (secret, name) =
                    envelope.open(&node, envelope.issuer(), offered.address, epoch)?;
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
    /// Restores group epochs the replica used but this file no longer
    /// records, as after restoring it on its own: an epoch comes back only
    /// when the vault still holds its secret. Returns whether any did.
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
    /// Opens a new invitation, replacing an earlier one and ending its
    /// join requests. With `profile` the invited device is admitted on
    /// arrival; without, it waits for `admit`.
    pub async fn invite(
        &mut self,
        path: &Path,
        vault: &Vault,
        transport: &dyn Transport,
        (profile, label): (Option<Profile>, Option<Vec<u8>>),
        registry: &Registry,
    ) -> Result<(String, u64, Vec<Note>), Failure> {
        if label
            .as_ref()
            .is_some_and(|label| label.len() > registry::MAX_LABEL)
        {
            return Err("invitation label exceeds 256 bytes".into());
        }
        if let Some(profile) = &profile {
            self.check_profile(profile)?;
        }
        self.issuing("invites devices")?;
        if registry.identities() >= registry::MAX_IDENTITIES {
            return Err(Box::new(RegistryFull));
        }
        let address = transport.address().await?;
        // Nothing was sealed under an unused invitation token.
        let token = vault.replace("invitation")?;
        let expires = now()? + INVITATION_TTL;
        let issuing = self.issuing("invites devices")?;
        issuing.invitation_label = label;
        issuing.invitation = Some(Invitation::open(&token, profile.clone(), expires));
        issuing.claimed = None;
        let notes = issuing.requests.end(Dropped::Replaced);
        self.save(path)?;
        let invite = Invite {
            v: INVITE_VERSION,
            routes: vec![Route {
                transport: transport.kind().into(),
                address,
            }],
            token: HEXLOWER.encode(&token),
            publisher: self.publisher,
            database: self.database_context()?,
            node: PrivateKey::from_bytes(&vault.get("node")?).public_key(),
            expires,
            profile,
            via_custody: false,
        };
        let uri = format!(
            "rocky://join/{}",
            BASE64URL_NOPAD.encode(&serde_json::to_vec(&invite)?)
        );
        Ok((uri, expires, notes))
    }
    /// Refuses a profile no device of this domain may be granted.
    fn check_profile(&self, profile: &Profile) -> Result<(), Failure> {
        let domain = self.domain()?;
        profile.grants(&domain.resources())?;
        grants_of(profile, &domain)?;
        Ok(())
    }
    /// When the invitation that requests answered expired: ends them.
    pub(crate) fn expire_requests(&mut self, time: u64) -> Vec<Note> {
        let Some(issuing) = self.authority.as_mut() else {
            return Vec::new();
        };
        let ended = issuing
            .invitation
            .as_ref()
            .is_none_or(|invitation| invitation.expires < time);
        if ended && issuing.requests.invitation.is_some() {
            issuing.requests.end(Dropped::Expired)
        } else {
            Vec::new()
        }
    }
    /// The pending join requests and the time their invitation expires.
    pub(crate) fn pending_requests(&self) -> (Vec<crate::join_requests::Request>, u64) {
        self.authority
            .as_ref()
            .map(|issuing| {
                (
                    issuing.requests.pending().cloned().collect(),
                    issuing
                        .invitation
                        .as_ref()
                        .map_or(0, |invitation| invitation.expires),
                )
            })
            .unwrap_or_default()
    }
    /// Admits `node`'s pending request as `profile` once `code` matches
    /// the code its device shows. It enrolls at the device's next retry.
    pub(crate) fn admit(
        &mut self,
        node: &PublicKey,
        code: &str,
        (profile, label): (Profile, Option<Vec<u8>>),
        registry: &Registry,
    ) -> Result<Vec<Note>, Failure> {
        if label
            .as_ref()
            .is_some_and(|label| label.len() > registry::MAX_LABEL)
        {
            return Err("device label exceeds 256 bytes".into());
        }
        self.check_profile(&profile)?;
        if registry.identities() >= registry::MAX_IDENTITIES {
            return Err(Box::new(RegistryFull));
        }
        self.issuing("admits devices")?
            .requests
            .admit(node, code, (profile, label))
    }
    pub(crate) fn refuse(&mut self, node: &PublicKey) -> Result<Vec<Note>, Failure> {
        self.issuing("refuses devices")?.requests.refuse(node)
    }

    pub(crate) fn invitation_identity(uri: &str) -> Result<(String, u64), Failure> {
        let invite = Invite::parse(uri)?;
        let token = HEXLOWER.decode(invite.token.as_bytes())?;
        Ok((
            HEXLOWER.encode(&commitment("rocky.sdk.invitation.v1", &token)),
            invite.expires,
        ))
    }

    /// Checks this issuer's persisted invitation without attempting enrollment.
    pub fn invitation_open(&self, uri: &str) -> Result<bool, Failure> {
        let authority = self
            .authority
            .as_ref()
            .ok_or("only an issuer checks invitations")?;
        let invite = Invite::parse(uri)?;
        let token: [u8; 32] = HEXLOWER
            .decode(invite.token.as_bytes())?
            .try_into()
            .map_err(|_| "invalid invitation token")?;
        let time = now()?;
        Ok(invite.database == self.database_context()?
            && authority
                .invitation
                .as_ref()
                .is_some_and(|invitation| invitation.is_open(&token, time)))
    }

    /// Answers one pairing request with what is sealed to its device and
    /// the registry rows to commit before it leaves. Save the identity
    /// first, even on failure: it counts the invitation's attempts.
    pub fn pair(
        &mut self,
        vault: &Vault,
        request: &[u8],
        registry: &Registry,
    ) -> Result<(Vec<u8>, Changes, Vec<Note>), Failure> {
        let request = PairRequest::from_bytes(request)?;
        let token = vault.get("invitation")?;
        let expected = context(&self.schema, self.database_context()?);
        if request.context != expected {
            return Err("application mismatch".into());
        }
        let time = now()?;
        let active = self
            .issuing("enrolls devices")?
            .invitation
            .as_mut()
            .ok_or("no active invitation")?;
        let admission = active.admit(&token, &request, time)?;
        let (profile, expires) = (active.profile.clone(), active.expires);
        if self.revoked_node(
            &NodeKeys {
                identity: request.user,
                node: request.node,
            },
            registry,
        ) {
            return Err("the node or its identity was revoked".into());
        }
        let seal = |outcome| {
            let answer = Answer {
                format: ANSWER_FORMAT,
                outcome,
            };
            Ok::<_, Failure>(seal_enrollment(
                &token,
                &request,
                &serde_json::to_vec(&answer)?,
            )?)
        };
        if let Some(profile) = profile {
            return self.pair_invited(vault, &request, registry, (&token, profile, admission));
        }
        let held = self
            .issuing("enrolls devices")?
            .requests
            .of(&request)
            .map(|held| held.state.clone());
        match held {
            Some(State::Pending) => Ok((
                seal(Outcome::Awaiting { expires })?,
                Changes::default(),
                Vec::new(),
            )),
            Some(State::Refused) => Ok((seal(Outcome::Refused)?, Changes::default(), Vec::new())),
            Some(State::Admitted {
                profile,
                label,
                authors,
            }) => {
                let authors = match authors {
                    Some(authors) => authors,
                    None => self.carve_authors(&profile)?,
                };
                if let Some(held) = self
                    .issuing("enrolls devices")?
                    .requests
                    .of_mut(&request.node)
                    && let State::Admitted { authors: kept, .. } = &mut held.state
                {
                    *kept = Some(authors);
                }
                let (enrollment, changes) = self.enroll(
                    vault,
                    &request,
                    registry,
                    (&token, profile.clone(), label),
                    authors,
                )?;
                let enrolled = Note::Enrolled {
                    node: request.node,
                    profile,
                };
                Ok((
                    seal(Outcome::Enrolled(Box::new(enrollment)))?,
                    changes,
                    vec![enrolled],
                ))
            }
            None => {
                if registry.admitted(&request.user) {
                    return Err("device already enrolled".into());
                }
                let issuer = PrivateKey::from_bytes(&vault.get("node")?).public_key();
                let notes = self.issuing("enrolls devices")?.requests.receive(
                    &crate::join_requests::invitation_digest(&token),
                    &issuer,
                    &request,
                    time,
                )?;
                Ok((
                    seal(Outcome::Awaiting { expires })?,
                    Changes::default(),
                    notes,
                ))
            }
        }
    }
    /// Answers a pre-approved invitation: its first device enrolls at once
    /// and consumes it; that device's retry is answered alike.
    fn pair_invited(
        &mut self,
        vault: &Vault,
        request: &PairRequest,
        registry: &Registry,
        (token, profile, admission): (&[u8; 32], Profile, Admission),
    ) -> Result<(Vec<u8>, Changes, Vec<Note>), Failure> {
        let authority = self.issuing("enrolls devices")?;
        let claimed = authority
            .claimed
            .as_ref()
            .filter(|claim| claim.node == request.node && claim.user == request.user)
            .cloned();
        match admission {
            Admission::First if registry.revoked(&request.user) => {
                return Err("identity was revoked".into());
            }
            // The bound refuses before the invitation is consumed.
            Admission::First
                if !registry.admitted(&request.user)
                    && registry.identities() >= registry::MAX_IDENTITIES =>
            {
                return Err(Box::new(RegistryFull));
            }
            Admission::Retry if claimed.is_none() => {
                return Err("invitation already used".into());
            }
            _ => {}
        }
        // A retry carves nothing again: the first answer's range is kept.
        let authors = match claimed.and_then(|claim| claim.authors) {
            Some(authors) => authors,
            None => self.carve_authors(&profile)?,
        };
        let authority = self.issuing("enrolls devices")?;
        authority
            .invitation
            .as_mut()
            .ok_or("no active invitation")?
            .claim(request);
        authority.claimed = Some(Claim {
            node: request.node,
            user: request.user,
            authors: Some(authors),
        });
        let label = authority.invitation_label.clone();
        let (enrollment, changes) = self.enroll(
            vault,
            request,
            registry,
            (token, profile.clone(), label),
            authors,
        )?;
        let answer = Answer {
            format: ANSWER_FORMAT,
            outcome: Outcome::Enrolled(Box::new(enrollment)),
        };
        let sealed = seal_enrollment(token, request, &serde_json::to_vec(&answer)?)?;
        let enrolled = Note::Enrolled {
            node: request.node,
            profile,
        };
        Ok((sealed, changes, vec![enrolled]))
    }
    /// The author numbers a device enrolled as `profile` gets: an issuer
    /// half of the free ones, any other device a grant of them.
    fn carve_authors(&mut self, profile: &Profile) -> Result<AuthorRange, Failure> {
        if profile.issues() {
            self.minting()?
                .split()
                .ok_or_else(|| "not enough free author numbers".into())
        } else {
            self.minting()?.grant(None)
        }
    }
    /// What admitting a device creates: its capability as `profile`, the
    /// enrollment that carries it and the registry rows recording it. The
    /// one place admission makes a device's credentials.
    fn enroll(
        &mut self,
        vault: &Vault,
        request: &PairRequest,
        registry: &Registry,
        (token, profile, label): (&[u8; 32], Profile, Option<Vec<u8>>),
        authors: AuthorRange,
    ) -> Result<(Enrollment, Changes), Failure> {
        if !registry.admitted(&request.user) && registry.identities() >= registry::MAX_IDENTITIES {
            return Err(Box::new(RegistryFull));
        }
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
            request.user,
            grants.clone(),
            (profile.issues(), Some(authors)),
        )?;
        self.learn_joined_peer(request.node, &request.routes, profile.issues());
        let mut changes = Changes::default();
        changes.admitted(
            &request.user,
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
                invitation: Some(crate::join_requests::invitation_digest(token)),
                single_use: false,
            },
            false,
        )?;
        if registry
            .get(&request.user)
            .and_then(|record| record.label.as_ref())
            .is_none()
            && let Some(label) = &label
        {
            // The label and enrollment record commit together, including after a claimed join restarts.
            changes.label(&request.user, label)?;
        }
        changes.capability(
            &request.user,
            registry::Capability {
                authors: issued
                    .capability
                    .authors()?
                    .ok_or("principal has no author range")?,
                id: issued.id()?,
                expires: issued.capability.expires,
            },
            registry.get(&request.user),
        )?;
        let keys = self.keys_for(vault, &domain, &grants)?;
        let enrollment = Enrollment {
            groups: self.groups_for(vault, request.node, &issued.capability, false)?,
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
            // An issuer signs through its own chain; no key travels.
            chain: links.clone(),
            ttl: self.ttl,
            profile,
            authors: Some(authors),
            issuers: self.issuer_contacts(&[request.node])?,
        };
        Ok((enrollment, changes))
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
        if !registry.admitted(&device.identity) && registry.identities() >= registry::MAX_IDENTITIES
        {
            return Err(Box::new(RegistryFull));
        }
        let authors = Some(self.reserve_authors(device.identity, Some(registry), terms.authors)?);
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
            (terms.delegations.unwrap_or(1), authors),
            (time, ttl, SKEW),
        )?;
        let mut keys = Vec::new();
        for (name, epochs) in self.keys_for(vault, &domain, &grants)? {
            for (epoch, key) in epochs {
                keys.push((name.clone(), epoch, hex32(&key)?));
            }
        }
        let mut changes = Changes::default();
        // A grant of another profile replaces what the record said.
        let replace = known
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
            replace,
        )?;
        changes.capability(
            &device.identity,
            registry::Capability {
                authors: capability
                    .capability
                    .authors()?
                    .ok_or("principal has no author range")?,
                id: capability.id()?,
                expires: capability.capability.expires,
            },
            known,
        )?;
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
    /// Delegates `profile`, which must narrow this device's own grants, to
    /// the device whose public keys `device` names: a link signed by this
    /// node's identity, with the reader keys the grants read. Any device
    /// whose capability allows a further delegation does this; the
    /// delegate renews by being delegated to again. `authors` carves fresh
    /// numbers from this device's range each time. On an issuer the
    /// registry rows recording the delegate come with it.
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
            if !registry.admitted(&device.identity)
                && registry.identities() >= registry::MAX_IDENTITIES
            {
                return Err(Box::new(RegistryFull));
            }
        }
        let node = Self::node_key(vault)?;
        let authors = Some(self.reserve_authors(device.identity, registry, terms.authors)?);
        // An issuer grants instead: its links refuse to delegate.
        let family_context = if domain.families.is_empty() {
            rocky_credentials::GrantFamilies::default()
        } else {
            domain.families.clone()
        };
        let chain = self.credentials.delegable().delegate_with_families(
            rocky_credentials::Signer::node(&node, &self.credentials)?,
            device.identity,
            rocky_core::PrincipalKind::User,
            domain.families.compact_resources(&grants),
            domain
                .families
                .compact_partitions(&whole_partitions(&domain, &grants)),
            (terms.delegations.unwrap_or(1), authors),
            (now()?, ttl, SKEW),
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
                    authors: leaf
                        .capability
                        .authors()?
                        .ok_or("principal has no author range")?,
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
            manifest: self.manifest()?,
            credentials: Credentials::of(self.publisher, self.revocations.clone(), &chain),
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
            .selectors(node.public_key(), now()?)?;
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
    pub(crate) fn reserve_authors(
        &mut self,
        principal: PublicKey,
        registry: Option<&Registry>,
        count: Option<u32>,
    ) -> Result<AuthorRange, Failure> {
        if let Some(range) = registry
            .and_then(|registry| registry.authors(principal))
            .or_else(|| self.granted_authors.get(&principal).copied())
        {
            return Ok(range);
        }
        let range = self.minting()?.grant(count)?;
        self.granted_authors.insert(principal, range);
        Ok(range)
    }
    /// Supplied credentials as this node's chain: a chain that ends at this
    /// identity, or at a certificate it signed for this node, is certified
    /// here, with this node's alias and author number.
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
                .selectors(node, now()?)?;
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
    fn minting(&mut self) -> Result<&mut Minting, Failure> {
        self.minting
            .as_mut()
            .ok_or_else(|| "this principal has no author range available for delegation".into())
    }

    /// The rows of this issuer's own identity record: its identity entry,
    /// current capability. Rows that exist are kept.
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
                authors: root
                    .capability
                    .authors()?
                    .ok_or("principal has no author range")?,
                id: root.id()?,
                expires: root.capability.expires,
            },
            known,
        )?;
        // A trusted node records its own grant: only the root revokes it.
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
        // A revocation interrupted before its tombstone was written, or one
        // learned from a statement.
        for (other, record) in &registry.records {
            if !record.revoked && record.admitted.is_some() && self.tombstoned(other) {
                changes.revoked(other)?;
            }
        }
        Ok(changes)
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
    /// Contacts of the issuers this device reaches, except `exclude`, for
    /// a device it enrolls or renews to learn.
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
    /// Adds the issuers another issuer named as peers that also renew this
    /// device. They are hints: a known peer keeps its routes, and a revoked
    /// node, a full list or an unusable contact is skipped.
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
        let full = (!self.peers.contains_key(&node) && self.contact_count() >= MAX_CONTACTS)
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
    /// Starts or resumes the join an invitation names: this device's keys
    /// exist once it returns, and nothing was sent.
    pub fn prepare_join(
        path: &Path,
        (invitation, name): (&str, &str),
        store: &std::sync::Arc<dyn crate::SecretStore>,
        provider: Option<Arc<dyn IdentityProvider>>,
    ) -> Result<Joining, Failure> {
        if name.len() > registry::MAX_LABEL {
            return Err(
                format!("a device name holds at most {} bytes", registry::MAX_LABEL).into(),
            );
        }
        let invite = Invite::parse(invitation)?;
        let token = hex32(&invite.token)?;
        let digest = commitment("rocky.sdk.invitation.v1", &token);
        let single_use = provider.is_none();
        let provider = provider.or_else(|| Some(Arc::new(crate::IdentityKey::generate())));
        let (intent, vault) = Intent::resume(
            &Intent::path(path, false),
            Some(digest),
            store,
            provider.as_ref(),
            single_use,
        )?;
        Ok(Joining {
            vault,
            invite,
            intent,
            token,
            name: name.into(),
            provider,
        })
    }
    /// The keys of a node that will be handed credentials from outside
    /// the SDK under the supplied identity and its node key. Asking again returns the same keys.
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
    /// Installs supplied credentials and keys for the device
    /// `prepare_install` made. Everything is verified first; nothing is
    /// trusted because of where it came from.
    pub fn install(
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
            issuers: Vec::new(),
            enrollment_upstream: None,
            delivery_nodes: BTreeMap::new(),
            contact_refs: BTreeMap::new(),
            resolved_refs: BTreeSet::new(),
            contact_meta: BTreeMap::new(),
            peers: BTreeMap::new(),
            enrollment_routes: BTreeMap::new(),
            logged_out: false,
            minting: None,
            granted_authors: BTreeMap::new(),
            author_high_water: 0,
            deliveries: BTreeMap::new(),
            claims: None,
            revocations_pushed: None,
            groups: BTreeMap::new(),
            epoch_statements: BTreeMap::new(),
        };
        let domain = settings.domain()?;
        if supplied.manifest.verify(credentials.publisher)?.hash() != domain.manifest().hash() {
            return Err("the domain's manifest differs from this application schema".into());
        }
        // A chain that ends at this node's identity is certified here.
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
            .selectors(node.public_key(), now()?)?;
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
            .map(|range| settings.minting_of(range));
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
        // The intent stays until the database opens: a failed install is
        // retried with the same device keys.
        settings.save(path)?;
        Ok(settings)
    }
    /// Stores a reader key and lists its epoch. A held epoch keeps its key.
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

/// The author numbers of a recovery: a slot of the root's reserve, which
/// no trusted node's range reaches, so nothing a lost node handed out
/// before or after the backup collides. Slots the archived identity's
/// grant used are skipped; among the rest one is random, so recoveries the
/// copy never learned of collide only with probability 1 in
/// `RECOVERY_SLOTS` each.
fn recovery_authors(archived: &Settings) -> Result<AuthorRange, Failure> {
    let used: BTreeSet<u32> = archived
        .credentials
        .0
        .iter()
        .filter_map(|link| link.capability.authors().ok().flatten())
        .filter_map(|range| range.recovery_slot())
        .collect();
    let free = rocky_credentials::RECOVERY_SLOTS - u32::try_from(used.len())?;
    let mut pick = u32::from_le_bytes(random::<4>()) % free.max(1);
    for slot in 0..rocky_credentials::RECOVERY_SLOTS {
        if used.contains(&slot) {
            continue;
        }
        if pick == 0 {
            return Ok(AuthorRange::recovery(slot)?);
        }
        pick -= 1;
    }
    Err("every recovery slot of author numbers is used".into())
}
/// A trusted grant's time: `time`, or later when a recovery would end a
/// grant valid from then.
fn after_recovery(revocations: &RevocationSet, time: u64) -> u64 {
    revocations.recovery().map_or(time, |recovery| {
        time.max(recovery.since + rocky_credentials::LIFETIME_SKEW + 1)
    })
}
/// A join whose keys exist and whose enrollment is still to be fetched.
pub struct Joining {
    pub vault: Vault,
    invite: Invite,
    intent: Intent,
    token: [u8; 32],
    /// What this device calls itself in its join request.
    name: String,
    provider: Option<Arc<dyn IdentityProvider>>,
}
impl Joining {
    /// Enrolls with the issuer over `transport` and stores what it sealed,
    /// or fails with `AwaitingAdmission` until the issuer admits this
    /// device, or with `JoinRefused`.
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
            name,
            provider,
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
        let identity = provider
            .as_ref()
            .ok_or("supply the identity for this installation")?
            .public_key()?;
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let mut request = PairRequest::new(
            &token,
            intent.nonce.clone(),
            (node.public_key(), identity),
            name,
            context(schema, invite.database),
        );
        // Every joining device supplies a return route for later commits.
        if let Ok(address) = transport.address().await
            && !address.is_empty()
        {
            request = request.with_routes(&token, vec![(transport.kind().into(), address)]);
        }
        let sealed = exchange(transport, &addresses, Protocol::Pair, &request.to_bytes()?).await?;
        let answer: serde_json::Value =
            serde_json::from_slice(&open_enrollment(&token, &request, &sealed)?)?;
        if answer["format"].as_u64() != Some(u64::from(ANSWER_FORMAT)) {
            return Err("unsupported enrollment format".into());
        }
        let enrollment = match serde_json::from_value::<Answer>(answer)?.outcome {
            Outcome::Enrolled(enrollment) => *enrollment,
            Outcome::Awaiting { expires } => {
                return Err(Box::new(AwaitingAdmission {
                    code: crate::join_requests::admission_code(
                        &invite.node,
                        &node.public_key(),
                        &intent.nonce,
                    ),
                    expires_epoch_seconds: expires,
                    identity: provider.ok_or("missing pending identity")?,
                    phrase: None,
                    installation: intent.installation.clone(),
                    invitation: intent.invitation.ok_or("missing invitation intent")?,
                }));
            }
            Outcome::Refused => return Err(Box::new(JoinRefused)),
        };
        enrollment.record.bytes()?;
        if enrollment.mesh != invite.database.scope.mesh
            || enrollment.domain != invite.database.scope.domain
        {
            return Err("enrollment belongs to another database".into());
        }
        enrollment.revocations.verify_in(invite.database)?;
        let mut settings = Settings {
            version: VERSION,
            installation: intent.installation.clone(),
            schema: enrollment.record.schema.clone(),
            generation: enrollment.record.generation,
            engine: enrollment.record.engine,
            pending: None,
            pending_follow: None,
            bootstrap: Some(enrollment.bootstrap),
            mesh: enrollment.mesh,
            domain: enrollment.domain,
            publisher: invite.publisher,
            identity,
            single_use: intent.single_use,
            genesis: enrollment.genesis,
            revocations: RevocationSet::new(),
            credentials: CredentialChain(Vec::new()),
            ttl: enrollment.ttl,
            profile: enrollment.profile,
            keys: BTreeMap::new(),
            authority: None,
            issuers: vec![enrollment.issuer],
            enrollment_upstream: None,
            delivery_nodes: BTreeMap::new(),
            contact_refs: BTreeMap::new(),
            resolved_refs: BTreeSet::new(),
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
            minting: None,
            granted_authors: BTreeMap::new(),
            author_high_water: 0,
            deliveries: BTreeMap::new(),
            claims: None,
            revocations_pushed: None,
            groups: BTreeMap::new(),
            epoch_statements: BTreeMap::new(),
        };
        // The schema and lineage are the publisher's.
        settings.verify_schema()?;
        let domain = settings.domain()?;
        settings.epoch_statements = enrollment.epoch_statements.clone();
        settings.learn_revocations(&enrollment.revocations)?;
        // The identity receives what the invitation said, or what the
        // admitting issuer chose, no more and no less, from the owner or an
        // issuer through its chain.
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
            || invite
                .profile
                .as_ref()
                .is_some_and(|invited| *invited != settings.profile)
            || issues != settings.profile.issues()
            || (!issues
                && (granted.len() != grants.len()
                    || granted.iter().any(|grant| !grants.contains(grant))
                    || grants.iter().any(|grant| !granted.contains(grant))))
        {
            return Err("the enrollment differs from its invitation".into());
        }
        let mut links = enrollment.chain.clone();
        links.0.push(enrollment.capability.clone());
        settings.credentials =
            crate::identities::with_identity(provider.ok_or("missing join identity")?, || {
                settings.certify(&vault, links, &domain)
            })?;
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
            // Issuer authority is the capability's; no key travels.
            settings.authority = Some(Issuing::default());
            hold_naming(&vault, enrollment.naming.as_deref())?;
            // Tombstones start with every statement already held.
            settings.learn_revocations(&RevocationSet::new())?;
            if enrollment.capability.capability.authors()? != enrollment.authors {
                return Err("the enrollment's author range differs from its capability".into());
            }
        }
        settings.minting = enrollment.authors.map(|range| settings.minting_of(range));
        settings.adopt_groups(&vault, &enrollment.groups)?;
        settings.learn_issuers(&vault, &enrollment.issuers);
        settings.save(path)?;
        rocky_platform::fs::remove_file(Intent::path(path, false))?;
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
    /// Renews this device's credentials; the root is never needed. A member
    /// asks its issuers in turn, `first` before the others. A trusted node,
    /// whose grant never expires, certifies its node again; with `learn` it
    /// first asks the other issuers, which is how it hears of their
    /// revocations. Another issuer asks issuers above it, and otherwise
    /// certifies its node again under the identity it holds.
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
        // The same grants, or this identity's profile over the current
        // generation, which tables granted like held ones widen.
        if !self.renews_identity(&renewal.capability.capability, &domain)? {
            return Err("renewal names another identity or other grants".into());
        }
        let mut next = self.clone();
        next.learn_revocations(&renewal.revocations)?;
        let chain = next.certify(vault, renewal.chain(), &domain)?;
        // Keys of tables the renewal widened to arrive with it.
        let grants = domain
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
    /// Whether this generation needs wider grants or newly inherited table keys.
    /// Families resolve immediately; renewal supplies missing reader keys.
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
    /// An issuer's own chain, signed again where it can.
    fn renew_locally(&self, vault: &Vault) -> Result<CredentialChain, Failure> {
        self.issue_own(vault)
    }
    /// An issuer's own chain for the current definition: its node certified
    /// again under the links it holds, which a trusted grant never needs
    /// renewed and an issuer above renews otherwise.
    pub(crate) fn issue_own(&self, vault: &Vault) -> Result<CredentialChain, Failure> {
        self.certify(vault, self.identity_links(), &self.domain()?)
    }
    /// Whether a renewed capability is this identity's own, with the
    /// authority it holds and the grants it holds or its profile's over
    /// `domain`.
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
    /// Saves what a renewal brought before the replica learns it: the keys
    /// in the provider, then the identity. Returns the keys now listed and
    /// whether the identity changed.
    pub fn adopt(
        &mut self,
        path: &Path,
        vault: &Vault,
        renewed: &Renewed,
    ) -> Result<(Vec<NewKey>, bool), Failure> {
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
        // Changing or removing a range must never rewind its allocator.
        if let Some(profile) = &renewed.profile {
            draft.profile = profile.clone();
            draft.author_high_water = draft
                .author_high_water
                .max(draft.minting.map_or(0, |minting| minting.next));
            draft.minting = renewed.chain.delegable().authors()?.map(|range| Minting {
                next: draft.minting_of(range).next.max(draft.author_high_water),
                range,
            });
        }
        let changed = !draft.persisted_eq(self);
        if changed {
            draft.save(path)?;
        }
        *self = draft;
        Ok((held, changed))
    }
    /// Answers a renewal with the registry rows to commit before it
    /// leaves. The request proves a device the publisher once issued a
    /// chain; it receives the same grants, the revocations this issuer
    /// holds and the reader keys those grants read. The record's tombstone
    /// and profile count wherever the device enrolled; without a record the
    /// presented chain is renewed and recorded. A node or user a statement
    /// revoked is refused, also after the statement left the set.
    /// `claims` is the fresh claim link `renew_claims` signed for it.
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
        // A chain names no generation. Tables later generations granted like
        // one the identity holds join its grants; nothing is narrowed. Issuer
        // authority names no grants: it holds every one.
        request.chain.verify_database(self.database_context()?)?;
        let mut prior = request
            .verify_with_families(me, self.publisher, (time, SKEW), &domain.families)?
            .clone();
        let known = registry.get(&prior.subject);
        if self.revoked_identity(&prior.subject, registry) {
            return Err("device was revoked".into());
        }
        if !prior.extended()?.issues {
            let profile = known
                .and_then(registry::IdentityRecord::profile)
                .cloned()
                .unwrap_or_else(|| profile_of(&prior.grants, &domain));
            let widened = profile_grants(&profile, &domain)?;
            if prior
                .grants
                .iter()
                .all(|grant| widened.iter().any(|wide| wide.includes(grant)))
            {
                prior.grants = widened;
            }
        }
        // Partitions it selects extend to tables that follow theirs.
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
        if !registry.admitted(&prior.subject) && registry.identities() >= registry::MAX_IDENTITIES {
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
            (time, self.ttl, SKEW),
            &serde_json::to_vec(&sealed)?,
        )?;
        let mut changes = Changes::default();
        // An identity another issuer enrolled is recorded once a node of it
        // renews here, unless its record already arrived.
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
                authors: renewal
                    .capability
                    .capability
                    .authors()?
                    .ok_or("principal has no author range")?,
                id: renewal.capability.id()?,
                expires: renewal.capability.capability.expires,
            },
            known,
        )?;
        Ok((renewal.to_bytes()?, changes))
    }
    /// Whether a key is revoked for good here: by a held statement or, on
    /// an issuer, a tombstone.
    pub(crate) fn tombstoned(&self, key: &PublicKey) -> bool {
        self.revocations.revokes(key)
            || self.authority.as_ref().is_some_and(|authority| {
                authority
                    .tombstones
                    .contains(&rocky_credentials::PeerId::of(*key))
            })
    }
    /// Whether a statement, a tombstone or the registry revoked `identity`.
    pub(crate) fn revoked_identity(&self, identity: &PublicKey, registry: &Registry) -> bool {
        self.tombstoned(identity) || registry.revoked(identity)
    }
    /// Whether `keys` names a revoked node or identity.
    pub(crate) fn revoked_node(&self, keys: &NodeKeys, registry: &Registry) -> bool {
        self.tombstoned(&keys.node) || self.revoked_identity(&keys.identity, registry)
    }
    /// The registry rows recording partitions granted or delegated to an
    /// identity, committed before the chain leaves this issuer so that
    /// revoking it rotates them. Every node the identity links reads them.
    /// Refuses a revoked node or identity.
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
        if !registry.admitted(&device.identity) && registry.identities() >= registry::MAX_IDENTITIES
        {
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
    /// Begins revoking `target`: an identity, and with it every node it
    /// linked (as `membership` knows them), or a claim right. Fixes its
    /// statement, from what the registry recorded of the identity, and the
    /// key epoch each collection and group partition it could read rotates
    /// to, the held maximum plus one. The intent is all that changes;
    /// `rotate` signs and completes it, now or at the next open. A trusted
    /// identity only `root` revokes: it signs here, and the statement also
    /// ends every identity that one first enrolled.
    #[cfg(test)]
    pub fn begin_revocation(
        &mut self,
        target: Revoke,
        (registry, membership): (&Registry, &Membership),
        root: Option<&dyn IdentityProvider>,
    ) -> Result<(), Failure> {
        self.begin_revocation_cut(target, (registry, membership), root, Vec::new())
    }
    /// As `begin_revocation`; a root statement also carries `cut`, the
    /// heads of the revoked nodes' chains of tip statements that stay valid.
    pub fn begin_revocation_cut(
        &mut self,
        target: Revoke,
        (registry, membership): (&Registry, &Membership),
        root: Option<&dyn IdentityProvider>,
        cut: Vec<rocky_credentials::Cut>,
    ) -> Result<(), Failure> {
        let own = self.identity;
        let trusted = match target {
            Revoke::Identity(identity) => registry.trusted.contains_key(&identity),
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
        let identities: Vec<PublicKey> = match target {
            Revoke::Identity(identity) => {
                if identity == own {
                    return Err("an issuer cannot revoke its own identity".into());
                }
                if identity == self.publisher {
                    return Err("the root is never revoked".into());
                }
                if self.tombstoned(&identity) {
                    return Err("identity is already revoked".into());
                }
                // A trusted identity and the identities it first enrolled,
                // which its revocation ends too.
                let mut identities = BTreeSet::from([identity]);
                if trusted {
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
        // An identity loses its capabilities, so no node of it renews.
        let issuer = if root.is_some() { self.publisher } else { own };
        let statement = match target {
            // The cascade is the root statement's: it names the trusted
            // identity alone, whose origin the enrolled ones carry.
            Revoke::Identity(identity) => Revocation {
                app: domain.manifest().app,
                issuer,
                time,
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
                nodes: BTreeSet::new(),
                identities: BTreeSet::new(),
                capabilities: [capability].into(),
                cut: Default::default(),
                recovery: None,
            },
        };
        // A claim right reads nothing: no key rotates.
        let trust = matches!(target, Revoke::Claims(_));
        // Only identities without any record use the full-rotation fallback.
        let recorded = !records.is_empty();
        let readable: Vec<Profile> = records
            .iter()
            .filter_map(|record| record.profile.as_ref())
            .map(|profile| domain.effective(profile))
            .collect();
        let mut keys = BTreeMap::new();
        for (name, epochs) in &self.keys {
            if trust || (recorded && !readable.iter().any(|profile| profile.reads(name))) {
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
        // the nodes could read, so every member table advances together.
        // Their profiles' tables and their recorded partition grants count; a
        // node absent from the registry may have read any known partition. A
        // partition whose secret this issuer never held has no epoch to
        // follow, and is left to an issuer that holds it.
        let mut groups = BTreeMap::new();
        for partition in self.partitions(&domain)? {
            let read = !recorded
                || readable.iter().any(|profile| {
                    partition
                        .members
                        .iter()
                        .any(|member| profile.reads(&member.table))
                })
                || records.iter().any(|record| record.selects(&partition));
            if let (true, false, Some(held)) = (read, trust, partition.record.current()) {
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
        let links = match root {
            Some(_) => CredentialChain::default(),
            None => self.issuer_links(),
        };
        if rocky_credentials::encode(&statement)?.len()
            + rocky_credentials::encode(&links)?.len()
            + rocky_credentials::SIGNATURE_OVERHEAD
            > rocky_credentials::MAX_REVOCATION_BYTES
        {
            return Err("the revocation names too many keys".into());
        }
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
            // An identity's record bounds what it read; a node alone, or an
            // identity without a record, may have read anything.
            let records: Vec<_> = signed
                .statement
                .identities
                .iter()
                .map(|identity| {
                    registry
                        .get(identity)
                        .filter(|record| record.admitted.is_some() || !record.grants.is_empty())
                        .map(Revoking::of)
                })
                .chain(signed.statement.nodes.iter().map(|node| {
                    self.revocations
                        .certificates()
                        .find(|certificate| {
                            rocky_credentials::PeerId::of(certificate.capability.subject) == *node
                        })
                        .and_then(|certificate| registry.get(&certificate.issuer))
                        .filter(|record| record.admitted.is_some() || !record.grants.is_empty())
                        .map(Revoking::of)
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
                let read = records.iter().any(|record| {
                    record.as_ref().is_none_or(|record| {
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
    /// Prepares a begun revocation: a fresh key epoch for every collection
    /// and group partition it names, the signed statement joins the set and
    /// the issuer renews its own chain. Earlier keys stay; history is not sealed
    /// again. Repeating it after an interruption makes the same keys and
    /// statement. The intent stays until `complete_revocation` observes the
    /// committed tombstone. Returns the new keys, or nothing when no revocation was
    /// begun.
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
        // Keep the redo intent until the registry commits its tombstone.
        Ok(Some(keys))
    }
    /// Discards the redo intent only after the completion transaction committed.
    /// The rotation's envelopes are saved with it.
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
        self.issuing("revokes devices")?.rotation = None;
        Ok(())
    }
}

/// What an identity's `capability` lets its node hold, by which grants and
/// partitions: its own, or for issuer authority, which names none, the
/// issuer profile's over `domain` as the node's certificate carries them.
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
    let span = tracing::debug_span!(
        "dial",
        addresses = addresses.len(),
        outcome = tracing::field::Empty
    );
    let result =
        tracing::Instrument::instrument(dial_any(transport, addresses, protocol), span.clone())
            .await;
    span.record(
        "outcome",
        match &result {
            Ok(_) => "ok",
            Err(error) => match error.downcast_ref::<ServiceError>() {
                Some(ServiceError::TimedOut(_)) => "timeout",
                _ => "unreachable",
            },
        },
    );
    result
}
async fn dial_any(
    transport: &dyn Transport,
    addresses: &[String],
    protocol: Protocol,
) -> Result<Stream, Failure> {
    let mut failure = None;
    let mut timed_out = false;
    for address in addresses {
        match rocky_platform::time::timeout(TIMEOUT, transport.connect(address, protocol)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => {
                timed_out = error.is::<rocky_platform::time::error::Elapsed>()
                    || error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::TimedOut);
                failure = Some(error.to_string());
            }
            Err(_) => {
                timed_out = true;
                failure = Some("the dial timed out".into());
            }
        }
    }
    let cause = failure.unwrap_or_else(|| "no route".into());
    Err(if timed_out {
        ServiceError::TimedOut(cause)
    } else {
        ServiceError::Unreachable(cause)
    }
    .into())
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
    rocky_platform::time::timeout(2 * TIMEOUT, async {
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
    let request = rocky_platform::time::timeout(TIMEOUT, read_message(&mut stream)).await??;
    let reply = match answer(request).await {
        Ok(reply) => serde_json::json!({"reply": HEXLOWER.encode(&reply)}),
        Err(error) => {
            serde_json::json!({"error": error.to_string(), "retryable": crate::node::error(&error)["retryable"]})
        }
    };
    let reply = serde_json::to_vec(&reply).unwrap_or_default();
    rocky_platform::time::timeout(TIMEOUT, async {
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
    contact: Contact,
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
    /// The contacts `save` writes: saved ones not yet resolved, and every
    /// known peer.
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
    /// Whether `save` writes the same identity for both, compared without
    /// encoding it. Naming every field keeps a new one from being missed.
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
            issuers: _,
            enrollment_upstream: _,
            peers: _,
            contact_meta: _,
            enrollment_routes: _,
            logged_out,
            minting,
            granted_authors,
            author_high_water,
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
            && *logged_out == other.logged_out
            && *granted_authors == other.granted_authors
            && *minting == other.minting
            && *author_high_water == other.author_high_water
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
        let (contact, protocol) = if entry.contact.routes.is_empty() {
            match &entry.enrollment {
                Some(contact) => (contact, Protocol::Renew),
                None => return Ok(None),
            }
        } else {
            (&entry.contact, Protocol::Link)
        };
        let addresses = contact.addresses(transport.kind());
        if addresses.is_empty() {
            return Ok(None);
        }
        let stream = dial(transport, &addresses, protocol).await?;
        Ok(Some(
            rocky_secure::network::NativeSession::resolve_peer(stream, id).await?,
        ))
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

// Multi-node protocol suites: native transports, threads, paused clocks and
// native temporary directories.
#[cfg(all(test, not(target_family = "wasm")))]
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

    // Needs tokio's paused clock.
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(start_paused = true)]
    async fn exchange_tries_later_routes_after_two_full_dial_deadlines() {
        let (client, server) = tokio::io::duplex(1024);
        let mut server = Stream::new(server);
        let transport = Routes {
            attempts: AtomicUsize::new(0),
            stream: Mutex::new(Some(Stream::new(client))),
        };
        let answer = rocky_platform::task::spawn(async move {
            assert_eq!(read_message(&mut server).await.unwrap(), b"request");
            // Use some of the exchange budget, after the two dial budgets.
            rocky_platform::time::sleep(TIMEOUT).await;
            write_message(&mut server, br#"{"reply":"6f6b"}"#)
                .await
                .unwrap();
        });
        let start = rocky_platform::time::Instant::now();
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
