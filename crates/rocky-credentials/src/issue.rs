//! Issuance: raw grants, the grant profiles built on them and what an
//! identity with issuer authority signs.
use crate::{
    AuthorRange, Capability, CredentialChain, Endorsement, Error, Extended, Extension,
    IdentityProvider, MAX_CHAIN_LIFETIME, MAX_CREDENTIAL_DEPTH, PartitionGrant, PublicKey,
    Revocation, RevocationSet, SignedCapability, SignedRevocation, denied,
};
use rocky_core::{
    Commitment, FieldSelection, PrincipalKind, ResourceGrant, ResourceScope, Right, RowSelection,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One reader scope an issuer can grant: its name, scope and columns.
#[derive(Clone, Debug)]
pub struct Resource {
    pub name: String,
    pub scope: ResourceScope,
    /// Each column's name and field commitment.
    pub columns: BTreeMap<String, Commitment>,
}

/// Most grants one capability holds.
pub const MAX_GRANTS: usize = 64;

/// A raw grant: rights over one named reader scope, optionally limited to
/// some of its columns. Any set is issuable, `Custody` included; the
/// profiles below are functions producing these.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub scope: String,
    #[serde(with = "named_rights")]
    pub rights: BTreeSet<Right>,
    /// The columns the rights cover; every column when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<BTreeSet<String>>,
}
impl Grant {
    /// The signed grant this names over `resources`. `PublishSchema` is no
    /// collection right.
    pub fn resolve(&self, resources: &[Resource]) -> Result<ResourceGrant, Error> {
        let resource = resources
            .iter()
            .find(|resource| resource.name == self.scope)
            .ok_or_else(|| denied("a grant names an unknown reader scope"))?;
        if self.rights.is_empty() || self.rights.contains(&Right::PublishSchema) {
            return Err(denied(
                "a grant names no right, or one that is not a collection's",
            ));
        }
        // Without columns the grant covers every field, in every generation.
        let fields = match &self.columns {
            None => FieldSelection::All,
            Some(names) => FieldSelection::Only(
                names
                    .iter()
                    .map(|name| {
                        resource
                            .columns
                            .get(name)
                            .copied()
                            .ok_or_else(|| denied("a grant names an unknown column"))
                    })
                    .collect::<Result<_, _>>()?,
            ),
        };
        Ok(ResourceGrant {
            scope: resource.scope,
            rows: RowSelection::All,
            rights: self.rights.clone(),
            fields,
        })
    }
    /// Every grant of `grants` resolved; there is no empty grant set.
    pub fn resolve_all(
        grants: &[Grant],
        resources: &[Resource],
    ) -> Result<Vec<ResourceGrant>, Error> {
        if grants.is_empty() || grants.len() > MAX_GRANTS {
            return Err(denied("a credential holds 1 to 64 grants"));
        }
        grants
            .iter()
            .map(|grant| grant.resolve(resources))
            .collect()
    }
    /// The raw grant a signed one amounts to over `resources`, if any.
    pub fn describe(grant: &ResourceGrant, resources: &[Resource]) -> Option<Self> {
        let resource = resources
            .iter()
            .find(|resource| resource.scope == grant.scope)?;
        if grant.rows != RowSelection::All {
            return None;
        }
        let columns = match &grant.fields {
            FieldSelection::All => None,
            FieldSelection::Only(fields) => Some(
                resource
                    .columns
                    .iter()
                    .filter(|(_, field)| fields.contains(*field))
                    .map(|(name, _)| name.clone())
                    .collect(),
            ),
        };
        let described = Self {
            scope: resource.name.clone(),
            rights: grant.rights.clone(),
            columns,
        };
        (described.resolve(std::slice::from_ref(resource)).ok()? == *grant).then_some(described)
    }
}
/// Rights by their lowercase names, as JSON and the call contract spell them.
mod named_rights {
    use rocky_core::Right;
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
    use std::collections::BTreeSet;
    const NAMES: [(Right, &str); 6] = [
        (Right::Read, "read"),
        (Right::Update, "update"),
        (Right::Insert, "insert"),
        (Right::Delete, "delete"),
        (Right::Custody, "custody"),
        (Right::PublishSchema, "publish_schema"),
    ];
    pub fn serialize<S: Serializer>(rights: &BTreeSet<Right>, to: S) -> Result<S::Ok, S::Error> {
        let names: Vec<&str> = rights
            .iter()
            .filter_map(|right| NAMES.iter().find(|(known, _)| known == right))
            .map(|(_, name)| *name)
            .collect();
        names.serialize(to)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<BTreeSet<Right>, D::Error> {
        Vec::<String>::deserialize(from)?
            .iter()
            .map(|name| {
                NAMES
                    .iter()
                    .find(|(_, known)| known == name)
                    .map(|(right, _)| *right)
                    .ok_or_else(|| D::Error::custom(format!("unknown right {name}")))
            })
            .collect()
    }
}

/// What an enrollment grants. There is no default: an issuer names one.
/// Reader, writer and issuer are the optional starter layer: functions
/// producing raw grants. A writer edits its scopes; issuers enroll and
/// renew devices. No profile holds `PublishSchema`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", from = "ProfileRecord")]
pub enum Profile {
    /// Holds ciphertext of the named scopes without reader or issuing keys.
    Custody { scopes: BTreeSet<String> },
    /// Reads the named reader scopes and holds their ciphertext.
    Reader { scopes: BTreeSet<String> },
    /// Reads and writes the named reader scopes.
    Writer { scopes: BTreeSet<String> },
    /// A writer of every reader scope with issuer authority: it enrolls,
    /// renews and revokes identities and signs the domain's statements.
    Issuer,
    /// Exactly these raw grants.
    Grants { grants: Vec<Grant> },
}
/// How a profile is read: a unit variant would accept unknown fields.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ProfileRecord {
    Custody { scopes: BTreeSet<String> },
    Reader { scopes: BTreeSet<String> },
    Writer { scopes: BTreeSet<String> },
    Issuer {},
    Grants { grants: Vec<Grant> },
}
impl From<ProfileRecord> for Profile {
    fn from(record: ProfileRecord) -> Self {
        match record {
            ProfileRecord::Custody { scopes } => Self::Custody { scopes },
            ProfileRecord::Reader { scopes } => Self::Reader { scopes },
            ProfileRecord::Writer { scopes } => Self::Writer { scopes },
            ProfileRecord::Issuer {} => Self::Issuer,
            ProfileRecord::Grants { grants } => Self::Grants { grants },
        }
    }
}
const READ: [Right; 2] = [Right::Read, Right::Custody];
const WRITE: [Right; 5] = [
    Right::Read,
    Right::Insert,
    Right::Update,
    Right::Delete,
    Right::Custody,
];
impl Profile {
    /// Whether the profile holds issuer authority.
    pub fn issues(&self) -> bool {
        matches!(self, Self::Issuer)
    }
    /// Whether the profile reads, and so holds the keys of, scope `name`.
    pub fn reads(&self, name: &str) -> bool {
        match self {
            Self::Reader { scopes } | Self::Writer { scopes } => scopes.contains(name),
            Self::Issuer => true,
            Self::Custody { .. } => false,
            Self::Grants { grants } => grants
                .iter()
                .any(|grant| grant.scope == name && grant.rights.contains(&Right::Read)),
        }
    }
    /// The raw grants this profile stands for over `resources`. A name
    /// that is no resource, or a profile that names none, is refused.
    pub fn raw(&self, resources: &[Resource]) -> Result<Vec<Grant>, Error> {
        let (names, rights): (Option<&BTreeSet<String>>, &[Right]) = match self {
            Self::Custody { scopes } => (Some(scopes), &[Right::Custody]),
            Self::Grants { grants } => return Ok(grants.clone()),
            Self::Reader { scopes } => (Some(scopes), &READ),
            Self::Writer { scopes } => (Some(scopes), &WRITE),
            Self::Issuer => (None, &WRITE),
        };
        if let Some(names) = names
            && (names.is_empty()
                || names
                    .iter()
                    .any(|name| !resources.iter().any(|resource| resource.name == *name)))
        {
            return Err(denied("grant profile names no or unknown reader scopes"));
        }
        Ok(resources
            .iter()
            .filter(|resource| names.is_none_or(|names| names.contains(&resource.name)))
            .map(|resource| Grant {
                scope: resource.name.clone(),
                rights: rights.iter().copied().collect(),
                // Custody covers ciphertext only: no column at all.
                columns: matches!(self, Self::Custody { .. }).then(BTreeSet::new),
            })
            .collect())
    }
    /// The whole-row grants of this profile over `resources`.
    pub fn grants(&self, resources: &[Resource]) -> Result<Vec<ResourceGrant>, Error> {
        Grant::resolve_all(&self.raw(resources)?, resources)
    }
    /// What signed `grants` amount to, for display and for renewal into
    /// another definition: a reader, writer or custody when they are exactly one,
    /// otherwise the raw grants. Grants no raw grant names are dropped.
    pub fn of(grants: &[ResourceGrant], resources: &[Resource]) -> Self {
        let named = |right: Right| -> BTreeSet<String> {
            resources
                .iter()
                .filter(|resource| {
                    grants
                        .iter()
                        .any(|grant| grant.scope == resource.scope && grant.rights.contains(&right))
                })
                .map(|resource| resource.name.clone())
                .collect()
        };
        let starter = [
            Self::Reader {
                scopes: named(Right::Read),
            },
            Self::Writer {
                scopes: named(Right::Insert),
            },
            Self::Custody {
                scopes: named(Right::Custody),
            },
        ];
        let same = |profile: &Self| {
            profile
                .grants(resources)
                .is_ok_and(|expected| same_set(&expected, grants))
        };
        if let Some(profile) = starter.into_iter().find(same) {
            return profile;
        }
        Self::Grants {
            grants: grants
                .iter()
                .filter_map(|grant| Grant::describe(grant, resources))
                .collect(),
        }
    }
}
fn same_set(left: &[ResourceGrant], right: &[ResourceGrant]) -> bool {
    left.len() == right.len()
        && left.iter().all(|grant| right.contains(grant))
        && right.iter().all(|grant| left.contains(grant))
}

