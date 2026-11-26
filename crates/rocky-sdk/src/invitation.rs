use crate::{
    Access, Failure, Partition, PersonId, Route, Timestamp,
    enrollment::{Contact, EnrollingIssuer, Enrollment, Intent, NodeKeys, Settings, exchange, now},
    partitions::StoredPartition,
    peer::{PeerError, PeerReply, PeerRequest},
    registry::{Changes, Registry, RegistryFull},
    schema::Schema,
    secrets::Vault,
    service::ServiceError,
    transport::Transport,
};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use rocky_core::Commitment;
use rocky_core::{AppId, DomainId, MeshId};
use rocky_credentials::{
    DatabaseContext, IdentityAnswer, IdentityEnrollment, IdentityProvider, IdentityReply,
    IdentityRequest, InvitedEnrollmentRequest, PublicKey,
};
use rocky_secure::{EncryptedReplica, PrivateKey};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::RangeInclusive, path::Path, sync::Arc, time::Duration};

pub(crate) const AUTHORITY_INVITATIONS_KEY: &str = "invitations";
const BOOK_VERSION: u32 = 4;
const INVITE_VERSION: u8 = 7;
const INVITE_PREFIX: &str = "rocky://invite/";
const CHECK_LEN: usize = 4;
const SKEW: u64 = 300;
const VALIDITY_SECONDS: RangeInclusive<u64> = 60..=30 * 24 * 60 * 60;
const MAX_OPEN: usize = 64;
const MAX_LABEL: usize = crate::registry::MAX_LABEL;

/// Identifies one invitation, for `cancel_invitation` and `Person::invitation`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InvitationId(pub(crate) Commitment);
impl InvitationId {
    fn of(secret: &[u8; 32]) -> Self {
        Self(rocky_core::commitment("rocky.sdk.invitation-id.v1", secret))
    }
}
impl std::fmt::Display for InvitationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&HEXLOWER.encode(&self.0))
    }
}
impl std::fmt::Debug for InvitationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "InvitationId({self})")
    }
}
impl std::str::FromStr for InvitationId {
    type Err = JoinError;
    fn from_str(hex: &str) -> Result<Self, JoinError> {
        crate::enrollment::hex32(hex)
            .map(Self)
            .map_err(|_| JoinError::Invalid)
    }
}
impl Serialize for InvitationId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for InvitationId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// What an invitation grants; pass it to `Database::invite`.
#[derive(Clone, Debug, PartialEq)]
pub struct Invite {
    access: Access,
    partitions: Vec<Partition>,
    label: Option<String>,
    valid_for: Duration,
    person: Option<PersonId>,
}
impl Invite {
    /// How long an invitation stays redeemable unless `valid_for` says otherwise.
    pub const DEFAULT_VALIDITY: Duration = Duration::from_secs(10 * 60);
    /// An invitation that grants `access`, valid for ten minutes.
    pub fn new(access: Access) -> Self {
        Self {
            access,
            partitions: Vec::new(),
            label: None,
            valid_for: Self::DEFAULT_VALIDITY,
            person: None,
        }
    }
    /// Also grants this partition; repeat for more.
    #[must_use]
    pub fn partition(mut self, partition: Partition) -> Self {
        self.partitions.push(partition);
        self
    }
    /// Names the device it admits and, on the person's first invitation, the person; at most 256
    /// bytes.
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
    /// How long it stays redeemable, from one minute to 30 days.
    #[must_use]
    pub fn valid_for(mut self, validity: Duration) -> Self {
        self.valid_for = validity;
        self
    }
    /// Admits only a joiner configured with this person's key; use it to re-invite a person who
    /// lost their only device.
    #[must_use]
    pub fn for_identity(mut self, person: PersonId) -> Self {
        self.person = Some(person);
        self
    }
    pub(crate) fn access(&self) -> &Access {
        &self.access
    }
    fn validity(&self) -> Result<u64, InviteError> {
        let seconds = self.valid_for.as_secs();
        if !VALIDITY_SECONDS.contains(&seconds) {
            return Err(InviteError::Validity {
                min: Duration::from_secs(*VALIDITY_SECONDS.start()),
                max: Duration::from_secs(*VALIDITY_SECONDS.end()),
            });
        }
        Ok(seconds)
    }
}

/// An issued invitation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invitation {
    /// Its id.
    pub id: InvitationId,
    /// The code to hand to the joiner; it admits one device of whoever holds it, once.
    pub code: InvitationCode,
    /// When it stops being redeemable.
    pub expires: Timestamp,
}

/// An invitation as `Database::invitations` lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingInvitation {
    /// Its id.
    pub id: InvitationId,
    /// The access it grants.
    pub access: Access,
    /// The partitions it grants.
    pub partitions: Vec<Partition>,
    /// Its label.
    pub label: Option<String>,
    /// When it stops being redeemable.
    pub expires: Timestamp,
    /// Who redeemed it, or `None` while open.
    pub redeemed_by: Option<PersonId>,
}

