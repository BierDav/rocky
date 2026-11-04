//! Counter claim rights through the call contract, and the claim chain this
//! node presents to its coordination service. See `crate::claims`.
use super::*;
use crate::claims::ClaimGrant;
use std::collections::BTreeSet;

/// Seconds between a member's renewals for a claim link it lacks or that
/// passed half its lifetime.
#[cfg(feature = "iroh")]
const ASK_INTERVAL: u64 = 60;
/// Seconds before a failed revocation push is retried, doubling per
/// failure up to `PUSH_RETRY_MAX`. A newly learned statement retries at once.
#[cfg(feature = "iroh")]
const PUSH_RETRY: u64 = 5;
#[cfg(feature = "iroh")]
const PUSH_RETRY_MAX: u64 = 300;

/// When this process next pushes a revocation set the coordination service
/// refused or did not answer: kept in memory, so a restart pushes at once.
#[cfg(feature = "iroh")]
#[derive(Default)]
pub(super) struct PushRetry {
    digest: Option<rocky_core::Commitment>,
    failures: u32,
    next: u64,
}
#[cfg(feature = "iroh")]
impl PushRetry {
    fn due(&self, digest: rocky_core::Commitment, time: u64) -> bool {
        self.digest != Some(digest) || self.next <= time
    }
    fn failed(&mut self, digest: rocky_core::Commitment, time: u64) -> u64 {
        if self.digest != Some(digest) {
            *self = Self {
                digest: Some(digest),
                ..Self::default()
            };
        }
        let delay = PUSH_RETRY
            .saturating_mul(1 << self.failures.min(6))
            .min(PUSH_RETRY_MAX);
        self.failures = self.failures.saturating_add(1);
        self.next = time.saturating_add(delay);
        self.next
    }
}

impl Node {
    /// `grant` a claim right to an identity, `install` one granted to this
    /// identity, read the `held` one, or `revoke` one an issuer granted.
    pub(super) async fn claims(&self, request: &Value) -> Result<Value, Failure> {
        let identity = || -> Result<PublicKey, Failure> {
            request["identity"]
                .as_str()
                .and_then(|key| key.parse().ok())
                .ok_or_else(|| refused("identity must be a hex key"))
        };
        match request["action"].as_str() {
            Some("grant") => {
                let identity = identity()?;
                let counters = match &request["counters"] {
                    Value::Null => None,
                    names => Some(
                        serde_json::from_value::<Vec<String>>(names.clone())
                            .ok()
                            .and_then(|names| {
                                names
                                    .iter()
                                    .map(|name| HEXLOWER.decode(name.as_bytes()).ok())
                                    .collect::<Option<BTreeSet<_>>>()
                            })
                            .ok_or_else(|| refused("counters must list hex counter names"))?,
                    ),
                };
                let delegations = match &request["delegations"] {
                    Value::Null => 1,
                    value => value
                        .as_u64()
                        .filter(|count| (1..=7).contains(count))
                        .ok_or_else(|| refused("delegations must be 1 to 7"))?
                        as u8,
                };
                let (grant, changes) = self.settings.lock().await.grant_claims(
                    &self.vault,
                    identity,
                    counters,
                    delegations,
                )?;
                // Recorded before the grant leaves, so every issuer renews it.
                if !changes.is_empty() {
                    let replaced = self.registry().await?.claims.get(&identity).cloned();
                    self.commit(changes).await?;
                    // The new row replaced the previous right, whose live
                    // links would otherwise still claim until they expire.
                    if let Some(record) = replaced {
                        self.revoke(crate::enrollment::Revoke::Claims(record.right.grant))
                            .await?;
                    }
                }
                self.activity.record(
                    "claims_granted",
                    json!({"identity": identity.to_hex(), "counters": grant.counters}),
                );
                Ok(serde_json::to_value(grant)?)
            }
            Some("install") => {
                let grant: ClaimGrant = serde_json::from_value(request["grant"].clone())
                    .map_err(|_| refused("grant must be a claim grant"))?;
                let links = HEXLOWER
                    .decode(grant.grant.as_bytes())
                    .ok()
                    .and_then(|bytes| {
                        rocky_credentials::decode_bounded::<CredentialChain>(
                            &bytes,
                            rocky_credentials::MAX_CREDENTIAL_BYTES,
                        )
                        .ok()
                    })
                    .ok_or_else(|| refused("grant must hold a hex claim chain"))?;
                let issuer = match &grant.issuer {
                    None => None,
                    Some(key) => Some(
                        key.parse::<PublicKey>()
                            .map_err(|_| refused("issuer must be a hex node key"))?,
                    ),
                };
                self.configure(|settings| settings.hold_claims(links, issuer))
                    .await
                    .map_err(refused)?;
                Ok(json!({"expires": grant.expires}))
            }
            Some("revoke") => {
                let identity = identity()?;
                let registry = self.registry().await?;
                let (grant, changes) = self
                    .settings
                    .lock()
                    .await
                    .revoke_claims(identity, &registry)
                    .map_err(refused)?;
                // The statement ends every live link of the grant, whichever
                // issuer signed it; the row's removal ends the renewals.
                self.revoke(crate::enrollment::Revoke::Claims(grant))
                    .await?;
                self.commit(changes).await?;
                self.activity.record(
                    "claims_revoked",
                    json!({"identity": identity.to_hex(), "grant": HEXLOWER.encode(&grant)}),
                );
                Ok(json!({"grant": HEXLOWER.encode(&grant)}))
            }
            Some("held") => Ok(serde_json::to_value(
                self.settings.lock().await.held_claims()?,
            )?),
            _ => Err(refused(
                "claims action must be grant, install, held or revoke",
            )),
        }
    }
}

