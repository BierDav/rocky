//! Counter claim rights: an issuer grants an identity the right to claim
//! the domain's counters on its coordination service without being one of
//! the control's trusted identities. The right is a link that lives at most
//! `rocky_credentials::CLAIM_LIFETIME`; the granting issuer renews it with
//! the holder's credentials until the right is revoked.
use crate::{
    Failure,
    enrollment::{Settings, now},
    secrets::Vault,
};
use rocky_core::Commitment;
use rocky_credentials::{
    CLAIM_LIFETIME, ClaimRight, CredentialChain, PrivateKey, PublicKey, RenewRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Live links an issuer signs one identity before it refuses renewals:
/// what revoking the right names.
const MAX_ISSUED: usize = 8;
/// Request clock difference a renewal allows, as credential renewal does.
const SKEW: u64 = 300;

/// A claim right granted to an identity: `grant` is opaque hex its device
/// installs (`Database::install_claims`); `issuer` is the node that renews
/// it, none for a delegated right, which its delegator grants again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimGrant {
    pub identity: String,
    /// Counter names, as hex; every counter when absent.
    pub counters: Option<Vec<String>>,
    pub grant: String,
    pub issuer: Option<String>,
    pub expires: u64,
}

/// This identity's claim link and the node that renews it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Holding {
    /// From the publisher to the claim link; no node certificate.
    pub links: CredentialChain,
    pub issuer: Option<PublicKey>,
}
/// An issuer's record of a right it granted, which it renews.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Granted {
    pub right: ClaimRight,
    pub delegations: u8,
    /// The live links it signed: their capability IDs and expiries.
    pub issued: Vec<(Commitment, u64)>,
}

