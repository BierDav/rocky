//! The identity a database's node belongs to: held in the secret store,
//! supplied by the application's `IdentityProvider`, or supplied for one
//! call. The identity signs
//! node certificates and, with issuer authority, what an issuer signs. No
//! identity is the root.
use crate::{Failure, domain::Domain, enrollment::Settings, secrets::Vault};
use rocky_credentials::{
    AuthorRange, Authority, CredentialChain, IdentityProvider, PrivateKey, PublicKey, Purpose,
    Signature, SignedCapability,
};
use std::sync::Arc;

/// Secret-store role of an identity the SDK holds itself.
pub(crate) const ROLE: &str = "identity";

tokio::task_local! {
    /// The identity one call supplied, in memory until the call ends.
    static SUPPLIED: Arc<dyn IdentityProvider>;
}

/// Runs `call` with `identity`, when given, as the identity it signs with.
/// Work the call hands to another task does not see it.
pub(crate) async fn supplying<T>(
    identity: Option<Arc<dyn IdentityProvider>>,
    call: impl std::future::Future<Output = T>,
) -> T {
    match identity {
        Some(identity) => SUPPLIED.scope(identity, call).await,
        None => call.await,
    }
}

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
    /// This node's identity: the one the current call supplied, the stored
    /// one, or the application's provider. It must be the key this database
    /// was installed under.
    pub(crate) fn identity(&self, vault: &Vault) -> Result<Arc<dyn IdentityProvider>, Failure> {
        let provider: Arc<dyn IdentityProvider> = if let Ok(supplied) =
            SUPPLIED.try_with(Arc::clone)
        {
            supplied
        } else if self.stored_identity {
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
    /// This node's key: it signs everything routine under its certificate.
    pub(crate) fn node_key(vault: &Vault) -> Result<PrivateKey, Failure> {
        Ok(PrivateKey::from_bytes(&vault.get("node")?))
    }
    /// The issuer links an issuer signs through: its capability chain from
    /// the root's trusted grant, without its node's certificate.
    pub(crate) fn issuer_links(&self) -> CredentialChain {
        self.credentials.capabilities()
    }
    /// The identity's chain as its issuers granted it: what a renewal
    /// replaces and the node certificate names.
    pub(crate) fn identity_links(&self) -> CredentialChain {
        self.credentials.capabilities()
    }
    /// What this issuer's node signs with, in every generation of the
    /// domain; no identity key.
    pub(crate) fn authority<'a>(&self, node: &'a PrivateKey) -> Result<Authority<'a>, Failure> {
        if !self.issuer() {
            return Err("only an issuer signs for the domain".into());
        }
        Ok(Authority::issuer(node, &self.credentials)?)
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
    /// This node's chain under `links`, the identity's capabilities. The
    /// node keeps the certificate it holds while it names this identity
    /// and a number of its range, so a renewal needs no identity key; only
    /// a node without one asks the identity to certify it, with the number
    /// it held before or a fresh one of its identity's node numbers.
    pub(crate) fn certify(
        &self,
        vault: &Vault,
        links: CredentialChain,
        domain: &Domain,
    ) -> Result<CredentialChain, Failure> {
        let key = Self::node_key(vault)?;
        let node = key.public_key();
        let range = links
            .authors()?
            .ok_or("the identity holds no author range")?;
        let identity = links.principal();
        let held = self.credentials.certificate().filter(|held| {
            Some(held.issuer) == identity
                && held.capability.subject == node
                && self
                    .credentials
                    .author_number()
                    .is_ok_and(|author| range.contains(author))
        });
        let certificate = match held {
            Some(held) => held.clone(),
            None => {
                let identity = self.identity(vault)?;
                let numbers = self.node_numbers(range);
                // A stored identity's one node takes the first number,
                // nodes of a supplied identity reserve theirs from its
                // provider.
                let author = match self.credentials.author_number() {
                    Ok(held) if numbers.contains(held) => held,
                    _ if self.stored_identity => numbers.first,
                    _ => identity.reserve_author(numbers)?,
                };
                certificate(&*identity, &links, node, author)?
            }
        };
        self.chain_for(&key, links, certificate, domain)
    }
    /// `links` completed with `certificate`, the certificate of `key`. An
    /// issuer identity's links name authority, not grants, so `key` signs
    /// the grants its identity holds over `domain` between them: no identity
    /// key. The chain is verified before it returns.
    pub(crate) fn chain_for(
        &self,
        key: &PrivateKey,
        links: CredentialChain,
        certificate: SignedCapability,
        domain: &Domain,
    ) -> Result<CredentialChain, Failure> {
        let issues = links
            .0
            .last()
            .map(|link| link.capability.extended())
            .transpose()?
            .is_some_and(|extended| extended.issues);
        let chain =
            if issues {
                let grants =
                    crate::enrollment::grants_of(&rocky_credentials::Profile::Issuer, domain)?;
                let partitions = crate::enrollment::whole_partitions(domain, &grants);
                let held = Authority::issuer(key, &links.with_certificate(certificate.clone()))?
                    .hold(grants, partitions, self.validity()?)?;
                let mut chain = links;
                chain.0.push(held);
                chain.0.push(certificate);
                chain
            } else {
                links.with_certificate(certificate)
            };
        chain.verify_selectors(
            self.publisher,
            key.public_key(),
            &self.revocations,
            crate::enrollment::now()?,
        )?;
        Ok(chain)
    }
}

/// The certificate `identity` signs for `node` with `author` under `links`.
pub(crate) fn certificate(
    identity: &dyn IdentityProvider,
    links: &CredentialChain,
    node: PublicKey,
    author: u32,
) -> Result<SignedCapability, Failure> {
    links
        .clone()
        .certify(identity, node, author)?
        .0
        .pop()
        .ok_or_else(|| "no node certificate".into())
}
