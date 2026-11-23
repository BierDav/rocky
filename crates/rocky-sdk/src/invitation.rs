use crate::{
    Failure,
    enrollment::{
        Contact, EnrollingIssuer, Enrollment, Intent, NodeKeys, Route, Settings, exchange, now,
    },
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
    IdentityRequest, InvitedEnrollmentRequest, Profile, PublicKey,
};
use rocky_secure::{EncryptedReplica, PrivateKey};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::RangeInclusive, path::Path, sync::Arc, time::Duration};

pub(crate) const AUTHORITY_INVITATIONS_KEY: &str = "invitations";
const BOOK_VERSION: u32 = 1;
const INVITE_VERSION: u32 = 2;
const INVITE_PREFIX: &str = "rocky://invite/";
const SKEW: u64 = 300;
const VALIDITY_SECONDS: RangeInclusive<u64> = 60..=30 * 24 * 60 * 60;
pub const DEFAULT_INVITATION_VALIDITY: Duration = Duration::from_secs(10 * 60);
pub const MAX_OPEN_INVITATIONS: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InvitationId(pub(crate) Commitment);
impl InvitationId {
    fn fresh() -> Self {
        Self(rocky_platform::entropy::random())
    }
    pub fn to_hex(&self) -> String {
        HEXLOWER.encode(&self.0)
    }
}
impl std::fmt::Display for InvitationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}
impl std::fmt::Debug for InvitationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "InvitationId({self})")
    }
}
impl std::str::FromStr for InvitationId {
    type Err = Failure;
    fn from_str(hex: &str) -> Result<Self, Failure> {
        Ok(Self(crate::enrollment::hex32(hex)?))
    }
}
impl Serialize for InvitationId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}
impl<'de> Deserialize<'de> for InvitationId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InviteTerms {
    pub valid_for: Option<Duration>,
    pub label: Option<Vec<u8>>,
}
impl InviteTerms {
    fn validity(&self) -> Result<u64, Failure> {
        let seconds = self
            .valid_for
            .unwrap_or(DEFAULT_INVITATION_VALIDITY)
            .as_secs();
        if !VALIDITY_SECONDS.contains(&seconds) {
            return Err("an invitation is valid for between a minute and 30 days".into());
        }
        Ok(seconds)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invitation {
    pub uri: String,
    pub id: InvitationId,
    pub identity: PublicKey,
    pub profile: crate::GrantProfile,
    pub expires: u64,
    pub sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenInvitation {
    pub id: InvitationId,
    pub identity: PublicKey,
    pub profile: crate::GrantProfile,
    pub expires: u64,
    pub redeemed_by: Option<PublicKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvitationRefusal {
    Unknown,
    Expired,
    Used,
    OtherIdentity,
}
impl InvitationRefusal {
    const ALL: [Self; 4] = [
        Self::Unknown,
        Self::Expired,
        Self::Used,
        Self::OtherIdentity,
    ];
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unknown => "invitation_unknown",
            Self::Expired => "invitation_expired",
            Self::Used => "invitation_used",
            Self::OtherIdentity => "invitation_other_identity",
        }
    }
    fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|refusal| refusal.code() == code)
    }
    fn carried(error: Failure) -> Failure {
        match error
            .downcast_ref::<PeerError>()
            .and_then(|peer| Self::from_code(&peer.code))
        {
            Some(refusal) => Box::new(refusal),
            None => error,
        }
    }
}
impl std::fmt::Display for InvitationRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => {
                "the issuer holds no such invitation; it was withdrawn or never issued"
            }
            Self::Expired => "the invitation expired",
            Self::Used => "another node already used the invitation",
            Self::OtherIdentity => "the invitation is for another identity",
        })
    }
}
impl std::error::Error for InvitationRefusal {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    identity: PublicKey,
    profile: Profile,
    label: Option<Vec<u8>>,
    expires: u64,
    redeemed_by: Option<PublicKey>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Book {
    version: u32,
    open: BTreeMap<InvitationId, Pending>,
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
        identity: PublicKey,
        (profile, label): (Profile, Option<Vec<u8>>),
        validity: u64,
        time: u64,
    ) -> Result<(InvitationId, u64), Failure> {
        self.forget_expired(time);
        if self.open.len() >= MAX_OPEN_INVITATIONS {
            return Err(format!(
                "at most {MAX_OPEN_INVITATIONS} invitations are open at once; withdraw one or let it expire"
            )
            .into());
        }
        let id = InvitationId::fresh();
        let expires = time.saturating_add(validity);
        self.open.insert(
            id,
            Pending {
                identity,
                profile,
                label,
                expires,
                redeemed_by: None,
            },
        );
        Ok((id, expires))
    }
    fn redeem(
        &mut self,
        id: InvitationId,
        device: NodeKeys,
        time: u64,
    ) -> Result<Redemption, InvitationRefusal> {
        let pending = self.open.get_mut(&id).ok_or(InvitationRefusal::Unknown)?;
        if pending.expires < time {
            return Err(InvitationRefusal::Expired);
        }
        if pending.identity != device.identity {
            return Err(InvitationRefusal::OtherIdentity);
        }
        if pending
            .redeemed_by
            .is_some_and(|redeemed| redeemed != device.node)
        {
            return Err(InvitationRefusal::Used);
        }
        let first = pending.redeemed_by.replace(device.node).is_none();
        Ok(Redemption {
            profile: pending.profile.clone(),
            label: pending.label.clone().filter(|_| first),
            first,
        })
    }
    pub(crate) fn release(&mut self, id: InvitationId, node: PublicKey) -> bool {
        match self.open.get_mut(&id) {
            Some(pending) if pending.redeemed_by == Some(node) => {
                pending.redeemed_by = None;
                true
            }
            _ => false,
        }
    }
    pub(crate) fn withdraw(&mut self, id: InvitationId, time: u64) -> Result<(), Failure> {
        self.forget_expired(time);
        let pending = self.open.get(&id).ok_or(InvitationRefusal::Unknown)?;
        if pending.redeemed_by.is_some() {
            return Err(Box::new(InvitationRefusal::Used));
        }
        self.open.remove(&id);
        Ok(())
    }
    pub(crate) fn listed(&self, time: u64) -> Vec<OpenInvitation> {
        self.open
            .iter()
            .filter(|(_, pending)| time <= pending.expires)
            .map(|(id, pending)| OpenInvitation {
                id: *id,
                identity: pending.identity,
                profile: pending.profile.clone(),
                expires: pending.expires,
                redeemed_by: pending.redeemed_by,
            })
            .collect()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invite {
    v: u32,
    id: InvitationId,
    publisher: PublicKey,
    database: DatabaseContext,
    issuer: PublicKey,
    routes: Vec<Route>,
    via_custody: bool,
    identity: PublicKey,
    expires: u64,
}
impl Invite {
    fn uri(&self) -> Result<String, Failure> {
        Ok(format!(
            "{INVITE_PREFIX}{}",
            BASE64URL_NOPAD.encode(&serde_json::to_vec(self)?)
        ))
    }
    fn parse(uri: &str) -> Result<Self, Failure> {
        let bytes = BASE64URL_NOPAD.decode(
            uri.trim()
                .strip_prefix(INVITE_PREFIX)
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
        if invite.routes.is_empty() {
            return Err("the invitation names no route to its issuer".into());
        }
        Contact::checked(invite.routes.clone())?;
        Ok(invite)
    }
}
fn keeps_recorded_profile(
    registry: &Registry,
    identity: &PublicKey,
    profile: &Profile,
) -> Result<(), Failure> {
    if registry
        .get(identity)
        .and_then(crate::registry::IdentityRecord::profile)
        .is_some_and(|held| held != profile)
    {
        return Err("the identity already holds another profile in this database".into());
    }
    Ok(())
}
pub(crate) fn check_uri(uri: &str) -> Result<(), Failure> {
    Invite::parse(uri).map(drop)
}
pub(crate) fn is_uri(uri: &str) -> bool {
    uri.trim().starts_with(INVITE_PREFIX)
}
pub(crate) fn routed_through(
    uri: &str,
    relay: crate::enrollment::ContactRecord,
) -> Result<String, Failure> {
    let mut invite = Invite::parse(uri)?;
    if invite.publisher != relay.publisher {
        return Err("relay belongs to another domain".into());
    }
    invite.routes = relay.routes;
    invite.via_custody = true;
    invite.uri()
}

pub(crate) struct Issued {
    pub(crate) uri: String,
    pub(crate) id: InvitationId,
    pub(crate) expires: u64,
}
#[derive(Debug, PartialEq)]
struct Redemption {
    profile: Profile,
    label: Option<Vec<u8>>,
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
        (identity, profile, terms): (PublicKey, Profile, &InviteTerms),
        registry: &Registry,
        time: u64,
    ) -> Result<Issued, Failure> {
        let validity = terms.validity()?;
        if terms
            .label
            .as_ref()
            .is_some_and(|label| label.len() > crate::registry::MAX_LABEL)
        {
            return Err("identity label exceeds 256 bytes".into());
        }
        if !self.issuer() {
            return Err("only an issuer invites identities".into());
        }
        self.check_profile(&profile)?;
        if self.revoked_identity(&identity, registry) {
            return Err("the identity was revoked".into());
        }
        keeps_recorded_profile(registry, &identity, &profile)?;
        if identity == self.identity || registry.trusted.contains_key(&identity) {
            return Err(
                "a trusted identity links its nodes instead of accepting an invitation".into(),
            );
        }
        if registry.full_for(&identity) {
            return Err(Box::new(RegistryFull));
        }
        let (id, expires) = book.open(identity, (profile, terms.label.clone()), validity, time)?;
        let uri = Invite {
            v: INVITE_VERSION,
            id,
            publisher: self.publisher,
            database: self.database_context()?,
            issuer: Self::node_key(vault)?.public_key(),
            routes,
            via_custody: false,
            identity,
            expires,
        }
        .uri()?;
        Ok(Issued { uri, id, expires })
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
            return Err("only an issuer answers invitations".into());
        }
        wire.verify_binding()?;
        let request = wire.request;
        let answering = Self::node_key(vault)?;
        request.verify(
            answering.public_key(),
            request.identity,
            (time, SKEW),
            self.database_context()?,
        )?;
        let id = InvitationId(wire.invitation);
        let device = NodeKeys {
            identity: request.identity,
            node: request.node,
        };
        let Redemption {
            profile,
            label,
            first,
        } = book.redeem(id, device, time)?;
        keeps_recorded_profile(registry, &device.identity, &profile)?;
        let (enrollment, changes) =
            self.enrollment_for(vault, device, (profile, label), Some(id), registry)?;
        Ok(Redeemed {
            answer: request
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
        provider: &Arc<dyn IdentityProvider>,
        invitation: &str,
    ) -> Result<(Intent, Vault), Failure> {
        let invite = Invite::parse(invitation)?;
        if provider.public_key()? != invite.identity {
            return Err(Box::new(InvitationRefusal::OtherIdentity));
        }
        Intent::resume(
            &Intent::path(path, false),
            Some(rocky_core::commitment(
                "rocky.sdk.invitation-join.v1",
                &invite.id.0,
            )),
            store,
            Some(provider),
            false,
        )
    }

    pub(crate) async fn enroll_invited(
        (path, schema): (&Path, &Schema),
        (intent, vault): (Intent, Vault),
        provider: Arc<dyn IdentityProvider>,
        invitation: &str,
        transport: &dyn Transport,
    ) -> Result<Self, Failure> {
        let invite = Invite::parse(invitation)?;
        let addresses = invite
            .routes
            .iter()
            .filter(|route| route.transport == transport.kind())
            .map(|route| route.address.clone())
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return Err(ServiceError::NoRoute {
                peer: "the inviting issuer",
                network: transport.kind().into(),
            }
            .into());
        }
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let (request, secret) =
            IdentityRequest::sign(&*provider, &node, invite.issuer, now()?, invite.database)?;
        let reply = exchange(
            transport,
            &addresses,
            invite.database.scope,
            &PeerRequest::Identity(Box::new(IdentityEnrollment::Invited(
                InvitedEnrollmentRequest::sign(request.clone(), invite.id.0, &node)?,
            ))),
        )
        .await
        .map_err(InvitationRefusal::carried)?;
        let PeerReply::Identity(IdentityReply::Enrolled(sealed)) = reply else {
            return Err("invitation reply kind".into());
        };
        let enrollment: Enrollment =
            serde_json::from_slice(&IdentityAnswer::from_bytes(&sealed)?.open(&request, &secret)?)?;
        if enrollment.issuer() != invite.issuer {
            return Err("the answer comes from another issuer than the invitation names".into());
        }
        let settings = Self::install_enrollment(
            schema,
            (intent.installation.clone(), &vault),
            EnrollingIssuer {
                publisher: invite.publisher,
                database: invite.database,
                routes: invite.routes,
                via_custody: invite.via_custody,
            },
            enrollment,
            (provider, intent.single_use),
        )?;
        settings.save(path).await?;
        rocky_platform::fs::remove_file(Intent::path(path, false))?;
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
    fn writer() -> Profile {
        serde_json::from_value(serde_json::json!({"kind": "writer", "scopes": ["items"]}))
            .expect("writer profile")
    }
    fn opened(book: &mut Book, identity: u8) -> InvitationId {
        book.open(key(identity), (writer(), None), OPEN_FOR, NOW)
            .expect("open invitation")
            .0
    }

    #[test]
    fn an_invitation_admits_only_its_identity_and_then_only_the_node_that_redeemed_it() {
        let mut book = Book::empty();
        let id = opened(&mut book, 1);
        assert_eq!(
            book.redeem(id, device(2, 10), NOW),
            Err(InvitationRefusal::OtherIdentity)
        );
        assert_eq!(
            book.redeem(id, device(1, 10), NOW),
            Ok(Redemption {
                profile: writer(),
                label: None,
                first: true
            })
        );
        assert_eq!(
            book.redeem(id, device(1, 10), NOW + 1),
            Ok(Redemption {
                profile: writer(),
                label: None,
                first: false
            }),
            "the redeeming node retries an answer it lost"
        );
        assert_eq!(
            book.redeem(id, device(1, 11), NOW + 1),
            Err(InvitationRefusal::Used),
            "a second node of the same identity links instead"
        );
    }

    #[test]
    fn a_retried_redemption_leaves_the_label_alone_and_a_released_one_starts_over() {
        let mut book = Book::empty();
        let id = book
            .open(key(1), (writer(), Some(b"Alice".to_vec())), OPEN_FOR, NOW)
            .expect("open invitation")
            .0;
        let first = book.redeem(id, device(1, 10), NOW).expect("redeemed");
        assert!(first.first);
        assert_eq!(first.label.as_deref(), Some(b"Alice".as_slice()));
        let retried = book.redeem(id, device(1, 10), NOW).expect("redeemed");
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
        let again = book.redeem(id, device(1, 11), NOW).expect("redeemed");
        assert!(again.first);
        assert_eq!(again.label.as_deref(), Some(b"Alice".as_slice()));
    }

    #[test]
    fn an_invitation_ends_at_its_expiry_even_for_the_node_that_redeemed_it() {
        let mut book = Book::empty();
        let id = opened(&mut book, 1);
        assert!(book.redeem(id, device(1, 10), NOW + OPEN_FOR).is_ok());
        assert_eq!(
            book.redeem(id, device(1, 10), NOW + OPEN_FOR + 1),
            Err(InvitationRefusal::Expired)
        );
        let unused = opened(&mut book, 2);
        assert_eq!(
            book.redeem(unused, device(2, 20), NOW + OPEN_FOR + 1),
            Err(InvitationRefusal::Expired)
        );
        assert!(book.listed(NOW + OPEN_FOR + 1).is_empty());
        book.forget_expired(NOW + OPEN_FOR + 1);
        assert_eq!(
            book.redeem(unused, device(2, 20), NOW + OPEN_FOR + 1),
            Err(InvitationRefusal::Unknown),
            "an expired invitation is forgotten once the book is next written"
        );
    }

    #[test]
    fn only_an_unused_invitation_is_withdrawn() {
        let mut book = Book::empty();
        let unused = opened(&mut book, 1);
        let used = opened(&mut book, 2);
        assert!(book.redeem(used, device(2, 20), NOW).is_ok());
        assert!(book.withdraw(unused, NOW).is_ok());
        assert_eq!(
            book.redeem(unused, device(1, 10), NOW),
            Err(InvitationRefusal::Unknown)
        );
        let refused = book
            .withdraw(used, NOW)
            .expect_err("a used invitation stays");
        assert_eq!(
            refused.downcast_ref::<InvitationRefusal>(),
            Some(&InvitationRefusal::Used)
        );
        let listed = book.listed(NOW);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].redeemed_by, Some(key(20)));
    }

    #[test]
    fn the_book_holds_a_bounded_number_of_open_invitations() {
        let mut book = Book::empty();
        for _ in 0..MAX_OPEN_INVITATIONS {
            opened(&mut book, 1);
        }
        assert!(book.open(key(1), (writer(), None), OPEN_FOR, NOW).is_err());
        assert!(
            book.open(key(1), (writer(), None), OPEN_FOR, NOW + OPEN_FOR + 1)
                .is_ok(),
            "expired invitations free their slots"
        );
    }

    #[test]
    fn the_book_reads_back_only_its_own_version() -> Result<(), Failure> {
        let mut book = Book::empty();
        opened(&mut book, 1);
        assert_eq!(Book::parse(&book.to_bytes()?)?, book);
        let mut other: serde_json::Value = serde_json::from_slice(&book.to_bytes()?)?;
        other["version"] = serde_json::json!(BOOK_VERSION + 1);
        assert!(Book::parse(&serde_json::to_vec(&other)?).is_err());
        Ok(())
    }

    #[test]
    fn an_invitation_uri_carries_no_secret_and_needs_a_route() -> Result<(), Failure> {
        let publisher = key(3);
        let invite = Invite {
            v: INVITE_VERSION,
            id: InvitationId([7; 32]),
            publisher,
            database: DatabaseContext {
                publisher,
                scope: rocky_core::Scope {
                    mesh: rocky_core::MeshId([1; 16]),
                    app: rocky_core::AppId([2; 16]),
                    domain: rocky_core::DomainId([3; 16]),
                },
            },
            issuer: key(4),
            routes: vec![Route {
                transport: "memory".into(),
                address: "issuer".into(),
            }],
            via_custody: false,
            identity: key(1),
            expires: NOW,
        };
        let parsed = Invite::parse(&invite.uri()?)?;
        assert_eq!(parsed.id, invite.id);
        assert_eq!(parsed.identity, key(1));
        let fields: serde_json::Value = serde_json::to_value(&invite)?;
        let mut names: Vec<&str> = fields
            .as_object()
            .ok_or("invite object")?
            .keys()
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "database",
                "expires",
                "id",
                "identity",
                "issuer",
                "publisher",
                "routes",
                "v",
                "via_custody"
            ],
            "an invitation names public keys and routes only"
        );
        let unroutable = Invite {
            routes: Vec::new(),
            ..invite
        };
        assert!(Invite::parse(&unroutable.uri()?).is_err());
        Ok(())
    }

    #[test]
    fn a_refusal_crosses_the_wire_as_its_code() {
        for refusal in InvitationRefusal::ALL {
            let failure: Failure = Box::new(refusal);
            let carried = crate::peer::failure(&failure);
            assert_eq!(carried.code, refusal.code());
            let received = InvitationRefusal::carried(Box::new(carried));
            assert_eq!(received.downcast_ref::<InvitationRefusal>(), Some(&refusal));
        }
    }
}
