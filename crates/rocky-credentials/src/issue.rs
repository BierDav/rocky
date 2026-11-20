use crate::{
    Capability, CredentialChain, Endorsement, Error, Extended, Extension, IdentityProvider,
    MAX_CHAIN_LIFETIME, MAX_CREDENTIAL_DEPTH, PartitionGrant, PeerId, PrivateKey, PublicKey,
    Revocation, RevocationSet, SignedCapability, SignedRevocation, Signer, denied,
};
use rocky_core::{
    AppId, Commitment, FieldSelection, PrincipalKind, ResourceGrant, ResourceScope, Right,
    RowSelection,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub struct Resource {
    pub name: String,
    pub scope: ResourceScope,
    pub columns: BTreeMap<String, Commitment>,
}

pub const MAX_GRANTS: usize = 64;
pub const NEVER: u64 = u64::MAX;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub scope: String,
    #[serde(with = "named_rights")]
    pub rights: BTreeSet<Right>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<BTreeSet<String>>,
}
impl Grant {
    pub fn resolve(&self, resources: &[Resource]) -> Result<ResourceGrant, Error> {
        let resource = resources
            .iter()
            .find(|resource| resource.name == self.scope)
            .ok_or_else(|| denied("a grant names an unknown reader scope"))?;
        self.resolve_resource(resource)
    }
    fn resolve_resource(&self, resource: &Resource) -> Result<ResourceGrant, Error> {
        if self.rights.is_empty() || self.rights.contains(&Right::PublishSchema) {
            return Err(denied(
                "a grant names no right, or one that is not a collection's",
            ));
        }
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
    pub fn resolve_all(
        grants: &[Grant],
        resources: &[Resource],
    ) -> Result<Vec<ResourceGrant>, Error> {
        if grants.is_empty() {
            return Err(denied("a profile holds at least one grant"));
        }
        let by_name: BTreeMap<_, _> = resources
            .iter()
            .map(|resource| (resource.name.as_str(), resource))
            .collect();
        grants
            .iter()
            .map(|grant| {
                let resource = by_name
                    .get(grant.scope.as_str())
                    .ok_or_else(|| denied("a grant names an unknown reader scope"))?;
                grant.resolve_resource(resource)
            })
            .collect()
    }
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", from = "ProfileRecord")]
pub enum Profile {
    Custody { scopes: BTreeSet<String> },
    Reader { scopes: BTreeSet<String> },
    Writer { scopes: BTreeSet<String> },
    Issuer,
    Grants { grants: Vec<Grant> },
}
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
    pub fn issues(&self) -> bool {
        matches!(self, Self::Issuer)
    }
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
    pub fn raw(&self, resources: &[Resource]) -> Result<Vec<Grant>, Error> {
        let (names, rights): (Option<&BTreeSet<String>>, &[Right]) = match self {
            Self::Custody { scopes } => (Some(scopes), &[Right::Custody]),
            Self::Grants { grants } => return Ok(grants.clone()),
            Self::Reader { scopes } => (Some(scopes), &READ),
            Self::Writer { scopes } => (Some(scopes), &WRITE),
            Self::Issuer => (None, &WRITE),
        };
        let known: BTreeSet<_> = resources
            .iter()
            .map(|resource| resource.name.as_str())
            .collect();
        if let Some(names) = names
            && (names.is_empty() || names.iter().any(|name| !known.contains(name.as_str())))
        {
            return Err(denied("grant profile names no or unknown reader scopes"));
        }
        let ciphertext_only = matches!(self, Self::Custody { .. });
        Ok(resources
            .iter()
            .filter(|resource| names.is_none_or(|names| names.contains(&resource.name)))
            .map(|resource| Grant {
                scope: resource.name.clone(),
                rights: rights.iter().copied().collect(),
                columns: ciphertext_only.then(BTreeSet::new),
            })
            .collect())
    }
    pub fn grants(&self, resources: &[Resource]) -> Result<Vec<ResourceGrant>, Error> {
        Grant::resolve_all(&self.raw(resources)?, resources)
    }
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

pub struct Authority<'a> {
    database: Option<crate::DatabaseContext>,
    pub signer: Signer<'a>,
    pub chain: CredentialChain,
}
impl<'a> Authority<'a> {
    pub fn owner(root: &'a dyn IdentityProvider) -> Self {
        Self {
            signer: Signer::Key(root),
            database: None,
            chain: CredentialChain::default(),
        }
    }
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
    pub fn prefix(&self) -> &CredentialChain {
        &self.chain
    }
    pub fn identity(&self) -> Result<PublicKey, Error> {
        self.signer.principal()
    }
    pub fn chain_of(&self, capability: SignedCapability) -> CredentialChain {
        let mut chain = self.chain.clone();
        chain.0.push(capability);
        chain
    }
    pub fn issue(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        validity: crate::Validity,
    ) -> Result<SignedCapability, Error> {
        self.issue_delegable(subject, grants, 1, validity)
    }
    pub fn issue_delegable(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        delegations: u8,
        validity: crate::Validity,
    ) -> Result<SignedCapability, Error> {
        self.issue_terms(
            subject,
            grants,
            crate::DelegationTerms { delegations },
            validity,
        )
    }
    pub fn issue_terms(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        crate::DelegationTerms { delegations }: crate::DelegationTerms,
        validity: crate::Validity,
    ) -> Result<SignedCapability, Error> {
        self.sign_link(
            subject,
            grants,
            Extended {
                origin: self.origin()?,
                ..Extended::default()
            },
            delegations,
            validity,
        )
    }
    pub fn issue_issuer(
        &self,
        subject: PublicKey,
        validity: crate::Validity,
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
                issues: true,
                origin: self.origin()?,
                ..Extended::default()
            },
            delegations,
            validity,
        )
    }
    pub fn issue_trusted(
        &self,
        subject: PublicKey,
        scope: rocky_core::Scope,
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
    pub fn hold(
        &self,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
        validity: crate::Validity,
    ) -> Result<SignedCapability, Error> {
        self.hold_terms(grants, partitions, validity, false)
    }
    pub fn hold_families(
        &self,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
        validity: crate::Validity,
    ) -> Result<SignedCapability, Error> {
        self.hold_terms(grants, partitions, validity, true)
    }
    fn hold_terms(
        &self,
        grants: Vec<ResourceGrant>,
        partitions: Vec<PartitionGrant>,
        validity: crate::Validity,
        families: bool,
    ) -> Result<SignedCapability, Error> {
        let crate::Validity {
            issued_at: time,
            lifetime,
            clock_skew,
        } = validity;
        let ttl = lifetime.as_secs();
        let skew = clock_skew.as_secs();
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
                    partitions,
                    families,
                    origin: self.origin()?,
                    ..Extended::default()
                })?,
            },
            self.signer.clone(),
        )
    }
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
    fn delegations(&self) -> Result<u8, Error> {
        match self.chain.0.last() {
            None => Ok(MAX_CREDENTIAL_DEPTH as u8),
            Some(last) => Ok(last.capability.delegations),
        }
    }
    pub(crate) fn sign_link(
        &self,
        subject: PublicKey,
        grants: Vec<ResourceGrant>,
        mut extended: Extended,
        delegations: u8,
        validity: crate::Validity,
    ) -> Result<SignedCapability, Error> {
        let crate::Validity {
            issued_at: time,
            lifetime,
            clock_skew,
        } = validity;
        let ttl = lifetime.as_secs();
        let skew = clock_skew.as_secs();
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
    pub fn renew(
        &self,
        prior: &Capability,
        revocations: &RevocationSet,
        validity: crate::Validity,
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
                partitions: extended.partitions,
                families: extended.families,
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
    pub fn endorse<T: serde::Serialize>(
        &self,
        label: &str,
        value: &T,
    ) -> Result<Endorsement, Error> {
        Endorsement::sign(self.signer.clone(), &self.chain, label, value)
    }
}

pub fn device_chain(
    root: SignedCapability,
    identity: &dyn IdentityProvider,
    node: &PrivateKey,
) -> Result<CredentialChain, Error> {
    CredentialChain(vec![root]).complete(identity, node)
}

pub(crate) fn node_certificate(
    holder: &dyn IdentityProvider,
    node: &PrivateKey,
    database: crate::DatabaseContext,
    not_before: u64,
    (sequence, previous): (u64, Option<rocky_core::Commitment>),
) -> Result<SignedCapability, Error> {
    let possession = crate::NodePossession::sign(node, holder.public_key()?, database, None)?;
    let leaf = Capability {
        subject: node.public_key(),
        kind: PrincipalKind::Node,
        delegations: 0,
        grants: Vec::new(),
        not_before,
        expires: NEVER,
        extension: Extension::new(Extended {
            node: Some(PeerId::of(node.public_key())),
            sequence: Some(sequence),
            previous,
            database: Some(database),
            possession: Some(possession),
            ..Extended::default()
        })?,
    };
    SignedCapability::sign(leaf, holder)
}

impl CredentialChain {
    pub fn complete(self, holder: &dyn IdentityProvider, node: &PrivateKey) -> Result<Self, Error> {
        self.certify(holder, node)
    }
    pub fn certify(self, holder: &dyn IdentityProvider, node: &PrivateKey) -> Result<Self, Error> {
        self.certify_member(holder, node, 0, None)
    }
    pub fn certify_member(
        mut self,
        holder: &dyn IdentityProvider,
        node: &PrivateKey,
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
        let not_before = self.holder(holder)?.not_before;
        let leaf = node_certificate(holder, node, database, not_before, (sequence, previous))?;
        self.0.push(leaf);
        Ok(self)
    }
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
                mode: crate::RevocationMode::Lost,
                recovery: None,
            },
            holder,
            &self.capabilities().with_certificate(certificate.clone()),
        )
    }
    pub fn delegate<'s>(
        &self,
        holder: impl Into<Signer<'s>>,
        request: crate::Delegation,
    ) -> Result<Self, Error> {
        self.delegate_with_families(holder, request, &crate::GrantFamilies::default())
    }
    pub fn delegate_with_families<'s>(
        &self,
        holder: impl Into<Signer<'s>>,
        request: crate::Delegation,
        families: &crate::GrantFamilies,
    ) -> Result<Self, Error> {
        let crate::Delegation {
            subject,
            kind,
            grants,
            partitions,
            terms,
            validity,
        } = request;
        let crate::DelegationTerms { delegations } = terms;
        let crate::Validity {
            issued_at: time,
            lifetime,
            clock_skew,
        } = validity;
        let ttl = lifetime.as_secs();
        let skew = clock_skew.as_secs();
        let holder = holder.into();
        let parent = self.holder_key(holder.principal()?)?;
        let held = parent.extended()?;
        let parent_families = families.enabled(held.families);
        if held.issues {
            return Err(denied("an issuer grants through its authority"));
        }
        if !matches!(kind, PrincipalKind::User | PrincipalKind::Deployment)
            || delegations == 0
            || delegations >= parent.delegations
            || self.0.len() + usize::from(delegations) >= crate::MAX_CREDENTIAL_DEPTH
            || (parent.kind == PrincipalKind::Deployment && kind == PrincipalKind::User)
            || (grants.is_empty() && partitions.is_empty())
            || families.resources(&grants).iter().any(|grant| {
                !parent_families
                    .resources(&parent.grants)
                    .iter()
                    .any(|held| held.includes(grant))
            })
            || families.partitions(&partitions).iter().any(|grant| {
                !parent_families
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
                partitions,
                origin: held.origin,
                families: families.authenticated(),
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
    pub(crate) fn holder(&self, holder: &dyn IdentityProvider) -> Result<&Capability, Error> {
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
    pub fn held(&self) -> Option<&Capability> {
        let index = match self.certificate() {
            Some(_) => self.0.len().checked_sub(2)?,
            None => self.0.len().checked_sub(1)?,
        };
        Some(&self.0[index].capability)
    }
    pub fn grants(&self) -> &[ResourceGrant] {
        self.held().map_or(&[], |held| held.grants.as_slice())
    }
    pub fn partitions(&self) -> Result<Vec<PartitionGrant>, Error> {
        Ok(match self.held() {
            Some(held) => held.extended()?.partitions,
            None => Vec::new(),
        })
    }
}
