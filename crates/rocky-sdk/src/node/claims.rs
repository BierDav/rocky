//! Counter claim rights through the call contract, and the claim chain this
//! node presents to its coordination service. See `crate::claims`.
use super::*;
use crate::claims::ClaimGrant;
use std::collections::BTreeSet;

/// Seconds between a member's renewals for a claim link it lacks or that
/// passed half its lifetime.
#[cfg(feature = "iroh")]
const ASK_INTERVAL: u64 = 60;

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
                    self.commit(changes).await?;
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
    /// Pushes this node's revocations to its coordination service: the
    /// claim rights they end are refused from then on. A failure is only
    /// recorded; the revoked links still lapse within their lifetime.
    pub(super) async fn push_revocations(&self) {
        let Some(client) = self.service.as_ref().and_then(|service| service.client()) else {
            return;
        };
        let revocations = self.settings.lock().await.revocations.clone();
        if revocations.is_empty() {
            return;
        }
        if let Err(error) = client.revoke(self.domain().scope, revocations).await {
            self.activity
                .record("revocations_unpushed", json!({"error": error.to_string()}));
        }
    }
    /// Presents this node's claim chain with its counter requests. A member
    /// whose claim link is missing or past half its lifetime first renews
    /// with its issuers, at most once per `ASK_INTERVAL`.
    pub(super) async fn present_claims(
        &self,
        client: &rocky_coordinator::service::NetworkCoordinator,
    ) -> Result<(), Failure> {
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
