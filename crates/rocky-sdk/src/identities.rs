//! The identity a database's node belongs to: held in the secret store, or
//! supplied by the application's `IdentityProvider`. The identity signs
//! node certificates and, with issuer authority, what an issuer signs; the
//! owner's identity key is the publisher key. See
//! `docs/runtime/sdk-enrollment.md#identities`.
use crate::{Failure, domain::Domain, enrollment::Settings, secrets::Vault};
use rocky_credentials::{
    AuthorRange, Authority, CredentialChain, IdentityProvider, PrivateKey, PublicKey, Purpose,
    Signature,
};
use std::sync::Arc;

/// Secret-store role of an identity the SDK holds itself.
pub(crate) const ROLE: &str = "identity";

/// An identity whose key is in an installation's secret store. That
/// installation is its only node; nodes of another installation need the
/// application's provider.
pub(crate) struct StoredIdentity(pub Vault);
impl StoredIdentity {
    fn key(&self) -> Result<PrivateKey, rocky_credentials::Error> {
        Ok(PrivateKey::from_bytes(&self.0.get(ROLE)?))
    }
}
impl IdentityProvider for StoredIdentity {
    fn public_key(&self) -> Result<PublicKey, rocky_credentials::Error> {
        Ok(self.key()?.public_key())
    }
    fn sign(&self, _: Purpose, message: &[u8]) -> Result<Signature, rocky_credentials::Error> {
        Ok(self.key()?.sign(message))
    }
}

impl Settings {
    /// This node's identity: the stored one, or the application's provider,
    /// which must supply the key this database was installed under.
    pub(crate) fn identity(&self, vault: &Vault) -> Result<Arc<dyn IdentityProvider>, Failure> {
        let provider: Arc<dyn IdentityProvider> = if self.stored_identity {
            Arc::new(StoredIdentity(vault.clone()))
        } else {
            self.provider.clone().ok_or(
                "this database's identity comes from the application's identity provider; supply it when opening",
            )?
        };
        if provider.public_key()? != self.identity {
            return Err("the identity provider supplies another identity".into());
        }
        Ok(provider)
    }
    /// Whether this node's identity is the owner's: its key is the
    /// publisher key.
    pub fn owner(&self) -> bool {
        self.identity == self.publisher
    }
    /// The issuer links an issuer signs through: none for the owner, else
    /// its chain without the node certificate.
    pub(crate) fn issuer_links(&self) -> CredentialChain {
        if self.owner() {
            CredentialChain::default()
        } else {
            self.credentials.delegable()
        }
    }
    /// The identity's chain to the node certificate: the links it holds
    /// its grants by.
    pub(crate) fn identity_links(&self) -> CredentialChain {
        self.credentials.delegable()
    }
    /// What this issuer signs with for `domain`'s current manifest.
    pub(crate) fn authority<'a>(
        &self,
        identity: &'a dyn IdentityProvider,
        links: &'a CredentialChain,
        domain: &Domain,
    ) -> Result<Authority<'a>, Failure> {
        if !self.issuer() {
            return Err("only an issuer signs for the domain".into());
        }
        Ok(Authority::issuer(identity, links, domain.manifest().hash()))
    }
    /// The numbers of `range`, an identity's, its nodes take: the first for
    /// a stored identity's one node; for a supplied one, which may certify
    /// many, the bottom of the range, at most 256 numbers and half of it.
    /// Splits hand away the top of a range, so they never reach these.
    pub(crate) fn node_numbers(&self, range: AuthorRange) -> AuthorRange {
        // Half the range, rounded down.
        let slice = (range.last - range.first).div_ceil(2).clamp(1, 256);
        let last = if self.stored_identity {
            range.first
        } else {
            range.first + slice - 1
        };
        AuthorRange {
            first: range.first,
            last,
        }
    }
    /// What this node hands to others of `range`: the numbers above its
    /// identity's node numbers, so no grant meets a node of the identity.
    pub(crate) fn minting_of(&self, range: AuthorRange) -> crate::enrollment::Minting {
        crate::enrollment::Minting {
            range,
            next: self.node_numbers(range).last + 1,
        }
    }
    /// Certifies this node under `links`, the identity's chain: the number
    /// it held before or a fresh one of its identity's node numbers, and for
    /// an issuer identity the grants its node holds. The chain is verified
    /// before it returns.
    pub(crate) fn certify(
        &self,
        vault: &Vault,
        links: CredentialChain,
        domain: &Domain,
    ) -> Result<CredentialChain, Failure> {
        let identity = self.identity(vault)?;
        let node = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        let range = links
            .authors()?
            .ok_or("the identity holds no author range")?;
        let numbers = self.node_numbers(range);
        // A node keeps its number; a stored identity's one node takes the
        // first, nodes of a supplied identity reserve theirs from its provider.
        let author = match self.credentials.author_number() {
            Ok(held) if numbers.contains(held) => held,
            _ if self.stored_identity => numbers.first,
            _ => identity.reserve_author(numbers)?,
        };
        let issues = links
            .0
            .last()
            .map(|link| link.capability.extended())
            .transpose()?
            .is_some_and(|extended| extended.issues);
        let chain = if issues {
            let grants = crate::enrollment::grants_of(&rocky_credentials::Profile::Issuer, domain)?;
            let partitions = crate::enrollment::whole_partitions(domain, &grants);
            links.certify_with(
                &*identity,
                (node, author),
                (grants, partitions),
                domain.manifest().hash(),
            )?
        } else {
            links.certify(&*identity, node, author)?
        };
        chain.verify_selectors(
            self.publisher,
            domain.manifest().hash(),
            node,
            &self.revocations,
            crate::enrollment::now()?,
        )?;
        Ok(chain)
    }
}