fn timestamp(seconds: u64) -> Timestamp {
    Timestamp::from_unix_micros(
        i64::try_from(seconds)
            .unwrap_or(i64::MAX)
            .saturating_mul(1_000_000),
    )
}

/// Why an invitation could not be issued or cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InviteError {
    /// Inviting needs an attached network the joiner can reach.
    NetworkRequired,
    /// Too many invitations are open; cancel one or let it expire.
    TooManyOpen {
        /// The most open at once.
        limit: usize,
    },
    /// The validity is outside the allowed range.
    Validity {
        /// The shortest allowed.
        min: Duration,
        /// The longest allowed.
        max: Duration,
    },
    /// The label is too long.
    LabelTooLong {
        /// The longest label in bytes.
        limit: usize,
    },
    /// The person is already a member with other access or an admin; link a device instead.
    AlreadyMember,
    /// No such open invitation.
    Unknown,
    /// The invitation was already redeemed.
    Redeemed,
}
impl InviteError {
    /// A stable snake-case code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NetworkRequired => "network_required",
            Self::TooManyOpen { .. } => "too_many_invitations",
            Self::Validity { .. } => "invitation_validity",
            Self::LabelTooLong { .. } => "label_too_long",
            Self::AlreadyMember => "already_member",
            Self::Unknown => "invitation_unknown",
            Self::Redeemed => "invitation_redeemed",
        }
    }
}
impl std::fmt::Display for InviteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NetworkRequired => {
                f.write_str("inviting needs an attached network the joiner can reach")
            }
            Self::TooManyOpen { limit } => write!(
                f,
                "at most {limit} invitations are open at once; cancel one or let it expire"
            ),
            Self::Validity { min, max } => write!(
                f,
                "an invitation is valid for between {}s and {}s",
                min.as_secs(),
                max.as_secs()
            ),
            Self::LabelTooLong { limit } => write!(f, "a label holds at most {limit} bytes"),
            Self::AlreadyMember => f.write_str(
                "the person is already a member with other access or an admin; link a device instead",
            ),
            Self::Unknown => f.write_str("no such open invitation"),
            Self::Redeemed => f.write_str("the invitation was already redeemed"),
        }
    }
}
impl std::error::Error for InviteError {}

/// Why an invitation could not be redeemed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinError {
    /// The code is malformed.
    Invalid,
    /// The invitation expired.
    Expired,
    /// The invitation was already redeemed.
    Used,
    /// The invitation was cancelled.
    Cancelled,
    /// A `for_identity` invitation was redeemed with another person key.
    OtherIdentity,
    /// The invitation is for another application.
    OtherApplication,
    /// The person is already a member; link a device instead.
    AlreadyMember,
}
impl JoinError {
    const CARRIED: [Self; 6] = [
        Self::Invalid,
        Self::Expired,
        Self::Used,
        Self::Cancelled,
        Self::OtherIdentity,
        Self::AlreadyMember,
    ];
    /// A stable snake-case code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "invitation_invalid",
            Self::Expired => "invitation_expired",
            Self::Used => "invitation_used",
            Self::Cancelled => "invitation_cancelled",
            Self::OtherIdentity => "invitation_other_identity",
            Self::OtherApplication => "invitation_other_application",
            Self::AlreadyMember => "already_member",
        }
    }
    fn carried(error: Failure) -> Failure {
        let refusal = error.downcast_ref::<PeerError>().and_then(|peer| {
            Self::CARRIED
                .into_iter()
                .find(|refusal| refusal.code() == peer.code)
        });
        match refusal {
            Some(refusal) => Box::new(refusal),
            None => error,
        }
    }
}
impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "the invitation code is not valid",
            Self::Expired => "the invitation expired",
            Self::Used => "another device already used the invitation",
            Self::Cancelled => "the issuer holds no such invitation; it was cancelled",
            Self::OtherIdentity => "the invitation is for another identity",
            Self::OtherApplication => "the invitation is for another application's database",
            Self::AlreadyMember => {
                "the identity is already a member; link this device from another of its devices"
            }
        })
    }
}
impl std::error::Error for JoinError {}

