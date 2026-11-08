//! Replayed tables keep no row records: their frames are their history, so
//! a rebuild empties them and replays every visible member again.
use super::*;
use rocky_credentials::SignedSeal;

/// Own outcomes kept after their records left, newest first; older ones
/// read as unknown.
pub(in crate::replica) const KEPT_OUTCOMES: i64 = 4096;

/// What `rederive_replayed` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::replica) enum Rederived {
    /// No clear left rows to derive again.
    Unmarked,
    /// The domain's replayed rows were derived again.
    Done,
    /// A marked scope still needs something first.
    Waiting(&'static str),
    /// A rebuild of the same inputs left a foreign key violated; it is not
    /// tried again until they change.
    Stuck(String),
}
impl Rederived {
    /// Why seal finalization and compaction wait, if they do.
    pub(in crate::replica) fn waits(&self) -> Option<String> {
        match self {
            Self::Unmarked | Self::Done => None,
            Self::Waiting(reason) => Some(format!(
                "the replayed rows wait to be derived again: {reason}"
            )),
            Self::Stuck(violation) => Some(format!(
                "the replayed rows cannot be derived again: {violation}"
            )),
        }
    }
}

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    /// Marks `scope` as a clear that reached other scopes does.
    pub(crate) async fn mark_rederive_for_test(&self, scope: ResourceScope) -> Result<(), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        let id = inner.find_scope_id(scope).await?.ok_or(Error::Closed)?;
        inner
            .conn
            .execute("INSERT INTO secure_replayed_rederive VALUES(?)", [id])
            .await?;
        inner.rederive_marked = true;
        Ok(())
    }
}