/// What an identity with issuer authority signs: the owner's, whose key is
/// the publisher key, or an issuer's through its chain of issuer links. The
/// identity's provider signs; no key is copied. What it signs holds in every
/// generation of the domain.
pub struct Authority<'a> {
    pub identity: &'a dyn IdentityProvider,
    /// The issuer links from the publisher to `identity`; empty for the owner.
    pub chain: &'a CredentialChain,
}
static OWNER: CredentialChain = CredentialChain(Vec::new());
impl<'a> Authority<'a> {
    /// The owner: its identity key is the publisher key.
    pub fn owner(identity: &'a dyn IdentityProvider) -> Self {
        Self {
            identity,
            chain: &OWNER,
        }
    }
    /// An issuer whose issuer links `chain` ends at `identity`.
    pub fn issuer(identity: &'a dyn IdentityProvider, chain: &'a CredentialChain) -> Self {
        Self { identity, chain }
    }
    /// The links a capability this authority signs follows.
    pub fn prefix(&self) -> &CredentialChain {
        self.chain
    }
    /// A capability this authority signed, with the links it follows.
    pub fn chain_of(&self, capability: SignedCapability) -> CredentialChain {
        let mut chain = self.chain.clone();
        chain.0.push(capability);
        chain
    }
    /// An identity capability for `subject`, valid from `skew` seconds
    /// before `time` for `ttl` seconds. Its subject certifies only its own
    /// nodes.
    pub fn issue(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        authors: AuthorRange,
        validity: (u64, u64, u64),
    ) -> Result<SignedCapability, Error> {
        self.issue_delegable(subject, grants, 1, authors, validity)
    }
    /// As `issue`, with `delegations` links allowed below it: 1 reaches
    /// the subject's own node certificates, 2 one further delegate.
    pub fn issue_delegable(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        delegations: u8,
        authors: AuthorRange,
        validity: (u64, u64, u64),
    ) -> Result<SignedCapability, Error> {
        self.issue_terms(subject, grants, (delegations, Some(authors)), validity)
    }
    /// As `issue_delegable`, granting the subject the author numbers
    /// `authors` for its nodes.
    pub fn issue_terms(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        (delegations, authors): (u8, Option<AuthorRange>),
        validity: (u64, u64, u64),
    ) -> Result<SignedCapability, Error> {
        self.sign_link(
            subject,
            grants,
            Extended {
                authors,
                ..Extended::default()
            },
            delegations,
            validity,
        )
    }
    /// Issuer authority for `subject`, one level below this authority,
    /// with the author numbers `authors` to hand out.
    pub fn issue_issuer(
        &self,
        subject: PublicKey,
        authors: Option<AuthorRange>,
        validity: (u64, u64, u64),
    ) -> Result<SignedCapability, Error> {
        let delegations = self
            .delegations()?
            .checked_sub(1)
            .filter(|delegations| *delegations > 1)
            .ok_or_else(|| denied("issuers nest too deep"))?;
        self.sign_link(
            subject,
            Vec::new(),
            Extended {
                authors,
                issues: true,
                ..Extended::default()
            },
            delegations,
            validity,
        )
    }
    /// The delegations this authority's identity holds.
    fn delegations(&self) -> Result<u8, Error> {
        match self.chain.0.last() {
            None => Ok(MAX_CREDENTIAL_DEPTH as u8),
            Some(last) => Ok(last.capability.delegations),
        }
    }
    /// Signs one link below this authority's identity.
    pub(crate) fn sign_link(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        extended: Extended,
        delegations: u8,
        (time, ttl, skew): (u64, u64, u64),
    ) -> Result<SignedCapability, Error> {
        if delegations == 0 || usize::from(delegations) >= MAX_CREDENTIAL_DEPTH {
            return Err(denied("a user capability allows 1 to 7 delegations"));
        }
        if ttl > MAX_CHAIN_LIFETIME || skew > crate::LIFETIME_SKEW {
            return Err(denied("a credential lives at most 366 days"));
        }
        let (mut not_before, mut expires) = (time.saturating_sub(skew), time.saturating_add(ttl));
        if let Some(parent) = self.chain.0.last() {
            let parent = &parent.capability;
            let held = parent.extended()?;
            // Ranges between issuers are an allocation discipline, as
            // between the owner and its issuers.
            if !held.issues
                || parent.subject != self.identity.public_key()?
                || delegations >= parent.delegations
                || self.chain.0.len() + usize::from(delegations) >= MAX_CREDENTIAL_DEPTH
            {
                return Err(denied("delegation expands authority"));
            }
            not_before = not_before.max(parent.not_before);
            expires = expires.min(parent.expires);
            if not_before >= expires {
                return Err(denied("delegation outlives its parent"));
            }
        }
        let signed = SignedCapability::sign(
            Capability {
                subject,
                kind: PrincipalKind::User,
                grants,
                not_before,
                expires,
                delegations,
                extension: Extension::new(extended)?,
            },
            self.identity,
        )?;
        Ok(signed)
    }
    /// The capability `prior` renewed: the same subject, grants,
    /// delegations, author range and authority, never more. A subject
    /// `revocations` revokes is refused, as is one this authority sits
    /// below: it is renewed by an issuer above it.
    pub fn renew(
        &self,
        prior: &Capability,
        revocations: &RevocationSet,
        validity: (u64, u64, u64),
    ) -> Result<SignedCapability, Error> {
        if prior.kind != PrincipalKind::User || revocations.revokes(&prior.subject) {
            return Err(denied("invalid or revoked credential"));
        }
        if prior.delegations >= self.delegations()? {
            return Err(denied("an issuer above this one renews the credential"));
        }
        let extended = prior.extended()?;
        self.sign_link(
            prior.subject,
            prior.grants.clone(),
            Extended {
                authors: extended.authors,
                partitions: extended.partitions,
                issues: extended.issues,
                ..Extended::default()
            },
            prior.delegations,
            validity,
        )
    }
    /// Signs `statement`, which names this authority's identity as issuer.
    pub fn revoke(&self, statement: Revocation) -> Result<SignedRevocation, Error> {
        SignedRevocation::sign(statement, self.identity, self.chain)
    }
    /// Endorses a domain statement under `label`.
    pub fn endorse<T: serde::Serialize>(
        &self,
        label: &str,
        value: &T,
    ) -> Result<Endorsement, Error> {
        Endorsement::sign(self.identity, self.chain, label, value)
    }
}

