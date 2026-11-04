//! Compaction borrows immutable seal rows instead of copying them at publication.
//! The cursor and digest commit together; cleanup never touches a selected base.
use super::*;

pub(super) const BASE_ROWS: usize = 64;

pub(in crate::replica) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_base_transition(scope INTEGER PRIMARY KEY,seal BLOB NOT NULL,version INTEGER NOT NULL,cursor BLOB,sets BLOB NOT NULL,ready INTEGER NOT NULL CHECK(ready IN(0,1))) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_base_cleanup(scope INTEGER PRIMARY KEY,depth INTEGER NOT NULL) WITHOUT ROWID",
];

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) const BASE_TRANSITION_ROWS: usize = BASE_ROWS;

    pub(crate) async fn pause_base_prepare(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, reached) = tokio::sync::oneshot::channel();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        self.inner.lock(Class::Background).await.base_prepare_pause = Some((entered, resumed));
        (reached, resume)
    }

    pub(crate) async fn retake_seal_rows(&self, scope: ResourceScope, seal: Commitment) {
        let mut inner = self.inner.lock(Class::Background).await;
        let id = inner.find_scope_id(scope).await.unwrap().unwrap();
        let depth = inner
            .finalized(scope.domain)
            .await
            .unwrap()
            .into_iter()
            .find(|record| record.seal == seal)
            .unwrap()
            .depth;
        transaction!(inner, |inner| inner
            .take_seal_rows(seal, depth, &[(scope, id)])
            .await)
        .unwrap();
    }

    pub(crate) async fn cleanup_bases_through(&self, scope: ResourceScope, depth: i64) {
        let mut inner = self.inner.lock(Class::Background).await;
        let id = inner.find_scope_id(scope).await.unwrap().unwrap();
        transaction!(inner, |inner| inner.queue_base_cleanup(id, depth).await).unwrap();
        while inner.base_cleanup_step().await.unwrap() {}
    }
}

