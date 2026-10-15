//! Owner-signed cuts on devices: a verified record fences the abandoned
//! positions of its log, and a durable rollback in five steps removes their
//! effects. See decision 0074 and `docs/runtime/stuck-recovery.md`.
use super::*;
use crate::replica::checkpoint::CoordinatedBinding;
use rocky_credentials::{CutStatement, MAX_CUT_AUTHORITY_BYTES, SignedCut};

pub(in crate::replica) const SCHEMA: &[&str] = &[
    // Every verified cut of a log, in order, with the owner's chain.
    "CREATE TABLE IF NOT EXISTS secure_cuts(log BLOB NOT NULL,sequence INTEGER NOT NULL,digest BLOB NOT NULL,record BLOB NOT NULL,authority BLOB NOT NULL,PRIMARY KEY(log,sequence)) WITHOUT ROWID",
    // The rollback of an admitted cut: its step and the progress within it.
    "CREATE TABLE IF NOT EXISTS secure_cut_work(log BLOB PRIMARY KEY,sequence INTEGER NOT NULL,step INTEGER NOT NULL,cursor BLOB) WITHOUT ROWID",
    // Scopes whose replay base holds abandoned effects: no write or sync
    // until a clean checkpoint is imported.
    "CREATE TABLE IF NOT EXISTS secure_cut_guard(scope BLOB PRIMARY KEY,log BLOB NOT NULL,sequence INTEGER NOT NULL) WITHOUT ROWID",
    // The log position of each applied frame of a coordinated write.
    "CREATE TABLE IF NOT EXISTS secure_entry_frames(hash BLOB PRIMARY KEY,log BLOB NOT NULL,position INTEGER NOT NULL) WITHOUT ROWID",
    "CREATE INDEX IF NOT EXISTS secure_entry_frames_position ON secure_entry_frames(log,position)",
];
/// Cut records one sync message carries.
pub const MAX_PAGE_CUTS: usize = 16;
/// Frames one classification step marks.
const CLASSIFY_PAGE: i64 = 256;
/// Positions an endorsed gap before the hash window may span.
const MAX_ENDORSED: u64 = 1 << 20;

/// A cut record with the owner's credential chain, as devices relay it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CutEvidence {
    pub record: SignedCut,
    pub authority: CredentialChain,
}
impl CutEvidence {
    pub fn from_log(cut: &rocky_core::LogCut) -> Result<Self, Error> {
        Ok(Self {
            record: SignedCut::from_bytes(&cut.record).map_err(credential)?,
            authority: decode_bounded(&cut.authority, MAX_CUT_AUTHORITY_BYTES)?,
        })
    }
    pub fn to_log(&self) -> Result<rocky_core::LogCut, Error> {
        let authority = encode(&self.authority)?;
        if authority.len() > MAX_CUT_AUTHORITY_BYTES {
            return Err(denied("cut authority quota"));
        }
        Ok(rocky_core::LogCut {
            record: self.record.to_bytes().map_err(credential)?,
            authority,
        })
    }
}
/// Why a cut refused a call. `code` is the JSON error code.
#[derive(Debug)]
pub struct CutRefusal {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for CutRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for CutRefusal {}
pub(in crate::replica) fn refusal(code: &'static str, message: &str) -> Error {
    Error::Application(Box::new(CutRefusal {
        code,
        message: message.into(),
    }))
}

/// A verified cut this replica holds.
#[derive(Clone, Debug)]
pub(in crate::replica) struct HeldCut {
    pub digest: Commitment,
    pub statement: CutStatement,
}
/// What the held cuts of a log say of an entry position.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::replica) enum Fence {
    Live,
    /// A cut abandoned it: consumed without effect.
    Abandoned,
    /// A cut occupies it: no frame may name it.
    Cut,
}
pub(in crate::replica) fn fence(cuts: &[HeldCut], position: u64) -> Fence {
    for cut in cuts {
        if cut.statement.entry() == position {
            return Fence::Cut;
        }
        if cut.statement.abandons(position) {
            return Fence::Abandoned;
        }
    }
    Fence::Live
}
/// What a checkpoint's binding means against the held cuts of its log.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::replica) enum Base {
    Clean,
    /// It may contain abandoned effects: never a replay base.
    Tainted,
    /// It names a cut this replica does not hold yet.
    Unknown,
}
pub(in crate::replica) fn classify(binding: &CoordinatedBinding, cuts: &[HeldCut]) -> Base {
    let Ok(applied) = usize::try_from(binding.cuts) else {
        return Base::Unknown;
    };
    if applied > cuts.len() {
        return Base::Unknown;
    }
    if applied > 0 && Some(cuts[applied - 1].digest) != binding.cut {
        return Base::Tainted;
    }
    if cuts[applied..]
        .iter()
        .any(|cut| binding.highest > cut.statement.retained.position)
    {
        return Base::Tainted;
    }
    Base::Clean
}
/// The binding of a checkpoint sealed over `highest` with `cuts` held.
pub(in crate::replica) fn binding(cuts: &[HeldCut], highest: u64) -> CoordinatedBinding {
    CoordinatedBinding {
        cuts: cuts.len() as u64,
        cut: cuts.last().map(|cut| cut.digest),
        highest,
    }
}