#[derive(Clone, PartialEq, Eq)]
struct Code {
    secret: [u8; 32],
    database: DatabaseContext,
    issuer: PublicKey,
    person: Option<PublicKey>,
    expires: u64,
    routes: Vec<Route>,
}
impl Code {
    fn fits(routes: &[Route]) -> bool {
        !routes.is_empty() && Contact::checked(routes.to_vec()).is_ok()
    }
    fn body(&self) -> Vec<u8> {
        let scope = &self.database.scope;
        let mut bytes = vec![INVITE_VERSION];
        bytes.extend_from_slice(&self.secret);
        bytes.extend_from_slice(self.database.publisher.as_bytes());
        bytes.extend_from_slice(&scope.mesh.0);
        bytes.extend_from_slice(&scope.app.0);
        bytes.extend_from_slice(&scope.domain.0);
        bytes.extend_from_slice(self.issuer.as_bytes());
        bytes.extend_from_slice(&self.expires.to_be_bytes());
        match &self.person {
            Some(person) => {
                bytes.push(1);
                bytes.extend_from_slice(person.as_bytes());
            }
            None => bytes.push(0),
        }
        for route in &self.routes {
            let route = route.as_str().as_bytes();
            put_length(&mut bytes, route.len());
            bytes.extend_from_slice(route);
        }
        bytes
    }
    fn check(body: &[u8]) -> [u8; CHECK_LEN] {
        let digest = rocky_core::commitment("rocky.sdk.invitation-code.v7", body);
        let mut check = [0; CHECK_LEN];
        check.copy_from_slice(&digest[..CHECK_LEN]);
        check
    }
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = self.body();
        bytes.extend_from_slice(&Self::check(&bytes));
        bytes
    }
    fn from_bytes(bytes: &[u8]) -> Result<Self, Failure> {
        let (body, check) = bytes
            .split_last_chunk::<CHECK_LEN>()
            .ok_or("invitation code too short")?;
        if *check != Self::check(body) {
            return Err("invitation code checksum".into());
        }
        let mut reader = Reader(body);
        if reader.array::<1>()? != [INVITE_VERSION] {
            return Err("unsupported invitation version".into());
        }
        let secret = reader.array()?;
        let publisher = PublicKey::from_bytes(&reader.array()?)?;
        let scope = rocky_core::Scope {
            mesh: MeshId(reader.array()?),
            app: AppId(reader.array()?),
            domain: DomainId(reader.array()?),
        };
        let issuer = PublicKey::from_bytes(&reader.array()?)?;
        let expires = u64::from_be_bytes(reader.array()?);
        let person = match reader.array::<1>()? {
            [0] => None,
            [1] => Some(PublicKey::from_bytes(&reader.array()?)?),
            _ => return Err("invitation holder".into()),
        };
        let mut routes = Vec::new();
        while !reader.0.is_empty() {
            let len = reader.length()?;
            let route = reader.take(len)?;
            routes.push(Route::try_from(String::from_utf8(route.to_vec())?)?);
        }
        if !Self::fits(&routes) {
            return Err("invalid invitation routes".into());
        }
        Ok(Self {
            secret,
            database: DatabaseContext { publisher, scope },
            issuer,
            person,
            expires,
            routes,
        })
    }
}
fn put_length(bytes: &mut Vec<u8>, len: usize) {
    let mut rest = len;
    while rest >= 0x80 {
        bytes.push(rest.to_le_bytes()[0] | 0x80);
        rest >>= 7;
    }
    bytes.push(rest.to_le_bytes()[0]);
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn length(&mut self) -> Result<usize, Failure> {
        let [low] = self.array()?;
        if low < 0x80 {
            return Ok(usize::from(low));
        }
        match self.array()? {
            [high @ 1..0x80] => Ok(usize::from(low & 0x7f) | usize::from(high) << 7),
            _ => Err("invitation route length".into()),
        }
    }
    fn take(&mut self, len: usize) -> Result<&'a [u8], Failure> {
        let (taken, rest) = self
            .0
            .split_at_checked(len)
            .ok_or("invitation code truncated")?;
        self.0 = rest;
        Ok(taken)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], Failure> {
        Ok(self.take(N)?.try_into()?)
    }
}