impl<A: RowApplication> Inner<A> {
    /// Hashes at most BASE_ROWS immutable rows. A missing snapshot uses the
    /// existing path; it is not claimed as bounded by this mechanism.
    pub(super) async fn prepare_base_step(
        &mut self,
        scope: ResourceScope,
        seal: Commitment,
    ) -> Result<bool, Error> {
        if !self.replayed_scope(scope) {
            return Ok(false);
        }
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(false);
        };
        let source = collect_rows(
            self.conn
                .query(
                    "SELECT version FROM secure_replayed_seals WHERE scope=? AND seal=?",
                    bindings(vec![SqlValue::Integer(id), SqlValue::Blob(seal.to_vec())]),
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Integer(version)]) = source.first().map(Vec::as_slice) else {
            return if source.is_empty() {
                Ok(false)
            } else {
                Err(Error::Corrupt("seal snapshot version".into()))
            };
        };
        let version = *version;
        transaction!(self, |inner| {
            inner
                .conn
                .execute(
                    "DELETE FROM secure_base_transition WHERE scope=? AND (seal<>? OR version<>?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(seal.to_vec()),
                        SqlValue::Integer(version),
                    ]),
                )
                .await?;
            inner
                .conn
                .execute(
                    "INSERT OR IGNORE INTO secure_base_transition VALUES(?,?,?,NULL,?,0)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(seal.to_vec()),
                        SqlValue::Integer(version),
                        SqlValue::Blob(Sets::new().to_bytes()),
                    ]),
                )
                .await?;
            let state = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT cursor,sets,ready FROM secure_base_transition WHERE scope=?",
                        [id],
                    )
                    .await?,
            )
            .await?;
            let Some([cursor, SqlValue::Blob(sets), SqlValue::Integer(ready)]) =
                state.first().map(Vec::as_slice)
            else {
                return Err(Error::Corrupt("base transition".into()));
            };
            if *ready == 1 {
                return Ok(false);
            }
            let mut sets = Sets::from_bytes(sets)?;
            let mut cursor = cursor.clone();
            let records = collect_rows(inner.conn.query(
                "SELECT row,image FROM secure_replayed_seal_rows WHERE scope=?1 AND seal=?2 AND version=?3 AND row>COALESCE(?4,x'') ORDER BY row LIMIT ?5",
                bindings(vec![SqlValue::Integer(id), SqlValue::Blob(seal.to_vec()), SqlValue::Integer(version), cursor.clone(), SqlValue::Integer(BASE_ROWS as i64)]),
            ).await?).await?;
            let images = records
                .iter()
                .map(|record| match record.as_slice() {
                    [SqlValue::Blob(_), SqlValue::Blob(image)] => Ok(image.as_slice()),
                    _ => Err(Error::Corrupt("base transition row".into())),
                })
                .collect::<Result<Vec<_>, _>>()?;
            let ordinals = inner.chunk_field_ordinals(images).await?;
            for record in &records {
                let [SqlValue::Blob(row), SqlValue::Blob(image)] = record.as_slice() else {
                    return Err(Error::Corrupt("base transition row".into()));
                };
                replay::add_row(
                    &mut sets,
                    &checkpoint::row_of(row)?,
                    &ordinals.unpack(image)?,
                );
                cursor = SqlValue::Blob(row.clone());
            }
            #[cfg(any(test, feature = "qualification"))]
            inner
                .flusher
                .measurements
                .work(lock::WorkUnit::TransitionHashRow, records.len() as u64);
            let more = records.len() == BASE_ROWS;
            inner
                .conn
                .execute(
                    "UPDATE secure_base_transition SET cursor=?,sets=?,ready=? WHERE scope=?",
                    bindings(vec![
                        cursor,
                        SqlValue::Blob(sets.to_bytes()),
                        SqlValue::Integer(i64::from(!more)),
                        SqlValue::Integer(id),
                    ]),
                )
                .await?;
            inner.fault(checkpoint::Stage::BasePrepare)?;
            Ok(more)
        })
    }

    pub(super) async fn prepared_base(
        &self,
        id: i64,
        seal: Commitment,
    ) -> Result<Option<Sets>, Error> {
        let record = collect_rows(self.conn.query(
            "SELECT t.sets FROM secure_base_transition t JOIN secure_replayed_seals s ON s.scope=t.scope AND s.seal=t.seal AND s.version=t.version WHERE t.scope=? AND t.seal=? AND t.ready=1",
            bindings(vec![SqlValue::Integer(id), SqlValue::Blob(seal.to_vec())]),
        ).await?).await?;
        match record.first().map(Vec::as_slice) {
            None => Ok(None),
            Some([SqlValue::Blob(sets)]) => Sets::from_bytes(sets).map(Some),
            _ => Err(Error::Corrupt("base transition summary".into())),
        }
    }

    /// In the same transaction as the signed checkpoint. No application rows
    /// change and no redo can run twice after a crash.
    pub(super) async fn publish_prepared_base(
        &self,
        id: i64,
        target: &Finalized,
    ) -> Result<(), Error> {
        self.conn.execute(
            "INSERT INTO secure_replayed_base_source SELECT scope,seal,version FROM secure_base_transition WHERE scope=?1 ON CONFLICT(scope) DO UPDATE SET seal=excluded.seal,version=excluded.version",
            [id],
        ).await?;
        self.conn
            .execute("DELETE FROM secure_base_transition WHERE scope=?", [id])
            .await?;
        self.queue_base_cleanup(id, target.depth).await?;
        #[cfg(any(test, feature = "qualification"))]
        self.flusher
            .measurements
            .work(lock::WorkUnit::TransitionPublication, 1);
        Ok(())
    }

    pub(super) async fn queue_base_cleanup(&self, id: i64, depth: i64) -> Result<(), Error> {
        self.conn.execute(
            "INSERT INTO secure_base_cleanup VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET depth=MAX(depth,excluded.depth)",
            [id, depth],
        ).await?;
        Ok(())
    }

    /// At most BASE_ROWS retired records across all tables, including headers.
    /// The queue and base selector are durable roots; no recovery replay is needed.
    pub(in crate::replica) async fn base_cleanup_step(&mut self) -> Result<bool, Error> {
        let queue = rows(
            &self.conn,
            "SELECT scope,depth FROM secure_base_cleanup ORDER BY scope LIMIT 1",
        )
        .await?;
        let Some([SqlValue::Integer(id), SqlValue::Integer(depth)]) =
            queue.first().map(Vec::as_slice)
        else {
            return if queue.is_empty() {
                Ok(false)
            } else {
                Err(Error::Corrupt("base cleanup".into()))
            };
        };
        let (id, depth) = (*id, *depth);
        let more = transaction!(self, |inner| {
            let mut remaining = BASE_ROWS;
            let deleted = inner.conn.execute(
                "DELETE FROM secure_replayed_base_rows WHERE scope=?1 AND row IN(SELECT row FROM secure_replayed_base_rows WHERE scope=?1 AND EXISTS(SELECT 1 FROM secure_replayed_base_source WHERE scope=?1) LIMIT ?2)",
                [id, remaining as i64],
            ).await? as usize;
            remaining -= deleted;
            if remaining > 0 {
                let retired = collect_rows(inner.conn.query(
                    "SELECT r.seal,r.version FROM secure_replayed_snapshots r WHERE r.scope=?1 AND NOT EXISTS(SELECT 1 FROM secure_replayed_seals s JOIN secure_finalized f ON f.seal=s.seal WHERE s.scope=r.scope AND s.seal=r.seal AND s.version=r.version AND (f.depth>?2 OR f.compacted=0)) AND NOT EXISTS(SELECT 1 FROM secure_replayed_base_source s WHERE s.scope=r.scope AND s.seal=r.seal AND s.version=r.version) AND NOT EXISTS(SELECT 1 FROM secure_base_transition t WHERE t.scope=r.scope AND t.seal=r.seal AND t.version=r.version) ORDER BY r.seal,r.version LIMIT 1",
                    [id, depth],
                ).await?).await?;
                if let Some([SqlValue::Blob(seal), SqlValue::Integer(version)]) =
                    retired.first().map(Vec::as_slice)
                {
                    let params = || {
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(seal.clone()),
                            SqlValue::Integer(*version),
                        ])
                    };
                    // Retire the current-snapshot header before deleting rows,
                    // so no reader mistakes a partial retired snapshot for a full one.
                    let headers = inner.conn.execute(
                        "DELETE FROM secure_replayed_seals WHERE scope=? AND seal=? AND version=?", params(),
                    ).await? as usize;
                    remaining -= headers;
                    let deleted = inner.conn.execute(
                        "DELETE FROM secure_replayed_seal_rows WHERE scope=?1 AND seal=?2 AND version=?3 AND row IN(SELECT row FROM secure_replayed_seal_rows WHERE scope=?1 AND seal=?2 AND version=?3 ORDER BY row LIMIT ?4)",
                        bindings(vec![SqlValue::Integer(id), SqlValue::Blob(seal.clone()), SqlValue::Integer(*version), SqlValue::Integer(remaining as i64)]),
                    ).await? as usize;
                    remaining -= deleted;
                    if remaining > 0 {
                        inner.conn.execute(
                            "DELETE FROM secure_replayed_snapshots WHERE scope=? AND seal=? AND version=?", params(),
                        ).await?;
                        remaining -= 1;
                    }
                    inner.fault(checkpoint::Stage::BaseCleanup)?;
                    #[cfg(any(test, feature = "qualification"))]
                    inner.flusher.measurements.work(
                        lock::WorkUnit::TransitionCleanupRow,
                        (BASE_ROWS - remaining) as u64,
                    );
                    return Ok(true);
                }
            }
            #[cfg(any(test, feature = "qualification"))]
            inner.flusher.measurements.work(
                lock::WorkUnit::TransitionCleanupRow,
                (BASE_ROWS - remaining) as u64,
            );
            if remaining > 0 {
                inner
                    .conn
                    .execute("DELETE FROM secure_base_cleanup WHERE scope=?", [id])
                    .await?;
            }
            inner.fault(checkpoint::Stage::BaseCleanup)?;
            Ok(true)
        })?;
        #[cfg(any(test, feature = "qualification"))]
        self.crash_at(|point| point == CrashPoint::BaseCleaned)?;
        Ok(more)
    }
}
