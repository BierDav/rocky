use crate::{
    Capability, CredentialChain, Endorsement, Error, Extended, Extension, IdentityProvider,
    MAX_CHAIN_LIFETIME, MAX_CREDENTIAL_DEPTH, PartitionGrant, PeerId, PrivateKey, PublicKey,
    Revocation, RevocationSet, SignedCapability, SignedRevocation, Signer, denied,
};
use rocky_core::{AppId, Commitment, PrincipalKind, ResourceGrant};
use std::collections::BTreeSet;

pub const NEVER: u64 = u64::MAX;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Granted {
    pub grants: Vec<ResourceGrant>,
    pub partitions: Vec<PartitionGrant>,
    pub whole: Option<crate::WholeAccess>,
    pub role: Option<Commitment>,
}
impl Granted {
    pub fn explicit(grants: Vec<ResourceGrant>) -> Self {
        Self {
            grants,
            ..Self::default()
        }
    }
    fn extended(self, origin: Option<crate::Origin>) -> (Vec<ResourceGrant>, Extended) {
        (
            self.grants,
            Extended {
                partitions: self.partitions,
                whole: self.whole,
                role: self.role,
                origin,
                ..Extended::default()
            },
        )
    }
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
        self.issue_person(subject, Granted::explicit(grants), validity)
    }
    pub fn issue_person(
        &self,
        subject: PublicKey,
        granted: Granted,
        validity: crate::Validity,
    ) -> Result<SignedCapability, Error> {
        let (grants, extended) = granted.extended(self.origin()?);
        self.sign_link(subject, grants, extended, 1, validity)
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
        granted: Granted,
        validity: crate::Validity,
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
            return Err(denied("a held link outlives its parent"));
        }
        let (grants, extension) = granted.extended(self.origin()?);
        SignedCapability::sign(
            Capability {
                subject: self.identity()?,
                kind: PrincipalKind::User,
                grants,
                not_before,
                expires,
                delegations: 1,
                extension: Extension::new(extension)?,
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
    pub(crate) fn delegations(&self) -> u8 {
        match self.chain.0.last() {
            None => MAX_CREDENTIAL_DEPTH as u8,
            Some(last) => last.capability.delegations,
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
                    .or_else(|| extended.partitions.first().map(|grant| grant.domain));
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
        let (mut not_before, mut expires) = (time.saturating_sub(skew), time.saturating_add(ttl));
        if let Some(parent) = self.chain.0.last() {
            let parent = &parent.capability;
            let held = parent.extended()?;
            if !held.issues
                || parent.subject != self.identity()?
                || delegations >= parent.delegations
                || self.chain.0.len() + usize::from(delegations) >= MAX_CREDENTIAL_DEPTH
            {
                return Err(denied("a link expands its issuer's authority"));
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
                .is_some_and(|origin| revocations.revokes_origin(&origin.identity))
        {
            return Err(denied("invalid or revoked credential"));
        }
        if prior.delegations >= self.delegations() {
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
                whole: extended.whole,
                role: extended.role,
                issues: extended.issues,
                trusted: None,
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
    let possession = crate::NodePossession::sign(node, holder.public_key()?, database)?;
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
            },
            holder,
            &self.capabilities().with_certificate(certificate.clone()),
        )
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
                "the chain does not end at a capability this key certifies nodes for",
            )),
        }
    }
    pub fn without_certificate(&self) -> Self {
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