/// The text a joiner redeems with `Configuration::join`: one URL-safe string, about 260
/// characters with a TCP route and 400 with an iroh one, naming the issuer's routes at issue.
#[derive(Clone, PartialEq, Eq)]
pub struct InvitationCode(Box<Code>);
impl InvitationCode {
    pub(crate) fn person(&self) -> Option<PublicKey> {
        self.0.person
    }
    pub(crate) fn intent(&self) -> Commitment {
        rocky_core::commitment("rocky.sdk.invitation-join.v2", &self.id().0)
    }
    fn id(&self) -> InvitationId {
        InvitationId::of(&self.0.secret)
    }
    #[cfg(test)]
    pub(crate) fn secret(&self) -> &[u8; 32] {
        &self.0.secret
    }
}
impl std::fmt::Display for InvitationCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{INVITE_PREFIX}{}",
            BASE64URL_NOPAD.encode(&self.0.to_bytes())
        )
    }
}
impl std::fmt::Debug for InvitationCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InvitationCode")
            .field("id", &self.id())
            .field("expires", &self.0.expires)
            .field("person", &self.0.person)
            .finish_non_exhaustive()
    }
}
impl std::str::FromStr for InvitationCode {
    type Err = JoinError;
    fn from_str(text: &str) -> Result<Self, JoinError> {
        let parsed = || -> Result<Code, Failure> {
            let encoded = text
                .trim()
                .strip_prefix(INVITE_PREFIX)
                .ok_or("invalid invitation code")?;
            Code::from_bytes(&BASE64URL_NOPAD.decode(encoded.as_bytes())?)
        };
        parsed()
            .map(|code| Self(Box::new(code)))
            .map_err(|_| JoinError::Invalid)
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    #[serde(with = "crate::registry::hex32")]
    secret: [u8; 32],
    person: Option<PublicKey>,
    access: Access,
    partitions: Vec<StoredPartition>,
    label: Option<String>,
    expires: u64,
    redeemed: Option<(PublicKey, PublicKey)>,
}

#[derive(PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Book {
    version: u32,
    open: BTreeMap<InvitationId, Pending>,
}
struct Opening {
    secret: [u8; 32],
    person: Option<PublicKey>,
    access: Access,
    partitions: Vec<StoredPartition>,
    label: Option<String>,
}
impl Book {
    fn empty() -> Self {
        Self {
            version: BOOK_VERSION,
            open: BTreeMap::new(),
        }
    }
    fn parse(bytes: &[u8]) -> Result<Self, Failure> {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        if value["version"].as_u64() != Some(u64::from(BOOK_VERSION)) {
            return Err("unsupported invitation book version".into());
        }
        Ok(serde_json::from_value(value)?)
    }
    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>, Failure> {
        Ok(serde_json::to_vec(self)?)
    }
    pub(crate) async fn read(
        replica: &EncryptedReplica<rocky_core::TypedRows>,
    ) -> Result<Self, Failure> {
        match replica
            .get_authority_state(AUTHORITY_INVITATIONS_KEY)
            .await?
        {
            Some((bytes, _)) => Self::parse(&bytes),
            None => Ok(Self::empty()),
        }
    }
    fn forget_expired(&mut self, time: u64) {
        self.open.retain(|_, pending| time <= pending.expires);
    }
    fn open(
        &mut self,
        opening: Opening,
        validity: u64,
        time: u64,
    ) -> Result<(InvitationId, u64), InviteError> {
        self.forget_expired(time);
        let unredeemed = self
            .open
            .values()
            .filter(|pending| pending.redeemed.is_none())
            .count();
        if unredeemed >= MAX_OPEN {
            return Err(InviteError::TooManyOpen { limit: MAX_OPEN });
        }
        let id = InvitationId::of(&opening.secret);
        let expires = time.saturating_add(validity);
        self.open.insert(
            id,
            Pending {
                secret: opening.secret,
                person: opening.person,
                access: opening.access,
                partitions: opening.partitions,
                label: opening.label,
                expires,
                redeemed: None,
            },
        );
        Ok((id, expires))
    }
    fn redeem(
        &mut self,
        wire: &InvitedEnrollmentRequest,
        device: NodeKeys,
        time: u64,
    ) -> Result<Redemption, JoinError> {
        let id = InvitationId(wire.invitation);
        let pending = self.open.get_mut(&id).ok_or(JoinError::Cancelled)?;
        if pending.expires < time {
            return Err(JoinError::Expired);
        }
        if !wire
            .proves_token(&pending.secret)
            .map_err(|_| JoinError::Invalid)?
        {
            return Err(JoinError::Invalid);
        }
        if pending
            .person
            .is_some_and(|person| person != device.identity)
        {
            return Err(JoinError::OtherIdentity);
        }
        let held = (device.identity, device.node);
        if pending.redeemed.is_some_and(|redeemed| redeemed != held) {
            return Err(JoinError::Used);
        }
        let first = pending.redeemed.replace(held).is_none();
        Ok(Redemption {
            access: pending.access.clone(),
            partitions: pending
                .partitions
                .iter()
                .map(StoredPartition::partition)
                .collect(),
            label: pending.label.clone().filter(|_| first),
            first,
        })
    }
    pub(crate) fn release(&mut self, id: InvitationId, node: PublicKey) -> bool {
        match self.open.get_mut(&id) {
            Some(pending) if pending.redeemed.is_some_and(|(_, held)| held == node) => {
                pending.redeemed = None;
                true
            }
            _ => false,
        }
    }
    pub(crate) fn cancel(&mut self, id: InvitationId, time: u64) -> Result<(), InviteError> {
        self.forget_expired(time);
        let pending = self.open.get(&id).ok_or(InviteError::Unknown)?;
        if pending.redeemed.is_some() {
            return Err(InviteError::Redeemed);
        }
        self.open.remove(&id);
        Ok(())
    }
    pub(crate) fn listed(&self, time: u64) -> Vec<PendingInvitation> {
        self.open
            .iter()
            .filter(|(_, pending)| time <= pending.expires)
            .map(|(id, pending)| PendingInvitation {
                id: *id,
                access: pending.access.clone(),
                partitions: pending
                    .partitions
                    .iter()
                    .map(StoredPartition::partition)
                    .collect(),
                label: pending.label.clone(),
                expires: timestamp(pending.expires),
                redeemed_by: pending
                    .redeemed
                    .map(|(identity, _)| PersonId::new(identity)),
            })
            .collect()
    }
}

