//! Lazy compaction at seals, in a domain with a tip graph. Every device
//! finalizes the state at each seal of its followed path once it holds every
//! frame through the seal's frontier: rows dead there stay dead. Compacting
//! moves the replay base to a finalized seal and releases the evidence
//! behind it. See `docs/runtime/lazy-compaction.md`.
use super::checkpoint::{
    CHECKPOINT_FORMAT, CheckpointStatement, LogPosition, Overlay, PruneBlock, STEP_OPERATIONS,
    STEP_ROWS, SetSummary, Sets,
};
use super::*;
use rocky_core::Scope;
use rocky_credentials::SignedSeal;
mod rebootstrap;
pub use rebootstrap::{FrameReplay, SealRebootstrap};

pub(super) const SCHEMA: &[&str] = &[
    // Seals of the followed path this replica finalized, oldest first. The
    // last compacted one is its scopes' replay base.
    "CREATE TABLE IF NOT EXISTS secure_finalized(domain BLOB NOT NULL,depth INTEGER NOT NULL,seal BLOB NOT NULL,compacted INTEGER NOT NULL,PRIMARY KEY(domain,depth)) WITHOUT ROWID",
    // Per scope and uncompacted finalized seal: the last applied position
    // it includes. Applied positions it excludes follow every included one.
    "CREATE TABLE IF NOT EXISTS secure_seal_bounds(scope INTEGER NOT NULL,depth INTEGER NOT NULL,position INTEGER NOT NULL,PRIMARY KEY(scope,depth)) WITHOUT ROWID",
    // What finalizing at that seal changed: a row without fields is dropped.
    "CREATE TABLE IF NOT EXISTS secure_seal_drops(scope INTEGER NOT NULL,depth INTEGER NOT NULL,row BLOB NOT NULL,fields BLOB,PRIMARY KEY(scope,depth,row)) WITHOUT ROWID",
    // Domains whose compacted seal lost a race: their base must be rebuilt
    // at the winner.
    "CREATE TABLE IF NOT EXISTS secure_compaction_lost(domain BLOB PRIMARY KEY,seal BLOB NOT NULL) WITHOUT ROWID",
];

/// One finalized seal of a domain's followed path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Finalized {
    depth: i64,
    seal: Commitment,
    compacted: bool,
}

/// What a `compact` call finalized, compacted and released.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompactReport {
    /// The newest seal whose state this replica finalized.
    pub finalized: Option<Commitment>,
    /// The seal the domain's replay base sits at.
    pub compacted: Option<Commitment>,
    /// Retained operations released behind it.
    pub pruned: u64,
    /// Why the next seal of the followed path is not finalized yet.
    pub waiting: Option<String>,
    /// What stopped releasing a scope's operations.
    pub blocked: Vec<(ResourceScope, PruneBlock)>,
}

/// Where a domain's compaction stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompactionStatus {
    pub finalized: Option<Commitment>,
    pub compacted: Option<Commitment>,
    /// Finalized seals whose evidence is still retained, oldest first.
    pub uncompacted: Vec<Commitment>,
    /// Why the next seal of the followed path is not finalized yet.
    pub waiting: Option<String>,
    /// The compacted seal lost a race; the base must be rebuilt at the
    /// winning seal from a checkpoint or replayed frames.
    pub rebootstrap: Option<Commitment>,
}

