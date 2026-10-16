//! Rebootstrap: exporting the checkpoint copy in pages, and importing a
//! peer's checkpoint in place of the projection.
use super::retention::*;
use super::*;
use crate::checkpoint::{CheckpointCursor, CheckpointPage, CheckpointRecord};
use crate::digest::SetDigest;
use rocky_core::Commitment;

impl<A> Worker<A>
where
    A: StoredApplication,
    A::State: Clone,
{
    /// One page of the checkpoint copy's rows, while it stands at the record.
    pub(super) async fn export(
        &self,
        after: Option<CheckpointCursor>,
    ) -> Result<CheckpointPage, Error> {
        let retention = &self.retention;
        let Some((record, commitment)) = &retention.record else {
            return Err(Error::Rejected("no checkpoint to export".into()));
        };
        if retention.stored.import.is_some()
            || retention.stored.shadow.as_ref() != Some(&marks(&record.frontier))
        {
            return Err(Error::Rejected(
                "a newer checkpoint is being recomputed; restart the export".into(),
            ));
        }
        let start = match after {
            Some(cursor) if cursor.checkpoint != *commitment => {
                return Err(Error::Rejected(
                    "the checkpoint was superseded; restart the export".into(),
                ));
            }
            Some(cursor) => cursor,
            None => CheckpointCursor {
                checkpoint: *commitment,
                table: 0,
                position: 0,
            },
        };
        let mut page = CheckpointPage {
            start,
            record: None,
            table: start.table,
            rows: Vec::new(),
            next: None,
        };
        if after.is_none() {
            page.record = Some(record.encode());
        }
        let mut cursor = start;
        while let Some((_, table)) = retention
            .checkpointed()
            .find(|(index, _)| *index == cursor.table as usize)
        {
            let name = quoted(&table.shadow);
            let (sql, hidden) = if table.without_rowid {
                (format!("SELECT * FROM {name} LIMIT -1 OFFSET ?"), 0)
            } else {
                (
                    format!("SELECT *,rowid FROM {name} WHERE rowid>? ORDER BY rowid"),
                    1,
                )
            };
            let read = |bytes| {
                self.conn().connection().page(
                    &sql,
                    crate::sqlite::params![cursor.position],
                    if bytes == usize::MAX { 1 } else { EXPORT_ROWS },
                    bytes,
                    hidden,
                )
            };
            let mut result = read(EXPORT_BYTES)?;
            if result.oversized {
                result = read(usize::MAX)?;
            }
            let last = match result.rows.last() {
                Some(row) if hidden == 1 => match row.last() {
                    Some(crate::sqlite::Value::Integer(rowid)) => *rowid,
                    _ => return Err(Error::Corrupt("checkpoint row position".into())),
                },
                _ => cursor.position + result.rows.len() as i64,
            };
            for mut row in result.rows {
                row.truncate(row.len() - hidden);
                page.rows.push(
                    row.into_iter()
                        .map(storage::sql_value)
                        .collect::<Result<_, _>>()?,
                );
            }
            cursor = if result.more {
                CheckpointCursor {
                    position: last,
                    ..cursor
                }
            } else {
                CheckpointCursor {
                    table: cursor.table + 1,
                    position: 0,
                    ..cursor
                }
            };
            // An empty table is skipped, so a page is empty only at the end.
            if !page.rows.is_empty() {
                break;
            }
            page.table = cursor.table;
        }
        if retention
            .checkpointed()
            .any(|(index, _)| index == cursor.table as usize)
        {
            page.next = Some(cursor);
        }
        Ok(page)
    }
    pub(super) async fn clear_shadow(&mut self) -> Result<(), Error> {
        for table in &self.retention.tables {
            self.conn()
                .connection()
                .execute(format!("DELETE FROM {}", quoted(&table.shadow)), ())
                .await?;
        }
        self.retention.rebase_shadow(SetDigest::default())
    }
    /// Stages one page in the checkpoint copy; the last page verifies the
    /// rows and switches the projection to them.
    pub(super) async fn import(
        &mut self,
        page: CheckpointPage,
    ) -> Result<Option<Commitment>, Error> {
        let outcome = self
            .transaction(async |worker| worker.import_page(&page).await)
            .await?;
        let (stored, switched) = outcome;
        self.retention.adopt(stored)?;
        let Some((switched, checkpoint)) = switched else {
            return Ok(None);
        };
        self.frontier = switched.frontier;
        self.lamport = switched.lamport;
        self.revision = switched.revision;
        self.retained = switched.retained.0;
        self.retained_bytes = switched
            .retained
            .1
            .saturating_add(switched.retained.0.saturating_mul(RECORD_OVERHEAD));
        self.retention.prune_wanted = true;
        let mut every = Changed::default();
        every.record(None);
        self.unpublished
            .changed(every, crate::observe::Cause::Rebuild);
        self.unpublished.meta = true;
        self.notify(true);
        Ok(Some(checkpoint))
    }
    pub(super) async fn import_page(
        &mut self,
        page: &CheckpointPage,
    ) -> Result<(Stored, Option<(Switched, Commitment)>), Error> {
        let mut stored = self.retention.stored.clone();
        if let Some(bytes) = &page.record {
            if (page.start.table, page.start.position) != (0, 0) {
                return Err(Error::Rejected("checkpoint record on a later page".into()));
            }
            let record = CheckpointRecord::decode(bytes)?;
            let descriptor = self.app.descriptor();
            if record.commitment() != page.start.checkpoint
                || record.scope != self.identity.scope
                || record.definition != self.retention.definition
                || record.versions != descriptor.operations
                || record.projection != descriptor.projection
                || record
                    .frontier
                    .0
                    .iter()
                    .any(|(node, _)| !self.config.authors.iter().any(|(author, _)| author == node))
            {
                return Err(Error::Incompatible(
                    "checkpoint is for another domain or definition".into(),
                ));
            }
            let covered = marks(&record.frontier);
            if stored
                .pruned_prefix
                .iter()
                .any(|(node, sequence)| at(&covered, *node) < *sequence)
            {
                return Err(Error::Rejected(
                    "checkpoint does not cover operations this replica has pruned".into(),
                ));
            }
            self.clear_shadow().await?;
            stored.shadow = None;
            stored.candidate = None;
            stored.import = Some(Import {
                record: bytes.clone(),
                table: 0,
                position: 0,
            });
        }
        let Some(import) = &mut stored.import else {
            return Err(Error::Rejected("no checkpoint import in progress".into()));
        };
        let record = CheckpointRecord::decode(&import.record)?;
        let checkpoint = record.commitment();
        if page.start.checkpoint != checkpoint {
            return Err(Error::Rejected(
                "page of another checkpoint; restart the import".into(),
            ));
        }
        if (page.start.table, page.start.position) != (import.table, import.position) {
            // A repeated page is already staged; anything else is a gap.
            return if (page.start.table, page.start.position) < (import.table, import.position) {
                Ok((self.retention.stored.clone(), None))
            } else {
                Err(Error::Rejected("checkpoint page out of order".into()))
            };
        }
        if !page.rows.is_empty() {
            let Some((_, table)) = self
                .retention
                .checkpointed()
                .find(|(index, _)| *index == page.table as usize)
            else {
                return Err(Error::Rejected(
                    "checkpoint page for an unknown table".into(),
                ));
            };
            let name = quoted(&table.shadow);
            for row in &page.rows {
                let placeholders = vec!["?"; row.len()].join(",");
                self.conn()
                    .connection()
                    .execute(
                        format!("INSERT INTO {name} VALUES({placeholders})"),
                        bindings(row.clone()),
                    )
                    .await?;
            }
        }
        if let Some(next) = page.next {
            import.table = next.table;
            import.position = next.position;
            self.persist(&stored).await?;
            self.inject_fault().await?;
            return Ok((stored, None));
        }
        if self.retention.current()?.1 != record.state {
            return Err(Error::Corrupt(
                "checkpoint rows differ from the record's commitment".into(),
            ));
        }
        let switched = self.switch(&mut stored, &record).await?;
        self.persist(&stored).await?;
        self.inject_fault().await?;
        Ok((stored, Some((switched, checkpoint))))
    }
    /// Replaces the projection with the staged checkpoint. Operations it
    /// covers are dropped; the others, local ones included, become pending
    /// and are applied again on top of it.
    pub(super) async fn switch(
        &mut self,
        stored: &mut Stored,
        record: &CheckpointRecord,
    ) -> Result<Switched, Error> {
        let raw = self.conn().connection();
        for (_, table) in self.retention.checkpointed() {
            let (live, shadow) = (quoted(&table.live), quoted(&table.shadow));
            raw.execute(format!("DELETE FROM {live}"), ()).await?;
            raw.execute(format!("INSERT INTO {live} SELECT * FROM {shadow}"), ())
                .await?;
        }
        self.retention.rebase_live(record.state)?;
        let covered = marks(&record.frontier);
        for (node, sequence) in &covered {
            let key = crate::sqlite::params![node.0.to_vec(), *sequence as i64];
            for sql in [
                "DELETE FROM rocky_operations WHERE node=? AND sequence<=?",
                "DELETE FROM rocky_peer_ack WHERE node=? AND sequence<=?",
                "DELETE FROM rocky_outbox WHERE node=? AND sequence<=?",
            ] {
                self.conn().execute(sql, key.clone()).await?;
            }
        }
        self.conn()
            .execute(
                "UPDATE rocky_operations SET status='pending' WHERE status='applied'",
                (),
            )
            .await?;
        self.conn()
            .execute("DELETE FROM rocky_rejected_local", ())
            .await?;
        let lamport = self.lamport.max(record.clock);
        let revision = self.revision.next()?;
        self.conn()
            .execute(
                "UPDATE rocky_replica SET lamport=?,revision=? WHERE id=1",
                [lamport as i64, revision.get() as i64],
            )
            .await?;
        stored.shadow = Some(covered.clone());
        stored.clock = record.clock;
        stored.history = (record.history.sum, record.history.count);
        stored.record = Some(record.encode());
        stored.candidate = None;
        stored.pruned_prefix = covered;
        stored.import = None;
        let budget = rows(
            self.conn(),
            "SELECT COUNT(*),COALESCE(SUM(LENGTH(bytes)),0) FROM rocky_operations",
        )
        .await?;
        let Some([SqlValue::Integer(count), SqlValue::Integer(bytes)]) =
            budget.first().map(Vec::as_slice)
        else {
            return Err(Error::Corrupt("history budget".into()));
        };
        Ok(Switched {
            frontier: record.frontier.0.iter().copied().collect(),
            lamport,
            revision,
            retained: (*count as usize, *bytes as usize),
        })
    }
}
