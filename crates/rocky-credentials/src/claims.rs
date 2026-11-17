use crate::{
    Authority, CredentialChain, Error, Extended, IdentityProvider, PublicKey, RevocationSet,
    SignedCapability, denied,
};
use crate::{Capability, Extension, PeerId};
use rocky_core::{Commitment, PrincipalKind, Scope};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const CLAIM_LIFETIME: u64 = crate::DAY;
pub const CLAIM_SKEW: u64 = 300;
pub const MAX_CLAIM_COUNTERS: usize = 64;
pub const MAX_CLAIM_COUNTER_NAME: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRight {
    pub scope: Scope,
    pub counters: Option<BTreeSet<Vec<u8>>>,
    pub grant: Commitment,
}
impl ClaimRight {
    pub fn validate(&self) -> Result<(), Error> {
        if let Some(counters) = &self.counters
            && (counters.is_empty()
                || counters.len() > MAX_CLAIM_COUNTERS
                || counters
                    .iter()
                    .any(|name| name.is_empty() || name.len() > MAX_CLAIM_COUNTER_NAME))
        {
            return Err(denied(
                "a claim right names 1 to 64 counters of 1 to 64 bytes",
            ));
        }
        Ok(())
    }
    pub fn allows(&self, scope: Scope, counter: &[u8]) -> bool {
        self.scope == scope
            && self
                .counters
                .as_ref()
                .is_none_or(|counters| counters.contains(counter))
    }
    pub fn includes(&self, other: &Self) -> bool {
        self.scope == other.scope
            && match (&self.counters, &other.counters) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(held), Some(asked)) => asked.is_subset(held),
            }
    }
}

impl Authority<'_> {
    pub fn issue_claims(
        &self,
        subject: PublicKey,
        right: ClaimRight,
        delegations: u8,
        (time, ttl): (u64, u64),
    ) -> Result<SignedCapability, Error> {
        self.sign_link(
            subject,
            Vec::new(),
            Extended {
                claims: Some(right),
                ..Extended::default()
            },
            delegations,
            crate::Validity {
                issued_at: time,
                lifetime: std::time::Duration::from_secs(ttl),
                clock_skew: std::time::Duration::from_secs(60),
            },
        )
    }
}

impl CredentialChain {
    pub fn delegate_claims<'s>(
        &self,
        holder: impl Into<crate::Signer<'s>>,
        subject: PublicKey,
        right: ClaimRight,
        delegations: u8,
        (time, ttl): (u64, u64),
    ) -> Result<Self, Error> {
        let holder = holder.into();
        let key = holder.principal()?;
        let parent = match self.0.last() {
            Some(last) if last.capability.subject == key => &last.capability,
            _ => return Err(denied("the chain does not end at the holder's claim link")),
        };
        let held = parent.extended()?;
        if !held.claims.as_ref().is_some_and(|own| own.includes(&right))
            || delegations == 0
            || delegations >= parent.delegations
            || self.0.len() + usize::from(delegations) >= crate::MAX_CREDENTIAL_DEPTH
            || ttl > CLAIM_LIFETIME
        {
            return Err(denied("delegation expands authority"));
        }
        let link = Capability {
            subject,
            kind: PrincipalKind::User,
            grants: Vec::new(),
            not_before: time.saturating_sub(60).max(parent.not_before),
            expires: time.saturating_add(ttl).min(parent.expires),
            delegations,
            extension: Extension::new(Extended {
                claims: Some(right),
                ..Extended::default()
            })?,
        };
        if link.not_before >= link.expires {
            return Err(denied("delegation outlives its parent"));
        }
        let mut chain = self.clone();
        chain.0.push(SignedCapability::sign(link, holder)?);
        chain.links(chain.0[0].issuer, None)?;
        Ok(chain)
    }
    pub fn certify_claims(
        mut self,
        holder: &dyn IdentityProvider,
        node: PublicKey,
    ) -> Result<Self, Error> {
        let key = holder.public_key()?;
        let parent = match self.0.last() {
            Some(last)
                if last.capability.subject == key
                    && last.capability.extended()?.claims.is_some() =>
            {
                last.capability.clone()
            }
            _ => return Err(denied("the chain does not end at the holder's claim link")),
        };
        let leaf = Capability {
            subject: node,
            kind: PrincipalKind::Node,
            delegations: 0,
            grants: Vec::new(),
            extension: Extension::new(Extended {
                node: Some(PeerId::of(node)),
                sequence: Some(0),
                database: Some(self.database_context(self.0[0].issuer)?),
                ..Extended::default()
            })?,
            not_before: parent.not_before,
            expires: crate::NEVER,
        };
        self.0.push(SignedCapability::sign(leaf, holder)?);
        Ok(self)
    }
    pub fn claim_right(
        &self,
        publisher: PublicKey,
        holder: PublicKey,
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<ClaimRight, Error> {
        let leaf = self.links(publisher, Some((revocations, time)))?;
        if leaf.kind != PrincipalKind::User || leaf.subject != holder {
            return Err(denied("the chain ends at another identity's link"));
        }
        leaf.extended()?
            .claims
            .ok_or_else(|| denied("the chain ends at no claim link"))
    }
    pub fn claimant(
        &self,
        publisher: PublicKey,
        node: PublicKey,
        (scope, counter): (Scope, &[u8]),
        revocations: &RevocationSet,
        time: u64,
    ) -> Result<PublicKey, Error> {
        let leaf = self.links(publisher, Some((revocations, time)))?;
        let holder = self
            .0
            .iter()
            .rev()
            .nth(1)
            .map(|link| &link.capability)
            .ok_or_else(|| denied("credential depth"))?;
        if leaf.kind != PrincipalKind::Node
            || leaf.subject != node
            || leaf.delegations != 0
            || !holder
                .extended()?
                .claims
                .is_some_and(|right| right.allows(scope, counter))
        {
            return Err(denied("no claim right for this counter"));
        }
        Ok(holder.subject)
    }
}

#[cfg(test)]
mod tests;
