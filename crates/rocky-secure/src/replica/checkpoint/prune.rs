//! Pruning of operations a verified checkpoint covers, in bounded batches.
use super::*;

/// Why covered operations are still retained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PruneBlock {
    /// The scope has no verified checkpoint.
    NoCheckpoint,
    /// No retention policy names the peers that must hold copies.
    NoPolicy,
    /// The authenticated existence base needs another durable holder.
    HorizonCustody { held: usize, needed: usize },
    /// The domain requires recovery custodians, whose archives need complete history.
    RecoveryCustody,
    /// A required peer has not acknowledged this operation.
    Unacknowledged { peer: PublicKey, operation: Hash },
    /// Too few other nodes acknowledged this operation: it is a sole copy.
    SoleCopy { operation: Hash },
    /// Too few configured full nodes have applied this operation.
    Custody {
        operation: Hash,
        applied: usize,
        needed: usize,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PruneReport {
    pub pruned: u64,
    /// What stopped pruning before the retained window was reached.
    pub blocked: Option<PruneBlock>,
}
pub(in crate::replica) struct PruneStep {
    pub pruned: u64,
    pub blocked: Option<PruneBlock>,
    pub more: bool,
}
impl PruneStep {
    fn blocked(block: PruneBlock) -> Self {
        Self {
            pruned: 0,
            blocked: Some(block),
            more: false,
        }
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Deletes operations, acknowledgments and outbox entries the scope's
    /// verified, flushed checkpoint covers, oldest first, while the retention
    /// policy's acknowledgments exist. Repeating it is safe; a failed call
    /// leaves whole batches pruned and the rest retained.
    pub async fn prune(&self, scope: ResourceScope) -> Result<PruneReport, Error> {
        let mut report = PruneReport {
            pruned: 0,
            blocked: None,
        };
        loop {
            let mut inner = self.inner.lock().await;
            inner.prune_strong(scope.domain).await?;
            let step = inner.prune_step(scope).await?;
            report.pruned += step.pruned;
            report.blocked = step.blocked;
            if !step.more {
                if report.blocked.is_none()
                    && inner.retention_policy(scope).await?.is_some()
                    && let Some(id) = inner.find_scope_id(scope).await?
                    && let Some(signed) = inner
                        .checkpoint_record(id, true)
                        .await?
                        .and_then(|record| record.signed)
                    && let Some(finalized) = &signed.statement.finalized
                {
                    inner.flush()?;
                    transaction!(inner, |inner| {
                        for descriptor in &finalized.statement.bases {
                            let expected = super::super::existence::ExistenceBaseAttachment {
                                horizon: finalized.clone(),
                                scope: descriptor.scope,
                            };
                            if inner
                                .existence_base(descriptor.scope)
                                .await?
                                .map(|base| base.id())
                                .transpose()?
                                == Some(expected.id()?)
                            {
                                inner.compact_existence_base(descriptor.scope).await?;
                            }
                        }
                        inner.collect_existence_archives().await?;
                        Ok(())
                    })?;
                }
                return Ok(report);
            }
        }
    }
}

impl<A: RowApplication> Inner<A> {
    pub(crate) async fn retention_policy(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<RetentionPolicy>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT policy FROM secure_retention WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        match result.next().await? {
            Some(row) => Ok(Some(decode(&row.get::<Vec<u8>>(0)?)?)),
            None => Ok(None),
        }
    }
    pub(in crate::replica) async fn prune_step(
        &mut self,
        scope: ResourceScope,
    ) -> Result<PruneStep, Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(PruneStep::blocked(PruneBlock::NoCheckpoint));
        };
        let Some(record) = self.checkpoint_record(id, true).await? else {
            return Ok(PruneStep::blocked(PruneBlock::NoCheckpoint));
        };
        // An unverifiable checkpoint blocks pruning with its error.
        self.verify_record(scope, &record)?;
        let Some(mut policy) = self.retention_policy(scope).await? else {
            return Ok(PruneStep::blocked(PruneBlock::NoPolicy));
        };
        if let Some(finalized) = self.verify_record(scope, &record)?.statement.finalized
            && !finalized.statement.bases.is_empty()
        {
            let held = self.horizon_copies(&finalized).await?;
            let needed = policy.copies.max(1);
            if held < needed {
                return Ok(PruneStep::blocked(PruneBlock::HorizonCustody {
                    held,
                    needed,
                }));
            }
        }
        if self
            .verify_record(scope, &record)?
            .statement
            .finalized
            .is_some()
        {
            for peer in &policy.required {
                if self.horizon_device_behind(scope, *peer).await? {
                    policy.behind.insert(*peer);
                }
            }
        }
        let mut durability = self
            .conn
            .query(
                "SELECT policy FROM secure_durability WHERE domain=?",
                [encode(&scope.domain)?],
            )
            .await?;
        if let Some(row) = durability.next().await?
            && matches!(
                decode::<DurabilityPolicy>(&row.get::<Vec<u8>>(0)?)?,
                DurabilityPolicy::RequiredCustodians { .. }
            )
        {
            return Ok(PruneStep::blocked(PruneBlock::RecoveryCustody));
        }
        // The checkpoint must survive power loss before anything it covers goes.
        if self
            .verified_at
            .get(&id)
            .is_none_or(|commit| self.flusher.durable() < *commit)
        {
            self.flush()?;
            self.verified_at.insert(id, self.flusher.durable());
        }
        let bound = collect_rows(
            self.conn
                .query(
                    "SELECT position FROM secure_applied WHERE scope=? AND position<=? ORDER BY position DESC LIMIT 1 OFFSET ?",
                    [id, record.position, policy.window as i64],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Integer(bound)]) = bound.first().map(Vec::as_slice) else {
            return Ok(PruneStep {
                pruned: 0,
                blocked: None,
                more: false,
            });
        };
        let bound = *bound;
        let copies = policy.copies.max(1);
        let role = self.role().await?;
        let blob = encode(&scope)?;
        transaction!(self, |inner| {
            let candidates = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT a.hash,o.header,LENGTH(o.header)+LENGTH(o.body) FROM secure_applied a JOIN secure_operations o ON o.hash=a.hash WHERE a.scope=? AND a.position<=? ORDER BY a.position LIMIT ?",
                        [id, bound, STEP_OPERATIONS as i64],
                    )
                    .await?,
            )
            .await?;
            let mut step = PruneStep {
                pruned: 0,
                blocked: None,
                more: candidates.len() == STEP_OPERATIONS,
            };
            let mut bytes = 0;
            for candidate in candidates {
                let [
                    SqlValue::Blob(hash),
                    SqlValue::Blob(header),
                    SqlValue::Integer(size),
                ] = candidate.as_slice()
                else {
                    return Err(Error::Corrupt("prune candidate".into()));
                };
                let operation = hash_of(hash)?;
                let header: p2panda_core::Header<Extensions> = decode(header)?;
                // Acknowledged per operation or through a reported frontier.
                let (mut peers, mut applied) = inner
                    .covering_peers(&blob, header.public_key, header.seq_num)
                    .await?;
                for peer in collect_rows(
                    inner
                        .conn
                        .query(
                            "SELECT peer,applied FROM secure_ack WHERE hash=?",
                            [hash.clone()],
                        )
                        .await?,
                )
                .await?
                {
                    let [SqlValue::Blob(peer), SqlValue::Integer(done)] = peer.as_slice() else {
                        return Err(Error::Corrupt("acknowledgment".into()));
                    };
                    peers.insert(key_of(peer)?);
                    if *done == 1 {
                        applied.insert(key_of(peer)?);
                    }
                }
                // Receipt alone is not custody: full nodes count once applied.
                let custodians = role
                    .full_nodes
                    .iter()
                    .filter(|node| applied.contains(node))
                    .count();
                step.blocked = policy
                    .required
                    .difference(&policy.behind)
                    .find(|peer| !peers.contains(peer))
                    .map(|peer| PruneBlock::Unacknowledged {
                        peer: *peer,
                        operation,
                    })
                    .or_else(|| {
                        (peers.len() < copies).then_some(PruneBlock::SoleCopy { operation })
                    })
                    .or_else(|| {
                        (custodians < role.custody).then_some(PruneBlock::Custody {
                            operation,
                            applied: custodians,
                            needed: role.custody,
                        })
                    });
                if step.blocked.is_some() {
                    step.more = false;
                    break;
                }
                for sql in [
                    "DELETE FROM secure_operations WHERE hash=?",
                    "DELETE FROM secure_outbox WHERE hash=?",
                    "DELETE FROM secure_ack WHERE hash=?",
                    "DELETE FROM secure_applied WHERE hash=?",
                ] {
                    inner.conn.execute(sql, [hash.clone()]).await?;
                }
                inner
                    .conn
                    .execute(
                        "UPDATE secure_logs SET horizon=MAX(horizon,?) WHERE scope=? AND author=?",
                        bindings(vec![
                            SqlValue::Integer(header.seq_num as i64),
                            SqlValue::Integer(id),
                            SqlValue::Blob(header.public_key.as_bytes().to_vec()),
                        ]),
                    )
                    .await?;
                bytes += *size;
                step.pruned += 1;
            }
            // With its last operation, in the same transaction, goes a chain.
            if step.pruned > 0 {
                inner.sweep_chains().await?;
            }
            if step.pruned > 0 {
                inner
                    .conn
                    .execute(
                        "UPDATE secure_retained SET records=records-?,bytes=bytes-?",
                        [step.pruned as i64, bytes],
                    )
                    .await?;
            }
            inner.forget_handles(STEP_OPERATIONS).await?;
            inner.fault(Stage::Prune)?;
            Ok(step)
        })
    }
}
