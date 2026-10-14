//! Removal: generations that drop tables and fields, or add a removed name
//! again. Stored rows, the verified checkpoint base and incoming frames are
//! read through the lineage's names the same way on every device, so stored
//! rows stay a replay of the base and the frames after it. See
//! `docs/runtime/removal.md`.
use super::checkpoint::Sets;
use super::generation::Names;
use super::*;

pub(super) const SCHEMA: &[&str] = &[
    // Scopes rebuilt from their checkpoint base and retained frames once a
    // removed name is added again or its baseline moves earlier.
    "CREATE TABLE IF NOT EXISTS secure_repull(scope BLOB PRIMARY KEY) WITHOUT ROWID",
];

/// Counts live stored rows by table, including constraint-suppressed rows
/// and every locally held partition. Reads one snapshot without opening a writer.
pub async fn local_row_counts(path: &Path) -> Result<BTreeMap<Commitment, u64>, Error> {
    super::check_replica_format(path).await?;
    let file = crate::sqlite::Connection::open_read_only(path)?;
    let mut counts = BTreeMap::<Commitment, u64>::new();
    for row in file.rows(
        "SELECT s.scope,count(r.fields) FROM secure_scope_state s LEFT JOIN secure_rows r ON r.scope=s.scope GROUP BY s.id,s.scope",
        (),
    )? {
        let [crate::sqlite::Value::Blob(scope), crate::sqlite::Value::Integer(count)] = row.as_slice() else {
            return Err(Error::Corrupt("local row counts".into()));
        };
        let scope: ResourceScope = decode(scope)?;
        let count = u64::try_from(*count).map_err(|_| Error::Corrupt("local row count".into()))?;
        let total = counts.entry(scope.table).or_default();
        *total = total.checked_add(count).ok_or_else(|| Error::Corrupt("local row count overflow".into()))?;
    }
    Ok(counts)
}

