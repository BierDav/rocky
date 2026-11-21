use super::checkpoint::{STEP_OPERATIONS, STEP_ROWS};
use super::roles::{Holding, ReplicaRole};
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvictReport {
    pub rows: u64,
    pub operations: u64,
    pub checkpoints: u64,
    pub recovery_unconfirmed: bool,
}

const EVICTED_SCOPE_STATE: &[&str] = &[
    "secure_seal_anchors",
    "secure_applied",
    "secure_stubs",
    "secure_logs",
    "secure_checkpoints",
    "secure_rebootstrap",
    "secure_rebootstrap_stubs",
    "secure_rebootstrap_pages",
    "secure_checkpoint_pages",
    "secure_checkpoint_stubs",
    "secure_compaction_live_creations",
];
const EVICTED_SEAL_BOUNDS: &[&str] = &["secure_seal_bounds", "secure_seal_drops"];
const EVICTED_REPLAYED_BASES: &[&str] = &[
    "secure_replayed_seals",
    "secure_replayed_seal_rows",
    "secure_replayed_snapshots",
    "secure_replayed_base_source",
    "secure_replayed_base_rows",
    "secure_base_transition",
    "secure_base_cleanup",
    "secure_replayed_staged",
];
const EVICTED_REPLAY_LEDGER: &[&str] = &[
    "secure_retention",
    "secure_peer_frontiers",
    "secure_identity_ledger",
    "secure_custody_receipts",
    "secure_custody_challenges",
];

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn evict(&self, scope: ResourceScope) -> Result<EvictReport, Error> {
        let recovery_unconfirmed = {
            let mut inner = self.inner.lock(crate::replica::Class::Background).await;
            if inner.holding(scope).await? != Some(Holding::Evicting) {
                inner.begin_eviction(scope).await?;
            }
            let mut saved = inner
                .conn
                .query(
                    "SELECT unconfirmed FROM secure_eviction_custody WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            let unconfirmed = saved
                .next()
                .await?
                .ok_or_else(|| Error::Corrupt("eviction custody report".into()))?
                .get::<i64>(0)?
                != 0;
            unconfirmed || inner.custody_coverage(scope).await? == 0
        };
        let mut report = EvictReport {
            rows: 0,
            operations: 0,
            checkpoints: 0,
            recovery_unconfirmed,
        };
        loop {
            let mut inner = self.inner.lock(crate::replica::Class::Background).await;
            if inner.eviction_step(scope, &mut report).await? {
                inner.load_direct().await?;
                inner.retry_rederive(scope.domain).await?;
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
        let needed = self.partition_custody(scope).await?.copies;
        let covering = self.custody_coverage(scope).await?;
        if covering < needed {
            return Err(denied(&format!(
                "eviction needs {needed} durable custody receipts; {covering} cover the scope"
            )));
        }
        transaction!(self, |inner| {
            inner.conn.execute("INSERT INTO secure_eviction_custody VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET unconfirmed=excluded.unconfirmed", bindings(vec![SqlValue::Blob(encode(&scope)?), SqlValue::Integer(i64::from(covering == 0))])).await?;
            inner.save_eviction_guard(scope).await?;
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
    async fn eviction_step(
        &mut self,
        scope: ResourceScope,
        report: &mut EvictReport,
    ) -> Result<bool, Error> {
        let blob = encode(&scope)?;
        let id = self.find_scope_id(scope).await?;
        let (rows, operations, checkpoints, done) = transaction!(self, |inner| {
            inner
                .replace_ledger(scope, identity::IdentityLedger::default())
                .await?;
            let mut rows = 0;
            if let Some(id) = id {
                for table in [
                    "secure_checkpoint_base",
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
            let mut removing = inner.conn.query("SELECT count(*) FROM (SELECT row FROM secure_row_records WHERE scope=? LIMIT ?)", bindings(vec![SqlValue::Blob(blob.clone()), SqlValue::Integer(STEP_ROWS as i64)])).await?;
            let count: i64 = removing
                .next()
                .await?
                .ok_or_else(|| Error::Corrupt("evicted row count".into()))?
                .get(0)?;
            rows += count as u64;
            drop(removing);
            inner.conn.execute(
                    "DELETE FROM secure_row_records WHERE scope=? AND row IN (SELECT row FROM secure_row_records WHERE scope=? LIMIT ?)",
                    bindings(vec![
                        SqlValue::Blob(blob.clone()),
                        SqlValue::Blob(blob.clone()),
                        SqlValue::Integer(STEP_ROWS as i64),
                    ]),
                )
                .await?;
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
                    "DELETE FROM secure_unoffered WHERE hash=?",
                    "DELETE FROM secure_ack WHERE hash=?",
                    "DELETE FROM secure_applied WHERE hash=?",
                    "DELETE FROM secure_publication_frames WHERE hash=?",
                ] {
                    inner.conn.execute(sql, [hash.clone()]).await?;
                }
                inner.clear_applied_cache();
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
            let archive_pages = inner.conn.execute("DELETE FROM secure_checkpoint_archive_pages WHERE scope=?1 AND (checkpoint,cursor) IN (SELECT checkpoint,cursor FROM secure_checkpoint_archive_pages WHERE scope=?1 LIMIT ?2)", bindings(vec![SqlValue::Blob(blob.clone()), SqlValue::Integer(STEP_ROWS as i64)])).await?;
            let checkpoints = inner.conn.execute("DELETE FROM secure_checkpoint_archive WHERE scope=?1 AND checkpoint IN (SELECT a.checkpoint FROM secure_checkpoint_archive a WHERE a.scope=?1 AND NOT EXISTS(SELECT 1 FROM secure_checkpoint_archive_pages p WHERE p.scope=a.scope AND p.checkpoint=a.checkpoint) LIMIT ?2)", bindings(vec![SqlValue::Blob(blob.clone()), SqlValue::Integer(STEP_ROWS as i64)])).await?;
            let done = rows == 0 && batch.is_empty() && archive_pages == 0 && checkpoints == 0;
            if done {
                if let Some(id) = id {
                    for table in EVICTED_SCOPE_STATE
                        .iter()
                        .chain(EVICTED_SEAL_BOUNDS)
                        .chain(EVICTED_REPLAYED_BASES)
                    {
                        inner
                            .conn
                            .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                            .await?;
                    }
                    inner
                        .store_scope_state(id, 0, 0, &checkpoint::LtHash16::new(), 0)
                        .await?;
                }
                for table in EVICTED_REPLAY_LEDGER {
                    inner
                        .conn
                        .execute(format!("DELETE FROM {table} WHERE scope=?"), [blob.clone()])
                        .await?;
                }
                inner.forget_evicted_escrow(scope).await?;
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
            Ok((rows, batch.len() as u64, checkpoints, done))
        })?;
        report.rows += rows;
        report.operations += operations;
        report.checkpoints += checkpoints;
        if done {
            if let Some(id) = id {
                self.verified_at.remove(&id);
            }
            self.publish_revision().await?;
        }
        Ok(done)
    }
}
