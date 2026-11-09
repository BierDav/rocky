//! Identity signers supplied for one call; installations retain only public credentials.
use crate::{Failure, domain::Domain, enrollment::Settings, secrets::Vault};
use rocky_credentials::{
    AuthorRange, Authority, CredentialChain, IdentityProvider, PrivateKey, SignedCapability,
};
use std::sync::Arc;

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

/// Supplies an identity only while an installation operation is running.
pub(crate) fn with_identity<T>(
    provider: Arc<dyn IdentityProvider>,
    operation: impl FnOnce() -> T,
) -> T {
    SUPPLIED.sync_scope(provider, operation)
}

impl Settings {
    pub(crate) fn identity(&self) -> Result<Arc<dyn IdentityProvider>, Failure> {
        if self.single_use {
            return Err("a single-use identity cannot link or unlink nodes".into());
        }
        self.installing_identity()
    }
    fn installing_identity(&self) -> Result<Arc<dyn IdentityProvider>, Failure> {
        let provider = SUPPLIED
            .try_with(Arc::clone)
            .map_err(|_| "supply the identity for this call")?;
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
    /// Reserves the lower half, at most 256 numbers, for this identity's nodes.
    /// Single-use identities need one; delegation draws from the remainder.
    pub(crate) fn node_numbers(&self, range: AuthorRange) -> AuthorRange {
        // Half the range, rounded down.
        let slice = (range.last - range.first).div_ceil(2).clamp(1, 256);
        let last = if self.single_use {
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
        if links.database_context(self.publisher)? != self.database_context()? {
            return Err("identity capability belongs to another database".into());
        }
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
        // A lost link acknowledgment retries the same certified node.
        if held.is_none()
            && let Some(certificate) = self.revocations.certificates().find(|certificate| {
                Some(certificate.issuer) == identity && certificate.capability.subject == node
            })
        {
            return self.chain_for(&key, links, certificate.clone(), domain);
        }
        let certificate = match held {
            Some(held) => held.clone(),
            None => {
                let identity = self.installing_identity()?;
                let known = &self.revocations;
                let membership = known.membership()?;
                if !membership.can_link(self.identity, |node| {
                    known.identity_unlinks(self.identity, node)
                }) {
                    return Err("identity membership limit reached".into());
                }
                let sequence = membership.next_sequence(self.identity)?;
                let previous = membership.head(self.identity).map(|head| head.certificate);
                let author = range
                    .first
                    .checked_add(u32::try_from(sequence)?)
                    .filter(|author| self.node_numbers(range).contains(*author))
                    .ok_or("the identity's node author range is exhausted")?;
                links
                    .clone()
                    .certify_member(&*identity, node, author, sequence, previous)?
                    .0
                    .pop()
                    .ok_or("missing certificate")?
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
        let chain = if issues {
            let grants = crate::enrollment::grants_of(&rocky_credentials::Profile::Issuer, domain)?;
            let partitions = crate::enrollment::whole_partitions(domain, &grants);
            let held = Authority::issuer(key, &links.with_certificate(certificate.clone()))?.hold(
                domain.families.compact_resources(&grants),
                domain.families.compact_partitions(&partitions),
                self.validity()?,
            )?;
            let mut chain = links;
            chain.0.push(held);
            chain.0.push(certificate);
            chain
        } else {
            links.with_certificate(certificate)
        };
        domain
            .verified(
                &chain,
                self.publisher,
                &self.revocations,
                crate::enrollment::now()?,
            )?
            .selectors(key.public_key(), crate::enrollment::now()?)?;
        Ok(chain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn identity_signers_are_call_scoped_and_single_use_identities_refuse_them()
    -> Result<(), Failure> {
        let directory = rocky_platform::fs::Scratch::new().await;
        let schema = crate::Schema::builder("identity_lifecycle", 1)
            .table(
                crate::Table::replayed("notes")
                    .column(crate::Column::new("body", crate::Affinity::Text)),
            )
            .build()?;
        let store: Arc<dyn crate::SecretStore> = Arc::new(crate::FileSecretStore::new(
            directory.path().join("secrets"),
        ));
        let key = crate::IdentityKey::generate();
        let provider = Arc::new(crate::IdentityKey::from_secret(key.secret()));
        let weak = Arc::downgrade(&provider);
        let mut settings = Settings::create(
            &schema,
            &store,
            crate::enrollment::DEFAULT_TTL,
            Some(provider.clone()),
            &crate::RootKey::generate(),
        )?;
        let vault = settings.vault(&store);
        assert!(settings.identity().is_err());
        for given in [
            json!({"phrase":key.phrase()}),
            json!({"secret":data_encoding::HEXLOWER.encode(&key.secret())}),
        ] {
            let supplied = crate::node::call_identity(&json!({"identity":given}), None)?;
            supplying(supplied, async {
                assert_eq!(settings.identity()?.public_key()?, key.public_key());
                Ok::<_, Failure>(())
            })
            .await?;
            assert!(settings.identity().is_err());
        }
        with_identity(Arc::new(crate::IdentityKey::generate()), || {
            assert!(
                settings
                    .identity()
                    .err()
                    .ok_or("accepted identity")?
                    .to_string()
                    .contains("another identity")
            );
            Ok::<_, Failure>(())
        })?;
        settings.single_use = true;
        with_identity(provider, || {
            assert!(
                settings
                    .identity()
                    .err()
                    .ok_or("accepted identity")?
                    .to_string()
                    .contains("single-use")
            );
            Ok::<_, Failure>(())
        })?;
        assert!(weak.upgrade().is_none());
        assert!(vault.find("identity")?.is_none());
        Ok(())
    }
}