impl Settings {
    /// Grants `identity` the right to claim `counters` of this domain, or
    /// every counter: an issuer signs and records it, another holder of a
    /// delegable right delegates within its own. Returns the links the
    /// identity installs and the node that renews them.
    pub(crate) fn grant_claims(
        &mut self,
        vault: &Vault,
        identity: PublicKey,
        counters: Option<BTreeSet<Vec<u8>>>,
        delegations: u8,
    ) -> Result<ClaimGrant, Failure> {
        if self.tombstoned(&identity) {
            return Err("identity was revoked".into());
        }
        let right = ClaimRight {
            scope: self.domain()?.scope,
            counters,
        };
        right.validate()?;
        let time = now()?;
        let signer = self.identity(vault)?;
        let (links, issuer) = if self.issuer() {
            let prefix = self.issuer_links();
            let authority = self.authority(&*signer, &prefix)?;
            let link = authority.issue_claims(
                identity,
                right.clone(),
                delegations,
                (time, CLAIM_LIFETIME),
            )?;
            let issued = (link.id()?, link.capability.expires);
            let links = authority.chain_of(link);
            let records = &mut self.issuing("grants claim rights")?.claim_rights;
            let mut live = records
                .remove(&identity)
                .map(|granted| granted.issued)
                .unwrap_or_default();
            live.retain(|(_, expires)| *expires > time);
            live.push(issued);
            records.insert(
                identity,
                Granted {
                    right: right.clone(),
                    delegations,
                    issued: live,
                },
            );
            let node = PrivateKey::from_bytes(&vault.get("node")?).public_key();
            (links, Some(node))
        } else {
            let held = self
                .claims
                .as_ref()
                .ok_or("only an issuer or a holder of a delegable claim right grants one")?;
            let links = held.links.delegate_claims(
                &*signer,
                identity,
                right.clone(),
                delegations,
                (time, CLAIM_LIFETIME),
            )?;
            (links, None)
        };
        Ok(ClaimGrant {
            identity: identity.to_hex(),
            counters: right.counters.map(|names| {
                names
                    .iter()
                    .map(|name| data_encoding::HEXLOWER.encode(name))
                    .collect()
            }),
            expires: links.expires(),
            grant: data_encoding::HEXLOWER.encode(&rocky_credentials::encode(&links)?),
            issuer: issuer.map(|node| node.to_hex()),
        })
    }
    /// Ends this issuer's renewals of `identity`'s right. Returns the IDs
    /// of the links it signed that are still live, which a revocation names.
    pub(crate) fn revoke_claims(
        &mut self,
        identity: PublicKey,
    ) -> Result<Vec<Commitment>, Failure> {
        let time = now()?;
        let granted = self
            .issuing("revokes claim rights")?
            .claim_rights
            .remove(&identity)
            .ok_or("this issuer granted that identity no claim right")?;
        Ok(granted
            .issued
            .into_iter()
            .filter(|(_, expires)| *expires > time)
            .map(|(id, _)| id)
            .collect())
    }
    /// A fresh claim link for the identity whose node sent the renewal
    /// `request`, when this issuer granted it a right it has not revoked.
    /// Recorded before it leaves, so a revocation names it.
    pub(crate) fn renew_claims(
        &mut self,
        vault: &Vault,
        request: &[u8],
    ) -> Result<Option<CredentialChain>, Failure> {
        if !self.issuer() {
            return Ok(None);
        }
        let request = RenewRequest::from_bytes(request)?;
        let me = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        let time = now()?;
        let identity = request.verify(me, self.publisher, (time, SKEW))?.subject;
        if self.tombstoned(&identity) {
            return Ok(None);
        }
        let Some(granted) = self
            .authority
            .as_ref()
            .and_then(|authority| authority.claim_rights.get(&identity))
        else {
            return Ok(None);
        };
        let mut issued = granted.issued.clone();
        issued.retain(|(_, expires)| *expires > time);
        if issued.len() >= MAX_ISSUED {
            return Ok(None);
        }
        let (right, delegations) = (granted.right.clone(), granted.delegations);
        let signer = self.identity(vault)?;
        let prefix = self.issuer_links();
        let authority = self.authority(&*signer, &prefix)?;
        let link = authority.issue_claims(identity, right, delegations, (time, CLAIM_LIFETIME))?;
        issued.push((link.id()?, link.capability.expires));
        let links = authority.chain_of(link);
        if let Some(granted) = self
            .issuing("renews claim rights")?
            .claim_rights
            .get_mut(&identity)
        {
            granted.issued = issued;
        }
        Ok(Some(links))
    }
    /// Keeps `links` as this identity's claim right, renewed by `issuer`,
    /// once they verify as a current right of this domain.
    pub(crate) fn hold_claims(
        &mut self,
        links: CredentialChain,
        issuer: Option<PublicKey>,
    ) -> Result<(), Failure> {
        let right = links.claim_right(self.publisher, self.identity, &self.revocations, now()?)?;
        if right.scope != self.domain()?.scope {
            return Err("the claim right names another domain".into());
        }
        self.claims = Some(Holding { links, issuer });
        Ok(())
    }
    /// The claim chain this node presents: its certificate under the held
    /// claim link, while that link is current.
    #[cfg(feature = "iroh")]
    pub(crate) fn claim_chain(&self, vault: &Vault) -> Result<Option<CredentialChain>, Failure> {
        let Some(held) = &self.claims else {
            return Ok(None);
        };
        if held.links.expires() <= now()? {
            return Ok(None);
        }
        let node = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        let author = self.credentials.author_number().unwrap_or(1);
        Ok(Some(held.links.clone().certify_claims(
            &*self.identity(vault)?,
            node,
            author,
        )?))
    }
    /// Whether a member should ask its issuer for a claim link: it holds
    /// none, or one past half its lifetime.
    #[cfg(feature = "iroh")]
    pub(crate) fn claims_due(&self, time: u64) -> bool {
        !self.issuer()
            && self
                .claims
                .as_ref()
                .is_none_or(|held| held.links.expires() < time.saturating_add(CLAIM_LIFETIME / 2))
    }
}