/// The authority fields of a link that may assign authors.
fn ranged(authors: Option<AuthorRange>) -> Result<Extension, Error> {
    Extension::new(Extended {
        authors,
        ..Extended::default()
    })
}

/// A node's chain under its identity capability: the identity certifies
/// its node key.
pub fn device_chain(
    root: SignedCapability,
    identity: &dyn IdentityProvider,
    node: PublicKey,
    author: u32,
) -> Result<CredentialChain, Error> {
    CredentialChain(vec![root]).complete(identity, node, author)
}

impl CredentialChain {
    /// Completes a chain that ends at `holder` with a certificate for its
    /// node key: the same grants and validity, delegating nothing further.
    pub fn complete(
        self,
        holder: &dyn IdentityProvider,
        node: PublicKey,
        author: u32,
    ) -> Result<Self, Error> {
        self.certify(holder, node, author)
    }
    /// Completes a node leaf; the signing key must be the node key.
    pub fn complete_with_alias(
        self,
        holder: &dyn IdentityProvider,
        node: PublicKey,
        alias: PublicKey,
        author: u32,
    ) -> Result<Self, Error> {
        if alias != node {
            return Err(denied("the node signs as its own key"));
        }
        self.certify(holder, node, author)
    }
    /// The identity `holder` signs a node certificate for `node` with one
    /// `author` number of its range. It adds no rights: it carries the
    /// identity's grants.
    pub fn certify(
        self,
        holder: &dyn IdentityProvider,
        node: PublicKey,
        author: u32,
    ) -> Result<Self, Error> {
        let last = self.holder(holder)?.clone();
        let extended = last.extended()?;
        self.certify_with(
            holder,
            (node, author),
            (last.grants.clone(), extended.partitions),
        )
    }
    /// As `certify`, with `grants` and `partitions` of the identity's: an
    /// issuer identity's certificate names what its node holds.
    pub fn certify_with(
        mut self,
        holder: &dyn IdentityProvider,
        (node, author): (PublicKey, u32),
        (grants, partitions): (Vec<ResourceGrant>, Vec<PartitionGrant>),
    ) -> Result<Self, Error> {
        let last = self.holder(holder)?;
        let authors = AuthorRange::new(author, author)?;
        if !last
            .extended()?
            .authors
            .is_some_and(|held| held.includes(&authors))
        {
            return Err(denied(
                "node number is outside the principal's range or signer differs from node",
            ));
        }
        let leaf = Capability {
            subject: node,
            kind: PrincipalKind::Node,
            delegations: 0,
            grants,
            extension: Extension::new(Extended {
                partitions,
                node: Some(crate::PeerId::of(node)),
                authors: Some(authors),
                ..Extended::default()
            })?,
            ..last.clone()
        };
        self.0.push(SignedCapability::sign(leaf, holder)?);
        Ok(self)
    }
    /// Delegates from the last link, which `holder` holds, to `subject`:
    /// a user or deployment link with `delegations` fewer than the
    /// holder's, `grants` it holds, validity inside its own and `authors`
    /// inside its range. Widening any of them is refused here and by every
    /// verifier. An issuer grants through its `Authority` instead.
    #[allow(clippy::too_many_arguments)]
    pub fn delegate(
        &self,
        holder: &dyn IdentityProvider,
        subject: PublicKey,
        kind: PrincipalKind,
        grants: Vec<ResourceGrant>,
        delegations: u8,
        authors: Option<AuthorRange>,
        (time, ttl, skew): (u64, u64, u64),
    ) -> Result<Self, Error> {
        let parent = self.holder(holder)?;
        let held = parent.extended()?;
        if held.issues {
            return Err(denied("an issuer grants through its authority"));
        }
        if authors.is_some_and(|range| !held.authors.is_some_and(|held| held.includes(&range))) {
            return Err(denied("delegation expands authority"));
        }
        if !matches!(kind, PrincipalKind::User | PrincipalKind::Deployment)
            || delegations == 0
            || delegations >= parent.delegations
            || self.0.len() + usize::from(delegations) >= crate::MAX_CREDENTIAL_DEPTH
            || (parent.kind == PrincipalKind::Deployment && kind == PrincipalKind::User)
            || grants.is_empty()
            || grants
                .iter()
                .any(|grant| !parent.grants.iter().any(|held| held.includes(grant)))
        {
            return Err(denied("delegation expands authority"));
        }
        let link = Capability {
            subject,
            kind,
            grants,
            not_before: time.saturating_sub(skew).max(parent.not_before),
            expires: time.saturating_add(ttl).min(parent.expires),
            delegations,
            extension: ranged(authors)?,
        };
        if link.not_before >= link.expires {
            return Err(denied("delegation outlives its parent"));
        }
        let mut chain = self.clone();
        chain.0.push(SignedCapability::sign(link, holder)?);
        Ok(chain)
    }
    /// The author range of the last link: what its holder assigns.
    pub fn authors(&self) -> Result<Option<AuthorRange>, Error> {
        self.0
            .last()
            .map_or(Ok(None), |last| last.capability.authors())
    }
    /// The last link, which `holder` must hold and which may delegate.
    fn holder(&self, holder: &dyn IdentityProvider) -> Result<&Capability, Error> {
        let key = holder.public_key()?;
        match self.0.last() {
            Some(last)
                if last.capability.subject == key
                    && last.capability.kind != PrincipalKind::Node
                    && last.capability.delegations > 0 =>
            {
                Ok(&last.capability)
            }
            _ => Err(denied(
                "the chain does not end at a capability this key may delegate",
            )),
        }
    }
    /// The chain without its node leaf: what its holder delegates from.
    pub fn delegable(&self) -> Self {
        let mut prefix = self.clone();
        if prefix
            .0
            .last()
            .is_some_and(|last| last.capability.kind == PrincipalKind::Node)
        {
            prefix.0.pop();
        }
        prefix
    }
    /// The grants of the leaf: what the chain's node may do.
    pub fn grants(&self) -> &[ResourceGrant] {
        self.0
            .last()
            .map_or(&[], |leaf| leaf.capability.grants.as_slice())
    }
}
