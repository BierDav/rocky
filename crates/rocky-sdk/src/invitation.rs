use crate::{
    Access, Failure, Partition, PersonId, Timestamp,
    enrollment::{
        Contact, EnrollingIssuer, Enrollment, Intent, NodeKeys, Route, Settings, exchange, now,
    },
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
use rocky_credentials::{
    DatabaseContext, IdentityAnswer, IdentityEnrollment, IdentityProvider, IdentityReply,
    IdentityRequest, InvitedEnrollmentRequest, PublicKey,
};
use rocky_secure::{EncryptedReplica, PrivateKey};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::RangeInclusive, path::Path, sync::Arc, time::Duration};

pub(crate) const AUTHORITY_INVITATIONS_KEY: &str = "invitations";
const BOOK_VERSION: u32 = 3;
const INVITE_VERSION: u32 = 4;
const INVITE_PREFIX: &str = "rocky://invite/";
const SKEW: u64 = 300;
const VALIDITY_SECONDS: RangeInclusive<u64> = 60..=30 * 24 * 60 * 60;
const MAX_OPEN: usize = 64;
const MAX_LABEL: usize = crate::registry::MAX_LABEL;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InvitationId(pub(crate) Commitment);
impl InvitationId {
    fn fresh() -> Self {
        Self(rocky_platform::entropy::random())
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

#[derive(Clone, Debug, PartialEq)]
pub struct Invite {
    access: Access,
    partitions: Vec<Partition>,
    label: Option<String>,
    valid_for: Duration,
    person: Option<PersonId>,
}
impl Invite {
    pub const DEFAULT_VALIDITY: Duration = Duration::from_secs(10 * 60);
    pub fn new(access: Access) -> Self {
        Self {
            access,
            partitions: Vec::new(),
            label: None,
            valid_for: Self::DEFAULT_VALIDITY,
            person: None,
        }
    }
    #[must_use]
    pub fn partition(mut self, partition: Partition) -> Self {
        self.partitions.push(partition);
        self
    }
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
    #[must_use]
    pub fn valid_for(mut self, validity: Duration) -> Self {
        self.valid_for = validity;
        self
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invitation {
    pub id: InvitationId,
    pub code: InvitationCode,
    pub expires: Timestamp,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingInvitation {
    pub id: InvitationId,
    pub access: Access,
    pub partitions: Vec<Partition>,
    pub label: Option<String>,
    pub expires: Timestamp,
    pub redeemed_by: Option<PersonId>,
}

fn timestamp(seconds: u64) -> Timestamp {
    Timestamp::from_unix_micros(
        i64::try_from(seconds)
            .unwrap_or(i64::MAX)
            .saturating_mul(1_000_000),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InviteError {
    AdminRequired,
    NetworkRequired,
    TooManyOpen { limit: usize },
    Validity { min: Duration, max: Duration },
    LabelTooLong { limit: usize },
    AlreadyMember,
    Removed,
    Unknown,
    Redeemed,
}
impl InviteError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::AdminRequired => "admin_required",
            Self::NetworkRequired => "network_required",
            Self::TooManyOpen { .. } => "too_many_invitations",
            Self::Validity { .. } => "invitation_validity",
            Self::LabelTooLong { .. } => "label_too_long",
            Self::AlreadyMember => "already_member",
            Self::Removed => "removed",
            Self::Unknown => "invitation_unknown",
            Self::Redeemed => "invitation_redeemed",
        }
    }
}
impl std::fmt::Display for InviteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AdminRequired => f.write_str("only an admin invites people"),
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
            Self::Removed => f.write_str("the person was removed"),
            Self::Unknown => f.write_str("no such open invitation"),
            Self::Redeemed => f.write_str("the invitation was already redeemed"),
        }
    }
}
impl std::error::Error for InviteError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinError {
    Invalid,
    Expired,
    Used,
    Cancelled,
    OtherIdentity,
    OtherApplication,
    IdentityRequired,
    AlreadyMember,
    Removed,
    IssuerUnreachable,
}
impl JoinError {
    const CARRIED: [Self; 7] = [
        Self::Invalid,
        Self::Expired,
        Self::Used,
        Self::Cancelled,
        Self::OtherIdentity,
        Self::AlreadyMember,
        Self::Removed,
    ];
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "invitation_invalid",
            Self::Expired => "invitation_expired",
            Self::Used => "invitation_used",
            Self::Cancelled => "invitation_cancelled",
            Self::OtherIdentity => "invitation_other_identity",
            Self::OtherApplication => "invitation_other_application",
            Self::IdentityRequired => "identity_required",
            Self::AlreadyMember => "already_member",
            Self::Removed => "removed",
            Self::IssuerUnreachable => "issuer_unreachable",
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(self, Self::IssuerUnreachable)
    }
    fn carried(error: Failure) -> Failure {
        if let Some(code) = error
            .downcast_ref::<PeerError>()
            .map(|peer| peer.code.clone())
        {
            return match Self::CARRIED
                .into_iter()
                .find(|refusal| refusal.code() == code)
            {
                Some(refusal) => Box::new(refusal),
                None => error,
            };
        }
        match error.downcast_ref::<ServiceError>() {
            Some(
                ServiceError::Unreachable(_)
                | ServiceError::TimedOut(_)
                | ServiceError::NoRoute { .. },
            ) => Box::new(Self::IssuerUnreachable),
            _ => error,
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
            Self::IdentityRequired => {
                "the invitation is for an identity; configure it with Configuration::identity"
            }
            Self::AlreadyMember => {
                "the identity is already a member; link this device from another of its devices"
            }
            Self::Removed => "the identity was removed from this database",
            Self::IssuerUnreachable => "the admin that issued the invitation is not reachable",
        })
    }
}
impl std::error::Error for JoinError {}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Holder {
    Person(PublicKey),
    Token(#[serde(with = "crate::registry::hex32")] [u8; 32]),
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Code {
    v: u32,
    id: InvitationId,
    publisher: PublicKey,
    database: DatabaseContext,
    issuer: PublicKey,
    routes: Vec<Route>,
    holder: Holder,
    access: Access,
    application: String,
    expires: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub struct InvitationCode(Box<Code>);
impl InvitationCode {
    fn token(&self) -> Option<&[u8; 32]> {
        match &self.0.holder {
            Holder::Token(token) => Some(token),
            Holder::Person(_) => None,
        }
    }
    pub(crate) fn person(&self) -> Option<PublicKey> {
        match self.0.holder {
            Holder::Person(person) => Some(person),
            Holder::Token(_) => None,
        }
    }
    pub(crate) fn access(&self) -> &Access {
        &self.0.access
    }
    pub(crate) fn application(&self) -> &str {
        &self.0.application
    }
    pub(crate) fn intent(&self) -> Commitment {
        rocky_core::commitment("rocky.sdk.invitation-join.v2", &self.0.id.0)
    }
}
impl std::fmt::Display for InvitationCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bytes = serde_json::to_vec(&self.0).map_err(|_| std::fmt::Error)?;
        write!(f, "{INVITE_PREFIX}{}", BASE64URL_NOPAD.encode(&bytes))
    }
}
impl std::fmt::Debug for InvitationCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("InvitationCode");
        debug
            .field("id", &self.0.id)
            .field("expires", &self.0.expires);
        match &self.0.holder {
            Holder::Person(person) => debug.field("person", person),
            Holder::Token(_) => debug.field("token", &"<redacted>"),
        };
        debug.finish()
    }
}
impl std::str::FromStr for InvitationCode {
    type Err = JoinError;
    fn from_str(text: &str) -> Result<Self, JoinError> {
        let parsed = || -> Result<Code, Failure> {
            let bytes = BASE64URL_NOPAD.decode(
                text.trim()
                    .strip_prefix(INVITE_PREFIX)
                    .ok_or("invalid invitation code")?
                    .as_bytes(),
            )?;
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            if value["v"].as_u64() != Some(u64::from(INVITE_VERSION)) {
                return Err("unsupported invitation version".into());
            }
            let code: Code = serde_json::from_value(value)?;
            if code.database.publisher != code.publisher || code.routes.is_empty() {
                return Err("invalid invitation code".into());
            }
            Contact::checked(code.routes.clone())?;
            Ok(code)
        };
        parsed()
            .map(|code| Self(Box::new(code)))
            .map_err(|_| JoinError::Invalid)
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    holder: Holder,
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
    holder: Holder,
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
        let id = InvitationId::fresh();
        let expires = time.saturating_add(validity);
        self.open.insert(
            id,
            Pending {
                holder: opening.holder,
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
        match &pending.holder {
            Holder::Person(person) if *person != device.identity => {
                return Err(JoinError::OtherIdentity);
            }
            Holder::Person(_) => {}
            Holder::Token(token) => {
                if !wire.proves_token(token).map_err(|_| JoinError::Invalid)? {
                    return Err(JoinError::Invalid);
                }
            }
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
        return Err(Box::new(JoinError::Removed));
    }
    let other_access = registry
        .get(identity)
        .and_then(crate::registry::IdentityRecord::access)
        .is_some_and(|held| held != access);
    if *identity == settings.identity
        || *identity == settings.publisher
        || registry.trusted.contains_key(identity)
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
            return Err(Box::new(InviteError::AdminRequired));
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
        let holder = match invite.person {
            Some(person) => {
                let identity = person.key();
                admissible(self, registry, &identity, &invite.access).map_err(|error| {
                    match error.downcast_ref::<JoinError>() {
                        Some(JoinError::Removed) => Box::new(InviteError::Removed) as Failure,
                        Some(_) => Box::new(InviteError::AlreadyMember),
                        None => error,
                    }
                })?;
                if registry.full_for(&identity) {
                    return Err(Box::new(RegistryFull));
                }
                Holder::Person(identity)
            }
            None => {
                if registry.identities() >= crate::registry::MAX_IDENTITIES {
                    return Err(Box::new(RegistryFull));
                }
                Holder::Token(rocky_platform::entropy::random())
            }
        };
        if routes.is_empty() {
            return Err(Box::new(InviteError::NetworkRequired));
        }
        let (id, expires) = book.open(
            Opening {
                holder: holder.clone(),
                access: invite.access.clone(),
                partitions: invite.partitions.iter().map(StoredPartition::of).collect(),
                label: invite.label.clone(),
            },
            validity,
            time,
        )?;
        let code = InvitationCode(Box::new(Code {
            v: INVITE_VERSION,
            id,
            publisher: self.publisher,
            database: self.database_context()?,
            issuer: Self::node_key(vault)?.public_key(),
            routes,
            holder,
            access: invite.access.clone(),
            application: self.schema.application.clone(),
            expires,
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
            return Err(Box::new(JoinError::Removed));
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
            (Some(_), None) => return Err(Box::new(JoinError::IdentityRequired)),
            (Some(person), Some(provider)) if provider.public_key()? != person => {
                return Err(Box::new(JoinError::OtherIdentity));
            }
            _ => {}
        }
        Intent::resume(&Intent::path(path, false), code.intent(), store, provider)
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
            .filter(|route| route.transport == transport.kind())
            .map(|route| route.address.clone())
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return Err(Box::new(JoinError::IssuerUnreachable));
        }
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let (request, secret) =
            IdentityRequest::sign(&*provider, &node, invite.issuer, now()?, invite.database)?;
        let reply = exchange(
            transport,
            &addresses,
            invite.database.scope,
            &PeerRequest::Identity(Box::new(IdentityEnrollment::Invited(
                InvitedEnrollmentRequest::sign(request.clone(), invite.id.0, code.token(), &node)?,
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
        let settings = Self::install_enrollment(
            schema,
            (intent.installation.clone(), &vault),
            EnrollingIssuer {
                publisher: invite.publisher,
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
    fn opening(holder: Holder) -> Opening {
        Opening {
            holder,
            access: Access::Full,
            partitions: Vec::new(),
            label: None,
        }
    }
    fn opened(book: &mut Book, holder: Holder) -> InvitationId {
        book.open(opening(holder), OPEN_FOR, NOW)
            .expect("open invitation")
            .0
    }
    fn wire(
        id: InvitationId,
        identity: u8,
        node: u8,
        token: Option<&[u8; 32]>,
    ) -> InvitedEnrollmentRequest {
        let person = PrivateKey::from_bytes(&[identity; 32]);
        let node = PrivateKey::from_bytes(&[node; 32]);
        let (request, _) =
            IdentityRequest::sign(&person, &node, key(4), NOW, database()).expect("request");
        InvitedEnrollmentRequest::sign(request, id.0, token, &node).expect("bound request")
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
    fn a_token_invitation_admits_the_first_device_that_proves_the_token_once() {
        let mut book = Book::empty();
        let token = [6; 32];
        let id = opened(&mut book, Holder::Token(token));
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 10, Some(&[7; 32])), NOW),
            Err(JoinError::Invalid),
            "a wrong token proves nothing"
        );
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 10, None), NOW),
            Err(JoinError::Invalid),
            "a token invitation needs the token's proof"
        );
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 10, Some(&token)), NOW),
            Ok(true)
        );
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 10, Some(&token)), NOW + 1),
            Ok(false),
            "the redeeming device retries an answer it lost"
        );
        assert_eq!(
            redeemed(&mut book, &wire(id, 2, 20, Some(&token)), NOW + 1),
            Err(JoinError::Used),
            "a token is single use"
        );
        assert_eq!(book.listed(NOW)[0].redeemed_by, Some(PersonId::new(key(1))));
    }

    #[test]
    fn an_identity_invitation_admits_only_that_identity() {
        let mut book = Book::empty();
        let id = opened(&mut book, Holder::Person(key(1)));
        assert_eq!(
            redeemed(&mut book, &wire(id, 2, 10, None), NOW),
            Err(JoinError::OtherIdentity)
        );
        assert_eq!(redeemed(&mut book, &wire(id, 1, 10, None), NOW), Ok(true));
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 11, None), NOW),
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
                    ..opening(Holder::Person(key(1)))
                },
                OPEN_FOR,
                NOW,
            )
            .expect("open invitation")
            .0;
        let first = book
            .redeem(&wire(id, 1, 10, None), device(1, 10), NOW)
            .expect("redeemed");
        assert_eq!(first.label.as_deref(), Some("Alice"));
        let retried = book
            .redeem(&wire(id, 1, 10, None), device(1, 10), NOW)
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
            .redeem(&wire(id, 1, 11, None), device(1, 11), NOW)
            .expect("redeemed");
        assert!(again.first);
    }

    #[test]
    fn an_invitation_ends_at_its_expiry_even_for_the_device_that_redeemed_it() {
        let mut book = Book::empty();
        let id = opened(&mut book, Holder::Person(key(1)));
        assert!(redeemed(&mut book, &wire(id, 1, 10, None), NOW + OPEN_FOR).is_ok());
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 10, None), NOW + OPEN_FOR + 1),
            Err(JoinError::Expired)
        );
        assert!(book.listed(NOW + OPEN_FOR + 1).is_empty());
        book.forget_expired(NOW + OPEN_FOR + 1);
        assert_eq!(
            redeemed(&mut book, &wire(id, 1, 10, None), NOW + OPEN_FOR + 1),
            Err(JoinError::Cancelled),
            "an expired invitation is forgotten once the book is next written"
        );
    }

    #[test]
    fn only_an_unused_invitation_is_cancelled() {
        let mut book = Book::empty();
        let unused = opened(&mut book, Holder::Person(key(1)));
        let used = opened(&mut book, Holder::Person(key(2)));
        assert!(redeemed(&mut book, &wire(used, 2, 20, None), NOW).is_ok());
        assert_eq!(book.cancel(unused, NOW), Ok(()));
        assert_eq!(
            redeemed(&mut book, &wire(unused, 1, 10, None), NOW),
            Err(JoinError::Cancelled)
        );
        assert_eq!(book.cancel(used, NOW), Err(InviteError::Redeemed));
        assert_eq!(book.cancel(unused, NOW), Err(InviteError::Unknown));
    }

    #[test]
    fn the_book_holds_a_bounded_number_of_open_invitations() {
        let mut book = Book::empty();
        for _ in 0..MAX_OPEN {
            opened(&mut book, Holder::Person(key(1)));
        }
        assert_eq!(
            book.open(opening(Holder::Person(key(1))), OPEN_FOR, NOW)
                .err(),
            Some(InviteError::TooManyOpen { limit: MAX_OPEN })
        );
        assert!(
            book.open(
                opening(Holder::Person(key(1))),
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
        opened(&mut book, Holder::Token([1; 32]));
        assert!(Book::parse(&book.to_bytes()?)? == book);
        let mut other: serde_json::Value = serde_json::from_slice(&book.to_bytes()?)?;
        other["version"] = serde_json::json!(BOOK_VERSION - 1);
        assert!(Book::parse(&serde_json::to_vec(&other)?).is_err());
        Ok(())
    }

    fn code(holder: Holder) -> Code {
        Code {
            v: INVITE_VERSION,
            id: InvitationId([7; 32]),
            publisher: key(3),
            database: database(),
            issuer: key(4),
            routes: vec![Route {
                transport: "memory".into(),
                address: "issuer".into(),
            }],
            holder,
            access: Access::Full,
            application: "shopping".into(),
            expires: NOW,
        }
    }

    #[test]
    fn a_code_round_trips_and_redacts_its_token() -> Result<(), Failure> {
        let token = InvitationCode(Box::new(code(Holder::Token([9; 32]))));
        let parsed: InvitationCode = token.to_string().parse()?;
        assert!(parsed == token);
        assert!(!format!("{token:?}").contains(&HEXLOWER.encode(&[9; 32])));
        let person = InvitationCode(Box::new(code(Holder::Person(key(1)))));
        assert_eq!(
            person.to_string().parse::<InvitationCode>()?.person(),
            Some(key(1))
        );
        let unroutable = InvitationCode(Box::new(Code {
            routes: Vec::new(),
            ..code(Holder::Person(key(1)))
        }));
        assert_eq!(
            unroutable.to_string().parse::<InvitationCode>().err(),
            Some(JoinError::Invalid)
        );
        let mut older = serde_json::to_value(code(Holder::Person(key(1))))?;
        older["v"] = serde_json::json!(INVITE_VERSION - 1);
        let older = format!(
            "{INVITE_PREFIX}{}",
            BASE64URL_NOPAD.encode(&serde_json::to_vec(&older)?)
        );
        assert_eq!(
            older.parse::<InvitationCode>().err(),
            Some(JoinError::Invalid)
        );
        Ok(())
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
