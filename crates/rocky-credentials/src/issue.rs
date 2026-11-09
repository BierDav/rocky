//! Issuance: raw grants, the grant profiles built on them and what an
//! identity with issuer authority signs.
use crate::{
    AuthorRange, Capability, CredentialChain, Endorsement, Error, Extended, Extension,
    IdentityProvider, MAX_CHAIN_LIFETIME, MAX_CREDENTIAL_DEPTH, PartitionGrant, PeerId, PrivateKey,
    PublicKey, Revocation, RevocationSet, SignedCapability, SignedRevocation, Signer, denied,
};
use rocky_core::{
    AppId, Commitment, FieldSelection, PrincipalKind, ResourceGrant, ResourceScope, Right,
    RowSelection,
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
/// The expiry of a root-signed trusted grant: none.
pub const NEVER: u64 = u64::MAX;

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
        if grants.is_empty() {
            return Err(denied("a profile holds at least one grant"));
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

/// What an identity with issuer authority signs: the root's, whose key is
/// the publisher key, or an issuer's, signed by one of its nodes through
/// the identity's chain of issuer links from the root's trusted grant. No
/// issuer identity key signs here. What it signs holds in every generation
/// of the domain.
pub struct Authority<'a> {
    database: Option<crate::DatabaseContext>,
    pub signer: Signer<'a>,
    /// The issuer links from the publisher to the signing identity; empty
    /// for the owner.
    pub chain: CredentialChain,
}
impl<'a> Authority<'a> {
    /// The root: its key is the publisher key. It is offline and signs only
    /// the domain's genesis, trusted grants and their revocations.
    pub fn owner(root: &'a dyn IdentityProvider) -> Self {
        Self {
            signer: Signer::Key(root),
            database: None,
            chain: CredentialChain::default(),
        }
    }
    /// A root issuing authority within one database namespace.
    pub fn owner_in(
        root: &'a dyn IdentityProvider,
        database: crate::DatabaseContext,
    ) -> Result<Self, Error> {
        if root.public_key()? != database.publisher {
            return Err(denied("database publisher mismatch"));
        }
        Ok(Self {
            database: Some(database),
            ..Self::owner(root)
        })
    }
    /// An issuer's node `node`, whose chain `credentials` ends at its
    /// certificate under the identity's issuer links.
    pub fn issuer(node: &'a PrivateKey, credentials: &CredentialChain) -> Result<Self, Error> {
        let database = credentials.database_context(
            credentials
                .0
                .first()
                .ok_or_else(|| denied("missing authority"))?
                .issuer,
        )?;
        credentials.verify_database(database)?;
        Ok(Self {
            signer: Signer::node(node, credentials)?,
            database: Some(database),
            chain: credentials.capabilities(),
        })
    }
    /// The links a capability this authority signs follows.
    pub fn prefix(&self) -> &CredentialChain {
        &self.chain
    }
    /// The identity this authority signs for.
    pub fn identity(&self) -> Result<PublicKey, Error> {
        self.signer.principal()
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
                origin: self.origin()?,
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
                origin: self.origin()?,
                ..Extended::default()
            },
            delegations,
            validity,
        )
    }
    /// The root's trusted grant: whole-domain tip and bookmark control
    /// with issuer authority over `authors`, valid from `time` and never
    /// expiring; only a root-signed revocation ends it. Only the root signs
    /// one, and no trusted grant is delegated.
    pub fn issue_trusted(
        &self,
        subject: PublicKey,
        scope: rocky_core::Scope,
        authors: AuthorRange,
        time: u64,
    ) -> Result<SignedCapability, Error> {
        if !self.chain.0.is_empty() {
            return Err(denied("only the root grants trusted identity authority"));
        }
        SignedCapability::sign(
            Capability {
                subject,
                kind: PrincipalKind::User,
                grants: Vec::new(),
                not_before: time.saturating_sub(crate::LIFETIME_SKEW),
                expires: NEVER,
                delegations: (MAX_CREDENTIAL_DEPTH - 1) as u8,
                extension: Extension::new(Extended {
                    authors: Some(authors),
                    issues: true,
                    trusted: Some(scope),
                    database: Some(crate::DatabaseContext {
                        publisher: self.identity()?,
                        scope,
                    }),
                    ..Extended::default()
                })?,
            },
            self.signer.clone(),
        )
    }
    /// The grants and partitions this authority's identity holds itself,
    /// signed by its node: an issuer identity's link names authority, not
    /// grants, so its nodes read and write by this one. Under a trusted
    /// grant it never expires; otherwise it lives inside its parent.
    pub fn hold(
        &self,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
        (time, ttl, skew): (u64, u64, u64),
    ) -> Result<SignedCapability, Error> {
        let parent = &self
            .chain
            .0
            .last()
            .ok_or_else(|| denied("the root holds no grants"))?
            .capability;
        let held = parent.extended()?;
        if !held.issues || self.signer.signing_node().is_none() || parent.delegations < 2 {
            return Err(denied(
                "only an issuer's node signs what its identity holds",
            ));
        }
        let not_before = time.saturating_sub(skew).max(parent.not_before);
        let expires = match parent.expires {
            NEVER => NEVER,
            expires => time
                .saturating_add(ttl.min(MAX_CHAIN_LIFETIME))
                .min(expires),
        };
        if not_before >= expires {
            return Err(denied("delegation outlives its parent"));
        }
        SignedCapability::sign(
            Capability {
                subject: self.identity()?,
                kind: PrincipalKind::User,
                grants,
                not_before,
                expires,
                delegations: 1,
                extension: Extension::new(Extended {
                    authors: held.authors,
                    partitions,
                    origin: self.origin()?,
                    ..Extended::default()
                })?,
            },
            self.signer.clone(),
        )
    }
    /// The trusted identity a link this authority signs descends from: its
    /// own when it holds a trusted grant, else the one its links name.
    /// The root's links descend from none.
    pub fn origin(&self) -> Result<Option<crate::Origin>, Error> {
        let Some(last) = self.chain.0.last() else {
            return Ok(None);
        };
        let extended = last.capability.extended()?;
        Ok(match extended.trusted {
            Some(_) => Some(crate::Origin {
                identity: last.capability.subject,
                since: last.capability.not_before,
            }),
            None => extended.origin,
        })
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
        mut extended: Extended,
        delegations: u8,
        (time, ttl, skew): (u64, u64, u64),
    ) -> Result<SignedCapability, Error> {
        let database = self
            .database
            .or(extended.database)
            .or_else(|| {
                let scope = grants
                    .first()
                    .map(|grant| grant.scope.domain)
                    .or_else(|| extended.partitions.first().map(|grant| grant.domain))
                    .or_else(|| extended.claims.as_ref().map(|claims| claims.scope));
                scope.and_then(|scope| {
                    self.signer
                        .principal()
                        .ok()
                        .map(|publisher| crate::DatabaseContext { publisher, scope })
                })
            })
            .ok_or_else(|| denied("an issuer needs an explicit database context"))?;
        if grants
            .iter()
            .any(|grant| grant.scope.domain != database.scope)
            || extended
                .partitions
                .iter()
                .any(|grant| grant.domain != database.scope)
            || extended
                .claims
                .as_ref()
                .is_some_and(|claims| claims.scope != database.scope)
        {
            return Err(denied("a grant belongs to another database"));
        }
        extended.database = Some(database);
        if delegations == 0 || usize::from(delegations) >= MAX_CREDENTIAL_DEPTH {
            return Err(denied("a user capability allows 1 to 7 delegations"));
        }
        if ttl > MAX_CHAIN_LIFETIME || skew > crate::LIFETIME_SKEW {
            return Err(denied("a credential lives at most 366 days"));
        }
        if extended.claims.is_some()
            && (ttl > crate::CLAIM_LIFETIME || skew > crate::CLAIM_SKEW || !grants.is_empty())
        {
            return Err(denied(
                "a claim link holds no grants and lives at most a day",
            ));
        }
        let (mut not_before, mut expires) = (time.saturating_sub(skew), time.saturating_add(ttl));
        if let Some(parent) = self.chain.0.last() {
            let parent = &parent.capability;
            let held = parent.extended()?;
            // Ranges between issuers are an allocation discipline, as
            // between the owner and its issuers.
            if !held.issues
                || parent.subject != self.identity()?
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
            self.signer.clone(),
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
        let extended = prior.extended()?;
        if prior.kind != PrincipalKind::User
            || revocations.revokes(&prior.subject)
            || extended
                .origin
                .is_some_and(|origin| revocations.ends_origin(&origin))
        {
            return Err(denied("invalid or revoked credential"));
        }
        if prior.delegations >= self.delegations()? {
            return Err(denied("an issuer above this one renews the credential"));
        }
        if extended.trusted.is_some() {
            return Err(denied("a trusted grant never expires and is never renewed"));
        }
        self.sign_link(
            prior.subject,
            prior.grants.clone(),
            Extended {
                authors: extended.authors,
                partitions: extended.partitions,
                issues: extended.issues,
                trusted: None,
                claims: extended.claims,
                origin: extended.origin,
                ..Extended::default()
            },
            prior.delegations,
            validity,
        )
    }
    /// Signs `statement`, which names this authority's identity as issuer.
    pub fn revoke(&self, statement: Revocation) -> Result<SignedRevocation, Error> {
        SignedRevocation::sign(statement, self.signer.clone(), &self.chain)
    }
    pub fn revoke_in(
        &self,
        statement: Revocation,
        database: crate::DatabaseContext,
    ) -> Result<SignedRevocation, Error> {
        SignedRevocation::sign_in(statement, self.signer.clone(), &self.chain, database)
    }
    /// Endorses a domain statement under `label`.
    pub fn endorse<T: serde::Serialize>(
        &self,
        label: &str,
        value: &T,
    ) -> Result<Endorsement, Error> {
        Endorsement::sign(self.signer.clone(), &self.chain, label, value)
    }
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
    /// The identity `holder` signs a certificate for `node` with one
    /// `author` number of its range. It holds no grants and never expires:
    /// the node acts with whatever its identity's link grants, while it
    /// does. A renewal of that link needs no new certificate.
    pub fn certify(
        self,
        holder: &dyn IdentityProvider,
        node: PublicKey,
        author: u32,
    ) -> Result<Self, Error> {
        self.certify_member(holder, node, author, 0, None)
    }
    /// Signs a node certificate at an explicit identity membership sequence.
    /// Sequence zero has no predecessor; later links must name the previous
    /// certificate ID so concurrent links can be detected as a fork.
    pub fn certify_member(
        mut self,
        holder: &dyn IdentityProvider,
        node: PublicKey,
        author: u32,
        sequence: u64,
        previous: Option<rocky_core::Commitment>,
    ) -> Result<Self, Error> {
        if (sequence == 0) != previous.is_none() {
            return Err(denied("invalid membership sequence"));
        }
        let database = self.database_context(
            self.0
                .first()
                .ok_or_else(|| denied("missing authority"))?
                .issuer,
        )?;
        let last = self.holder(holder)?;
        let authors = AuthorRange::new(author, author)?;
        if !last.extended()?.authors.is_some_and(|held| {
            held.includes(&authors)
                && u32::try_from(sequence)
                    .ok()
                    .and_then(|sequence| held.first.checked_add(sequence))
                    == Some(author)
        }) {
            return Err(denied(
                "node number is outside the principal's range or signer differs from node",
            ));
        }
        let leaf = Capability {
            subject: node,
            kind: PrincipalKind::Node,
            delegations: 0,
            grants: Vec::new(),
            not_before: last.not_before,
            expires: NEVER,
            extension: Extension::new(Extended {
                node: Some(crate::PeerId::of(node)),
                authors: Some(authors),
                sequence: Some(sequence),
                previous,
                database: Some(database),
                ..Extended::default()
            })?,
        };
        self.0.push(SignedCapability::sign(leaf, holder)?);
        Ok(self)
    }
    /// Signs an identity-owned unlink for one of its node certificates.
    pub fn unlink(
        &self,
        holder: &dyn IdentityProvider,
        app: AppId,
        time: u64,
        node: PublicKey,
    ) -> Result<SignedRevocation, Error> {
        let identity = holder.public_key()?;
        let certificate = self
            .certificate()
            .ok_or_else(|| denied("unlink requires the node's certificate"))?;
        if certificate.issuer != identity || certificate.capability.subject != node {
            return Err(denied("the unlink signer does not own this node"));
        }
        SignedRevocation::sign(
            Revocation {
                app,
                issuer: identity,
                time,
                nodes: [PeerId::of(node)].into(),
                identities: BTreeSet::new(),
                capabilities: BTreeSet::new(),
                cut: Vec::new(),
                recovery: None,
            },
            holder,
            &self.capabilities().with_certificate(certificate.clone()),
        )
    }
    /// Delegates from the last link, which `holder` holds, to `subject`:
    /// a user or deployment link with `delegations` fewer than the
    /// holder's, `grants` it holds, validity inside its own and `authors`
    /// inside its range. Widening any of them is refused here and by every
    /// verifier. An issuer grants through its `Authority` instead.
    #[allow(clippy::too_many_arguments)]
    pub fn delegate<'s>(
        &self,
        holder: impl Into<Signer<'s>>,
        subject: PublicKey,
        kind: PrincipalKind,
        grants: Vec<ResourceGrant>,
        delegations: u8,
        authors: Option<AuthorRange>,
        (time, ttl, skew): (u64, u64, u64),
    ) -> Result<Self, Error> {
        self.delegate_with_families(
            holder,
            subject,
            kind,
            grants,
            Vec::new(),
            (delegations, authors),
            (time, ttl, skew),
            &crate::GrantFamilies::default(),
        )
    }
    /// Delegates resolved authority, including selector grants, under an
    /// authenticated schema. Both issuance and receipt check attenuation.
    #[allow(clippy::too_many_arguments)]
    pub fn delegate_with_families<'s>(
        &self,
        holder: impl Into<Signer<'s>>,
        subject: PublicKey,
        kind: PrincipalKind,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
        (delegations, authors): (u8, Option<AuthorRange>),
        (time, ttl, skew): (u64, u64, u64),
        families: &crate::GrantFamilies,
    ) -> Result<Self, Error> {
        let holder = holder.into();
        let parent = self.holder_key(holder.principal()?)?;
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
            || (grants.is_empty() && partitions.is_empty())
            || families.resources(&grants).iter().any(|grant| {
                !families
                    .resources(&parent.grants)
                    .iter()
                    .any(|held| held.includes(grant))
            })
            || families.partitions(&partitions).iter().any(|grant| {
                !families
                    .partitions(&held.partitions)
                    .iter()
                    .any(|held| held.includes(grant))
            })
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
            extension: Extension::new(Extended {
                authors,
                partitions,
                origin: held.origin,
                ..Extended::default()
            })?,
        };
        if link.not_before >= link.expires {
            return Err(denied("delegation outlives its parent"));
        }
        let mut chain = self.clone();
        chain.0.push(SignedCapability::sign(link, holder)?);
        chain.links_with_families(chain.0[0].issuer, None, families)?;
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
        self.holder_key(holder.public_key()?)
    }
    fn holder_key(&self, key: PublicKey) -> Result<&Capability, Error> {
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
    /// The link a node certificate leaf names, or the leaf itself: what
    /// the chain grants.
    pub fn held(&self) -> Option<&Capability> {
        let index = match self.certificate() {
            Some(_) => self.0.len().checked_sub(2)?,
            None => self.0.len().checked_sub(1)?,
        };
        Some(&self.0[index].capability)
    }
    /// The grants of the identity link: what the chain's node may do.
    pub fn grants(&self) -> &[ResourceGrant] {
        self.held().map_or(&[], |held| held.grants.as_slice())
    }
    /// The partition grants of the identity link.
    pub fn partitions(&self) -> Result<Vec<PartitionGrant>, Error> {
        Ok(match self.held() {
            Some(held) => held.extended()?.partitions,
            None => Vec::new(),
        })
    }
}
