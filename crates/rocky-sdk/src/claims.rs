use crate::{
    Failure,
    enrollment::{Settings, now, random},
    registry::{Changes, ClaimRecord, Registry},
    secrets::Vault,
};
use rocky_core::Commitment;
use rocky_credentials::{
    CLAIM_LIFETIME, ClaimRight, CredentialChain, PrivateKey, PublicKey, RenewRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const SKEW: u64 = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimCounters<'a> {
    Every,
    Named(&'a [&'a [u8]]),
}

impl ClaimCounters<'_> {
    pub(crate) fn names(self) -> Option<std::collections::BTreeSet<Vec<u8>>> {
        match self {
            Self::Every => None,
            Self::Named(names) => Some(names.iter().map(|name| name.to_vec()).collect()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimGrant {
    pub identity: String,
    pub counters: Option<Vec<String>>,
    pub grant: String,
    pub issuer: Option<String>,
    pub expires: u64,
}

impl ClaimGrant {
    fn of(
        identity: PublicKey,
        links: &CredentialChain,
        issuer: Option<PublicKey>,
    ) -> Result<Self, Failure> {
        let right = links
            .0
            .last()
            .ok_or("an empty claim chain")?
            .capability
            .extended()?
            .claims
            .ok_or("the chain ends at no claim link")?;
        Ok(Self {
            identity: identity.to_hex(),
            counters: right.counters.map(|names| {
                names
                    .iter()
                    .map(|name| data_encoding::HEXLOWER.encode(name))
                    .collect()
            }),
            expires: links.expires(),
            grant: data_encoding::HEXLOWER.encode(&rocky_credentials::encode(links)?),
            issuer: issuer.map(|node| node.to_hex()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Holding {
    pub links: CredentialChain,
    pub issuer: Option<PublicKey>,
}

impl Settings {
    pub(crate) fn grant_claims(
        &self,
        vault: &Vault,
        identity: PublicKey,
        counters: Option<BTreeSet<Vec<u8>>>,
        delegations: u8,
    ) -> Result<(ClaimGrant, Changes), Failure> {
        if self.tombstoned(&identity) {
            return Err("identity was revoked".into());
        }
        let right = ClaimRight {
            scope: self.domain()?.scope,
            counters,
            grant: random(),
        };
        right.validate()?;
        let time = now()?;
        let node = Self::node_key(vault)?;
        let mut changes = Changes::default();
        let (links, issuer) = if self.issuer() {
            let authority = self.authority(&node)?;
            let link = authority.issue_claims(
                identity,
                right.clone(),
                delegations,
                (time, CLAIM_LIFETIME),
            )?;
            changes.claims(
                &identity,
                &ClaimRecord {
                    right: right.clone(),
                    delegations,
                    issuer: self.identity,
                    granted: time,
                },
            )?;
            let node = PrivateKey::from_bytes(&vault.get("node")?).public_key();
            (authority.chain_of(link), Some(node))
        } else {
            let held = self
                .claims
                .as_ref()
                .ok_or("only an issuer or a holder of a delegable claim right grants one")?;
            let links = held.links.delegate_claims(
                rocky_credentials::Signer::node(&node, &self.credentials)?,
                identity,
                right.clone(),
                delegations,
                (time, CLAIM_LIFETIME),
            )?;
            (links, None)
        };
        Ok((ClaimGrant::of(identity, &links, issuer)?, changes))
    }
    pub(crate) fn held_claims(&self) -> Result<Option<ClaimGrant>, Failure> {
        self.claims
            .as_ref()
            .map(|held| ClaimGrant::of(self.identity, &held.links, held.issuer))
            .transpose()
    }
    pub(crate) fn revoke_claims(
        &self,
        identity: PublicKey,
        registry: &Registry,
    ) -> Result<(Commitment, Changes), Failure> {
        if !self.issuer() {
            return Err("only an issuer revokes claim rights".into());
        }
        let record = registry
            .claims
            .get(&identity)
            .ok_or("no issuer granted that identity a claim right")?;
        let mut changes = Changes::default();
        changes.unclaim(&identity)?;
        Ok((record.right.grant, changes))
    }
    pub(crate) fn renew_claims(
        &self,
        vault: &Vault,
        request: &[u8],
        registry: &Registry,
        time: u64,
    ) -> Result<Option<CredentialChain>, Failure> {
        if !self.issuer() {
            return Ok(None);
        }
        let request = RenewRequest::from_bytes(request)?;
        let me = PrivateKey::from_bytes(&vault.get("node")?).public_key();
        let identity = request.verify(me, self.publisher, (time, SKEW))?.subject;
        let Some(record) = registry.claims.get(&identity) else {
            return Ok(None);
        };
        if self.tombstoned(&identity)
            || self.revocations.revokes_capability(&record.right.grant)
            || record.right.scope != self.domain()?.scope
        {
            return Ok(None);
        }
        let node = Self::node_key(vault)?;
        let authority = self.authority(&node)?;
        let link = authority.issue_claims(
            identity,
            record.right.clone(),
            record.delegations,
            (time, CLAIM_LIFETIME),
        )?;
        Ok(Some(authority.chain_of(link)))
    }
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
    pub(crate) fn claim_chain(&self, _vault: &Vault) -> Result<Option<CredentialChain>, Failure> {
        let Some(held) = &self.claims else {
            return Ok(None);
        };
        if held.links.expires() <= now()? {
            return Ok(None);
        }
        let certificate = self
            .credentials
            .certificate()
            .ok_or("missing node certificate")?
            .clone();
        Ok(Some(held.links.with_certificate(certificate)))
    }
    pub(crate) fn claims_due(&self, time: u64) -> bool {
        !self.issuer()
            && self
                .claims
                .as_ref()
                .is_none_or(|held| held.links.expires() < time.saturating_add(CLAIM_LIFETIME / 2))
    }
}
