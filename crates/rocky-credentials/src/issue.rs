//! Issuance: raw grants, the grant profiles built on them and what a
//! holder of the publisher key signs.
use crate::{
    AuthorRange, Capability, CredentialChain, Error, Extended, Extension, PolicyStatement,
    PrivateKey, PublicKey, SealedDevice, SignedCapability, SignedPolicy, denied,
};
use rocky_core::{Commitment, PrincipalKind, ResourceGrant, ResourceScope, Right, RowSelection};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One reader scope an issuer can grant: its name, scope and columns.
#[derive(Clone, Debug)]
pub struct Resource {
    pub name: String,
    pub scope: ResourceScope,
    /// Each column's name and field commitment.
    pub columns: BTreeMap<String, Commitment>,
    /// The command methods whose arguments are sealed for this scope.
    pub invokes: BTreeSet<Commitment>,
    /// The command methods that read or write this scope.
    pub executes: BTreeSet<Commitment>,
}

/// Most grants one capability holds.
pub const MAX_GRANTS: usize = 64;

/// A raw grant: rights over one named reader scope, optionally limited to
/// some of its columns. Any set is issuable, `Execute` and `Custody`
/// included; the profiles below are functions producing these.
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
    /// The signed grant this names over `resources`. `Invoke` and
    /// `Execute` pair the scope's command methods, and are refused on a
    /// scope that has none. `PublishSchema` is no collection right.
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
        let mut fields: BTreeSet<Commitment> = match &self.columns {
            None => resource.columns.values().copied().collect(),
            Some(names) => names
                .iter()
                .map(|name| {
                    resource
                        .columns
                        .get(name)
                        .copied()
                        .ok_or_else(|| denied("a grant names an unknown column"))
                })
                .collect::<Result<_, _>>()?,
        };
        // A method is named in `fields`, paired with its scope.
        for (right, methods) in [
            (Right::Invoke, &resource.invokes),
            (Right::Execute, &resource.executes),
        ] {
            if self.rights.contains(&right) {
                if methods.is_empty() {
                    return Err(denied(
                        "a grant names a command right on a scope no command uses",
                    ));
                }
                fields.extend(methods);
            }
        }
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
        let columns: BTreeSet<String> = resource
            .columns
            .iter()
            .filter(|(_, field)| grant.fields.contains(*field))
            .map(|(name, _)| name.clone())
            .collect();
        let described = Self {
            scope: resource.name.clone(),
            rights: grant.rights.clone(),
            columns: (columns.len() != resource.columns.len()).then_some(columns),
        };
        (described.resolve(std::slice::from_ref(resource)).ok()? == *grant).then_some(described)
    }
}
/// Rights by their lowercase names, as JSON and the call contract spell them.
mod named_rights {
    use rocky_core::Right;
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
    use std::collections::BTreeSet;
    const NAMES: [(Right, &str); 8] = [
        (Right::Read, "read"),
        (Right::Update, "update"),
        (Right::Insert, "insert"),
        (Right::Delete, "delete"),
        (Right::Invoke, "invoke"),
        (Right::Execute, "execute"),
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
/// producing raw grants. A writer of a scope invokes the commands sealed
/// for it; only an issuer executes commands among them. No profile holds
/// `PublishSchema`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", from = "ProfileRecord")]
pub enum Profile {
    /// Holds ciphertext of the named scopes without reader or issuing keys.
    Custody { scopes: BTreeSet<String> },
    /// Reads the named reader scopes and holds their ciphertext.
    Reader { scopes: BTreeSet<String> },
    /// Reads and writes the named reader scopes.
    Writer { scopes: BTreeSet<String> },
    /// A writer of every reader scope that also holds the publisher key:
    /// it enrolls, renews and revokes devices.
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
    /// Whether the profile holds the publisher key.
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
            .map(|resource| {
                let mut rights: BTreeSet<Right> = rights.iter().copied().collect();
                if rights.contains(&Right::Update) && !resource.invokes.is_empty() {
                    rights.insert(Right::Invoke);
                }
                if self.issues() && !resource.executes.is_empty() {
                    rights.insert(Right::Execute);
                }
                Grant {
                    scope: resource.name.clone(),
                    rights,
                    // Custody covers ciphertext only: no column at all.
                    columns: matches!(self, Self::Custody { .. }).then(BTreeSet::new),
                }
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

/// What the publisher key signs for one manifest. Whoever holds the key is
/// an issuer; the key never leaves its `KeyProvider` except sealed to an
/// invited issuer device.
pub struct Authority<'a> {
    pub publisher: &'a PrivateKey,
    pub manifest: Commitment,
}
impl Authority<'_> {
    /// A user capability for `subject` under policy `epoch`, valid from
    /// `skew` seconds before `time` for `ttl` seconds. Its subject
    /// delegates only to its own node key.
    pub fn issue(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        epoch: u64,
        validity: (u64, u64, u64),
    ) -> Result<SignedCapability, Error> {
        self.issue_delegable(subject, grants, 1, epoch, validity)
    }
    /// As `issue`, with `delegations` links allowed below it: 1 reaches
    /// the subject's own node key, 2 one further delegate and its node.
    pub fn issue_delegable(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        delegations: u8,
        epoch: u64,
        validity: (u64, u64, u64),
    ) -> Result<SignedCapability, Error> {
        self.issue_terms(subject, grants, (delegations, None), epoch, validity)
    }
    /// As `issue_delegable`, letting the subject assign the author numbers
    /// `authors` to pseudonyms.
    pub fn issue_terms(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        (delegations, authors): (u8, Option<AuthorRange>),
        epoch: u64,
        (time, ttl, skew): (u64, u64, u64),
    ) -> Result<SignedCapability, Error> {
        if delegations == 0 || usize::from(delegations) >= crate::MAX_CREDENTIAL_DEPTH {
            return Err(denied("a user capability allows 1 to 7 delegations"));
        }
        SignedCapability::sign(
            Capability {
                manifest: self.manifest,
                subject,
                kind: PrincipalKind::User,
                grants,
                not_before: time.saturating_sub(skew),
                expires: time.saturating_add(ttl),
                epoch,
                delegations,
                extension: ranged(authors)?,
            },
            self.publisher,
        )
    }
    /// The capability `prior` renewed: the same subject, grants,
    /// delegations and author range, never more, under `policy`. A revoked
    /// subject is refused.
    pub fn renew(
        &self,
        prior: &Capability,
        policy: &PolicyStatement,
        validity: (u64, u64, u64),
    ) -> Result<SignedCapability, Error> {
        if prior.manifest != self.manifest
            || prior.kind != PrincipalKind::User
            || policy.manifest != self.manifest
            || policy
                .revoked
                .contains(&crate::revoked_subject(&prior.subject))
        {
            return Err(denied("invalid or revoked credential"));
        }
        if !prior.extended()?.partitions.is_empty() {
            return self.issue_partition_terms(
                prior.subject,
                prior.grants.clone(),
                prior.extended()?.partitions,
                (prior.delegations, prior.authors()?),
                policy.epoch,
                validity,
            );
        }
        self.issue_terms(
            prior.subject,
            prior.grants.clone(),
            (prior.delegations, prior.authors()?),
            policy.epoch,
            validity,
        )
    }
    /// The next policy: one epoch later, with `revoked` added. Every chain
    /// issued before it ends.
    pub fn revoke(
        &self,
        current: &PolicyStatement,
        revoked: impl IntoIterator<Item = Commitment>,
    ) -> Result<SignedPolicy, Error> {
        let mut all = current.revoked.clone();
        all.extend(revoked);
        SignedPolicy::sign(
            PolicyStatement {
                manifest: self.manifest,
                epoch: current
                    .epoch
                    .checked_add(1)
                    .ok_or_else(|| denied("invalid policy"))?,
                revoked: all,
            },
            self.publisher,
        )
    }
}

/// The extension of a link that assigns `authors`, if any.
fn ranged(authors: Option<AuthorRange>) -> Result<Option<Extension>, Error> {
    authors
        .map(|authors| {
            Extension::new(Extended {
                partitions: Vec::new(),
                authors: Some(authors),
                device: None,
            })
        })
        .transpose()
}

/// A device's chain under its user capability: the user delegates the
/// same grants to its node key.
pub fn device_chain(
    root: SignedCapability,
    user: &PrivateKey,
    node: PublicKey,
) -> Result<CredentialChain, Error> {
    CredentialChain(vec![root]).complete(user, node)
}

impl CredentialChain {
    /// Completes a chain that ends at `holder` with its node key: the same
    /// grants and validity, delegating nothing further.
    pub fn complete(mut self, holder: &PrivateKey, node: PublicKey) -> Result<Self, Error> {
        let last = self.holder(holder)?;
        let leaf = Capability {
            subject: node,
            kind: PrincipalKind::Node,
            delegations: 0,
            extension: if last.extended()?.partitions.is_empty() {
                None
            } else {
                Some(Extension::new(Extended {
                    partitions: last.extended()?.partitions,
                    ..Extended::default()
                })?)
            },
            ..last.clone()
        };
        self.0.push(SignedCapability::sign(leaf, holder)?);
        Ok(self)
    }
    /// Delegates from the last link, which `holder` holds, to `subject`:
    /// a user or deployment link with `delegations` fewer than the
    /// holder's, `grants` it holds, validity inside its own and `authors`
    /// inside its range. Widening any of them is refused here and by every
    /// verifier.
    #[allow(clippy::too_many_arguments)]
    pub fn delegate(
        &self,
        holder: &PrivateKey,
        subject: PublicKey,
        kind: PrincipalKind,
        grants: Vec<ResourceGrant>,
        delegations: u8,
        authors: Option<AuthorRange>,
        (time, ttl, skew): (u64, u64, u64),
    ) -> Result<Self, Error> {
        let parent = self.holder(holder)?;
        let held = parent.authors()?;
        if authors.is_some_and(|range| !held.is_some_and(|held| held.includes(&range))) {
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
            manifest: parent.manifest,
            subject,
            kind,
            grants,
            not_before: time.saturating_sub(skew).max(parent.not_before),
            expires: time.saturating_add(ttl).min(parent.expires),
            epoch: parent.epoch,
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
    /// Assigns `author` to the pseudonym `key` of `device` under the last
    /// link, which `holder` holds and whose range contains the number. The
    /// pseudonym gets `grants`, all on one table and held by the holder,
    /// and seals the device to the publisher. It expires with the holder
    /// and no later than `until`.
    #[allow(clippy::too_many_arguments)]
    pub fn assign(
        &self,
        holder: &PrivateKey,
        key: PublicKey,
        author: u32,
        device: PublicKey,
        grants: Vec<ResourceGrant>,
        until: u64,
        (time, ttl, skew): (u64, u64, u64),
    ) -> Result<Self, Error> {
        let parent = self.holder(holder)?;
        let publisher = self
            .0
            .first()
            .map(|root| root.issuer)
            .ok_or_else(|| denied("credential depth"))?;
        if !parent
            .authors()?
            .is_some_and(|range| range.contains(author))
        {
            return Err(denied("the author number is outside the assigner's range"));
        }
        if grants.is_empty()
            || grants
                .iter()
                .any(|grant| !parent.grants.iter().any(|held| held.includes(grant)))
        {
            return Err(denied("delegation expands authority"));
        }
        let leaf = Capability {
            manifest: parent.manifest,
            subject: key,
            kind: PrincipalKind::Pseudonym,
            grants,
            not_before: time.saturating_sub(skew).max(parent.not_before),
            expires: time.saturating_add(ttl).min(parent.expires).min(until),
            epoch: parent.epoch,
            delegations: 0,
            extension: Some(Extension::new(Extended {
                partitions: Vec::new(),
                authors: Some(AuthorRange::new(author, author)?),
                device: Some(SealedDevice::seal(publisher, key, author, device)?),
            })?),
        };
        if leaf.not_before >= leaf.expires {
            return Err(denied("delegation outlives its parent"));
        }
        let mut chain = self.clone();
        chain.0.push(SignedCapability::sign(leaf, holder)?);
        Ok(chain)
    }
    /// The author range of the last link: what its holder assigns.
    pub fn authors(&self) -> Result<Option<AuthorRange>, Error> {
        self.0
            .last()
            .map_or(Ok(None), |last| last.capability.authors())
    }
    /// The last link, which `holder` must hold and which may delegate.
    fn holder(&self, holder: &PrivateKey) -> Result<&Capability, Error> {
        match self.0.last() {
            Some(last)
                if last.capability.subject == holder.public_key()
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