#[cfg(feature = "iroh")]
impl Node {
    /// Pushes the revocations this node holds to its coordination service
    /// until it acknowledges their digest: the claim rights they end are
    /// refused from that commit on. Any node holding a statement pushes it,
    /// at a revocation, a sync pass or a claim; a failed push is retried
    /// with backoff and the acknowledgment persists across restarts.
    pub(super) async fn push_revocations(&self) -> Result<(), Failure> {
        let Some(client) = self.service.as_ref().and_then(|service| service.client()) else {
            return Ok(());
        };
        let time = now()?;
        let Some((revocations, digest)) = self.settings.lock().await.unpushed_revocations(time)?
        else {
            self.coordination.observe_push(true, time);
            return Ok(());
        };
        // One push at a time; a concurrent caller leaves it to the first.
        let Ok(_pushing) = self.revocations_pushing.try_lock() else {
            return Ok(());
        };
        if !self
            .push_retry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .due(digest, time)
        {
            return Ok(());
        }
        match client.revoke(self.domain().scope, revocations).await {
            Ok(_) => {
                *self
                    .push_retry
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = PushRetry::default();
                // Statements learned meanwhile change the held digest and
                // stay unpushed.
                self.configure(|settings| {
                    settings.revocations_pushed = Some(digest);
                    Ok(())
                })
                .await?;
                self.coordination.observe_push(true, time);
                self.activity.record(
                    "revocations_pushed",
                    json!({"digest": HEXLOWER.encode(&digest)}),
                );
            }
            Err(error) => {
                let retry = self
                    .push_retry
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .failed(digest, time);
                self.coordination.observe_push(false, time);
                self.activity.record(
                    "revocations_unpushed",
                    json!({"error": error.to_string(), "retry": retry}),
                );
            }
        }
        Ok(())
    }
    /// Presents this node's claim chain with its counter requests. A member
    /// whose claim link is missing or past half its lifetime first renews
    /// with its issuers, at most once per `ASK_INTERVAL`.
    pub(super) async fn present_claims(
        &self,
        client: &rocky_coordinator::service::NetworkCoordinator,
    ) -> Result<(), Failure> {
        if let Err(error) = self.push_revocations().await {
            self.activity
                .record("revocations_unpushed", json!({"error": error.to_string()}));
        }
        let (due, issuer) = {
            let settings = self.settings.lock().await;
            (
                settings.claims_due(now()?),
                settings.claims.as_ref().and_then(|held| held.issuer),
            )
        };
        let ask = due && {
            let time = now()?;
            let mut asked = self
                .claims_asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let ask = asked.saturating_add(ASK_INTERVAL) <= time;
            if ask {
                *asked = time;
            }
            ask
        };
        if ask && let Err(error) = self.renew(None, false, issuer, false).await {
            self.activity
                .record("claims_renewal_failed", json!({"error": error.to_string()}));
        }
        let chain = self.settings.lock().await.claim_chain(&self.vault)?;
        client.present_claims(chain);
        Ok(())
    }
}

#[cfg(all(test, feature = "iroh"))]
#[test]
fn a_failed_push_backs_off_and_a_new_statement_pushes_at_once() {
    let mut retry = PushRetry::default();
    let (held, learned) = ([1; 32], [2; 32]);
    assert!(retry.due(held, 100));
    let delays: Vec<u64> = (0..8)
        .map(|_| retry.failed(held, 100).saturating_sub(100))
        .collect();
    assert_eq!(delays, [5, 10, 20, 40, 80, 160, 300, 300]);
    assert!(!retry.due(held, 399));
    assert!(retry.due(held, 400));
    assert!(retry.due(learned, 101));
    assert_eq!(retry.failed(learned, 101), 106);
}