fn admissible(
    settings: &Settings,
    registry: &Registry,
    identity: &PublicKey,
    access: &Access,
) -> Result<(), Failure> {
    if settings.revoked_identity(identity, registry) {
        return Err(Box::new(crate::Error::removed()));
    }
    let other_access = registry
        .get(identity)
        .and_then(crate::registry::IdentityRecord::access)
        .is_some_and(|held| held != access);
    if *identity == settings.identity
        || *identity == settings.publisher
        || registry.admins.contains_key(identity)
        || other_access
    {
        return Err(Box::new(JoinError::AlreadyMember));
    }
    Ok(())
}

pub(crate) struct Issued {
    pub(crate) code: InvitationCode,
    pub(crate) id: InvitationId,
    pub(crate) expires: u64,
}
impl Issued {
    pub(crate) fn invitation(self) -> Invitation {
        Invitation {
            id: self.id,
            code: self.code,
            expires: timestamp(self.expires),
        }
    }
}
#[derive(Debug, PartialEq)]
struct Redemption {
    access: Access,
    partitions: Vec<Partition>,
    label: Option<String>,
    first: bool,
}

pub(crate) struct Redeemed {
    pub(crate) answer: Vec<u8>,
    pub(crate) changes: Changes,
    pub(crate) id: InvitationId,
    pub(crate) device: NodeKeys,
    pub(crate) first: bool,
}

impl Settings {
    pub(crate) fn invite(
        &self,
        (vault, routes): (&Vault, Vec<Route>),
        book: &mut Book,
        invite: &Invite,
        registry: &Registry,
        time: u64,
    ) -> Result<Issued, Failure> {
        if !self.issuer() {
            return Err(Box::new(crate::Error::refused(
                crate::Right::Administer,
                crate::DenyReason::AdminRequired,
            )));
        }
        let validity = invite.validity()?;
        if invite
            .label
            .as_ref()
            .is_some_and(|label| label.len() > MAX_LABEL)
        {
            return Err(Box::new(InviteError::LabelTooLong { limit: MAX_LABEL }));
        }
        let domain = self.domain()?;
        let targets: Vec<_> = invite.partitions.iter().map(Partition::target).collect();
        domain.compile(&invite.access, &domain.partition_targets(&targets)?)?;
        let person = match invite.person {
            Some(person) => {
                let identity = person.key();
                admissible(self, registry, &identity, &invite.access).map_err(|error| {
                    if error.is::<JoinError>() {
                        Box::new(InviteError::AlreadyMember)
                    } else {
                        error
                    }
                })?;
                if registry.full_for(&identity) {
                    return Err(Box::new(RegistryFull));
                }
                Some(identity)
            }
            None => {
                if registry.identities() >= crate::registry::MAX_IDENTITIES {
                    return Err(Box::new(RegistryFull));
                }
                None
            }
        };
        if !Code::fits(&routes) {
            return Err(Box::new(InviteError::NetworkRequired));
        }
        let secret = rocky_platform::entropy::random();
        let (id, expires) = book.open(
            Opening {
                secret,
                person,
                access: invite.access.clone(),
                partitions: invite.partitions.iter().map(StoredPartition::of).collect(),
                label: invite.label.clone(),
            },
            validity,
            time,
        )?;
        let code = InvitationCode(Box::new(Code {
            secret,
            database: self.database_context()?,
            issuer: Self::node_key(vault)?.public_key(),
            person,
            expires,
            routes,
        }));
        Ok(Issued { code, id, expires })
    }

    pub(crate) fn answer_invitation(
        &mut self,
        vault: &Vault,
        book: &mut Book,
        wire: InvitedEnrollmentRequest,
        registry: &Registry,
        time: u64,
    ) -> Result<Redeemed, Failure> {
        if !self.issuer() {
            return Err("only an admin answers invitations".into());
        }
        wire.verify_binding()?;
        let answering = Self::node_key(vault)?;
        wire.request.verify(
            answering.public_key(),
            wire.request.identity,
            (time, SKEW),
            self.database_context()?,
        )?;
        let id = InvitationId(wire.invitation);
        let device = NodeKeys {
            identity: wire.request.identity,
            node: wire.request.node,
        };
        let Redemption {
            access,
            partitions,
            label,
            first,
        } = book.redeem(&wire, device, time)?;
        admissible(self, registry, &device.identity, &access)?;
        if self.tombstoned(&device.node) {
            return Err(Box::new(crate::Error::removed()));
        }
        let (enrollment, changes) = self.enrollment_for(
            vault,
            device,
            (access, &partitions, label.map(String::into_bytes)),
            Some(id),
            registry,
        )?;
        Ok(Redeemed {
            answer: wire
                .request
                .answer(&answering, &serde_json::to_vec(&enrollment)?)?
                .to_bytes()?,
            changes,
            id,
            device,
            first,
        })
    }

