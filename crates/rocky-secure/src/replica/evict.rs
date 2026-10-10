//! Explicit scope eviction on a partial replica, in bounded resumable steps.
use super::checkpoint::{STEP_OPERATIONS, STEP_ROWS};
use super::roles::{Holding, ReplicaRole};
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvictReport {
    /// Stored rows and checkpoint rows dropped.
    pub rows: u64,
    /// Retained operations dropped.
    pub operations: u64,
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Drops a scope's rows and history once enough full nodes have applied
    /// every operation this node authored in it. Neither deletion nor
    /// pruning: peers keep the rows, and `subscribe` brings them back. An
    /// interrupted eviction continues on the next call; until it ends the
    /// scope reads as unavailable.
    pub async fn evict(&self, scope: ResourceScope) -> Result<EvictReport, Error> {
        {
            let mut inner = self.inner.lock().await;
            if inner.holding(scope).await? != Some(Holding::Evicting) {
                inner.begin_eviction(scope).await?;
            }
        }
        let mut report = EvictReport {
            rows: 0,
            operations: 0,
        };
        loop {
            let mut inner = self.inner.lock().await;
            if inner.eviction_step(scope, &mut report).await? {
                return Ok(report);
            }
        }
    }
}

impl<A: RowApplication> Inner<A> {
    async fn begin_eviction(&mut self, scope: ResourceScope) -> Result<(), Error> {
        let role = self.role().await?;
        if role.role != ReplicaRole::Partial {
            return Err(denied("a full replica does not evict"));
        }
        if self.holding(scope).await? == Some(Holding::Evicted) {
            return Err(denied("scope is already evicted"));
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
            return Err(denied(
                "the domain requires recovery custodians, whose archives need complete history",
            ));
        }
        let needed = role.custody.max(1);
        let covering = self.custody_coverage(scope, &role.full_nodes).await?;
        if covering < needed {
            return Err(denied(&format!(
                "eviction needs {needed} full nodes to have applied every local operation; {covering} have"
            )));
        }
        transaction!(self, |inner| {
            if let Some(id) = inner.find_scope_id(scope).await? {
                inner.save_eviction_guard(scope, id).await?;
            }
            inner.mark_evicting(scope, false).await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
            Ok(())
        })?;
        self.publish_revision().await
    }
    /// Full nodes that applied every operation this node authored in the scope.
    async fn custody_coverage(
        &self,
        scope: ResourceScope,
        full_nodes: &BTreeSet<PublicKey>,
    ) -> Result<usize, Error> {
        let me = self.config.node.public_key();
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(full_nodes.len());
        };
        let Some(log) = self.log(id, me).await? else {
            return Ok(full_nodes.len());
        };
        let blob = encode(&scope)?;
        let mut covering = 0;
        for node in full_nodes {
            let (_, applied) = self.peer_mark(*node, &blob, me).await?;
            if applied as i64 > log.sequence {
                covering += 1;
                continue;
            }
            // Pruned operations are known applied only through the frontier.
            if log.horizon >= 0 && applied as i64 <= log.horizon {
                continue;
            }
            let missing = collect_rows(
                self.conn
                    .query(
                        "SELECT COUNT(*) FROM secure_operations o WHERE o.author=? AND o.scope=? AND o.sequence>=? AND NOT EXISTS(SELECT 1 FROM secure_ack a WHERE a.hash=o.hash AND a.peer=? AND a.applied=1)",
                        bindings(vec![
                            SqlValue::Blob(me.as_bytes().to_vec()),
                            SqlValue::Blob(blob.clone()),
                            SqlValue::Integer(applied as i64),
                            SqlValue::Blob(node.as_bytes().to_vec()),
                        ]),
                    )
                    .await?,
            )
            .await?;
            if missing == vec![vec![SqlValue::Integer(0)]] {
                covering += 1;
            }
        }
        Ok(covering)
    }
    /// Deletes one bounded batch; true once the scope is evicted.
    async fn eviction_step(
        &mut self,
        scope: ResourceScope,
        report: &mut EvictReport,
    ) -> Result<bool, Error> {
        let blob = encode(&scope)?;
        let id = self.find_scope_id(scope).await?;
        let (rows, operations, done) = transaction!(self, |inner| {
            let mut rows = 0;
            if let Some(id) = id {
                for table in [
                    "secure_checkpoint_rows",
                    "secure_checkpoint_overlay",
                    "secure_checkpoint_scratch",
                    "secure_rebootstrap_rows",
                ] {
                    rows += inner
                        .conn
                        .execute(
                            format!("DELETE FROM {table} WHERE scope=? AND row IN (SELECT row FROM {table} WHERE scope=? LIMIT ?)"),
                            [id, id, STEP_ROWS as i64],
                        )
                        .await?;
                }
            }
            rows += inner
                .conn
                .execute(
                    "DELETE FROM secure_rows WHERE rowid IN (SELECT rowid FROM secure_rows WHERE scope=? LIMIT ?)",
                    bindings(vec![
                        SqlValue::Blob(blob.clone()),
                        SqlValue::Integer(STEP_ROWS as i64),
                    ]),
                )
                .await?;
            // The typed rows go with the first step: an evicted scope is unavailable.
            inner.cause(crate::observe::Cause::Authority);
            inner.clear_typed(scope).await?;
            let batch = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT hash,LENGTH(header)+LENGTH(body) FROM secure_operations WHERE scope=? LIMIT ?",
                        bindings(vec![
                            SqlValue::Blob(blob.clone()),
                            SqlValue::Integer(STEP_OPERATIONS as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            let mut bytes = 0;
            for record in &batch {
                let [SqlValue::Blob(hash), SqlValue::Integer(size)] = record.as_slice() else {
                    return Err(Error::Corrupt("retained operation".into()));
                };
                for sql in [
                    "DELETE FROM secure_operations WHERE hash=?",
                    "DELETE FROM secure_outbox WHERE hash=?",
                    "DELETE FROM secure_ack WHERE hash=?",
                    "DELETE FROM secure_applied WHERE hash=?",
                ] {
                    inner.conn.execute(sql, [hash.clone()]).await?;
                }
                bytes += *size;
            }
            if !batch.is_empty() {
                inner.sweep_chains().await?;
                inner
                    .conn
                    .execute(
                        "UPDATE secure_retained SET records=records-?,bytes=bytes-?",
                        [batch.len() as i64, bytes],
                    )
                    .await?;
            }
            let done = rows == 0 && batch.is_empty();
            if done {
                if let Some(id) = id {
                    for table in [
                        "secure_applied",
                        "secure_stubs",
                        "secure_logs",
                        "secure_checkpoints",
                        "secure_rebootstrap",
                        "secure_rebootstrap_stubs",
                        "secure_checkpoint_stubs",
                        "secure_checkpoint_build_stubs",
                        "secure_horizon_live_creations",
                    ] {
                        inner
                            .conn
                            .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                            .await?;
                    }
                    inner
                        .store_scope_state(id, 0, 0, &checkpoint::Lt::new(), 0)
                        .await?;
                }
                for table in ["secure_retention", "secure_peer_frontiers"] {
                    inner
                        .conn
                        .execute(format!("DELETE FROM {table} WHERE scope=?"), [blob.clone()])
                        .await?;
                }
                inner.mark_evicting(scope, true).await?;
                inner
                    .conn
                    .execute(
                        "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                        (),
                    )
                    .await?;
            }
            inner.fault(checkpoint::Stage::Evict)?;
            Ok((rows, batch.len() as u64, done))
        })?;
        report.rows += rows;
        report.operations += operations;
        if done {
            if let Some(id) = id {
                self.verified_at.remove(&id);
            }
            self.publish_revision().await?;
        }
        Ok(done)
    }
}
