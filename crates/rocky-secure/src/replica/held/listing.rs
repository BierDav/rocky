//! A held listing reads one live row at a time, then validates the source.
use super::*;

/// One header or current row per acquisition; provisional until phone sizing.
const LISTING_ROWS: usize = 1;

struct Source {
    writes: Arc<crate::sqlite::Writes>,
    tables: [String; 2],
    versions: [u64; 2],
    authority: u64,
    tip: TipId,
    manifest: Commitment,
}
impl Source {
    fn check<A: RowApplication>(&self, inner: &Inner<A>) -> Result<(), Error> {
        if self.authority != inner.authority
            || self.tip != inner.workspace.tip
            || self.manifest != inner.manifest
            || self
                .tables
                .iter()
                .zip(self.versions)
                .any(|(table, version)| self.writes.table_count(table) != version)
        {
            return Err(Error::RetryExhausted);
        }
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub(super) async fn list_held_changes(&self) -> Result<Vec<HeldChange>, Error> {
        let source = {
            let mut inner = self.inner.lock(Class::Background).await;
            inner = flush_guard(inner, &self.inner).await?;
            let tables = ["secure_held", "secure_rows"].map(|table| {
                inner
                    .conn
                    .names()
                    .physical(table)
                    .unwrap_or(table)
                    .to_owned()
            });
            let writes = inner
                .conn
                .connection()
                .watch_writes(tables.iter().map(|table| (table.clone(), true)))?;
            Source {
                versions: tables.each_ref().map(|table| writes.table_count(table)),
                writes,
                tables,
                authority: inner.authority,
                tip: inner.workspace.tip,
                manifest: inner.manifest,
            }
        };
        let mut after = Vec::new();
        let mut changes = Vec::new();
        loop {
            tokio::task::yield_now().await;
            let records = {
                let inner = self.inner.lock(Class::Background).await;
                source.check(&inner)?;
                let records = collect_rows(
                    inner
                        .conn
                        .query(
                            "SELECT id,change FROM secure_held WHERE id>? ORDER BY id LIMIT ?",
                            bindings(vec![
                                SqlValue::Blob(after.clone()),
                                SqlValue::Integer(LISTING_ROWS as i64),
                            ]),
                        )
                        .await?,
                )
                .await?;
                #[cfg(any(test, feature = "qualification"))]
                inner.flusher.measurements.work(
                    crate::replica::lock::WorkUnit::HeldListingRow,
                    records.len() as u64,
                );
                records
            };
            if records.is_empty() {
                // The final acquisition validated both the held set and every
                // current row used in the listing before returning it.
                return Ok(changes);
            }
            #[cfg(test)]
            {
                let pause = self
                    .inner
                    .lock(Class::Background)
                    .await
                    .held_listing_pause
                    .take();
                if let Some((reached, resume)) = pause {
                    let _ = reached.send(());
                    let _ = resume.await;
                }
            }
            for record in records {
                let [SqlValue::Blob(id), SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(Error::Corrupt("held change".into()));
                };
                after = id.clone();
                // A stored bundle can be large; decoding and accumulating its
                // result need neither the replica nor a database transaction.
                let stored: Stored = decode_bounded(bytes, MAX_BUNDLE_BYTES * 2)?;
                let mut rows = Vec::with_capacity(stored.rows.len());
                for row in stored.rows {
                    tokio::task::yield_now().await;
                    let inner = self.inner.lock(Class::Background).await;
                    source.check(&inner)?;
                    #[cfg(any(test, feature = "qualification"))]
                    inner
                        .flusher
                        .measurements
                        .work(crate::replica::lock::WorkUnit::HeldListingRow, 1);
                    rows.push(inner.held_row(row).await?);
                }
                changes.push(HeldChange {
                    id: id
                        .as_slice()
                        .try_into()
                        .map_err(|_| Error::Corrupt("held change".into()))?,
                    time: stored.time,
                    rows,
                });
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn pause_held_listing_for_test(
        &self,
        reached: tokio::sync::oneshot::Sender<()>,
        resume: tokio::sync::oneshot::Receiver<()>,
    ) {
        self.inner.lock(Class::Background).await.held_listing_pause = Some((reached, resume));
    }
}
