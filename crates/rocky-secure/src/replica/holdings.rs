//! Holding evidence never replaces current credential verification.
use super::*;
use crate::network::holdings::{HoldingProof, key_proof};

impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn holding_required(&self, scope: ResourceScope) -> bool {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        // Known unpartitioned tables retain their grant-only rule.
        if inner.partitioned(scope) {
            return true;
        }
        if let Some(typed) = &inner.workspace.typed {
            return !typed.declarations.iter().any(|table| table.scope == scope);
        }
        !inner.tables.contains(&scope.table)
            || !inner.domains.contains(&scope.domain.domain)
            || scope.domain.app != inner.definition.app
    }

    pub(crate) async fn holding_proofs(
        &self,
        binding: Commitment,
        scopes: &[ResourceScope],
    ) -> Result<Vec<(ResourceScope, Vec<HoldingProof>)>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut proofs = Vec::new();
        for scope in scopes {
            if !inner.partitioned(*scope) {
                continue;
            }
            let mut keys: Vec<_> = inner
                .opening_keys()
                .filter(|key| key.scope == *scope)
                .collect();
            keys.sort_unstable_by_key(|key| std::cmp::Reverse(key.generation));
            proofs.push((
                *scope,
                keys.into_iter()
                    .take(4)
                    .map(|key| {
                        Ok(HoldingProof {
                            epoch: key.generation,
                            value: key_proof(key, binding)?,
                        })
                    })
                    .collect::<Result<_, Error>>()?,
            ));
        }
        Ok(proofs)
    }

    pub(crate) async fn tuple_proofs(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scopes: &[ResourceScope],
    ) -> Result<Vec<super::partition::PartitionOffer>, Error> {
        let mut offers = self.custody_offers(peer, credentials, scopes).await?;
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let own = inner.config.node.public_key();
        for scope in scopes {
            let Some(offer) = inner.partition_offer(*scope) else {
                continue;
            };
            let grants =
                inner.partition_grants(&inner.config.credentials, own, *scope, &offer.1)?;
            // The tuple and its naming secret go only to a peer entitled
            // to the partition, never to one that merely asks.
            let entitled = inner
                .partition_grants(credentials, peer, *scope, &offer.1)
                .is_ok_and(|grants| {
                    grants.iter().any(|g| {
                        g.rows == RowSelection::All
                            && (g.rights.contains(&Right::Read)
                                || g.rights.contains(&Right::Custody))
                    })
                });
            if entitled
                && grants.iter().any(|g| g.rights.contains(&Right::Custody))
                && !grants.iter().any(|g| g.rights.contains(&Right::Read))
                && !offers.iter().any(|(offered, ..)| offered == scope)
            {
                offers.push(offer);
            }
        }
        Ok(offers)
    }

    pub(crate) async fn verify_holding(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        binding: Commitment,
        scope: ResourceScope,
        proofs: &[HoldingProof],
        offer: Option<(&rocky_core::PartitionTuple, &rocky_core::PartitionName)>,
    ) -> Result<bool, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        if !inner.partitioned(scope) {
            return Ok(false);
        }
        let known = match offer {
            Some((tuple, name)) => {
                if inner.check_tuple(scope, tuple, name).is_err() {
                    return Ok(false);
                }
                tuple
            }
            None => match inner.partitions.get(&scope) {
                Some(tuple) => tuple,
                None => return Ok(false),
            },
        };
        let grants = match inner.partition_grants(credentials, peer, scope, known) {
            Ok(grants) => grants,
            Err(Error::Rejected(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        let whole = |right| {
            grants
                .iter()
                .any(|g| g.rows == RowSelection::All && g.rights.contains(&right))
        };
        if !whole(Right::Read) && !whole(Right::Custody) {
            return Ok(false);
        }
        let own = match inner.partition_grants(
            &inner.config.credentials,
            inner.config.node.public_key(),
            scope,
            known,
        ) {
            Ok(grants) => grants,
            Err(Error::Rejected(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        let keyless = own.iter().any(|g| g.rights.contains(&Right::Custody))
            && !own.iter().any(|g| g.rights.contains(&Right::Read));
        if offer.is_some() && (keyless || (whole(Right::Custody) && !whole(Right::Read))) {
            return Ok(true);
        }
        for proof in proofs {
            if let Some(key) = inner.opening_key(scope, proof.epoch)
                && crate::network::holdings::verify_key_proof(key, binding, &proof.value)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
