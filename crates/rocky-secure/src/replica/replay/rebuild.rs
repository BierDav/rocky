//! Replayed tables keep no row records: their frames are their history, so
//! a rebuild empties them and replays every visible member again.
use super::*;
use rocky_credentials::SignedSeal;

/// Why a replayed scope's rows are cleared.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::replica) enum Clear {
    /// Its history restarts and fills it again: from a base, or from frames
    /// that apply again. Rows of other scopes its delete actions reach are
    /// derived again from their own history once it is whole.
    Refill,
    /// The device stops holding it. Rows of held scopes that reference its
    /// rows follow their declared delete action, as SQLite applies it; their
    /// outcomes stay as replayed. Holding the scope again refills it.
    Evict,
}

impl<A: RowApplication> Inner<A> {
    /// Forgets replayed scope `scope`'s replay, inside the caller's
    /// transaction: its rows (its partition's, or the whole unpartitioned
    /// table) and where its members stood. Their outcomes stay readable.
    ///
    /// SQLite runs delete actions inside the transaction and cannot switch
    /// foreign keys off there, so clearing a parent reaches rows of other
    /// scopes. Putting reached rows back as they stood would not match a
    /// replica that never cleared: the parent's own refill may delete,
    /// recreate or rekey it. On a refill, every scope that a delete action
    /// of this table reaches and that holds history marks the scope instead:
    /// once nothing it needs waits, `rederive_replayed` rebuilds the
    /// domain's replayed rows from bases and frames in canonical order,
    /// exactly as a fresh replica of the same state would.
    pub(in crate::replica) async fn clear_replayed(
        &self,
        scope: ResourceScope,
        clear: Clear,
    ) -> Result<(), Error> {
        let (table, condition, values) = self.scope_rows(scope)?;
        let id = self.find_scope_id(scope).await?;
        if let Some(id) = id {
            if clear == Clear::Refill && self.reaches_history(scope, id).await? {
                self.conn
                    .execute(
                        "INSERT INTO secure_replayed_rederive VALUES(?) ON CONFLICT DO NOTHING",
                        [id],
                    )
                    .await?;
            }
            self.conn
                .execute(
                    "INSERT INTO secure_kept_outcomes SELECT hash,txn,dropped,sealed,violated FROM secure_replayed WHERE scope=? ON CONFLICT(hash) DO UPDATE SET dropped=excluded.dropped,sealed=excluded.sealed,violated=excluded.violated",
                    [id],
                )
                .await?;
            self.conn
                .execute("DELETE FROM secure_replayed WHERE scope=?", [id])
                .await?;
        }
        self.touch(&table);
        self.conn
            .execute(
                format!("DELETE FROM {} WHERE {condition}", typed::quoted(&table)),
                bindings(values),
            )
            .await?;
        Ok(())
    }
    /// `clear_replayed` for a refill, then `rows`, a base at a seal of tip
    /// `source`, become the scope's rows. The caller rederives once the
    /// operations after the base apply again.
    pub(in crate::replica) async fn replace_replayed(
        &self,
        scope: ResourceScope,
        source: TipId,
        rows: &[(Commitment, Vec<u8>)],
    ) -> Result<(), Error> {
        self.clear_replayed(scope, Clear::Refill).await?;
        self.restore_images(scope, source, rows).await
    }
    /// The scope's replayed table and the condition selecting its rows.
    fn scope_rows(&self, scope: ResourceScope) -> Result<(String, String, Vec<SqlValue>), Error> {
        let corrupt = || Error::Corrupt("replayed scope".into());
        let typed = self.workspace.typed.as_ref().ok_or_else(corrupt)?;
        let projected = typed.by_scope(scope).ok_or_else(corrupt)?;
        let schema = typed.rows.schema();
        let logical = schema
            .table(&projected.table.sql_name)
            .ok_or_else(corrupt)?;
        if logical.partition_by.is_empty() {
            return Ok((logical.sql_name.clone(), "1".into(), Vec::new()));
        }
        let tuple = self.partitions.get(&scope).ok_or_else(corrupt)?;
        let condition = logical
            .partition_by
            .iter()
            .map(|column| format!("{}=?", typed::quoted(column)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let values = super::super::partition::partition_values(schema, logical, tuple)?;
        Ok((logical.sql_name.clone(), condition, values))
    }
    /// Whether a scope other than `scope` (stored as `id`) of a table that
    /// the delete actions of `scope`'s table reach, transitively and through
    /// any foreign key, its own table included, holds replay history: rows,
    /// a base or replayed members.
    async fn reaches_history(&self, scope: ResourceScope, id: i64) -> Result<bool, Error> {
        let corrupt = || Error::Corrupt("replayed scope".into());
        let typed = self.workspace.typed.as_ref().ok_or_else(corrupt)?;
        let schema = typed.rows.schema();
        let (table, condition, values) = self.scope_rows(scope)?;
        let mut reached = BTreeSet::new();
        let mut work = vec![table.to_ascii_lowercase()];
        while let Some(parent) = work.pop() {
            for child in schema.tables.iter().filter(|child| {
                child.class == TableClass::Replayed
                    && child
                        .foreign_keys
                        .iter()
                        .any(|key| key.table.eq_ignore_ascii_case(&parent))
            }) {
                if reached.insert(child.sql_name.to_ascii_lowercase()) {
                    work.push(child.sql_name.to_ascii_lowercase());
                }
            }
        }
        if reached.is_empty() {
            return Ok(false);
        }
        for name in &reached {
            let Some(projected) = typed.by_name(name) else {
                continue;
            };
            let sql = match name.eq_ignore_ascii_case(&table) {
                true => format!(
                    "SELECT 1 FROM {} WHERE NOT({condition}) LIMIT 1",
                    typed::quoted(&projected.table.sql_name)
                ),
                false => format!(
                    "SELECT 1 FROM {} LIMIT 1",
                    typed::quoted(&projected.table.sql_name)
                ),
            };
            let bound = match name.eq_ignore_ascii_case(&table) {
                true => values.clone(),
                false => Vec::new(),
            };
            if !collect_rows(self.conn.query(sql, bindings(bound)).await?)
                .await?
                .is_empty()
            {
                return Ok(true);
            }
        }
        for record in rows(&self.conn, "SELECT id,scope FROM secure_scope_state").await? {
            let [SqlValue::Integer(other), SqlValue::Blob(blob)] = record.as_slice() else {
                return Err(corrupt());
            };
            if *other == id {
                continue;
            }
            let other_scope: ResourceScope = decode(blob)?;
            let reaches = other_scope.domain == scope.domain
                && typed
                    .rows
                    .table(&other_scope.table)
                    .is_some_and(|table| reached.contains(&table.sql_name.to_ascii_lowercase()));
            if reaches
                && !collect_rows(
                    self.conn
                        .query(
                            "SELECT 1 WHERE EXISTS(SELECT 1 FROM secure_replayed WHERE scope=?1) OR EXISTS(SELECT 1 FROM secure_replayed_base WHERE scope=?1)",
                            [*other],
                        )
                        .await?,
                )
                .await?
                .is_empty()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// Inside the caller's transaction, rebuilds the replayed rows of the
    /// followed domain from bases and frames once no scope a refill marked
    /// still waits: none needs a seal rebootstrap or holds pending frames,
    /// and no direct import other than `importing` fills one. A rebuild
    /// that would leave a foreign key violated (a parent still missing)
    /// rolls back and the marks stay for the next attempt.
    pub(in crate::replica) async fn rederive_replayed(
        &self,
        importing: Option<ResourceScope>,
        time: u64,
    ) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("rederived scope".into());
        let marked = rows(
            &self.conn,
            "SELECT r.scope,s.scope FROM secure_replayed_rederive r LEFT JOIN secure_scope_state s ON s.id=r.scope",
        )
        .await?;
        if marked.is_empty() {
            return Ok(());
        }
        let Some(typed) = self.workspace.typed.as_ref() else {
            return Ok(());
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
            if waiting.contains(&scope)
                || !collect_rows(
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
                return Ok(());
            }
        }
        if self
            .direct
            .iter()
            .any(|scope| scope.domain == domain && Some(*scope) != importing)
        {
            return Ok(());
        }
        self.conn.execute("SAVEPOINT rederive", ()).await?;
        let rebuilt = async {
            self.replay_from_frames(time).await?;
            self.replayed_keys_hold().await
        }
        .await;
        match rebuilt {
            Ok(true) => {
                self.conn.execute("RELEASE rederive", ()).await?;
                for id in cleared {
                    self.conn
                        .execute("DELETE FROM secure_replayed_rederive WHERE scope=?", [id])
                        .await?;
                }
                Ok(())
            }
            other => {
                self.conn.execute("ROLLBACK TO rederive", ()).await?;
                self.conn.execute("RELEASE rederive", ()).await?;
                other.map(drop)
            }
        }
    }
    /// Whether every foreign key of the replayed tables holds.
    async fn replayed_keys_hold(&self) -> Result<bool, Error> {
        let Some(typed) = self.workspace.typed.as_ref() else {
            return Ok(true);
        };
        for table in typed.rows.schema().tables.iter().filter(|table| {
            table.class == TableClass::Replayed
                && !table.foreign_keys.is_empty()
                && typed.by_name(&table.sql_name).is_some()
        }) {
            if !collect_rows(
                self.conn
                    .query(
                        "SELECT 1 FROM pragma_foreign_key_check(?) LIMIT 1",
                        [table.sql_name.clone()],
                    )
                    .await?,
            )
            .await?
            .is_empty()
            {
                return Ok(false);
            }
        }
        Ok(true)
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