/// What admitting a record did.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::replica) enum Admission {
    /// Already held.
    Held,
    /// Its predecessor is missing; nothing was stored.
    Waiting,
    Admitted,
}
/// The rollback of one admitted cut.
struct Work {
    log: TipLog,
    sequence: u64,
    step: i64,
    cursor: Option<Vec<u8>>,
}

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) fn held_cuts(&self, log: TipLog) -> &[HeldCut] {
        self.cuts.get(&log).map_or(&[], Vec::as_slice)
    }
    pub(in crate::replica) async fn load_cuts(&mut self) -> Result<(), Error> {
        let mut cuts: BTreeMap<TipLog, Vec<HeldCut>> = BTreeMap::new();
        for record in rows(
            &self.conn,
            "SELECT record FROM secure_cuts ORDER BY log,sequence",
        )
        .await?
        {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("cut record".into()));
            };
            let signed = SignedCut::from_bytes(bytes).map_err(credential)?;
            cuts.entry(signed.statement.log).or_default().push(HeldCut {
                digest: signed.digest().map_err(credential)?,
                statement: signed.statement,
            });
        }
        self.cuts = cuts;
        Ok(())
    }
    /// The scopes of `log`'s domain this replica stores or holds a key of.
    pub(in crate::replica) async fn log_scopes(
        &self,
        log: TipLog,
    ) -> Result<Vec<ResourceScope>, Error> {
        let mut scopes = BTreeSet::new();
        for record in rows(&self.conn, "SELECT scope FROM secure_scope_state").await? {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("scope state".into()));
            };
            let scope: ResourceScope = decode(bytes)?;
            if scope.domain == log.scope {
                scopes.insert(scope);
            }
        }
        scopes.extend(
            self.config
                .keys
                .iter()
                .map(|key| key.scope)
                .filter(|scope| scope.domain == log.scope),
        );
        Ok(scopes.into_iter().collect())
    }
    /// The id of `scope` when this replica projects its rows: it reads the
    /// scope's key and stores it, not as keyless custody.
    pub(in crate::replica) async fn projected(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<i64>, Error> {
        let readable = self.config.keys.iter().any(|key| key.scope == scope)
            && self.can_read_key(scope, &self.local_grants(now()?)?);
        if !readable || self.direct.contains(&scope) {
            return Ok(None);
        }
        self.find_scope_id(scope).await
    }
    /// Cut records of `log` after the first `held`, for a peer that holds
    /// that many.
    pub(in crate::replica) async fn cuts_after(
        &self,
        log: TipLog,
        held: u64,
    ) -> Result<Vec<CutEvidence>, Error> {
        let mut cuts = Vec::new();
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT record,authority FROM secure_cuts WHERE log=? AND sequence>? ORDER BY sequence LIMIT ?",
                    bindings(vec![
                        SqlValue::Blob(encode(&log)?),
                        SqlValue::Integer(held.min(i64::MAX as u64) as i64),
                        SqlValue::Integer(MAX_PAGE_CUTS as i64),
                    ]),
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(record), SqlValue::Blob(authority)] = record.as_slice() else {
                return Err(Error::Corrupt("cut record".into()));
            };
            cuts.push(CutEvidence::from_log(&rocky_core::LogCut {
                record: record.clone(),
                authority: authority.clone(),
            })?);
        }
        Ok(cuts)
    }
    /// Notes the log position of frames of a coordinated write.
    pub(in crate::replica) async fn record_entry_frames(
        &self,
        log: TipLog,
        position: u64,
        ids: &[Hash],
    ) -> Result<(), Error> {
        for id in ids {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO secure_entry_frames VALUES(?,?,?)",
                    bindings(vec![
                        SqlValue::Blob(id.as_bytes().to_vec()),
                        SqlValue::Blob(encode(&log)?),
                        SqlValue::Integer(position as i64),
                    ]),
                )
                .await?;
        }
        Ok(())
    }
    /// The highest entry position among applied frames of scope `id` at
    /// `secure_applied` positions `after..=through`, at least `floor`.
    pub(in crate::replica) async fn highest_entry(
        &self,
        id: i64,
        (after, through): (i64, i64),
        floor: u64,
    ) -> Result<u64, Error> {
        let record = collect_rows(
            self.conn
                .query(
                    "SELECT COALESCE(MAX(e.position),0) FROM secure_entry_frames e JOIN secure_applied a ON a.hash=e.hash JOIN secure_operations o ON o.hash=e.hash WHERE a.scope=? AND a.position>? AND a.position<=? AND o.status='applied'",
                    [id, after, through],
                )
                .await?,
        )
        .await?;
        match record.first().map(Vec::as_slice) {
            Some([SqlValue::Integer(highest)]) => Ok(floor.max(*highest as u64)),
            _ => Err(Error::Corrupt("entry positions".into())),
        }
    }
    /// Consumes a frame without effect: its log continues and checkpoints
    /// cover it, but it is never projected.
    pub(in crate::replica) async fn abandon_frame(
        &self,
        frame: &EncryptedFrame,
    ) -> Result<(), Error> {
        let operation = frame.operation()?;
        let id = self.scope_id(operation.header.extensions.scope).await?;
        self.note_applied(id, &operation).await?;
        self.conn
            .execute(
                "UPDATE secure_operations SET status='abandoned' WHERE hash=?",
                [operation.hash.as_bytes().to_vec()],
            )
            .await?;
        Ok(())
    }
    /// Whether a dependency of `header` was abandoned by a cut.
    pub(in crate::replica) async fn abandoned_dependency(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<bool, Error> {
        for id in header.previous.iter().chain(header.backlink.iter()) {
            if self.stored_status(*id).await?.as_deref() == Some("abandoned") {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// A mutation whose frame a cut abandoned reports that, not success.
    pub(in crate::replica) async fn refuse_abandoned(&self, hash: Hash) -> Result<(), Error> {
        if self.stored_status(hash).await?.as_deref() == Some("abandoned") {
            return Err(refusal(
                "abandoned",
                "an owner-signed cut abandoned this mutation",
            ));
        }
        Ok(())
    }
    pub(in crate::replica) async fn cut_guarded(
        &self,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        let mut found = self
            .conn
            .query(
                "SELECT 1 FROM secure_cut_guard WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        Ok(found.next().await?.is_some())
    }
    /// Refuses writes and sync of a scope whose replay base a cut tainted.
    pub(in crate::replica) async fn ensure_cut_clean(
        &self,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        if self.cut_guarded(scope).await? {
            return Err(refusal(
                "cut_base_missing",
                "the scope's replay base holds abandoned coordinated effects; import a clean checkpoint",
            ));
        }
        Ok(())
    }
    /// A checkpoint is a replay base only when no held cut taints it; one
    /// naming a cut this replica lacks waits for it.
    pub(in crate::replica) fn check_binding(
        &self,
        statement: &CheckpointStatement,
    ) -> Result<(), Error> {
        match classify(
            &statement.coordinated,
            self.held_cuts(self.tip_log(statement.scope.domain)),
        ) {
            Base::Clean => Ok(()),
            Base::Tainted => Err(Error::Rejected(
                "checkpoint base holds abandoned coordinated effects".into(),
            )),
            Base::Unknown => Err(Error::Protocol(rocky_core::ProtocolError::NotReady(
                Vec::new(),
            ))),
        }
    }
    /// The binding of a checkpoint of scope `id` sealed at `through` over
    /// the verified base.
    pub(in crate::replica) async fn seal_binding(
        &self,
        scope: ResourceScope,
        id: i64,
        through: i64,
    ) -> Result<CoordinatedBinding, Error> {
        let (after, floor) = match self.checkpoint_record(id, true).await? {
            Some(record) => (
                record.position,
                record
                    .signed
                    .map_or(0, |signed| signed.statement.coordinated.highest),
            ),
            None => (0, 0),
        };
        let highest = self.highest_entry(id, (after, through), floor).await?;
        Ok(binding(self.held_cuts(self.tip_log(scope.domain)), highest))
    }
    /// Verifies a record's signature and that its signer is the owner of
    /// every scope of its domain this replica holds.
    pub(in crate::replica) async fn verify_cut(&self, evidence: &CutEvidence) -> Result<(), Error> {
        let record = &evidence.record;
        record.verify().map_err(credential)?;
        let scopes = self.log_scopes(record.statement.log).await?;
        if scopes.is_empty() {
            return Err(denied(
                "the cut names a domain this replica holds nothing of",
            ));
        }
        let scopes: Vec<_> = scopes
            .iter()
            .map(|scope| (*scope, self.partitions.get(scope)))
            .collect();
        record
            .verify_owner(&evidence.authority, self.config.publisher, &scopes)
            .map_err(credential)
    }
    /// Verifies a record and its owner and stores it with its rollback,
    /// inside the caller's transaction; `load_cuts` follows the commit.
    pub(in crate::replica) async fn admit_cut(
        &self,
        evidence: &CutEvidence,
    ) -> Result<Admission, Error> {
        self.verify_cut(evidence).await?;
        let record = &evidence.record;
        let log = record.statement.log;
        let digest = record.digest().map_err(credential)?;
        let held = self.held_cuts(log);
        let sequence = record.statement.sequence;
        let count = held.len() as u64;
        if sequence <= count {
            return match held[(sequence - 1) as usize].digest == digest {
                true => Ok(Admission::Held),
                false => Err(refusal(
                    "cut_conflict",
                    "the owner signed two cuts of one sequence",
                )),
            };
        }
        if sequence > count + 1 {
            return Ok(Admission::Waiting);
        }
        if record.statement.previous != held.last().map(|cut| cut.digest) {
            return Err(refusal(
                "cut_conflict",
                "the cut follows another history of cuts",
            ));
        }
        if held
            .last()
            .is_some_and(|last| record.statement.retained.position < last.statement.entry())
        {
            return Err(denied("a cut never abandons a cut"));
        }
        let blob = encode(&log)?;
        self.conn
            .execute(
                "INSERT INTO secure_cuts VALUES(?,?,?,?,?)",
                bindings(vec![
                    SqlValue::Blob(blob.clone()),
                    SqlValue::Integer(sequence as i64),
                    SqlValue::Blob(digest.to_vec()),
                    SqlValue::Blob(record.to_bytes().map_err(credential)?),
                    SqlValue::Blob(encode(&evidence.authority)?),
                ]),
            )
            .await?;
        self.conn
            .execute(
                "INSERT INTO secure_cut_work VALUES(?,?,2,NULL)",
                bindings(vec![
                    SqlValue::Blob(blob),
                    SqlValue::Integer(sequence as i64),
                ]),
            )
            .await?;
        Ok(Admission::Admitted)
    }
    /// Whether a cut of `log` still has rollback work: no checkpoint of its
    /// scopes begins or seals meanwhile.
    pub(in crate::replica) async fn cut_pending(&self, log: TipLog) -> Result<bool, Error> {
        let mut found = self
            .conn
            .query("SELECT 1 FROM secure_cut_work WHERE log=?", [encode(&log)?])
            .await?;
        Ok(found.next().await?.is_some())
    }
    async fn cut_work(&self) -> Result<Option<Work>, Error> {
        let record = rows(
            &self.conn,
            "SELECT log,sequence,step,cursor FROM secure_cut_work ORDER BY log LIMIT 1",
        )
        .await?;
        let Some(record) = record.first() else {
            return Ok(None);
        };
        let [
            SqlValue::Blob(log),
            SqlValue::Integer(sequence),
            SqlValue::Integer(step),
            cursor,
        ] = record.as_slice()
        else {
            return Err(Error::Corrupt("cut work".into()));
        };
        Ok(Some(Work {
            log: decode(log)?,
            sequence: *sequence as u64,
            step: *step,
            cursor: match cursor {
                SqlValue::Blob(cursor) => Some(cursor.clone()),
                _ => None,
            },
        }))
    }
    fn work_cut(&self, work: &Work) -> Result<HeldCut, Error> {
        self.held_cuts(work.log)
            .get((work.sequence as usize).wrapping_sub(1))
            .cloned()
            .ok_or_else(|| Error::Corrupt("cut work without its record".into()))
    }
    async fn advance_work(
        &self,
        work: &Work,
        step: i64,
        cursor: Option<Vec<u8>>,
    ) -> Result<(), Error> {
        self.conn
            .execute(
                "UPDATE secure_cut_work SET step=?,cursor=? WHERE log=?",
                bindings(vec![
                    SqlValue::Integer(step),
                    cursor.map_or(SqlValue::Null, SqlValue::Blob),
                    SqlValue::Blob(encode(&work.log)?),
                ]),
            )
            .await?;
        Ok(())
    }
    /// Step 2: marks one page of held frames in the abandoned range; the
    /// last page also drops the log's entries there.
    async fn classify_step(&self, work: &Work) -> Result<(), Error> {
        let statement = self.work_cut(work)?.statement;
        let (retained, replaced) = (statement.retained.position, statement.replaced.position);
        let blob = encode(&work.log)?;
        let page = collect_rows(
            self.conn
                .query(
                    "SELECT hash FROM secure_entry_frames WHERE log=? AND position>? AND position<=? AND hash>? ORDER BY hash LIMIT ?",
                    bindings(vec![
                        SqlValue::Blob(blob.clone()),
                        SqlValue::Integer(retained as i64),
                        SqlValue::Integer(replaced as i64),
                        SqlValue::Blob(work.cursor.clone().unwrap_or_default()),
                        SqlValue::Integer(CLASSIFY_PAGE),
                    ]),
                )
                .await?,
        )
        .await?;
        let mut last = None;
        for record in &page {
            let [SqlValue::Blob(hash)] = record.as_slice() else {
                return Err(Error::Corrupt("entry frame".into()));
            };
            self.conn
                .execute(
                    "UPDATE secure_operations SET status='abandoned' WHERE hash=? AND status='applied'",
                    [hash.clone()],
                )
                .await?;
            last = Some(hash.clone());
        }
        if (page.len() as i64) < CLASSIFY_PAGE {
            // This device's own entries there were committed and abandoned:
            // their frames are consumed, no longer staged.
            for entry in self.entries(work.log, &["staged", "assigned"]).await? {
                if entry.position <= retained || entry.position > replaced {
                    continue;
                }
                let ids: Vec<Hash> = decode_bounded(&entry.frames, MAX_SECURE_BYTES)?;
                for id in ids {
                    for sql in [
                        "DELETE FROM secure_staged WHERE hash=?",
                        "DELETE FROM secure_unoffered WHERE hash=?",
                    ] {
                        self.conn.execute(sql, [id.as_bytes().to_vec()]).await?;
                    }
                }
            }
            self.conn
                .execute(
                    "DELETE FROM secure_entries WHERE log=? AND position>? AND position<=?",
                    bindings(vec![
                        SqlValue::Blob(blob),
                        SqlValue::Integer(retained as i64),
                        SqlValue::Integer(replaced as i64),
                    ]),
                )
                .await?;
            return self.advance_work(work, 3, None).await;
        }
        self.advance_work(work, 2, last).await
    }
    /// Step 3: rebuilds the next scope of the log from a clean base, or
    /// guards it until a clean checkpoint is imported.
    async fn rebase_step(&mut self, work: &Work) -> Result<(), Error> {
        let mut next = None;
        for scope in self.log_scopes(work.log).await? {
            let blob = encode(&scope)?;
            if work.cursor.as_ref().is_none_or(|cursor| blob > *cursor) {
                next = Some((scope, blob));
                break;
            }
        }
        let Some((scope, blob)) = next else {
            return transaction!(self, |inner| inner.advance_work(work, 4, None).await);
        };
        let Some(id) = self.projected(scope).await? else {
            // Keyless custody projects nothing; an import replaces the rows.
            return transaction!(self, |inner| {
                inner.advance_work(work, 3, Some(blob.clone())).await
            });
        };
        self.settle(scope).await?;
        let base = self
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed);
        let clean = base.is_none_or(|base| {
            classify(&base.statement.coordinated, self.held_cuts(work.log)) == Base::Clean
        });
        transaction!(self, |inner| {
            // A build begun before the cut replayed abandoned frames.
            inner
                .conn
                .execute(
                    "UPDATE secure_checkpoints SET state='failed' WHERE scope=? AND state='building'",
                    [id],
                )
                .await?;
            inner
                .conn
                .execute("DELETE FROM secure_checkpoint_overlay WHERE scope=?", [id])
                .await?;
            if clean {
                inner.rebuild_rows(scope).await?;
            } else {
                inner
                    .conn
                    .execute(
                        "INSERT OR REPLACE INTO secure_cut_guard VALUES(?,?,?)",
                        bindings(vec![
                            SqlValue::Blob(blob.clone()),
                            SqlValue::Blob(encode(&work.log)?),
                            SqlValue::Integer(work.sequence as i64),
                        ]),
                    )
                    .await?;
            }
            inner.advance_work(work, 3, Some(blob.clone())).await
        })
    }
    /// Moves a tracked log past `cut` when this replica holds the retained
    /// prefix: at its end, or beyond it on the abandoned side.
    pub(in crate::replica) async fn jump_cut(&self, cut: &HeldCut) -> Result<bool, Error> {
        let statement = &cut.statement;
        let log = statement.log;
        let (position, running, genesis, _) = self.log_state(log).await?;
        let entry = statement.entry();
        if running.is_none() || position >= entry {
            return Ok(false);
        }
        let retained = statement.retained;
        let at = if position == retained.position {
            running
        } else if position > retained.position && retained.position == 0 {
            genesis
        } else if position > retained.position {
            let record = collect_rows(
                self.conn
                    .query(
                        "SELECT running FROM secure_entries WHERE log=? AND state='committed' AND position=?",
                        bindings(vec![
                            SqlValue::Blob(encode(&log)?),
                            SqlValue::Integer(retained.position as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            match record.first().map(Vec::as_slice) {
                Some([SqlValue::Blob(running)]) => Some(commitment_of(running)?),
                _ => None,
            }
        } else {
            None
        };
        if at != Some(retained.running) {
            return Ok(false);
        }
        let after = coordinated_chain(statement.replaced.running, entry, cut.digest);
        let blob = encode(&log)?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO secure_entries VALUES(?,?,?,'committed',NULL,?,?,NULL,NULL)",
                bindings(vec![
                    SqlValue::Blob(blob.clone()),
                    SqlValue::Blob(cut.digest.to_vec()),
                    SqlValue::Integer(entry as i64),
                    SqlValue::Blob(encode(&Vec::<Hash>::new())?),
                    SqlValue::Blob(after.to_vec()),
                ]),
            )
            .await?;
        self.conn
            .execute(
                "UPDATE secure_coordinated SET position=?,running=? WHERE log=?",
                bindings(vec![
                    SqlValue::Integer(entry as i64),
                    SqlValue::Blob(after.to_vec()),
                    SqlValue::Blob(blob),
                ]),
            )
            .await?;
        Ok(true)
    }
    /// The held cut whose retained prefix ends at `position`, if one does.
    pub(in crate::replica) fn cut_after(&self, log: TipLog, position: u64) -> Option<HeldCut> {
        self.held_cuts(log)
            .iter()
            .find(|cut| cut.statement.retained.position == position)
            .cloned()
    }
    /// The next held cut whose retained prefix ends after `position`.
    pub(in crate::replica) fn next_cut(&self, log: TipLog, position: u64) -> Option<HeldCut> {
        self.held_cuts(log)
            .iter()
            .find(|cut| cut.statement.retained.position > position)
            .cloned()
    }
    /// Runs the next rollback step in its own transaction; false when no
    /// cut has work left.
    pub(in crate::replica) async fn cut_step(&mut self) -> Result<bool, Error> {
        let Some(work) = self.cut_work().await? else {
            return Ok(false);
        };
        match work.step {
            2 => transaction!(self, |inner| inner.classify_step(&work).await)?,
            3 => self.rebase_step(&work).await?,
            4 => transaction!(self, |inner| {
                let cut = inner.work_cut(&work)?;
                inner.jump_cut(&cut).await?;
                inner.advance_work(&work, 5, None).await
            })?,
            5 => transaction!(self, |inner| {
                inner
                    .conn
                    .execute(
                        "DELETE FROM secure_cut_work WHERE log=?",
                        [encode(&work.log)?],
                    )
                    .await?;
                Ok(())
            })?,
            _ => return Err(Error::Corrupt("cut work step".into())),
        }
        self.flush()?;
        self.publish_revision().await?;
        self.cut_fault(work.step)?;
        Ok(true)
    }
    #[cfg(test)]
    fn cut_fault(&mut self, step: i64) -> Result<(), Error> {
        if self.cut_fault == Some(step) {
            self.cut_fault = None;
            return Err(Error::InjectedFailure);
        }
        Ok(())
    }
    #[cfg(not(test))]
    fn cut_fault(&mut self, _step: i64) -> Result<(), Error> {
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Admits verified cut records in sequence order and rolls each one's
    /// abandoned history back. A record whose predecessor is missing waits;
    /// one conflicting with a held record is refused with `cut_conflict`.
    pub async fn receive_cuts(&self, cuts: &[CutEvidence]) -> Result<(), Error> {
        if cuts.len() > MAX_PAGE_CUTS {
            return Err(denied("cut page quota"));
        }
        let mut ordered: Vec<&CutEvidence> = cuts.iter().collect();
        ordered.sort_by_key(|cut| (cut.record.statement.log, cut.record.statement.sequence));
        for cut in ordered {
            self.drive_cuts().await?;
            let admitted = {
                let mut inner = self.inner.lock().await;
                let admitted = transaction!(inner, |inner| inner.admit_cut(cut).await)?;
                inner.load_cuts().await?;
                inner.flush()?;
                if admitted == Admission::Admitted {
                    inner.cut_fault(1)?;
                }
                admitted
            };
            if admitted == Admission::Admitted {
                self.drive_cuts().await?;
            }
        }
        Ok(())
    }
    /// Runs every admitted cut's remaining rollback steps; each commits its
    /// work with its progress, so a crash resumes where it stopped.
    pub async fn drive_cuts(&self) -> Result<(), Error> {
        while self.inner.lock().await.cut_step().await? {}
        Ok(())
    }
    /// The cut records of `log` this replica holds.
    pub async fn cuts(&self, log: TipLog) -> Result<Vec<CutEvidence>, Error> {
        let inner = self.inner.lock().await;
        let mut all = Vec::new();
        loop {
            let page = inner.cuts_after(log, all.len() as u64).await?;
            if page.is_empty() {
                return Ok(all);
            }
            all.extend(page);
        }
    }
    /// How many cut records this replica holds of each log of `scopes`.
    pub async fn held_cut_counts(&self, scopes: &[ResourceScope]) -> Vec<(TipLog, u64)> {
        let inner = self.inner.lock().await;
        let logs: BTreeSet<TipLog> = scopes
            .iter()
            .map(|scope| inner.tip_log(scope.domain))
            .collect();
        logs.into_iter()
            .map(|log| (log, inner.held_cuts(log).len() as u64))
            .collect()
    }
    /// The cut records of the logs of `scopes` after the counts a peer
    /// holds, at most `MAX_PAGE_CUTS`.
    pub async fn cuts_for(
        &self,
        scopes: &[ResourceScope],
        held: &[(TipLog, u64)],
    ) -> Result<Vec<CutEvidence>, Error> {
        let inner = self.inner.lock().await;
        let logs: BTreeSet<TipLog> = scopes
            .iter()
            .map(|scope| inner.tip_log(scope.domain))
            .collect();
        let mut cuts = Vec::new();
        for log in logs {
            let count = held
                .iter()
                .find(|(held, _)| *held == log)
                .map_or(0, |(_, count)| *count);
            cuts.extend(inner.cuts_after(log, count).await?);
        }
        cuts.truncate(MAX_PAGE_CUTS);
        Ok(cuts)
    }
    /// The scopes a cut left without a clean replay base.
    pub async fn cut_guarded_scopes(&self) -> Result<Vec<ResourceScope>, Error> {
        let inner = self.inner.lock().await;
        rows(
            &inner.conn,
            "SELECT scope FROM secure_cut_guard ORDER BY scope",
        )
        .await?
        .iter()
        .map(|record| match record.as_slice() {
            [SqlValue::Blob(bytes)] => decode(bytes),
            _ => Err(Error::Corrupt("cut guard".into())),
        })
        .collect()
    }
    #[cfg(test)]
    pub(crate) async fn fail_after_cut_step(&self, step: i64) {
        self.inner.lock().await.cut_fault = Some(step);
    }
}

/// Verifies committed entries of an endorsed gap before the hash window:
/// the hashes from `running` at `position` must chain to the cut's retained
/// running hash. Returns the verified hash of every position through it.
pub(in crate::replica) async fn endorsed_hashes(
    log: TipLog,
    cut: &HeldCut,
    (position, running): (u64, Commitment),
    peers: &dyn EntryPeers,
) -> Result<Vec<Commitment>, CoordinatedError> {
    let retained = cut.statement.retained;
    if retained.position <= position || retained.position - position > MAX_ENDORSED {
        return Err(CoordinatedError::Behind);
    }
    let mut hashes = Vec::new();
    let mut chained = running;
    let mut at = position;
    while at < retained.position {
        let entries = peers
            .committed(log, at)
            .await
            .map_err(|_| CoordinatedError::Behind)?;
        if entries.is_empty() {
            return Err(CoordinatedError::Behind);
        }
        for entry in entries
            .into_iter()
            .take_while(|entry| entry.position <= retained.position)
        {
            if entry.position != at + 1
                || entry_hash(log, entry.position, &entry.ids)? != entry.hash
            {
                return Err(CoordinatedError::Behind);
            }
            chained = coordinated_chain(chained, entry.position, entry.hash);
            hashes.push(entry.hash);
            at = entry.position;
        }
    }
    if chained != retained.running {
        return Err(CoordinatedError::Behind);
    }
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_credentials::CutPoint;
    fn held(retained: u64, replaced: u64, byte: u8) -> HeldCut {
        let point = |position| CutPoint {
            position,
            running: [byte; 32],
        };
        HeldCut {
            digest: [byte; 32],
            statement: CutStatement {
                log: TipLog {
                    scope: Scope {
                        mesh: rocky_core::MeshId([1; 16]),
                        app: rocky_core::AppId([2; 16]),
                        domain: rocky_core::DomainId([3; 16]),
                    },
                    tip: rocky_core::TipId([4; 32]),
                },
                sequence: 1,
                previous: None,
                verified: point(0),
                retained: point(retained),
                replaced: point(replaced),
                epoch: 0,
                issued: 0,
            },
        }
    }
    #[test]
    fn positions_fence_by_signed_range_and_never_reuse_a_cut_position() {
        let cuts = [held(3, 6, 1), held(9, 12, 2)];
        let expected = [
            (1, Fence::Live),
            (3, Fence::Live),
            (4, Fence::Abandoned),
            (6, Fence::Abandoned),
            (7, Fence::Cut),
            (8, Fence::Live),
            (9, Fence::Live),
            (10, Fence::Abandoned),
            (12, Fence::Abandoned),
            (13, Fence::Cut),
            (14, Fence::Live),
        ];
        for (position, fenced) in expected {
            assert_eq!(fence(&cuts, position), fenced, "{position}");
        }
        assert_eq!(fence(&[held(5, 5, 1)], 5), Fence::Live);
        assert_eq!(fence(&[held(5, 5, 1)], 6), Fence::Cut);
    }
    #[test]
    fn bindings_are_clean_tainted_or_waiting_against_the_held_cuts() {
        let cuts = [held(3, 6, 1), held(9, 12, 2)];
        let at = |cuts, cut, highest| CoordinatedBinding { cuts, cut, highest };
        // Sealed before any cut: clean while it covers only retained entries.
        assert_eq!(classify(&at(0, None, 3), &cuts), Base::Clean);
        assert_eq!(classify(&at(0, None, 4), &cuts), Base::Tainted);
        assert_eq!(classify(&at(0, None, 0), &[]), Base::Clean);
        // After the first cut: the second abandons beyond 9.
        assert_eq!(classify(&at(1, Some([1; 32]), 9), &cuts), Base::Clean);
        assert_eq!(classify(&at(1, Some([1; 32]), 10), &cuts), Base::Tainted);
        assert_eq!(classify(&at(1, Some([1; 32]), 10), &cuts[..1]), Base::Clean);
        // Another history of cuts, or one this replica lacks.
        assert_eq!(classify(&at(1, Some([7; 32]), 2), &cuts), Base::Tainted);
        assert_eq!(classify(&at(2, Some([2; 32]), 20), &cuts), Base::Clean);
        assert_eq!(classify(&at(3, Some([3; 32]), 20), &cuts), Base::Unknown);
        assert_eq!(binding(&cuts, 14), at(2, Some([2; 32]), 14));
        assert_eq!(binding(&[], 0), at(0, None, 0));
    }
}
