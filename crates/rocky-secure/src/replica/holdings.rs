//! Holding evidence never replaces current credential verification.
use super::*;
use crate::network::holdings::{HoldingProof, key_proof};

impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn holding_required(&self, scope: ResourceScope) -> bool {
        let inner = self.inner.lock().await;
        // Known unpartitioned tables retain their grant-only rule.
        if inner.partitioned(scope) {
            return true;
        }
        if let Some(typed) = &inner.typed {
            return !typed.tables.iter().any(|table| table.scope == scope);
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
        let inner = self.inner.lock().await;
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
    ) -> Result<Vec<(ResourceScope, rocky_core::PartitionTuple)>, Error> {
        let mut offers = self.custody_offers(peer, credentials, scopes).await?;
        let inner = self.inner.lock().await;
        let own = inner.config.node.public_key();
        for scope in scopes {
            let Some(tuple) = inner.partitions.get(scope) else {
                continue;
            };
            let grants = inner.partition_grants(&inner.config.credentials, own, *scope, tuple)?;
            if grants.iter().any(|g| g.rights.contains(&Right::Custody))
                && !grants.iter().any(|g| g.rights.contains(&Right::Read))
                && !offers.iter().any(|(offered, _)| offered == scope)
            {
                offers.push((*scope, tuple.clone()));
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
        tuple: Option<&rocky_core::PartitionTuple>,
    ) -> Result<bool, Error> {
        let inner = self.inner.lock().await;
        if !inner.partitioned(scope) {
            return Ok(false);
        }
        let Some(known) = tuple.or_else(|| inner.partitions.get(&scope)) else {
            return Ok(false);
        };
        if inner.check_tuple(scope, known).is_err() {
            return Ok(false);
        }
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
        if tuple.is_some() && (keyless || (whole(Right::Custody) && !whole(Right::Read))) {
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