impl<A: RowApplication> Inner<A> {
    /// The seals the followed tip's state passes through, oldest first: the
    /// canonical seal leaving each tip of its ancestry, through openings,
    /// then the followed tip's own once sealed.
    pub(super) fn path_seals(&self, domain: Scope) -> Result<Vec<SignedSeal>, Error> {
        let Some(graph) = self.graphs.get(&domain) else {
            return Ok(Vec::new());
        };
        if self.followed.as_ref().map(|(scope, _)| *scope) != Some(domain) {
            return Ok(Vec::new());
        }
        let mut seals = Vec::new();
        if let Some(own) = graph.seal(self.tip).map_err(credential)? {
            seals.push(own.clone());
        }
        let mut cursor = self.tip;
        let mut visited = BTreeSet::new();
        while visited.insert(cursor) {
            let mut parent = None;
            for seal in &graph.seals {
                if seal.successor().map_err(credential)? == Some(cursor) {
                    parent = Some(seal.clone());
                    break;
                }
            }
            if parent.is_none() {
                for opening in &graph.openings {
                    if opening.tip().map_err(credential)? == cursor {
                        parent = Some(
                            graph
                                .seal_by_id(opening.statement.seal)
                                .map_err(credential)?
                                .clone(),
                        );
                        break;
                    }
                }
            }
            let Some(seal) = parent else { break };
            cursor = seal.statement.tip;
            seals.push(seal);
        }
        seals.reverse();
        Ok(seals)
    }
    /// Where `seal` lies on the followed path; none off it.
    pub(super) fn path_index(
        &self,
        domain: Scope,
        seal: Commitment,
    ) -> Result<Option<usize>, Error> {
        Ok(self
            .path_seals(domain)?
            .iter()
            .position(|known| known.id().is_ok_and(|id| id == seal)))
    }
    async fn finalized(&self, domain: Scope) -> Result<Vec<Finalized>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT depth,seal,compacted FROM secure_finalized WHERE domain=? ORDER BY depth",
                    [encode(&domain)?],
                )
                .await?,
        )
        .await?;
        records
            .into_iter()
            .map(|record| match record.as_slice() {
                [
                    SqlValue::Integer(depth),
                    SqlValue::Blob(seal),
                    SqlValue::Integer(compacted),
                ] => Ok(Finalized {
                    depth: *depth,
                    seal: checkpoint::row_of(seal)?,
                    compacted: *compacted != 0,
                }),
                _ => Err(Error::Corrupt("finalized seal".into())),
            })
            .collect()
    }
    /// The keyed scopes of `domain` this replica holds state of.
    async fn domain_scopes(&self, domain: Scope) -> Result<Vec<(ResourceScope, i64)>, Error> {
        let mut scopes = Vec::new();
        for record in rows(
            &self.conn,
            "SELECT id,scope FROM secure_scope_state ORDER BY id",
        )
        .await?
        {
            let [SqlValue::Integer(id), SqlValue::Blob(scope)] = record.as_slice() else {
                return Err(Error::Corrupt("scope state".into()));
            };
            let scope: ResourceScope = decode(scope)?;
            // A partitioned table's own scope holds no rows.
            if scope.domain == domain
                && self.config.keys.iter().any(|key| key.scope == scope)
                && !(self.partitioned(scope) && !self.partitions.contains_key(&scope))
            {
                scopes.push((scope, *id));
            }
        }
        Ok(scopes)
    }
    /// Per uncompacted finalized seal of scope `id`, in order: its depth and
    /// the last applied position it includes.
    async fn seal_bounds(&self, id: i64) -> Result<Vec<(i64, i64)>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT depth,position FROM secure_seal_bounds WHERE scope=? ORDER BY depth",
                    [id],
                )
                .await?,
        )
        .await?;
        records
            .into_iter()
            .map(|record| match record.as_slice() {
                [SqlValue::Integer(depth), SqlValue::Integer(position)] => Ok((*depth, *position)),
                _ => Err(Error::Corrupt("seal bound".into())),
            })
            .collect()
    }
    /// Whether a scope's state passed a seal: some rows may be absent
    /// because they were dead there.
    pub(super) async fn seal_finalized(&self, scope: ResourceScope) -> Result<bool, Error> {
        if !self.graphs.contains_key(&scope.domain) {
            return Ok(false);
        }
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(false);
        };
        if !self.seal_bounds(id).await?.is_empty() {
            return Ok(true);
        }
        Ok(self
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed)
            .is_some_and(|signed| signed.statement.seal.is_some()))
    }
    /// A tip rebuild that keeps no seal base replays from nothing.
    pub(super) fn ignores_base(&self) -> bool {
        self.rebuilding_tip && !self.tip_base
    }
    /// `replay`, applying what finalizing a seal changed once the replay
    /// passes the last position the seal includes.
    pub(super) async fn replay_sealed(
        &self,
        scope: ResourceScope,
        id: i64,
        overlay: Overlay,
        (after, through): (i64, i64),
        limit: usize,
        sets: &mut Sets,
    ) -> Result<Option<i64>, Error> {
        let bounds = self.seal_bounds(id).await?;
        let end = bounds
            .iter()
            .map(|(_, position)| *position)
            .filter(|position| *position > after && *position <= through)
            .min()
            .unwrap_or(through);
        let last = self
            .replay(scope, id, overlay, (after, end), limit, sets)
            .await?;
        if last == Some(end) {
            for (depth, _) in bounds.iter().filter(|(_, position)| *position == end) {
                self.apply_drops(id, *depth, overlay, sets).await?;
            }
        }
        Ok(last)
    }
    async fn apply_drops(
        &self,
        id: i64,
        depth: i64,
        overlay: Overlay,
        sets: &mut Sets,
    ) -> Result<(), Error> {
        let drops = collect_rows(
            self.conn
                .query(
                    "SELECT row,fields FROM secure_seal_drops WHERE scope=? AND depth=? ORDER BY row",
                    [id, depth],
                )
                .await?,
        )
        .await?;
        for record in drops {
            let (row, fields) = match record.as_slice() {
                [SqlValue::Blob(row), SqlValue::Null] => (checkpoint::row_of(row)?, Vec::new()),
                [SqlValue::Blob(row), SqlValue::Blob(fields)] => {
                    (checkpoint::row_of(row)?, fields.clone())
                }
                _ => return Err(Error::Corrupt("seal drop".into())),
            };
            let old = self.replay_row(overlay, id, row).await?;
            match (&old, fields.is_empty()) {
                (Some(old), true) => sets.remove(&row, old)?,
                (old, false) => sets.replace(&row, old.as_deref(), &fields)?,
                (None, true) => continue,
            }
            self.conn
                .execute(
                    format!("INSERT OR REPLACE INTO {} VALUES(?,?,?)", overlay.table()),
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(row.to_vec()),
                        SqlValue::Blob(fields),
                    ]),
                )
                .await?;
        }
        Ok(())
    }
    /// The position the replay of scope `id` has passed the last finalized
    /// seal at: its bound, else its base's.
    async fn last_bound(&self, id: i64) -> Result<i64, Error> {
        if let Some((_, position)) = self.seal_bounds(id).await?.last() {
            return Ok(*position);
        }
        Ok(self
            .checkpoint_record(id, true)
            .await?
            .filter(|record| {
                record
                    .signed
                    .as_ref()
                    .is_some_and(|signed| signed.statement.seal.is_some())
            })
            .map_or(0, |record| record.position))
    }
    /// Why `seal` cannot be finalized yet, after `previous` on the path: a
    /// held scope misses a frame through the frontier, or its state cannot
    /// be rebuilt there.
    async fn seal_waits(
        &self,
        domain: Scope,
        seal: &SignedSeal,
        previous: Option<&SignedSeal>,
        scopes: &[(ResourceScope, i64)],
    ) -> Result<Option<String>, Error> {
        if self.cut_pending(self.tip_log(domain)).await? {
            return Ok(Some("a cut of the tip's log is rolling back".into()));
        }
        let at = self.path_index(domain, seal.id().map_err(credential)?)?;
        // Scopes whose frames through the frontier this replica must hold:
        // a base at this seal or a later one already holds its state.
        let mut open = Vec::new();
        for (scope, id) in scopes {
            if self.direct.contains(scope) || self.importing(*scope).await? {
                return Ok(Some("a scope is being imported".into()));
            }
            if self.staged_in(*scope).await? {
                return Ok(Some("a coordinated entry is undecided".into()));
            }
            match self.base_seal(*id).await? {
                Some(base) if self.path_index(domain, base)? >= at => continue,
                Some(_) => {}
                None if self.has_horizon(*scope).await? => {
                    return Ok(Some(
                        "history pruned before the seals; a checkpoint at a seal replaces it"
                            .into(),
                    ));
                }
                None => {}
            }
            open.push((*scope, *id));
        }
        let scopes = open.as_slice();
        // Positions of every node's frames and voids this replica holds.
        let mut held = BTreeMap::<u32, BTreeSet<u64>>::new();
        let mut last = BTreeMap::<(i64, u32), u64>::new();
        let ids: BTreeMap<ResourceScope, i64> = scopes.iter().copied().collect();
        // Per held logged scope and node: frames' positions and the
        // existence records each counts.
        let logged: BTreeSet<ResourceScope> = scopes
            .iter()
            .map(|(scope, _)| *scope)
            .filter(|scope| self.existence_table(*scope))
            .collect();
        let mut counted = BTreeMap::<(ResourceScope, u32), BTreeMap<u64, u64>>::new();
        for record in rows(
            &self.conn,
            "SELECT header FROM secure_operations WHERE status<>'pending' UNION ALL SELECT header FROM secure_voids",
        )
        .await?
        {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("frame header".into()));
            };
            let header: p2panda_core::Header<Extensions> = decode(bytes)?;
            let e = &header.extensions;
            if e.scope.domain != domain {
                continue;
            }
            let Some(author) = e.author else { continue };
            held.entry(author).or_default().insert(e.author_sequence);
            if e.existence != 0 && logged.contains(&e.scope) {
                counted
                    .entry((e.scope, author))
                    .or_default()
                    .insert(e.author_sequence, u64::from(e.existence));
            }
            if let Some(id) = ids.get(&e.scope) {
                let mark = last.entry((*id, author)).or_insert(e.author_sequence);
                *mark = (*mark).max(e.author_sequence);
            }
        }
        for mark in &seal.statement.frontier {
            let from = previous
                .and_then(|previous| {
                    previous
                        .statement
                        .frontier
                        .iter()
                        .find(|known| known.author == mark.author)
                })
                .map_or(0, |known| known.sequence.saturating_add(1));
            if from > mark.sequence {
                continue;
            }
            let missing = highest_missing(held.get(&mark.author), from, mark.sequence);
            let Some(missing) = missing else { continue };
            // A missing position below a scope's last held frame of the
            // author is in another scope: the scope's log is contiguous.
            for (_, id) in scopes {
                if last
                    .get(&(*id, mark.author))
                    .is_none_or(|last| missing >= *last)
                {
                    return Ok(Some(format!(
                        "missing frames of author {} through the seal's frontier",
                        mark.author
                    )));
                }
            }
        }
        // Every existence record the frames through the frontier count,
        // unless a base at this seal or a later one folded them.
        for scope in &logged {
            if let Some(base) = self.existence_base(*scope).await?
                && let Some(folded) = base.seal()
                && self.path_index(domain, folded)? >= at
            {
                continue;
            }
            let records = self.existence_positions(*scope).await?;
            for mark in &seal.statement.frontier {
                let from = previous
                    .and_then(|previous| {
                        previous
                            .statement
                            .frontier
                            .iter()
                            .find(|known| known.author == mark.author)
                    })
                    .map_or(0, |known| known.sequence.saturating_add(1));
                if from > mark.sequence {
                    continue;
                }
                let sum = |counts: Option<&BTreeMap<u64, u64>>| -> u64 {
                    counts.map_or(0, |counts| {
                        counts.range(from..=mark.sequence).map(|(_, n)| n).sum()
                    })
                };
                if sum(records.get(&mark.author)) < sum(counted.get(&(*scope, mark.author))) {
                    return Ok(Some(format!(
                        "missing existence records of author {} through the seal's frontier",
                        mark.author
                    )));
                }
            }
        }
        Ok(None)
    }
    /// What finalizing changes in a scope's stored rows, now at the seal's
    /// frontier with their typed projection: tombstones and rows the typed
    /// table does not show are dropped; a shown row keeps its register
    /// stamps and takes the values a policy substituted.
    async fn seal_drops(
        &self,
        scope: ResourceScope,
    ) -> Result<Vec<(Commitment, Option<Vec<u8>>)>, Error> {
        let typed = self.typed.clone();
        let table = typed.as_ref().and_then(|typed| typed.by_scope(scope));
        let id_column = match (&typed, table) {
            (Some(typed), Some(table)) => typed
                .rows
                .schema()
                .table(&table.table.sql_name)
                .and_then(|logical| {
                    logical
                        .columns
                        .iter()
                        .position(|column| column.sql_name.eq_ignore_ascii_case("id"))
                }),
            _ => None,
        };
        let blob = encode(&scope)?;
        let mut drops = Vec::new();
        let mut after = Vec::new();
        loop {
            let records = collect_rows(
                self.conn
                    .query(
                        "SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?",
                        bindings(vec![
                            SqlValue::Blob(blob.clone()),
                            SqlValue::Blob(after.clone()),
                            SqlValue::Integer(STEP_ROWS as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            if records.is_empty() {
                return Ok(drops);
            }
            for record in records {
                let [SqlValue::Blob(key), SqlValue::Blob(fields)] = record.as_slice() else {
                    return Err(Error::Corrupt("stored row".into()));
                };
                after = key.clone();
                let row = checkpoint::row_of(key)?;
                let mut record: Materialized = decode(fields)?;
                if record.deleted {
                    drops.push((row, None));
                    continue;
                }
                let (Some(typed), Some(table)) = (&typed, table) else {
                    continue;
                };
                let values = typed
                    .rows
                    .values(&table.table, &record.fields)
                    .map_err(Error::Corrupt)?;
                let parameters = table
                    .table
                    .key
                    .iter()
                    .map(|index| values[*index].clone())
                    .collect();
                let select = if typed
                    .rows
                    .schema()
                    .table(&table.table.sql_name)
                    .is_some_and(|logical| logical.column("row_kind").is_some())
                {
                    format!("{} AND row_kind=0", table.select)
                } else {
                    table.select.clone()
                };
                let image = collect_rows(self.conn.query(select, bindings(parameters)).await?)
                    .await?
                    .pop()
                    .filter(|image| match (id_column, record.identity) {
                        (Some(column), Some(identity)) => {
                            image.get(column) == Some(&SqlValue::Integer(identity))
                        }
                        (None, _) => true,
                        (Some(_), None) => false,
                    });
                let Some(image) = image else {
                    drops.push((row, None));
                    continue;
                };
                let mut changed = false;
                for (field, value) in typed
                    .rows
                    .fields(&table.table, &image)
                    .map_err(Error::Corrupt)?
                {
                    if record.fields.get(&field) != Some(&value) {
                        record.fields.insert(field, value);
                        changed = true;
                    }
                }
                if changed {
                    drops.push((row, Some(encode(&record)?)));
                }
            }
        }
    }
    /// Per held scope: its applied positions after its last bound that
    /// `seal` excludes and the last one it includes. A scope changes when
    /// the seal includes any; once a constrained one does, every
    /// constrained scope changes, its dependencies may have.
    async fn seal_changes(
        &self,
        domain: Scope,
        seal: &SignedSeal,
        scopes: &[(ResourceScope, i64)],
    ) -> Result<Vec<SealChange>, Error> {
        let graph = self.graph_for(domain)?;
        let constrained = |scope: ResourceScope| {
            self.typed
                .as_ref()
                .and_then(|typed| typed.constraints.as_ref())
                .and_then(|constraints| constraints.node(scope))
                .is_some()
        };
        let mut changes = Vec::new();
        for (scope, id) in scopes {
            let bound = self.last_bound(*id).await?;
            let records = collect_rows(
                self.conn
                    .query(
                        "SELECT a.position,a.hash,o.header FROM secure_applied a LEFT JOIN secure_operations o ON o.hash=a.hash WHERE a.scope=? AND a.position>? ORDER BY a.position",
                        [*id, bound],
                    )
                    .await?,
            )
            .await?;
            let mut change = SealChange {
                scope: *scope,
                id: *id,
                through: bound,
                excluded: Vec::new(),
                changed: false,
            };
            for record in records {
                let [
                    SqlValue::Integer(position),
                    SqlValue::Blob(hash),
                    SqlValue::Blob(header),
                ] = record.as_slice()
                else {
                    return Err(Error::Corrupt(
                        "an applied operation after the last finalized seal is not retained".into(),
                    ));
                };
                let header: p2panda_core::Header<Extensions> = decode(header)?;
                let e = &header.extensions;
                let author = e.author.ok_or_else(|| denied("node author missing"))?;
                if self.visible_frame(
                    graph,
                    e.tip,
                    author,
                    e.author_sequence,
                    seal.statement.tip,
                )? {
                    change.through = *position;
                } else {
                    change.excluded.push((*position, hash.clone()));
                }
            }
            change.changed = change.through > bound;
            changes.push(change);
        }
        if changes
            .iter()
            .any(|change| change.changed && constrained(change.scope))
        {
            for change in &mut changes {
                change.changed |= constrained(change.scope);
            }
        }
        Ok(changes)
    }
}

/// How a seal changes one held scope.
struct SealChange {
    scope: ResourceScope,
    id: i64,
    /// The last position this seal includes.
    through: i64,
    /// Positions after `bound` it excludes, in order, with their hashes.
    excluded: Vec<(i64, Vec<u8>)>,
    changed: bool,
}

/// Where the finalized seals start on the path: only the latest compacted
/// one is kept of those before. None when the oldest left the path.
fn offset(path: &[SignedSeal], finalized: &[Finalized]) -> Option<usize> {
    match finalized.first() {
        None => Some(0),
        Some(first) => path
            .iter()
            .position(|seal| seal.id().is_ok_and(|id| id == first.seal)),
    }
}

/// The highest position in `from..=to` that `positions` lacks.
fn highest_missing(positions: Option<&BTreeSet<u64>>, from: u64, to: u64) -> Option<u64> {
    let mut expected = to;
    if let Some(positions) = positions {
        for position in positions.range(from..=to).rev() {
            if *position != expected {
                return Some(expected);
            }
            if expected == from {
                return None;
            }
            expected -= 1;
        }
    }
    Some(expected)
}

impl<A: RowApplication> Inner<A> {
    /// Finalizes the domain's state at `seal`, inside the caller's
    /// transaction: each changed scope's excluded operations move after its
    /// included ones, its rows are rebuilt at the frontier, what is dead
    /// there is recorded, and its rows are rebuilt again with every
    /// operation. Unchanged scopes are not touched.
    async fn finalize_seal(
        &mut self,
        domain: Scope,
        seal: &SignedSeal,
        changes: &[SealChange],
    ) -> Result<(), Error> {
        let depth = match collect_rows(
            self.conn
                .query(
                    "SELECT COALESCE(MAX(depth),0)+1 FROM secure_finalized WHERE domain=?",
                    [encode(&domain)?],
                )
                .await?,
        )
        .await?
        .first()
        .map(Vec::as_slice)
        {
            Some([SqlValue::Integer(depth)]) => *depth,
            _ => return Err(Error::Corrupt("finalized depth".into())),
        };
        let changed: Vec<&SealChange> = changes.iter().filter(|change| change.changed).collect();
        // Application order stays causal: every operation the seal
        // excludes, in order, after every one it includes.
        for change in &changed {
            for (position, hash) in &change.excluded {
                self.conn
                    .execute("DELETE FROM secure_applied WHERE position=?", [*position])
                    .await?;
                self.conn
                    .execute(
                        "INSERT INTO secure_applied(scope,hash) VALUES(?,?)",
                        bindings(vec![
                            SqlValue::Integer(change.id),
                            SqlValue::Blob(hash.clone()),
                        ]),
                    )
                    .await?;
            }
        }
        for change in &changed {
            self.restore_rows(change.scope, change.through).await?;
        }
        for change in &changed {
            self.reproject(change.scope).await?;
        }
        let mut drops = Vec::new();
        for change in &changed {
            drops.push((change.id, self.seal_drops(change.scope).await?));
        }
        self.conn
            .execute(
                "INSERT INTO secure_finalized VALUES(?,?,?,0)",
                bindings(vec![
                    SqlValue::Blob(encode(&domain)?),
                    SqlValue::Integer(depth),
                    SqlValue::Blob(seal.id().map_err(credential)?.to_vec()),
                ]),
            )
            .await?;
        for change in changes {
            self.conn
                .execute(
                    "INSERT INTO secure_seal_bounds VALUES(?,?,?)",
                    [change.id, depth, change.through],
                )
                .await?;
        }
        for (id, rows) in drops {
            for (row, fields) in rows {
                self.conn
                    .execute(
                        "INSERT INTO secure_seal_drops VALUES(?,?,?,?)",
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Integer(depth),
                            SqlValue::Blob(row.to_vec()),
                            fields.map_or(SqlValue::Null, SqlValue::Blob),
                        ]),
                    )
                    .await?;
            }
        }
        for change in &changed {
            self.restore_rows(change.scope, i64::MAX).await?;
            self.reproject(change.scope).await?;
        }
        self.conn
            .execute(
                "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                (),
            )
            .await?;
        Ok(())
    }
    /// Finalizes the followed path's seals in order while this replica
    /// holds every frame through the next one. Returns how many it
    /// finalized and why the next waits.
    pub(super) async fn finalize_ready(
        &mut self,
        domain: Scope,
    ) -> Result<(usize, Option<String>), Error> {
        let mut count = 0;
        loop {
            if self.compaction_lost(domain).await?.is_some() {
                return Ok((count, Some("the compacted seal lost a race".into())));
            }
            let path = self.path_seals(domain)?;
            let finalized = self.finalized(domain).await?;
            let Some(start) = offset(&path, &finalized) else {
                return Ok((count, Some("the finalized seals left the path".into())));
            };
            let next = start + finalized.len();
            let Some(seal) = path.get(next).cloned() else {
                return Ok((count, None));
            };
            let previous = next
                .checked_sub(1)
                .and_then(|index| path.get(index))
                .cloned();
            let scopes = self.domain_scopes(domain).await?;
            if let Some(reason) = self
                .seal_waits(domain, &seal, previous.as_ref(), &scopes)
                .await?
            {
                return Ok((count, Some(reason)));
            }
            transaction!(self, |inner| {
                // A replay base at another cut covers operations a seal may
                // exclude; nothing it covers is pruned, so it goes.
                for (scope, id) in &scopes {
                    let ordinary =
                        inner
                            .checkpoint_record(*id, false)
                            .await?
                            .is_some_and(|record| {
                                record
                                    .signed
                                    .as_ref()
                                    .is_none_or(|signed| signed.statement.seal.is_none())
                            });
                    if ordinary {
                        inner.clear_checkpoints(*scope, *id).await?;
                    }
                }
                let changes = inner.seal_changes(domain, &seal, &scopes).await?;
                inner.finalize_seal(domain, &seal, &changes).await
            })?;
            count += 1;
        }
    }
    async fn clear_checkpoints(&mut self, _scope: ResourceScope, id: i64) -> Result<(), Error> {
        for table in [
            "secure_checkpoints",
            "secure_checkpoint_rows",
            "secure_checkpoint_overlay",
            "secure_checkpoint_scratch",
            "secure_checkpoint_pages",
            "secure_checkpoint_stubs",
            "secure_checkpoint_build_stubs",
        ] {
            self.conn
                .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                .await?;
        }
        self.verified_at.remove(&id);
        Ok(())
    }
    pub(super) async fn compaction_lost(&self, domain: Scope) -> Result<Option<Commitment>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT seal FROM secure_compaction_lost WHERE domain=?",
                    [encode(&domain)?],
                )
                .await?,
        )
        .await?;
        match records.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(seal)]) => Ok(Some(checkpoint::row_of(seal)?)),
            None => Ok(None),
            _ => Err(Error::Corrupt("lost compaction".into())),
        }
    }
    /// Inside the caller's transaction, forgets the finalized seals no
    /// longer on the followed path after a race resolved again or the
    /// bookmark moved, and with `restore` rebuilds the scopes they changed.
    /// A compacted seal that left the path cannot be undone: the domain
    /// waits for a rebootstrap.
    pub(super) async fn revalidate_finalized(
        &mut self,
        domain: Scope,
        restore: bool,
    ) -> Result<(), Error> {
        if !self.graphs.contains_key(&domain) {
            return Ok(());
        }
        let path = self.path_seals(domain)?;
        let finalized = self.finalized(domain).await?;
        let mut valid = 0;
        let start = offset(&path, &finalized).unwrap_or(path.len());
        for (known, seal) in finalized.iter().zip(&path[start..]) {
            if known.seal != seal.id().map_err(credential)? {
                break;
            }
            valid += 1;
        }
        let Some(first) = finalized.get(valid) else {
            // The compacted seal is on the path again.
            self.conn
                .execute(
                    "DELETE FROM secure_compaction_lost WHERE domain=?",
                    [encode(&domain)?],
                )
                .await?;
            return Ok(());
        };
        if let Some(compacted) = finalized[valid..]
            .iter()
            .rev()
            .find(|known| known.compacted)
        {
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO secure_compaction_lost VALUES(?,?)",
                    bindings(vec![
                        SqlValue::Blob(encode(&domain)?),
                        SqlValue::Blob(compacted.seal.to_vec()),
                    ]),
                )
                .await?;
            return Ok(());
        }
        self.conn
            .execute(
                "DELETE FROM secure_finalized WHERE domain=? AND depth>=?",
                bindings(vec![
                    SqlValue::Blob(encode(&domain)?),
                    SqlValue::Integer(first.depth),
                ]),
            )
            .await?;
        for (scope, id) in self.domain_scopes(domain).await? {
            let dropped = collect_rows(
                self.conn
                    .query(
                        "SELECT 1 FROM secure_seal_drops WHERE scope=? AND depth>=? LIMIT 1",
                        [id, first.depth],
                    )
                    .await?,
            )
            .await?;
            for table in ["secure_seal_bounds", "secure_seal_drops"] {
                self.conn
                    .execute(
                        format!("DELETE FROM {table} WHERE scope=? AND depth>=?"),
                        [id, first.depth],
                    )
                    .await?;
            }
            if restore && !dropped.is_empty() {
                self.restore_rows(scope, i64::MAX).await?;
                self.reproject(scope).await?;
            }
        }
        Ok(())
    }
    /// The newest finalized seal that is final, or has at least `keep`
    /// seals after it on the path, with every uncompacted one before it so.
    async fn compaction_target(
        &self,
        domain: Scope,
        keep: usize,
    ) -> Result<Option<(Finalized, SignedSeal)>, Error> {
        let graph = self.graph_for(domain)?;
        let path = self.path_seals(domain)?;
        let mut target = None;
        for known in self.finalized(domain).await? {
            if known.compacted {
                continue;
            }
            let Some(index) = path
                .iter()
                .position(|seal| seal.id().is_ok_and(|id| id == known.seal))
            else {
                break;
            };
            if graph.is_final(known.seal).map_err(credential)? || path.len() - 1 - index >= keep {
                target = Some((known, path[index].clone()));
            } else {
                break;
            }
        }
        Ok(target)
    }
    /// Makes `seal`'s state each held scope's replay base, signed by this
    /// node, and forgets the finalized seals up to it. Creation stubs of
    /// rows no longer stored go with it.
    async fn compact_to(
        &mut self,
        domain: Scope,
        (target, seal): &(Finalized, SignedSeal),
    ) -> Result<(), Error> {
        let at = self.path_index(domain, target.seal)?;
        for (scope, id) in self.domain_scopes(domain).await? {
            // A base imported at a later seal stays.
            if let Some(base) = self.base_seal(id).await?
                && self.path_index(domain, base)? > at
            {
                continue;
            }
            let logged = self.existence_table(scope);
            transaction!(self, |inner| {
                inner.compact_scope(scope, id, target, seal).await?;
                if logged {
                    inner.fold_existence(scope, seal).await?;
                }
                Ok(())
            })?;
            let commit = self.commit;
            self.verified_at.insert(id, commit);
        }
        // Only the replay base's seal is kept of the compacted ones.
        for sql in [
            "UPDATE secure_finalized SET compacted=1 WHERE domain=? AND depth=?",
            "DELETE FROM secure_finalized WHERE domain=? AND depth<?",
        ] {
            self.conn
                .execute(
                    sql,
                    bindings(vec![
                        SqlValue::Blob(encode(&domain)?),
                        SqlValue::Integer(target.depth),
                    ]),
                )
                .await?;
        }
        Ok(())
    }
    async fn compact_scope(
        &mut self,
        scope: ResourceScope,
        id: i64,
        target: &Finalized,
        seal: &SignedSeal,
    ) -> Result<(), Error> {
        let previous = self
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed.map(|signed| (record.position, signed)));
        let base = previous
            .as_ref()
            .filter(|(_, signed)| signed.statement.seal.is_some());
        let mut cursor = base.map_or(0, |(position, _)| *position);
        // The bound of the target, else of the newest finalized seal before
        // it this scope has one for: a scope that appeared later has none.
        let through = self
            .seal_bounds(id)
            .await?
            .into_iter()
            .filter(|(depth, _)| *depth <= target.depth)
            .map(|(_, position)| position)
            .next_back()
            .unwrap_or(cursor);
        let mut sets = match self.checkpoint_record(id, true).await? {
            Some(record) if base.is_some() => record.sets,
            _ => Sets::new(),
        };
        self.conn
            .execute("DELETE FROM secure_checkpoint_overlay WHERE scope=?", [id])
            .await?;
        while let Some(last) = self
            .replay_sealed(
                scope,
                id,
                Overlay::Build,
                (cursor, through),
                STEP_OPERATIONS,
                &mut sets,
            )
            .await?
        {
            cursor = last;
        }
        // Operations the base covers: its frontier per log, and its count.
        let mut frontier = BTreeMap::<PublicKey, LogPosition>::new();
        let mut operations = base.map_or(0, |(_, signed)| signed.statement.operations);
        if let Some((_, signed)) = base {
            for log in &signed.statement.frontier {
                frontier.insert(log.author, log.clone());
            }
        }
        let from = base.map_or(0, |(position, _)| *position);
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT a.hash,o.header FROM secure_applied a JOIN secure_operations o ON o.hash=a.hash WHERE a.scope=? AND a.position>? AND a.position<=? ORDER BY a.position",
                    [id, from, through],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(hash), SqlValue::Blob(header)] = record.as_slice() else {
                return Err(Error::Corrupt("covered operation".into()));
            };
            let header: p2panda_core::Header<Extensions> = decode(header)?;
            operations += 1;
            let log = LogPosition {
                author: header.public_key,
                sequence: header.seq_num,
                hash: checkpoint::hash_of(hash)?,
                clock: header.timestamp,
            };
            if frontier
                .get(&log.author)
                .is_none_or(|known| known.sequence < log.sequence)
            {
                frontier.insert(log.author, log);
            }
        }
        // The overlay becomes the base rows.
        for sql in [
            "DELETE FROM secure_checkpoint_rows WHERE scope=?1 AND row IN (SELECT row FROM secure_checkpoint_overlay WHERE scope=?1)",
            "INSERT INTO secure_checkpoint_rows SELECT scope,row,fields FROM secure_checkpoint_overlay WHERE scope=?1 AND fields<>X''",
            "DELETE FROM secure_checkpoint_overlay WHERE scope=?1",
        ] {
            self.conn.execute(sql, [id]).await?;
        }
        // The base names the creations of the rows it stores, as an import
        // rebuilds them; rows dropped at a seal take theirs along.
        sets.creations = 0;
        sets.creation_digest = checkpoint::Lt::new();
        for creation in self.base_creations(id).await? {
            let (_, clock) = self
                .stub(creation)
                .await?
                .ok_or_else(|| Error::Corrupt("base row creation is unknown".into()))?;
            sets.create(creation, clock);
        }
        let tip = seal.statement.tip;
        let schema = self
            .generation_of(self.manifest_of_tip(tip)?, false)?
            .schema;
        let (handles, handle_digest) = self.handle_set().await?;
        let frontier: Vec<LogPosition> = frontier.into_values().collect();
        let statement = CheckpointStatement {
            format: CHECKPOINT_FORMAT,
            seal: Some(target.seal),
            horizon: None,
            finalized: None,
            partition: self.partitions.get(&scope).cloned(),
            identities: self.identity_marks(scope).await?,
            moves: self.imported_moves(id).await?,
            tip,
            schema,
            revocations: self.revocation_digest()?,
            scope,
            key_epoch: self.scope_key(scope)?.0,
            sequence: previous
                .as_ref()
                .map_or(0, |(_, signed)| signed.statement.sequence)
                + 1,
            previous: previous
                .as_ref()
                .map(|(_, signed)| signed.id())
                .transpose()?,
            clock: frontier.iter().map(|log| log.clock).max().unwrap_or(0),
            frontier,
            operations,
            rows: sets.rows_summary(),
            tombstones: sets.tombstones,
            creations: sets.creations_summary(),
            handles: SetSummary {
                count: handles,
                digest: handle_digest.digest(),
            },
            custody: self.custody_summary(id, through).await?,
            coordinated: self.seal_binding(scope, id, through).await?,
        };
        let signed = self.sign_checkpoint(statement)?;
        for table in [
            "secure_checkpoints",
            "secure_checkpoint_pages",
            "secure_checkpoint_stubs",
            "secure_checkpoint_build_stubs",
        ] {
            self.conn
                .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                .await?;
        }
        self.conn
            .execute(
                "INSERT INTO secure_checkpoints VALUES(?,?,'verified',?,?,?,NULL,?)",
                bindings(vec![
                    SqlValue::Integer(id),
                    SqlValue::Integer(signed.statement.sequence as i64),
                    SqlValue::Integer(through),
                    SqlValue::Integer(through),
                    SqlValue::Blob(sets.to_bytes()),
                    SqlValue::Blob(signed.to_bytes()?),
                ]),
            )
            .await?;
        for table in ["secure_seal_bounds", "secure_seal_drops"] {
            self.conn
                .execute(
                    format!("DELETE FROM {table} WHERE scope=? AND depth<=?"),
                    [id, target.depth],
                )
                .await?;
        }
        // Stubs witness creations of stored rows and pruned dependencies;
        // past the seal only the creations of stored and base rows are needed.
        self.conn
            .execute(
                "DELETE FROM secure_horizon_live_creations WHERE scope=?",
                [id],
            )
            .await?;
        for creation in self.base_creations(id).await? {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO secure_horizon_live_creations VALUES(?,?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(creation.as_bytes().to_vec()),
                    ]),
                )
                .await?;
        }
        let blob = encode(&scope)?;
        let mut after = Vec::new();
        loop {
            let records = collect_rows(
                self.conn
                    .query(
                        "SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?",
                        bindings(vec![
                            SqlValue::Blob(blob.clone()),
                            SqlValue::Blob(after.clone()),
                            SqlValue::Integer(STEP_ROWS as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            if records.is_empty() {
                break;
            }
            for record in records {
                let [SqlValue::Blob(key), SqlValue::Blob(fields)] = record.as_slice() else {
                    return Err(Error::Corrupt("stored row".into()));
                };
                after = key.clone();
                let record: Materialized = decode(fields)?;
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO secure_horizon_live_creations VALUES(?,?)",
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(record.creation.as_bytes().to_vec()),
                        ]),
                    )
                    .await?;
            }
        }
        self.conn
            .execute(
                "DELETE FROM secure_stubs WHERE scope=?1 AND (creation=0 OR hash NOT IN (SELECT hash FROM secure_horizon_live_creations WHERE scope=?1))",
                [id],
            )
            .await?;
        self.conn
            .execute(
                "DELETE FROM secure_horizon_live_creations WHERE scope=?",
                [id],
            )
            .await?;
        Ok(())
    }
}