impl<A: RowApplication> Inner<A> {
    /// Forgets replayed scope `scope`'s replay, inside the caller's
    /// transaction: its rows (its partition's, or the whole unpartitioned
    /// table) and where its members stood. Outcomes of members this node
    /// authored stay readable, with the finality they had.
    ///
    /// SQLite runs delete actions inside the transaction and cannot switch
    /// foreign keys off there, so clearing a parent can change rows of other
    /// scopes. Putting them back as they stood would not match a replica
    /// that never cleared: the parent's refill may delete, recreate or rekey
    /// it, and an evicted parent leaves its children as a replica that never
    /// held it replays them. When the clear changed any row outside the
    /// scope, the scope is marked: once nothing it needs waits,
    /// `rederive_replayed` rebuilds the domain's replayed rows from bases and
    /// frames in canonical order, as a fresh replica holding the same scopes
    /// computes them. Seals do not finalize meanwhile.
    pub(in crate::replica) async fn clear_replayed(
        &mut self,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        let id = self.find_scope_id(scope).await?;
        if let Some(id) = id {
            self.keep_outcomes(id).await?;
            self.conn
                .execute("DELETE FROM secure_replayed WHERE scope=?", [id])
                .await?;
        }
        // A partition whose tuple never arrived holds no rows.
        let Some((table, condition, values)) = self.scope_rows(scope)? else {
            return Ok(());
        };
        self.touch(&table);
        let quoted = typed::quoted(&table);
        let held = self
            .count(
                &format!("SELECT COUNT(*) FROM {quoted} WHERE {condition}"),
                values.clone(),
            )
            .await?;
        let before = self.conn.connection().total_changes()?;
        self.conn
            .execute(
                format!("DELETE FROM {quoted} WHERE {condition}"),
                bindings(values),
            )
            .await?;
        // Delete actions and the `SET NULL` triggers count in the
        // connection's total changes: more than the scope's rows reached
        // others. (The SQL function is replaced for replay.)
        let changed = self.conn.connection().total_changes()? - before;
        if let Some(id) = id.filter(|_| changed > held as u64) {
            self.conn
                .execute(
                    "INSERT INTO secure_replayed_rederive VALUES(?) ON CONFLICT DO NOTHING",
                    [id],
                )
                .await?;
            self.rederive_marked = true;
        }
        Ok(())
    }
    /// A reset can retain canonical seal images without applied frames or a base.
    pub(in crate::replica) async fn holds_replayed_rows(
        &self,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        let Some((table, condition, values)) = self.scope_rows(scope)? else {
            return Ok(false);
        };
        Ok(self
            .count(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {} WHERE {condition})",
                    typed::quoted(&table)
                ),
                values,
            )
            .await?
            != 0)
    }
    async fn count(&self, sql: &str, values: Vec<SqlValue>) -> Result<i64, Error> {
        match collect_rows(self.conn.query(sql, bindings(values)).await?)
            .await?
            .first()
            .map(Vec::as_slice)
        {
            Some([SqlValue::Integer(count)]) => Ok(*count),
            _ => Err(Error::Corrupt("replayed row count".into())),
        }
    }
    /// Moves the records of scope `id`'s members this node authored to
    /// `secure_kept_outcomes`, keeping what a base decided for a member,
    /// and keeps only the newest `KEPT_OUTCOMES`.
    async fn keep_outcomes(&self, id: i64) -> Result<(), Error> {
        let Some(author) = self.node_author.as_ref().map(|author| author.number) else {
            return Ok(());
        };
        self.conn
            .execute(
                "INSERT INTO secure_kept_outcomes SELECT hash,txn,scope,clock,sequence,dropped,sealed,violated,0 FROM secure_replayed WHERE scope=? AND author=? ON CONFLICT(hash) DO UPDATE SET dropped=excluded.dropped,sealed=excluded.sealed,violated=excluded.violated WHERE secure_kept_outcomes.based=0",
                [id, i64::from(author)],
            )
            .await?;
        self.prune_kept_outcomes(KEPT_OUTCOMES).await
    }
    /// Keeps the newest `limit` kept outcomes.
    pub(crate) async fn prune_kept_outcomes(&self, limit: i64) -> Result<(), Error> {
        self.conn
            .execute(
                "DELETE FROM secure_kept_outcomes WHERE hash IN(SELECT hash FROM secure_kept_outcomes ORDER BY clock DESC,hash LIMIT -1 OFFSET ?)",
                [limit],
            )
            .await?;
        Ok(())
    }
    /// The scope's replayed table and the condition selecting its rows;
    /// none for a partition whose tuple this replica never learned.
    fn scope_rows(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<(String, String, Vec<SqlValue>)>, Error> {
        let corrupt = || Error::Corrupt("replayed scope".into());
        let typed = self.workspace.typed.as_ref().ok_or_else(corrupt)?;
        let projected = typed.by_scope(scope).ok_or_else(corrupt)?;
        let schema = typed.rows.schema();
        let logical = schema
            .table(&projected.table.sql_name)
            .ok_or_else(corrupt)?;
        if logical.partition_by.is_empty() {
            return Ok(Some((logical.sql_name.clone(), "1".into(), Vec::new())));
        }
        let Some(tuple) = self.partitions.get(&scope) else {
            return Ok(None);
        };
        let condition = logical
            .partition_by
            .iter()
            .map(|column| format!("{}=?", typed::quoted(column)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let values = super::super::partition::partition_values(schema, logical, tuple)?;
        Ok(Some((logical.sql_name.clone(), condition, values)))
    }
    /// Inside the caller's transaction, rebuilds the replayed rows of the
    /// followed domain from bases and frames once no scope a clear marked
    /// still waits: none needs a seal rebootstrap, holds pending frames or
    /// is being evicted, and no direct import other than `importing` fills
    /// one.
    ///
    /// The rebuild is everything canonical replay derives from what this
    /// device holds, so it equals a fresh replica holding the same scopes:
    /// a member that references a row of a scope not held drops, as there.
    /// Only a restored base row whose parent the rebuild lacks can leave a
    /// foreign key violated, a state no fresh replica commits either; then
    /// the rebuild rolls back and is stuck until its inputs change.
    pub(in crate::replica) async fn rederive_replayed(
        &self,
        importing: Option<ResourceScope>,
        time: u64,
    ) -> Result<Rederived, Error> {
        let corrupt = || Error::Corrupt("rederived scope".into());
        let marked = rows(
            &self.conn,
            "SELECT r.scope,s.scope FROM secure_replayed_rederive r LEFT JOIN secure_scope_state s ON s.id=r.scope",
        )
        .await?;
        if marked.is_empty() {
            return Ok(Rederived::Unmarked);
        }
        let Some(typed) = self.workspace.typed.as_ref() else {
            return Ok(Rederived::Waiting("the replayed tables are not installed"));
        };
        let domain = typed.domain;
        let waiting: BTreeSet<ResourceScope> = self
            .seal_rebootstraps(domain)
            .await?
            .into_iter()
            .map(|need| need.scope)
            .collect();
        let mut cleared = Vec::new();
        for record in &marked {
            let (id, blob) = match record.as_slice() {
                [SqlValue::Integer(id), SqlValue::Blob(blob)] => (*id, blob),
                // A scope no longer stored has nothing to wait for.
                [SqlValue::Integer(id), SqlValue::Null] => {
                    cleared.push(*id);
                    continue;
                }
                _ => return Err(corrupt()),
            };
            let scope: ResourceScope = decode(blob)?;
            if scope.domain != domain {
                continue;
            }
            cleared.push(id);
            if waiting.contains(&scope) {
                return Ok(Rederived::Waiting("a scope needs a seal rebootstrap"));
            }
            if !collect_rows(
                self.conn
                    .query(
                        "SELECT 1 FROM secure_operations WHERE scope=? AND status='pending' LIMIT 1",
                        [blob.clone()],
                    )
                    .await?,
            )
            .await?
            .is_empty()
            {
                return Ok(Rederived::Waiting("a scope holds pending frames"));
            }
            if self.holding(scope).await? == Some(super::super::roles::Holding::Evicting) {
                return Ok(Rederived::Waiting("a scope is being evicted"));
            }
        }
        if self
            .direct
            .iter()
            .any(|scope| scope.domain == domain && Some(*scope) != importing)
        {
            return Ok(Rederived::Waiting("a direct import fills a scope"));
        }
        let inputs = self.rederive_inputs().await?;
        if let Some([SqlValue::Blob(stuck), SqlValue::Text(violation)]) = rows(
            &self.conn,
            "SELECT inputs,violation FROM secure_replayed_stuck",
        )
        .await?
        .first()
        .map(Vec::as_slice)
            && *stuck == inputs
        {
            return Ok(Rederived::Stuck(violation.clone()));
        }
        self.conn.execute("SAVEPOINT rederive", ()).await?;
        let rebuilt = async {
            // Unmarked first, so the rebuild publishes the outcomes it
            // decides; a rollback marks the scopes again.
            for id in &cleared {
                self.conn
                    .execute("DELETE FROM secure_replayed_rederive WHERE scope=?", [*id])
                    .await?;
            }
            self.replay_from_frames(time).await?;
            self.replayed_violation_any().await
        }
        .await;
        let undo = async || -> Result<(), Error> {
            self.conn.execute("ROLLBACK TO rederive", ()).await?;
            self.conn.execute("RELEASE rederive", ()).await?;
            Ok(())
        };
        match rebuilt {
            Ok(None) => {
                self.conn.execute("RELEASE rederive", ()).await?;
                self.conn
                    .execute("DELETE FROM secure_replayed_stuck", ())
                    .await?;
                self.prune_shadowed_outcomes(None).await?;
                Ok(Rederived::Done)
            }
            Ok(Some(violation)) => {
                undo().await?;
                tracing::warn!(%violation, "replayed rows cannot be derived again");
                self.conn
                    .execute(
                        "INSERT OR REPLACE INTO secure_replayed_stuck VALUES(1,?,?)",
                        bindings(vec![
                            SqlValue::Blob(inputs),
                            SqlValue::Text(violation.clone()),
                        ]),
                    )
                    .await?;
                Ok(Rederived::Stuck(violation))
            }
            // A rewound transaction's partition key is not held yet.
            Err(Error::Rejected(reason)) if reason == KEY_WAITS => {
                undo().await?;
                Ok(Rederived::Waiting(KEY_WAITS))
            }
            Err(error) => {
                undo().await?;
                Err(error)
            }
        }
    }
    /// Derives the domain's replayed rows again if a clear left them
    /// waiting, in a transaction of its own. Callers hold the replica as
    /// background work, so like a rewind it starts only when no interactive
    /// waiter is registered. Returns whether it rebuilt, and why seal
    /// finalization and compaction of `domain` wait, if they do.
    pub(in crate::replica) async fn retry_rederive(
        &mut self,
        domain: rocky_core::Scope,
    ) -> Result<(bool, Option<String>), Error> {
        if !self.rederive_marked
            || self
                .workspace
                .typed
                .as_ref()
                .is_some_and(|typed| typed.domain != domain)
        {
            return Ok((false, None));
        }
        let time = now()?;
        let result = transaction!(self, |inner| {
            let result = inner.rederive_replayed(None, time).await?;
            if result == Rederived::Done {
                inner
                    .conn
                    .execute(
                        "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                        (),
                    )
                    .await?;
            }
            Ok(result)
        })?;
        match result {
            Rederived::Unmarked => self.rederive_marked = false,
            Rederived::Done => {
                self.rederive_marked =
                    !rows(&self.conn, "SELECT 1 FROM secure_replayed_rederive LIMIT 1")
                        .await?
                        .is_empty();
                self.publish_revision().await?;
            }
            Rederived::Waiting(_) | Rederived::Stuck(_) => {}
        }
        Ok((result == Rederived::Done, result.waits()))
    }
    /// Why seal finalization of `domain` waits for rows to be derived
    /// again, without trying: a diagnostic for status reads.
    pub(in crate::replica) async fn rederive_waits(
        &self,
        domain: rocky_core::Scope,
    ) -> Result<Option<String>, Error> {
        if !self.rederive_marked
            || self
                .workspace
                .typed
                .as_ref()
                .is_none_or(|typed| typed.domain != domain)
            || rows(&self.conn, "SELECT 1 FROM secure_replayed_rederive LIMIT 1")
                .await?
                .is_empty()
        {
            return Ok(None);
        }
        let stuck = rows(&self.conn, "SELECT violation FROM secure_replayed_stuck").await?;
        Ok(match stuck.first().map(Vec::as_slice) {
            Some([SqlValue::Text(violation)]) => Rederived::Stuck(violation.clone()),
            _ => Rederived::Waiting("not tried yet"),
        }
        .waits())
    }
    /// What a rebuild derives from: applied and pending frames, bases,
    /// finalized seals, holdings and the followed tip. A stuck rebuild is
    /// tried again only once one of them changed.
    async fn rederive_inputs(&self) -> Result<Vec<u8>, Error> {
        let mut bytes = values_blob(
            rows(
                &self.conn,
                "SELECT (SELECT COALESCE(MAX(seq),0) FROM sqlite_sequence WHERE name='secure_applied'),(SELECT COUNT(*) FROM secure_operations WHERE status='pending'),(SELECT COALESCE(group_concat(c,','),'') FROM (SELECT scope||':'||sequence||':'||position AS c FROM secure_checkpoints ORDER BY scope,sequence)),(SELECT COALESCE(group_concat(f,','),'') FROM (SELECT hex(seal) AS f FROM secure_finalized ORDER BY domain,depth)),(SELECT COALESCE(group_concat(h,','),'') FROM (SELECT hex(scope)||state AS h FROM secure_subscriptions ORDER BY scope)),(SELECT COUNT(*) FROM secure_scope_state)",
            )
            .await?
            .first()
            .ok_or_else(|| Error::Corrupt("rederive inputs".into()))?,
        );
        bytes.extend_from_slice(&self.workspace.tip.0);
        Ok(rocky_core::commitment("rocky.rederive.inputs.v1", &bytes).to_vec())
    }
    /// A foreign key of the replayed tables that does not hold, named by
    /// child and parent table.
    async fn replayed_violation_any(&self) -> Result<Option<String>, Error> {
        let Some(typed) = self.workspace.typed.as_ref() else {
            return Ok(None);
        };
        for table in typed.rows.schema().tables.iter().filter(|table| {
            table.class == TableClass::Replayed
                && !table.foreign_keys.is_empty()
                && typed.by_name(&table.sql_name).is_some()
        }) {
            if let Some([_, _, SqlValue::Text(parent), ..]) = collect_rows(
                self.conn
                    .query(
                        "SELECT * FROM pragma_foreign_key_check(?) LIMIT 1",
                        [table.sql_name.clone()],
                    )
                    .await?,
            )
            .await?
            .first()
            .map(Vec::as_slice)
            {
                return Ok(Some(format!(
                    "a row of {} references a missing row of {parent}",
                    table.sql_name
                )));
            }
        }
        Ok(None)
    }
    /// Deletes kept outcomes whose member has a live record again, of scope
    /// `id` or of every scope.
    pub(in crate::replica) async fn prune_shadowed_outcomes(
        &self,
        id: Option<i64>,
    ) -> Result<(), Error> {
        match id {
            Some(id) => self.conn.execute(
                "DELETE FROM secure_kept_outcomes WHERE scope=?1 AND hash IN(SELECT hash FROM secure_replayed WHERE scope=?1)",
                [id],
            ).await?,
            None => self.conn.execute(
                "DELETE FROM secure_kept_outcomes WHERE hash IN(SELECT hash FROM secure_replayed)",
                (),
            ).await?,
        };
        Ok(())
    }

    /// Rebuilds every replayed table from its frames inside the caller's
    /// transaction: the tables empty and each visible applied intent member
    /// replays again in `(era, stamp)` order, admitted as it was recorded.
    /// A member's era is one below the first finalized seal whose bound
    /// covers it, else the open era; members those bounds cover end sealed
    /// again.
    pub(in crate::replica) async fn replay_from_frames(&self, time: u64) -> Result<(), Error> {
        self.replay_from(time, None).await
    }

    /// `replay_from_frames`, each scope starting from the newest rows the
    /// followed path passes that it holds, so that no member a seal covers
    /// replays under a later schema: kept seal `kept`'s rows when given,
    /// which every table returns to, else the rows taken at a finalized
    /// seal, else its replay base. Members the start covers keep their
    /// record; the rest replay.
    pub(in crate::replica) async fn replay_from(
        &self,
        time: u64,
        kept: Option<&history::Rewind>,
    ) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("replayed history".into());
        let Some(typed) = self.workspace.typed.as_ref() else {
            return Ok(());
        };
        let domain = typed.domain;
        let mut scopes = Vec::new();
        for record in rows(&self.conn, "SELECT id,scope FROM secure_scope_state").await? {
            let [SqlValue::Integer(id), SqlValue::Blob(scope)] = record.as_slice() else {
                return Err(corrupt());
            };
            let scope: ResourceScope = decode(scope)?;
            if scope.domain == domain && self.replayed_scope(scope) {
                scopes.push((*id, scope));
            }
        }
        let open = self.open_era(domain).await?;
        let path = self.path_seals(domain)?;
        let mut members = Vec::new();
        let mut bounds = BTreeMap::new();
        let mut restored = Vec::new();
        let mut covered_members = BTreeMap::new();
        let mut based = BTreeMap::new();
        for (id, scope) in &scopes {
            let mut start = match kept {
                Some(kept) => Start {
                    floor: kept.at(*id).map_or(0, |(_, bound)| bound),
                    based: self
                        .path_base(*id, &path)
                        .await?
                        .map(|(_, signed)| signed.statement.identities)
                        .unwrap_or_default(),
                    ..Start::default()
                },
                None => self.replay_start(*id, &path).await?,
            };
            let mut covered: Vec<(i64, i64)> = Vec::new();
            for record in collect_rows(
                self.conn
                    .query(
                        "SELECT depth,position FROM secure_seal_bounds WHERE scope=? ORDER BY depth",
                        [*id],
                    )
                    .await?,
            )
            .await?
            {
                let [SqlValue::Integer(depth), SqlValue::Integer(position)] = record.as_slice()
                else {
                    return Err(corrupt());
                };
                covered.push((*depth, *position));
            }
            let recorded: BTreeSet<Hash> = collect_rows(
                self.conn
                    .query("SELECT hash FROM secure_replayed WHERE scope=?", [*id])
                    .await?,
            )
            .await?
            .iter()
            .map(|record| match record.as_slice() {
                [SqlValue::Blob(hash)] => super::super::checkpoint::hash_of(hash),
                _ => Err(corrupt()),
            })
            .collect::<Result<_, _>>()?;
            // A start whose covered members lost their record, as when they
            // left the path, cannot say how they ended: the scope starts
            // from the next older rows instead.
            // So does one whose rows lack a field the path keeps.
            let scanned = loop {
                let serves = match &start.rows {
                    Some((tip, rows)) => self.images_serve(*scope, *tip, rows)?,
                    None => true,
                };
                match serves {
                    true => match self
                        .scan_members(*id, &start, &covered, &recorded, open)
                        .await?
                    {
                        Some(scanned) => break scanned,
                        None => start = self.older_start(*id, &path, &start).await?,
                    },
                    false => start = self.older_start(*id, &path, &start).await?,
                }
            };
            members.extend(scanned.0);
            covered_members.insert(*id, (start.floor, scanned.1));
            based.insert(*id, std::mem::take(&mut start.based));
            if let Some((tip, rows)) = start.rows {
                restored.push((*scope, tip, rows));
            }
            if let Some((_, bound)) = covered.last() {
                bounds.insert(*id, *bound);
            }
        }
        // Every replayed table empties, or returns to the kept seal's rows:
        // a cascade reaches only rows cleared anyway.
        if let Some(kept) = kept {
            self.rewind_live(kept.mark()).await?;
        }
        for table in typed.tables.iter().filter(|_| kept.is_none()) {
            if typed
                .rows
                .schema()
                .table(&table.table.sql_name)
                .is_some_and(|logical| logical.class == TableClass::Replayed)
            {
                self.touch(&table.table.sql_name);
                self.conn
                    .execute(
                        format!("DELETE FROM {}", typed::quoted(&table.table.sql_name)),
                        (),
                    )
                    .await?;
            }
        }
        // Members keep the admission they were recorded with.
        let mut recorded = BTreeMap::new();
        for (id, _) in &scopes {
            let (floor, kept_members) = covered_members.remove(id).unwrap_or_default();
            for record in collect_rows(
                self.conn
                    .query(
                        "SELECT r.hash,r.dropped,r.sealed=0 OR a.position IS NULL OR a.position>? FROM secure_replayed r LEFT JOIN secure_applied a ON a.hash=r.hash WHERE r.scope=?",
                        [floor, *id],
                    )
                    .await?,
            )
            .await?
            {
                let [SqlValue::Blob(bytes), dropped, SqlValue::Integer(after)] = record.as_slice()
                else {
                    return Err(corrupt());
                };
                let hash = super::super::checkpoint::hash_of(bytes)?;
                let admitted = !matches!(dropped, SqlValue::Text(reason) if reason == "admission");
                recorded.insert(hash, admitted);
                // Members the start covers stay as they ended.
                if *after != 0 && !kept_members.contains(&hash) {
                    self.conn
                        .execute(
                            "DELETE FROM secure_replayed WHERE hash=?",
                            [bytes.clone()],
                        )
                        .await?;
                }
            }
        }
        for (id, _) in &scopes {
            self.reset_replay_ledger(*id, based.get(id).map_or(&[][..], Vec::as_slice))
                .await?;
        }
        for (scope, source, rows) in &restored {
            self.restore_images(*scope, *source, rows).await?;
        }
        members.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        let mut redo: Vec<Redo> = Vec::new();
        for (order, _, transaction, member) in members {
            match redo.iter_mut().find(|redo| redo.transaction == transaction) {
                Some(redo) => redo.members.push(member),
                None => redo.push(Redo {
                    transaction,
                    members: vec![member],
                    order,
                }),
            }
        }
        self.redo_recorded(redo, time, Some(&recorded)).await?;
        for (id, bound) in bounds {
            self.seal_replayed(id, bound).await?;
        }
        Ok(())
    }

    /// The newest rows on `path` scope `id` holds: rows taken at a
    /// finalized seal, or its replay base, else none.
    async fn replay_start(&self, id: i64, path: &[SignedSeal]) -> Result<Start, Error> {
        self.start_before(id, path, path.len()).await
    }
    /// The start after `start` fails: the next older one, and after the
    /// base every frame, which a pruned scope no longer holds.
    async fn older_start(
        &self,
        id: i64,
        path: &[SignedSeal],
        start: &Start,
    ) -> Result<Start, Error> {
        if start.base {
            return Ok(Start::default());
        }
        match start.seal.as_ref().and_then(|seal| {
            path.iter()
                .position(|known| known.id().ok() == Some(seal.0))
        }) {
            Some(index) => self.start_before(id, path, index).await,
            None => Err(Error::Corrupt(
                "a replayed scope's covered members are missing".into(),
            )),
        }
    }
    /// Scope `id`'s replay base when its seal is on `path`: its index there
    /// and the signed checkpoint, with the base's applied position.
    async fn path_base(
        &self,
        id: i64,
        path: &[SignedSeal],
    ) -> Result<Option<((usize, i64), SignedCheckpoint)>, Error> {
        Ok(self.checkpoint_record(id, true).await?.and_then(|record| {
            let signed = record.signed?;
            let seal = signed.statement.seal?;
            let index = path
                .iter()
                .position(|known| known.id().ok() == Some(seal))?;
            Some(((index, record.position), signed))
        }))
    }
    /// The newest start at a seal of `path` before index `end`.
    async fn start_before(&self, id: i64, path: &[SignedSeal], end: usize) -> Result<Start, Error> {
        let base = self
            .path_base(id, path)
            .await?
            .filter(|((index, _), _)| *index < end);
        let floor = base.as_ref().map_or(0, |((_, position), _)| *position);
        let based = base
            .as_ref()
            .map(|(_, signed)| signed.statement.identities.clone())
            .unwrap_or_default();
        for index in (0..end).rev() {
            if base.as_ref().is_some_and(|((at, _), _)| *at >= index) {
                break;
            }
            let seal = &path[index];
            let seal_id = seal.id().map_err(credential)?;
            if let Some(rows) = self.seal_rows(id, seal_id).await? {
                return Ok(Start {
                    floor,
                    seal: Some((seal_id, seal.clone())),
                    rows: Some((seal.statement.tip, rows)),
                    based,
                    base: false,
                });
            }
        }
        Ok(match base {
            Some((_, signed)) => Start {
                floor,
                seal: None,
                rows: Some((
                    signed.statement.tip,
                    self.stored_images("secure_replayed_base", id, None).await?,
                )),
                based,
                base: true,
            },
            None => Start::default(),
        })
    }
    /// Resets scope `id`'s replay ledger to where its replay starts: the
    /// `based` marks and the births of the members it keeps.
    pub(in crate::replica) async fn reset_replay_ledger(
        &self,
        id: i64,
        based: &[checkpoint::CoveredMark],
    ) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("replayed births".into());
        let mut ledger = identity::IdentityLedger::based(based)?;
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT births FROM secure_replayed WHERE scope=? AND births IS NOT NULL",
                    [id],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(births)] = record.as_slice() else {
                return Err(corrupt());
            };
            for mark in decode::<Vec<checkpoint::IdentityMark>>(births)? {
                ledger.merge(mark)?;
            }
        }
        let scope = match rows(
            &self.conn,
            &format!("SELECT scope FROM secure_scope_state WHERE id={id}"),
        )
        .await?
        .first()
        .map(Vec::as_slice)
        {
            Some([SqlValue::Blob(scope)]) => decode(scope)?,
            _ => return Err(corrupt()),
        };
        self.replace_ledger(scope, ledger).await
    }
    /// The members of scope `id` after `start` that the path shows, in
    /// replay order, and those the start's seal covers; none when a
    /// covered member has no record.
    #[allow(clippy::type_complexity)]
    async fn scan_members(
        &self,
        id: i64,
        start: &Start,
        covered: &[(i64, i64)],
        recorded: &BTreeSet<Hash>,
        open: i64,
    ) -> Result<Option<(Vec<Member>, BTreeSet<Hash>)>, Error> {
        let corrupt = || Error::Corrupt("replayed history".into());
        let mut members = Vec::new();
        let mut kept = BTreeSet::new();
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT position,hash FROM secure_applied WHERE scope=? AND position>? ORDER BY position",
                    [id, start.floor],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Integer(position), SqlValue::Blob(hash)] = record.as_slice() else {
                return Err(corrupt());
            };
            let hash = super::super::checkpoint::hash_of(hash)?;
            let frame = self
                .frame(hash)
                .await?
                .ok_or_else(|| Error::Corrupt("replayed history is pruned".into()))?;
            let operation = frame.operation()?;
            if !self.visible_in_workspace(&operation)? {
                continue;
            }
            let payload = self.row_payload(&frame)?;
            let (Some(_), Some(membership)) = (payload.intent(), payload.membership()) else {
                continue;
            };
            if let Some((_, seal)) = &start.seal {
                let extension = &operation.header.extensions;
                let author = extension.author.ok_or_else(corrupt)?;
                if Self::covers_exact(
                    self.graph_for(extension.scope.domain)?,
                    extension.tip,
                    author,
                    extension.author_sequence,
                    seal,
                )? {
                    if !recorded.contains(&hash) {
                        return Ok(None);
                    }
                    kept.insert(hash);
                    continue;
                }
            }
            let era = covered
                .iter()
                .find(|(_, bound)| *position <= *bound)
                .map_or(open, |(depth, _)| depth - 1);
            members.push((
                (era, stamp(&operation.header)),
                id,
                membership.transaction,
                (hash, membership.index),
            ));
        }
        Ok(Some((members, kept)))
    }
}

/// A member to replay: its order, scope, transaction and `(frame, index)`.
type Member = (Order, i64, Commitment, (Hash, u16));

/// Where a replayed scope's rebuild starts: rows at a seal of the path and
/// the last applied position below which nothing replays.
/// Rows by row id with their images.
type Images = Vec<(Commitment, Vec<u8>)>;

#[derive(Default)]
struct Start {
    floor: i64,
    /// The seal the rows were taken at, when not the replay base's.
    seal: Option<(Commitment, SignedSeal)>,
    rows: Option<(TipId, Images)>,
    /// The identity marks the replay base on the path signs.
    based: Vec<checkpoint::CoveredMark>,
    /// The rows are the replay base: no older start exists.
    base: bool,
}
