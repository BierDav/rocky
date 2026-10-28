//! Validated accounting over bounded input pages, including support history.
use super::*;

/// Input records per acquisition; provisional until phone calibration.
const STORAGE_RECORDS: usize = if cfg!(test) { 1 } else { 32 };
const TABLES: [&str; 4] = [
    "secure_migration_publications",
    "secure_migration_frames",
    "secure_migration_nodes",
    "secure_existence_log",
];

struct Source {
    writes: Arc<crate::sqlite::Writes>,
    tables: [String; 4],
    versions: [u64; 4],
}
impl Source {
    fn check(&self) -> Result<(), Error> {
        if self
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

fn add(total: &mut u64, amount: u64) -> Result<(), Error> {
    *total = total
        .checked_add(amount)
        .ok_or_else(|| denied("migration support overflow"))?;
    Ok(())
}

impl<A: RowApplication> EncryptedReplica<A> {
    #[cfg(test)]
    pub(crate) async fn pause_migration_storage_for_test(
        &self,
        reached: tokio::sync::oneshot::Sender<()>,
        resume: tokio::sync::oneshot::Receiver<()>,
    ) {
        self.inner
            .lock(Class::Background)
            .await
            .migration_storage_pause = Some((reached, resume));
    }

    async fn storage_page(
        &self,
        source: &Source,
        query: &str,
        mut parameters: Vec<SqlValue>,
    ) -> Result<Vec<Vec<SqlValue>>, Error> {
        tokio::task::yield_now().await;
        let records = {
            let inner = self.inner.lock(Class::Background).await;
            source.check()?;
            parameters.push(SqlValue::Integer(STORAGE_RECORDS as i64));
            let records =
                collect_rows(inner.conn.query(query, bindings(parameters)).await?).await?;
            #[cfg(any(test, feature = "qualification"))]
            inner.flusher.measurements.work(
                crate::replica::lock::WorkUnit::MigrationStorageRecord,
                records.len() as u64,
            );
            records
        };
        #[cfg(test)]
        if !records.is_empty() {
            let pause = self
                .inner
                .lock(Class::Background)
                .await
                .migration_storage_pause
                .take();
            if let Some((reached, resume)) = pause {
                let _ = reached.send(());
                let _ = resume.await;
            }
        }
        Ok(records)
    }

    /// Storage charged to authenticated migration history, separate from ordinary history.
    pub async fn migration_storage(&self) -> Result<MigrationStorage, Error> {
        let source = {
            let inner = self.inner.lock(Class::Background).await;
            let tables = TABLES.map(|table| {
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
            }
        };
        let mut totals = MigrationStorage {
            frames: 0,
            bytes: 0,
            support_bytes: 0,
        };
        let mut ranges = BTreeMap::<(Vec<u8>, i64), Vec<i64>>::new();
        let mut after = Vec::new();
        loop {
            let page = self.storage_page(&source,
                "SELECT publication,statement,retained_frames,retained_bytes,length(statement)+coalesce(length(signed),0)+length(writer)+coalesce(length(conflict),0)+length(publication)+length(tip) FROM secure_migration_publications WHERE publication>? ORDER BY publication LIMIT ?",
                vec![SqlValue::Blob(after.clone())]).await?;
            if page.is_empty() {
                break;
            }
            for row in page {
                let [
                    SqlValue::Blob(publication),
                    SqlValue::Blob(statement),
                    frames,
                    bytes,
                    support,
                ] = row.as_slice()
                else {
                    return Err(Error::Corrupt("migration support account".into()));
                };
                add(&mut totals.frames, number(frames)?)?;
                add(&mut totals.bytes, number(bytes)?)?;
                add(&mut totals.support_bytes, number(support)?)?;
                let statement: MigrationPublicationStatement = decode(statement)?;
                if let Some(last) = statement.frames.checked_sub(1) {
                    ranges
                        .entry((statement.writer.to_vec(), i64::from(statement.author)))
                        .or_default()
                        .push(i64::try_from(last).unwrap_or(i64::MAX));
                }
                after = publication.clone();
            }
        }
        for bounds in ranges.values_mut() {
            bounds.sort_unstable();
        }

        after.clear();
        loop {
            let page = self.storage_page(&source,
                "SELECT hash,length(proof)+length(hash)+length(publication)+16 FROM secure_migration_frames WHERE hash>? ORDER BY hash LIMIT ?",
                vec![SqlValue::Blob(after.clone())]).await?;
            if page.is_empty() {
                break;
            }
            for row in page {
                let [SqlValue::Blob(hash), support] = row.as_slice() else {
                    return Err(Error::Corrupt("migration support account".into()));
                };
                add(&mut totals.support_bytes, number(support)?)?;
                after = hash.clone();
            }
        }

        let mut cursor = vec![
            SqlValue::Blob(Vec::new()),
            SqlValue::Integer(0),
            SqlValue::Integer(0),
        ];
        loop {
            let page = self.storage_page(&source,
                "SELECT publication,start,frames,length(hash)+length(publication)+24 FROM secure_migration_nodes WHERE (publication,start,frames)>(?,?,?) ORDER BY publication,start,frames LIMIT ?",
                cursor.clone()).await?;
            if page.is_empty() {
                break;
            }
            for row in page {
                let [
                    SqlValue::Blob(_),
                    SqlValue::Integer(_),
                    SqlValue::Integer(_),
                    support,
                ] = row.as_slice()
                else {
                    return Err(Error::Corrupt("migration support account".into()));
                };
                add(&mut totals.support_bytes, number(support)?)?;
                cursor = row[..3].to_vec();
            }
        }

        if ranges.is_empty() {
            return Ok(totals);
        }
        cursor = vec![
            SqlValue::Blob(Vec::new()),
            SqlValue::Blob(Vec::new()),
            SqlValue::Integer(-1),
        ];
        loop {
            let page = self.storage_page(&source,
                "SELECT scope,signer,sequence,author,position,length(record)+length(scope)+length(signer)+length(hash)+32 FROM secure_existence_log WHERE (scope,signer,sequence)>(?,?,?) ORDER BY scope,signer,sequence LIMIT ?",
                cursor.clone()).await?;
            // Even an empty final page validates every table before returning.
            if page.is_empty() {
                return Ok(totals);
            }
            for row in page {
                let [
                    SqlValue::Blob(_),
                    SqlValue::Blob(signer),
                    SqlValue::Integer(_),
                    SqlValue::Integer(author),
                    SqlValue::Integer(position),
                    support,
                ] = row.as_slice()
                else {
                    return Err(Error::Corrupt("migration support account".into()));
                };
                if *position >= 0
                    && let Some(bounds) = ranges.get(&(signer.clone(), *author))
                {
                    // Preserve the charge per publication, including overlapping extents.
                    let copies = bounds.len() - bounds.partition_point(|last| last < position);
                    let bytes = number(support)?
                        .checked_mul(copies as u64)
                        .ok_or_else(|| denied("migration support overflow"))?;
                    add(&mut totals.support_bytes, bytes)?;
                }
                cursor = row[..3].to_vec();
            }
        }
    }
}
