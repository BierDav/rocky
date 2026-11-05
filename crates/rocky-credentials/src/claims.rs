//! Claim rights: short-lived links that let an identity's nodes claim
//! values of a domain's named counters on its coordinator, and nothing else.
//! The coordinator verifies them from the publisher alone; it never holds
//! the domain's data, keys or registry.
use crate::{
    AuthorRange, Authority, CredentialChain, Error, Extended, IdentityProvider, PublicKey,
    RevocationSet, SignedCapability, denied,
};
use crate::{Capability, Extension, PeerId};
use rocky_core::{Commitment, PrincipalKind, Scope};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Longest a claim link lives: a holder renews it with an issuer, so an
/// identity no issuer renews stops claiming within this bound, whether or
/// not its revocation reached the coordinator.
pub const CLAIM_LIFETIME: u64 = crate::DAY;
/// Backdating a claim link may add to its lifetime.
pub const CLAIM_SKEW: u64 = 300;
/// Counters one claim right names.
pub const MAX_CLAIM_COUNTERS: usize = 64;
/// Longest counter name, as the coordinator's.
pub const MAX_CLAIM_COUNTER_NAME: usize = 64;

/// Counters of `scope` whose values the holder may claim and read:
/// every counter, or only `counters`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRight {
    pub scope: Scope,
    /// Exact counter names; every counter of the domain when absent.
    pub counters: Option<BTreeSet<Vec<u8>>>,
    /// Names the grant across renewals: a revocation naming it ends every
    /// link that carries it, whichever issuer signed it. A delegation
    /// carries its own.
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
    /// Whether the right covers `counter` of `scope`.
    pub fn allows(&self, scope: Scope, counter: &[u8]) -> bool {
        self.scope == scope
            && self
                .counters
                .as_ref()
                .is_none_or(|counters| counters.contains(counter))
    }
    /// Whether `other` is within this right, whatever grant it names.
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
    /// A claim link for `subject` holding `right`, valid for `ttl` (at most
    /// `CLAIM_LIFETIME`) from `time`. `delegations` above 1 lets the holder
    /// delegate within it.
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
            (time, ttl, 60),
        )
    }
}

impl CredentialChain {
    /// Delegates from the claim link `holder` holds to `subject` a right
    /// within its own, for at most its remaining validity.
    pub fn delegate_claims(
        &self,
        holder: &dyn IdentityProvider,
        subject: PublicKey,
        right: ClaimRight,
        delegations: u8,
        (time, ttl): (u64, u64),
    ) -> Result<Self, Error> {
        let key = holder.public_key()?;
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
    /// Completes a chain that ends at `holder`'s claim link with a
    /// certificate for its node key. The node's `author` number is its own;
    /// it writes nothing under this chain.
    pub fn certify_claims(
        mut self,
        holder: &dyn IdentityProvider,
        node: PublicKey,
        author: u32,
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
                authors: Some(AuthorRange::new(author, author)?),
                ..Extended::default()
            })?,
            not_before: parent.not_before,
            expires: crate::NEVER,
        };
        self.0.push(SignedCapability::sign(leaf, holder)?);
        Ok(self)
    }
    /// The right of the claim link this chain ends at, held by `holder`:
    /// every link signed down from `publisher`, valid at `time` and ended
    /// by no statement of `revocations`.
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
    /// The identity whose claim link certifies the node `node` to claim
    /// `counter` of `scope` at `time`: every link signed down from
    /// `publisher`, valid and ended by no statement of `revocations`, the
    /// leaf a certificate of `node` under a claim link that covers the
    /// counter.
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