impl<A: RowApplication> Inner<A> {
    /// The creation operations of scope `id`'s base rows.
    async fn base_creations(&self, id: i64) -> Result<BTreeSet<Hash>, Error> {
        let mut creations = BTreeSet::new();
        let mut after = Vec::new();
        loop {
            let records = collect_rows(
                self.conn
                    .query(
                        "SELECT row,fields FROM secure_checkpoint_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?",
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(after.clone()),
                            SqlValue::Integer(STEP_ROWS as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            if records.is_empty() {
                return Ok(creations);
            }
            for record in records {
                let [SqlValue::Blob(key), SqlValue::Blob(fields)] = record.as_slice() else {
                    return Err(Error::Corrupt("base row".into()));
                };
                after = key.clone();
                creations.insert(decode::<Materialized>(fields)?.creation);
            }
        }
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Finalizes every seal of the followed path whose frames this replica
    /// holds, then makes the newest finalized seal that is final, or has at
    /// least `keep` seals after it on the path, the replay base, and
    /// releases the operations behind it as far as retention allows. A
    /// seal that loses after compaction needs a rebootstrap; `keep` trades
    /// storage against that chance.
    pub async fn compact(&self, domain: Scope, keep: usize) -> Result<CompactReport, Error> {
        let mut inner = self.inner.lock().await;
        inner.graph_for(domain)?;
        let (_, waiting) = inner.finalize_ready(domain).await?;
        if let Some(target) = inner.compaction_target(domain, keep).await? {
            inner.compact_to(domain, &target).await?;
        }
        let mut report = CompactReport {
            waiting,
            ..CompactReport::default()
        };
        for (scope, _) in inner.domain_scopes(domain).await? {
            loop {
                let step = inner.prune_step(scope).await?;
                report.pruned += step.pruned;
                if let Some(block) = step.blocked {
                    if !matches!(block, PruneBlock::NoPolicy) {
                        report.blocked.push((scope, block));
                    }
                    break;
                }
                if !step.more {
                    break;
                }
            }
        }
        let finalized = inner.finalized(domain).await?;
        report.finalized = finalized.last().map(|known| known.seal);
        report.compacted = finalized
            .iter()
            .rev()
            .find(|known| known.compacted)
            .map(|known| known.seal);
        inner.flush()?;
        inner.publish_revision().await?;
        Ok(report)
    }
    pub async fn compaction_status(&self, domain: Scope) -> Result<CompactionStatus, Error> {
        let inner = self.inner.lock().await;
        inner.graph_for(domain)?;
        let finalized = inner.finalized(domain).await?;
        let path = inner.path_seals(domain)?;
        let next = offset(&path, &finalized).map(|start| start + finalized.len());
        let waiting = match next.and_then(|next| path.get(next)) {
            Some(seal) => {
                let previous = next
                    .and_then(|next| next.checked_sub(1))
                    .and_then(|index| path.get(index))
                    .cloned();
                let scopes = inner.domain_scopes(domain).await?;
                inner
                    .seal_waits(domain, seal, previous.as_ref(), &scopes)
                    .await?
            }
            None => None,
        };
        let rebootstrap = inner.compaction_lost(domain).await?;
        Ok(CompactionStatus {
            finalized: finalized.last().map(|known| known.seal),
            compacted: finalized
                .iter()
                .rev()
                .find(|known| known.compacted)
                .map(|known| known.seal),
            uncompacted: finalized
                .iter()
                .filter(|known| !known.compacted)
                .map(|known| known.seal)
                .collect(),
            waiting,
            rebootstrap,
        })
    }
}