    pub(crate) fn prepare_invited(
        path: &Path,
        store: &Arc<dyn crate::SecretStore>,
        provider: Option<&Arc<dyn IdentityProvider>>,
        code: &InvitationCode,
    ) -> Result<crate::enrollment::Prepared, Failure> {
        match (code.person(), provider) {
            (Some(_), None) => {
                return Err(Box::new(crate::Error::refused(
                    crate::Right::Sign,
                    crate::DenyReason::IdentityRequired,
                )));
            }
            (Some(person), Some(provider)) if provider.public_key()? != person => {
                return Err(Box::new(JoinError::OtherIdentity));
            }
            _ => {}
        }
        Intent::resume(&Intent::path(path), code.intent(), store, provider)
    }

    pub(crate) async fn enroll_invited(
        (path, schema): (&Path, Option<&Schema>),
        (intent, vault): (Intent, Vault),
        provider: Arc<dyn IdentityProvider>,
        code: &InvitationCode,
        transport: &dyn Transport,
    ) -> Result<Self, Failure> {
        let invite = &code.0;
        let addresses = invite
            .routes
            .iter()
            .filter(|route| transport.claims(route.as_str()))
            .map(|route| route.as_str().to_owned())
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return Err(ServiceError::no_route("the issuer", transport).into());
        }
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let (request, secret) =
            IdentityRequest::sign(&*provider, &node, invite.issuer, now()?, invite.database)?;
        let reply = exchange(
            transport,
            &addresses,
            invite.database.scope,
            &PeerRequest::Identity(Box::new(IdentityEnrollment::Invited(
                InvitedEnrollmentRequest::sign(
                    request.clone(),
                    code.id().0,
                    Some(&invite.secret),
                    &node,
                )?,
            ))),
        )
        .await
        .map_err(JoinError::carried)?;
        let PeerReply::Identity(IdentityReply::Enrolled(sealed)) = reply else {
            return Err("invitation reply kind".into());
        };
        let enrollment: Enrollment =
            serde_json::from_slice(&IdentityAnswer::from_bytes(&sealed)?.open(&request, &secret)?)?;
        if enrollment.issuer() != invite.issuer {
            return Err("the answer comes from another admin than the invitation names".into());
        }
        if let Some(refusal) = enrollment.refused_by(schema) {
            return Err(refusal);
        }
        let settings = Self::install_enrollment(
            schema,
            (intent.installation.clone(), &vault),
            EnrollingIssuer {
                publisher: invite.database.publisher,
                database: invite.database,
                routes: invite.routes.clone(),
            },
            enrollment,
            provider,
        )?;
        settings.save(path).await?;
        intent.finish(path, &vault)?;
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPEN_FOR: u64 = 600;
    const NOW: u64 = 1_000_000;
    const SECRET: [u8; 32] = [6; 32];

    fn key(byte: u8) -> PublicKey {
        PrivateKey::from_bytes(&[byte; 32]).public_key()
    }
    fn device(identity: u8, node: u8) -> NodeKeys {
        NodeKeys {
            identity: key(identity),
            node: key(node),
        }
    }
    fn database() -> DatabaseContext {
        DatabaseContext {
            publisher: key(3),
            scope: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
        }
    }
    fn opening(secret: [u8; 32], person: Option<PublicKey>) -> Opening {
        Opening {
            secret,
            person,
            access: Access::Full,
            partitions: Vec::new(),
            label: None,
        }
    }
    fn opened(book: &mut Book, secret: [u8; 32], person: Option<PublicKey>) -> InvitationId {
        book.open(opening(secret, person), OPEN_FOR, NOW)
            .expect("open invitation")
            .0
    }
    fn wire(
        id: InvitationId,
        identity: u8,
        node: u8,
        secret: Option<&[u8; 32]>,
    ) -> InvitedEnrollmentRequest {
        let person = PrivateKey::from_bytes(&[identity; 32]);
        let node = PrivateKey::from_bytes(&[node; 32]);
        let (request, _) =
            IdentityRequest::sign(&person, &node, key(4), NOW, database()).expect("request");
        InvitedEnrollmentRequest::sign(request, id.0, secret, &node).expect("bound request")
    }
    fn redeemed(
        book: &mut Book,
        wire: &InvitedEnrollmentRequest,
        time: u64,
    ) -> Result<bool, JoinError> {
        let device = NodeKeys {
            identity: wire.request.identity,
            node: wire.request.node,
        };
        book.redeem(wire, device, time)
            .map(|redemption| redemption.first)
    }

