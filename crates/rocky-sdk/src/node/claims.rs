use super::*;
use crate::claims::ClaimGrant;
use std::collections::BTreeSet;

const ASK_INTERVAL: u64 = 60;
const PUSH_RETRY: u64 = 5;

const PUSH_RETRY_MAX: u64 = 300;

#[derive(Default)]
pub(super) struct PushRetry {
    digest: Option<rocky_core::Commitment>,
    failures: u32,
    next: u64,
}

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
    pub(super) async fn grant_claims(
        &self,
        identity: PublicKey,
        counters: Option<BTreeSet<Vec<u8>>>,
        delegations: u8,
    ) -> Result<ClaimGrant, Failure> {
        if !(1..=7).contains(&delegations) {
            return Err(refused("delegations must be 1 to 7"));
        }
        let (grant, changes) = self.settings.lock().await.grant_claims(
            &self.vault,
            identity,
            counters,
            delegations,
        )?;
        if !changes.is_empty() {
            let replaced = self
                .registry(&*self.settings_snapshot().await)
                .await?
                .claims
                .get(&identity)
                .cloned();
            self.commit(changes).await?;
            if let Some(record) = replaced {
                self.revoke(crate::enrollment::Revoke::Claims(record.right.grant))
                    .await?;
            }
        }
        self.activity.record(crate::ActivityKind::ClaimsGranted {
            identity: (identity).to_owned(),
            counters: grant.counters.clone(),
        });
        Ok(grant)
    }

    pub(super) async fn install_claims(&self, grant: ClaimGrant) -> Result<(), Failure> {
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
        let issuer = grant
            .issuer
            .as_deref()
            .map(str::parse::<PublicKey>)
            .transpose()
            .map_err(|_| refused("issuer must be a hex node key"))?;
        self.configure(|settings| settings.hold_claims(links, issuer))
            .await
            .map_err(refused)
    }

    pub(super) async fn revoke_claims(
        &self,
        identity: PublicKey,
    ) -> Result<rocky_core::Commitment, Failure> {
        let registry = self.registry(&*self.settings_snapshot().await).await?;
        let (grant, changes) = self
            .settings
            .lock()
            .await
            .revoke_claims(identity, &registry)
            .map_err(refused)?;
        self.revoke(crate::enrollment::Revoke::Claims(grant))
            .await?;
        self.commit(changes).await?;
        self.activity.record(crate::ActivityKind::ClaimsRevoked {
            identity: (identity).to_owned(),
            grant,
        });
        Ok(grant)
    }

    pub(super) async fn held_claims(&self) -> Result<Option<ClaimGrant>, Failure> {
        self.settings.lock().await.held_claims()
    }
}

impl Node {
    pub(super) async fn push_revocations(&self) -> Result<(), Failure> {
        let Some(client) = self.coordinator() else {
            return Ok(());
        };
        let time = now()?;
        let Some((revocations, digest)) = self.settings.lock().await.unpushed_revocations(time)?
        else {
            self.coordination.observe_push(true, time);
            return Ok(());
        };
        let Ok(_one_push_at_a_time) = self.revocations_pushing.try_lock() else {
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
                self.configure(|settings| {
                    settings.revocations_pushed = Some(digest);
                    Ok(())
                })
                .await?;
                self.coordination.observe_push(true, time);
                self.activity
                    .record(crate::ActivityKind::RevocationsPushed { digest });
            }
            Err(error) => {
                let retry = self
                    .push_retry
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .failed(digest, time);
                self.coordination.observe_push(false, time);
                self.activity
                    .record(crate::ActivityKind::RevocationsUnpushed {
                        error: (error.to_string()).to_owned(),
                        retry: Some(retry),
                    });
            }
        }
        Ok(())
    }
    pub(super) async fn present_claims(
        &self,
        client: &dyn rocky_credentials::Coordinator,
    ) -> Result<(), Failure> {
        if let Err(error) = self.push_revocations().await {
            self.activity
                .record(crate::ActivityKind::RevocationsUnpushed {
                    error: (error.to_string()).to_owned(),
                    retry: None,
                });
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
                .record(crate::ActivityKind::ClaimsRenewalFailed {
                    error: (error.to_string()).to_owned(),
                });
        }
        let chain = self.settings.lock().await.claim_chain(&self.vault)?;
        client.present_claims(chain);
        Ok(())
    }
}

#[cfg(test)]
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
