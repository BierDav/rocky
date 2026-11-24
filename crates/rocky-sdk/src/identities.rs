use crate::{Failure, domain::Domain, enrollment::Settings, secrets::Vault};
use rocky_credentials::{
    Authority, CredentialChain, IdentityProvider, PrivateKey, SignedCapability,
};
use std::sync::Arc;

tokio::task_local! {
    static SUPPLIED: Loan;
}

pub(crate) async fn supplying<T>(
    identity: Option<Arc<dyn IdentityProvider>>,
    call: impl std::future::Future<Output = T>,
) -> T {
    match identity {
        Some(identity) => Loan::untracked(identity).supply(call).await,
        None => call.await,
    }
}

pub(crate) struct Loan {
    provider: Option<Arc<dyn IdentityProvider>>,
    returned: Option<tokio::sync::oneshot::Sender<()>>,
}

pub(crate) struct Return(tokio::sync::oneshot::Receiver<()>);

pub(crate) fn lend(provider: Arc<dyn IdentityProvider>) -> (Loan, Return) {
    let (returned, released) = tokio::sync::oneshot::channel();
    (
        Loan {
            provider: Some(provider),
            returned: Some(returned),
        },
        Return(released),
    )
}

impl Loan {
    pub(crate) fn untracked(provider: Arc<dyn IdentityProvider>) -> Self {
        Self {
            provider: Some(provider),
            returned: None,
        }
    }
    pub(crate) fn signer(&self) -> Option<&dyn IdentityProvider> {
        self.provider.as_deref()
    }
    pub(crate) async fn supply<T>(self, call: impl std::future::Future<Output = T>) -> T {
        SUPPLIED.scope(self, call).await
    }
}

impl Drop for Loan {
    fn drop(&mut self) {
        drop(self.provider.take());
        if let Some(returned) = self.returned.take() {
            let _ = returned.send(());
        }
    }
}

impl Return {
    pub(crate) async fn released(self) {
        let _ = self.0.await;
    }
}

pub(crate) fn with_identity<T>(
    provider: Arc<dyn IdentityProvider>,
    operation: impl FnOnce() -> T,
) -> T {
    SUPPLIED.sync_scope(Loan::untracked(provider), operation)
}

impl Settings {
    pub(crate) fn identity(&self) -> Result<Arc<dyn IdentityProvider>, Failure> {
        self.installing_identity()
    }
    fn installing_identity(&self) -> Result<Arc<dyn IdentityProvider>, Failure> {
        let provider = SUPPLIED
            .try_with(|loan| loan.provider.clone())
            .ok()
            .flatten()
            .ok_or(crate::LinkError::IdentityRequired)?;
        if provider.public_key()? != self.identity {
            return Err("the identity provider supplies another identity".into());
        }
        Ok(provider)
    }
    pub(crate) fn node_key(vault: &Vault) -> Result<PrivateKey, Failure> {
        Ok(PrivateKey::from_bytes(&vault.get("node")?))
    }
    pub(crate) fn issuer_links(&self) -> CredentialChain {
        self.credentials.capabilities()
    }
    pub(crate) fn identity_links(&self) -> CredentialChain {
        self.credentials.capabilities()
    }
    pub(crate) fn authority<'a>(&self, node: &'a PrivateKey) -> Result<Authority<'a>, Failure> {
        if !self.issuer() {
            return Err("only an issuer signs for the domain".into());
        }
        Ok(Authority::issuer(node, &self.credentials)?)
    }
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
        let identity = links.principal();
        let held = self
            .credentials
            .certificate()
            .filter(|held| Some(held.issuer) == identity && held.capability.subject == node);
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
                links
                    .clone()
                    .certify_member(&*identity, &key, sequence, previous)?
                    .0
                    .pop()
                    .ok_or("missing certificate")?
            }
        };
        self.chain_for(&key, links, certificate, domain)
    }
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
            let certified = links.with_certificate(certificate.clone());
            let authority = Authority::issuer(key, &certified)?;
            let held = authority.hold(domain.administer(), self.validity()?)?;
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
            .selectors(
                key.public_key(),
                &self.revocations,
                crate::enrollment::now()?,
            )?;
        Ok(chain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn identity_signers_are_call_scoped_and_never_stored() -> Result<(), Failure> {
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
        let settings = Settings::create(
            &schema,
            &store,
            crate::enrollment::DEFAULT_TTL,
            provider.clone(),
            &crate::RootKey::generate(),
        )?;
        let vault = settings.vault(&store);
        assert!(settings.identity().is_err());
        for supplied in [
            crate::IdentityKey::from_phrase(&key.phrase())?,
            crate::IdentityKey::from_secret(key.secret()),
        ] {
            let supplied: Option<Arc<dyn crate::IdentityProvider>> = Some(Arc::new(supplied));
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
        drop(provider);
        assert!(weak.upgrade().is_none());
        assert!(vault.find("identity")?.is_none());
        Ok(())
    }
}