    #[test]
    fn an_invitation_admits_the_first_device_that_proves_its_secret_once() {
        let mut book = Book::empty();
        let id = opened(&mut book, SECRET, None);
        assert_eq!(
            id,
            InvitationId::of(&SECRET),
            "the id commits to the secret"
        );
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 10, Some(&[7; 32])), NOW),
            Err(JoinError::Invalid),
            "a wrong secret proves nothing"
        );
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 10, None), NOW),
            Err(JoinError::Invalid),
            "the id alone redeems nothing"
        );
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 10, Some(&SECRET)), NOW),
            Ok(true)
        );
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 10, Some(&SECRET)), NOW + 1),
            Ok(false),
            "the redeeming device retries an answer it lost"
        );
        assert_eq!(
            redeemed(&mut book, &wire(id, 2, 20, Some(&SECRET)), NOW + 1),
            Err(JoinError::Used),
            "a code is single use"
        );
        assert_eq!(book.listed(NOW)[0].redeemed_by, Some(PersonId::new(key(1))));
    }

    #[test]
    fn an_identity_invitation_admits_only_that_identity() {
        let mut book = Book::empty();
        let id = opened(&mut book, SECRET, Some(key(1)));
        assert_eq!(
            redeemed(&mut book, &wire(id, 2, 10, Some(&SECRET)), NOW),
            Err(JoinError::OtherIdentity)
        );
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 10, Some(&SECRET)), NOW),
            Ok(true)
        );
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 11, Some(&SECRET)), NOW),
            Err(JoinError::Used),
            "a second device of the same identity links instead"
        );
    }

    #[test]
    fn a_retried_redemption_leaves_the_label_alone_and_a_released_one_starts_over() {
        let mut book = Book::empty();
        let id = book
            .open(
                Opening {
                    label: Some("Alice".into()),
                    ..opening(SECRET, Some(key(1)))
                },
                OPEN_FOR,
                NOW,
            )
            .expect("open invitation")
            .0;
        let first = book
            .redeem(&wire(id, 1, 10, Some(&SECRET)), device(1, 10), NOW)
            .expect("redeemed");
        assert_eq!(first.label.as_deref(), Some("Alice"));
        let retried = book
            .redeem(&wire(id, 1, 10, Some(&SECRET)), device(1, 10), NOW)
            .expect("redeemed");
        assert!(!retried.first);
        assert_eq!(
            retried.label, None,
            "a retry does not overwrite a label edited since the first redemption"
        );
        assert!(
            !book.release(id, key(11)),
            "only the redeeming node is released"
        );
        assert!(book.release(id, key(10)));
        assert!(book.listed(NOW)[0].redeemed_by.is_none());
        let again = book
            .redeem(&wire(id, 1, 11, Some(&SECRET)), device(1, 11), NOW)
            .expect("redeemed");
        assert!(again.first);
    }

    #[test]
    fn an_invitation_ends_at_its_expiry_even_for_the_device_that_redeemed_it() {
        let mut book = Book::empty();
        let id = opened(&mut book, SECRET, Some(key(1)));
        let request = wire(id, 1, 10, Some(&SECRET));
        assert!(redeemed(&mut book, &request, NOW + OPEN_FOR).is_ok());
        assert_eq!(
            redeemed(&mut book, &request, NOW + OPEN_FOR + 1),
            Err(JoinError::Expired)
        );
        assert!(book.listed(NOW + OPEN_FOR + 1).is_empty());
        book.forget_expired(NOW + OPEN_FOR + 1);
        assert_eq!(
            redeemed(&mut book, &request, NOW + OPEN_FOR + 1),
            Err(JoinError::Cancelled),
            "an expired invitation is forgotten once the book is next written"
        );
    }

    #[test]
    fn only_an_unused_invitation_is_cancelled() {
        let mut book = Book::empty();
        let unused = opened(&mut book, [1; 32], Some(key(1)));
        let used = opened(&mut book, [2; 32], Some(key(2)));
        assert!(redeemed(&mut book, &wire(used, 2, 20, Some(&[2; 32])), NOW).is_ok());
        assert_eq!(book.cancel(unused, NOW), Ok(()));
        assert_eq!(
            redeemed(&mut book, &wire(unused, 1, 10, Some(&[1; 32])), NOW),
            Err(JoinError::Cancelled)
        );
        assert_eq!(book.cancel(used, NOW), Err(InviteError::Redeemed));
        assert_eq!(book.cancel(unused, NOW), Err(InviteError::Unknown));
    }

    #[test]
    fn the_book_holds_a_bounded_number_of_open_invitations() {
        let mut book = Book::empty();
        for secret in 0..MAX_OPEN {
            opened(&mut book, [secret as u8; 32], Some(key(1)));
        }
        assert_eq!(
            book.open(opening([255; 32], Some(key(1))), OPEN_FOR, NOW)
                .err(),
            Some(InviteError::TooManyOpen { limit: MAX_OPEN })
        );
        assert!(
            book.open(
                opening([255; 32], Some(key(1))),
                OPEN_FOR,
                NOW + OPEN_FOR + 1
            )
            .is_ok(),
            "expired invitations free their slots"
        );
    }

    #[test]
    fn the_book_reads_back_only_its_own_version() -> Result<(), Failure> {
        let mut book = Book::empty();
        opened(&mut book, SECRET, None);
        assert!(Book::parse(&book.to_bytes()?)? == book);
        let mut other: serde_json::Value = serde_json::from_slice(&book.to_bytes()?)?;
        other["version"] = serde_json::json!(BOOK_VERSION - 1);
        assert!(Book::parse(&serde_json::to_vec(&other)?).is_err());
        Ok(())
    }

    fn code(person: Option<PublicKey>) -> InvitationCode {
        InvitationCode(Box::new(Code {
            secret: SECRET,
            database: database(),
            issuer: key(4),
            person,
            expires: NOW,
            routes: vec![
                Route::try_from(String::from("tcp://192.168.178.20:4433")).expect("a route"),
            ],
        }))
    }

    #[test]
    fn a_code_round_trips_in_under_300_characters_and_redacts_its_secret() -> Result<(), Failure> {
        let token = code(None);
        let text = token.to_string();
        assert!(text.len() <= 300, "{} characters: {text}", text.len());
        assert!(text.starts_with(INVITE_PREFIX));
        assert!(
            text.trim_start_matches(INVITE_PREFIX)
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' })
        );
        assert!(text.parse::<InvitationCode>()? == token);
        assert!(!format!("{token:?}").contains(&HEXLOWER.encode(&SECRET)));
        let person = code(Some(key(1)));
        assert_eq!(
            person.to_string().parse::<InvitationCode>()?.person(),
            Some(key(1))
        );
        Ok(())
    }

    #[test]
    fn a_code_carries_a_route_of_the_longest_length_next_to_a_short_one() -> Result<(), Failure> {
        let longest = Route::try_from(format!("iroh:{}", "A".repeat(Route::MAX_LEN - 5)))?;
        let token = InvitationCode(Box::new(Code {
            routes: vec![longest, code(None).0.routes[0].clone()],
            ..*code(None).0
        }));
        assert!(token.to_string().parse::<InvitationCode>()? == token);
        Ok(())
    }

    #[test]
    fn an_iroh_code_from_a_host_with_many_interfaces_stays_short() -> Result<(), Failure> {
        let address = rocky_iroh::iroh::EndpointAddr::new(
            rocky_iroh::iroh::SecretKey::from_bytes(&[9; 32]).public(),
        )
        .with_relay_url("https://euc1-1.relay.n0.iroh-canary.iroh.link./".parse()?)
        .with_addrs(
            [
                "100.101.102.103:50123",
                "[fd7a:115c:a1e0::1234:5678]:50123",
                "192.168.178.20:50123",
                "10.0.0.5:50123",
                "172.17.0.1:50123",
                "[fe80::1c2b:3aff:fe4d:5e6f]:50123",
                "127.0.0.1:50123",
            ]
            .map(|text| rocky_iroh::iroh::TransportAddr::Ip(text.parse().expect("a socket"))),
        );
        let token = InvitationCode(Box::new(Code {
            routes: vec![Route::try_from(rocky_iroh::route(&address)?)?],
            ..*code(None).0
        }));
        let text = token.to_string();
        assert!(text.len() <= 460, "{} characters: {text}", text.len());
        assert!(text.parse::<InvitationCode>()? == token);
        Ok(())
    }

    #[test]
    fn a_tampered_truncated_or_other_version_code_is_invalid() {
        let text = code(None).to_string();
        let mut tampered = text.clone().into_bytes();
        let last = tampered.len() - 10;
        tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).expect("ascii");
        let truncated = &text[..text.len() - 4];
        let mut older = code(None).0.body();
        older[0] = INVITE_VERSION - 1;
        older.extend_from_slice(&Code::check(&older));
        let older = format!("{INVITE_PREFIX}{}", BASE64URL_NOPAD.encode(&older));
        let mut padded = code(None).0.body();
        let length = padded.len() - code(None).0.routes[0].as_str().len() - 1;
        padded[length] |= 0x80;
        padded.insert(length + 1, 0);
        padded.extend_from_slice(&Code::check(&padded));
        let padded = format!("{INVITE_PREFIX}{}", BASE64URL_NOPAD.encode(&padded));
        let routeless = InvitationCode(Box::new(Code {
            routes: Vec::new(),
            ..*code(None).0
        }))
        .to_string();
        for invalid in [
            tampered.as_str(),
            truncated,
            &older,
            &padded,
            &routeless,
            "rocky://invite/",
        ] {
            assert_eq!(
                invalid.parse::<InvitationCode>().err(),
                Some(JoinError::Invalid),
                "{invalid}"
            );
        }
    }

    #[test]
    fn a_refusal_crosses_the_wire_as_its_code() {
        for refusal in JoinError::CARRIED {
            let failure: Failure = Box::new(refusal);
            let carried = crate::peer::failure(&failure);
            assert_eq!(carried.code, refusal.code());
            let received = JoinError::carried(Box::new(carried));
            assert_eq!(received.downcast_ref::<JoinError>(), Some(&refusal));
        }
    }
}