impl<A: RowApplication> Inner<A> {
    /// Moves stored state from the names `before` to the current ones,
    /// inside the caller's transaction: a removed table's rows go, and every
    /// other scope keeps only what the current names admit. Scopes of a
    /// re-added name are queued to pull their frames again.
    pub(super) async fn apply_names(&self, before: &Names) -> Result<(), Error> {
        if *before == self.names {
            return Ok(());
        }
        for (id, scope) in self.stored_scopes().await? {
            if self.names.retired_tables.contains(&scope.table) {
                self.drop_scope(id, scope).await?;
                continue;
            }
            // The projection plan that follows rebuilds the tables.
            self.keep_current(id, scope, false).await?;
            if self.names.pulls_again(before, scope.table) {
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO secure_repull VALUES(?)",
                        [encode(&scope)?],
                    )
                    .await?;
            }
        }
        Ok(())
    }
    /// Every scope this replica holds state for.
    async fn stored_scopes(&self) -> Result<Vec<(i64, ResourceScope)>, Error> {
        let mut scopes = Vec::new();
        for row in rows(&self.conn, "SELECT id,scope FROM secure_scope_state").await? {
            let [SqlValue::Integer(id), SqlValue::Blob(scope)] = row.as_slice() else {
                return Err(Error::Corrupt("scope state".into()));
            };
            scopes.push((*id, decode(scope)?));
        }
        Ok(scopes)
    }
    /// A removed table's stored rows, subscription and retention go. Its
    /// signed history and checkpoint stay, so a generation that adds it
    /// again pulls them again.
    async fn drop_scope(&self, id: i64, scope: ResourceScope) -> Result<(), Error> {
        let blob = encode(&scope)?;
        for sql in [
            "DELETE FROM secure_rows WHERE scope=?",
            "DELETE FROM secure_row_keys WHERE scope=?",
            "DELETE FROM secure_move_rows WHERE scope=?",
            "DELETE FROM secure_subscriptions WHERE scope=?",
            "DELETE FROM secure_retention WHERE scope=?",
        ] {
            self.conn.execute(sql, [blob.clone()]).await?;
        }
        self.touch_scope(scope);
        self.recount(id, scope).await
    }
    /// Strips what the current names no longer admit from a scope's stored
    /// rows and from its verified checkpoint base, which is sealed again
    /// when it changes; with `project`, the rows project again. Returns
    /// whether anything changed.
    pub(super) async fn keep_current(
        &self,
        id: i64,
        scope: ResourceScope,
        project: bool,
    ) -> Result<bool, Error> {
        let blob = encode(&scope)?;
        let stored = self
            .strip_rows(
                "SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT 256",
                "UPDATE secure_rows SET fields=? WHERE scope=? AND row=?",
                "DELETE FROM secure_rows WHERE scope=? AND row=?",
                SqlValue::Blob(blob.clone()),
                scope,
            )
            .await?;
        if stored {
            self.conn
                .execute(
                    "DELETE FROM secure_row_keys WHERE scope=?1 AND row NOT IN(SELECT row FROM secure_rows WHERE scope=?1)",
                    [blob.clone()],
                )
                .await?;
            self.recount(id, scope).await?;
            if project {
                self.reproject(scope).await?;
            }
        }
        Ok(self.rebase(id, scope).await? || stored)
    }
    /// Rewrites each row a `select` page returns as the current names keep
    /// it. Returns whether any changed.
    async fn strip_rows(
        &self,
        select: &str,
        update: &str,
        delete: &str,
        key: SqlValue,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        let (mut after, mut changed) = (Vec::new(), false);
        loop {
            let page = collect_rows(
                self.conn
                    .query(
                        select,
                        bindings(vec![key.clone(), SqlValue::Blob(after.clone())]),
                    )
                    .await?,
            )
            .await?;
            for record in &page {
                let [SqlValue::Blob(row), SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(Error::Corrupt("stored row".into()));
                };
                after = row.clone();
                let mut record: Materialized = decode(bytes)?;
                let target = vec![key.clone(), SqlValue::Blob(row.clone())];
                if self.names.before_readding(scope.table, &record) {
                    self.conn.execute(delete, bindings(target)).await?;
                    changed = true;
                } else if self.names.strip(&mut record) {
                    let mut values = vec![SqlValue::Blob(encode(&record)?)];
                    values.extend(target);
                    self.conn.execute(update, bindings(values)).await?;
                    changed = true;
                }
            }
            if page.len() < 256 {
                return Ok(changed);
            }
        }
    }
    /// Strips the verified checkpoint base and seals it again under the
    /// current definition, so a replay over it reproduces the stripped
    /// stored rows. Unfinished checkpoints start over. Returns whether it
    /// changed.
    async fn rebase(&self, id: i64, scope: ResourceScope) -> Result<bool, Error> {
        let Some(record) = self.checkpoint_record(id, true).await? else {
            return Ok(false);
        };
        let Some(previous) = record.signed.clone() else {
            return Ok(false);
        };
        let changed = self
            .strip_rows(
                "SELECT row,fields FROM secure_checkpoint_rows WHERE scope=? AND row>? ORDER BY row LIMIT 256",
                "UPDATE secure_checkpoint_rows SET fields=? WHERE scope=? AND row=?",
                "DELETE FROM secure_checkpoint_rows WHERE scope=? AND row=?",
                SqlValue::Integer(id),
                scope,
            )
            .await?;
        if !changed {
            return Ok(false);
        }
        let sets = self
            .counted(id, "secure_checkpoint_rows", record.sets)
            .await?;
        // It names the generation its signer's credentials name.
        let mut statement = previous.statement.clone();
        statement.definition = self.signing_definition(scope.table);
        let generation = self.generation_of(statement.definition, false)?;
        statement.schema = generation.schema;
        statement.handler = generation.handler;
        statement.sequence = statement
            .sequence
            .checked_add(1)
            .ok_or_else(|| denied("checkpoint sequence exhausted"))?;
        statement.previous = Some(previous.id()?);
        statement.rows = sets.rows_summary();
        statement.tombstones = sets.tombstones;
        let signed = self.sign_checkpoint(statement)?;
        for sql in [
            "DELETE FROM secure_checkpoint_pages WHERE scope=?",
            "DELETE FROM secure_checkpoint_overlay WHERE scope=?",
            "DELETE FROM secure_checkpoints WHERE scope=? AND state<>'verified'",
        ] {
            self.conn.execute(sql, [id]).await?;
        }
        self.conn
            .execute(
                "UPDATE secure_checkpoints SET sets=?,signed=? WHERE scope=? AND state='verified' AND sequence=?",
                bindings(vec![
                    SqlValue::Blob(sets.to_bytes()),
                    SqlValue::Blob(signed.to_bytes()?),
                    SqlValue::Integer(id),
                    SqlValue::Integer(record.sequence),
                ]),
            )
            .await?;
        Ok(true)
    }
    /// `sets` with the row counts and digest of `table`'s rows of scope
    /// `id`; creations are kept.
    async fn counted(&self, id: i64, table: &str, sets: Sets) -> Result<Sets, Error> {
        let mut counted = Sets::new();
        counted.creations = sets.creations;
        counted.creation_digest = sets.creation_digest;
        let mut after = Vec::new();
        loop {
            let page = collect_rows(
                self.conn
                    .query(
                        format!("SELECT row,fields FROM {table} WHERE scope=? AND row>? ORDER BY row LIMIT 256"),
                        bindings(vec![SqlValue::Integer(id), SqlValue::Blob(after.clone())]),
                    )
                    .await?,
            )
            .await?;
            for record in &page {
                let [SqlValue::Blob(row), SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(Error::Corrupt("checkpoint row".into()));
                };
                after = row.clone();
                counted.replace(&checkpoint::row_of(row)?, None, bytes)?;
            }
            if page.len() < 256 {
                return Ok(counted);
            }
        }
    }
    /// Recomputes a scope's row counts and digest from its stored rows.
    async fn recount(&self, id: i64, scope: ResourceScope) -> Result<(), Error> {
        let blob = encode(&scope)?;
        let mut sets = Sets::new();
        let mut after = Vec::new();
        loop {
            let page = collect_rows(
                self.conn
                    .query(
                        "SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT 256",
                        bindings(vec![SqlValue::Blob(blob.clone()), SqlValue::Blob(after.clone())]),
                    )
                    .await?,
            )
            .await?;
            for record in &page {
                let [SqlValue::Blob(row), SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(Error::Corrupt("stored row".into()));
                };
                after = row.clone();
                sets.replace(&checkpoint::row_of(row)?, None, bytes)?;
            }
            if page.len() < 256 {
                break;
            }
        }
        let applied = self.scope_state(id).await?.3;
        self.store_scope_state(id, sets.rows, sets.tombstones, &sets.digest, applied)
            .await
    }
    /// The projection of a removed table is gone with its schema; tell
    /// observers it changed.
    fn touch_scope(&self, scope: ResourceScope) {
        if let Some(table) = self
            .typed
            .as_ref()
            .and_then(|typed| typed.rows.table(&scope.table))
        {
            self.touch(&table.sql_name);
        }
    }
    /// Rebuilds each queued scope from its checkpoint base and the retained
    /// frames after it, one transaction per scope: writes to a re-added
    /// name stamped after its baseline apply again. A scope this replica
    /// cannot read waits in the queue.
    pub(super) async fn pull_again(&mut self) -> Result<(), Error> {
        let queued = rows(&self.conn, "SELECT scope FROM secure_repull").await?;
        for row in queued {
            let [SqlValue::Blob(blob)] = row.as_slice() else {
                return Err(Error::Corrupt("pull-again queue".into()));
            };
            let scope: ResourceScope = decode(blob)?;
            if !self.names.retired_tables.contains(&scope.table)
                && !self.config.keys.iter().any(|key| key.scope == scope)
            {
                continue;
            }
            self.settle(scope).await?;
            transaction!(self, |inner| {
                if !inner.names.retired_tables.contains(&scope.table) {
                    inner.rebuild_rows(scope).await?;
                }
                inner
                    .conn
                    .execute("DELETE FROM secure_repull WHERE scope=?", [blob.clone()])
                    .await?;
                Ok(())
            })?;
        }
        Ok(())
    }
}
