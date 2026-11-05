//! Finite history preparation. Only mutations of the captured prefix invalidate
//! it; successful writes beyond that cut do not create catch-up work.
use super::*;
use rocky_core::CommitmentBuilder;

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_base_history(scope INTEGER PRIMARY KEY,token BLOB NOT NULL,last INTEGER NOT NULL,valid INTEGER NOT NULL CHECK(valid IN(0,1))) WITHOUT ROWID",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_applied_insert AFTER INSERT ON secure_applied BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=NEW.scope AND NEW.position<=last; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_applied_delete AFTER DELETE ON secure_applied BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=OLD.scope AND OLD.position<=last; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_applied_update AFTER UPDATE ON secure_applied BEGIN UPDATE secure_base_history SET valid=0 WHERE (scope=OLD.scope AND OLD.position<=last) OR (scope=NEW.scope AND NEW.position<=last); END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_operation_update AFTER UPDATE ON secure_operations BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=(SELECT scope FROM secure_applied WHERE hash=OLD.hash) AND last>=(SELECT position FROM secure_applied WHERE hash=OLD.hash); END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_operation_delete BEFORE DELETE ON secure_operations BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=(SELECT scope FROM secure_applied WHERE hash=OLD.hash) AND last>=(SELECT position FROM secure_applied WHERE hash=OLD.hash); END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_births_update AFTER UPDATE OF births ON secure_replayed BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=OLD.scope AND last>=(SELECT position FROM secure_applied WHERE hash=OLD.hash); END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_births_delete BEFORE DELETE ON secure_replayed BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=OLD.scope AND last>=(SELECT position FROM secure_applied WHERE hash=OLD.hash); END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_ack_insert AFTER INSERT ON secure_ack BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=(SELECT scope FROM secure_applied WHERE hash=NEW.hash) AND last>=(SELECT position FROM secure_applied WHERE hash=NEW.hash); END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_ack_delete AFTER DELETE ON secure_ack BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=(SELECT scope FROM secure_applied WHERE hash=OLD.hash) AND last>=(SELECT position FROM secure_applied WHERE hash=OLD.hash); END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_checkpoint_delete AFTER DELETE ON secure_checkpoints BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=OLD.scope; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_checkpoint_insert AFTER INSERT ON secure_checkpoints BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=NEW.scope; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_checkpoint_update AFTER UPDATE ON secure_checkpoints BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=OLD.scope OR scope=NEW.scope; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_replayed_seals_insert AFTER INSERT ON secure_replayed_seals BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=NEW.scope; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_replayed_seals_delete AFTER DELETE ON secure_replayed_seals BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=OLD.scope; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_replayed_seals_update AFTER UPDATE ON secure_replayed_seals BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=OLD.scope OR scope=NEW.scope; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_replayed_snapshots_insert AFTER INSERT ON secure_replayed_snapshots BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=NEW.scope; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_replayed_snapshots_delete AFTER DELETE ON secure_replayed_snapshots BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=OLD.scope; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_replayed_snapshots_update AFTER UPDATE ON secure_replayed_snapshots BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=OLD.scope OR scope=NEW.scope; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_replayed_seal_rows_insert AFTER INSERT ON secure_replayed_seal_rows BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=NEW.scope; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_replayed_seal_rows_delete AFTER DELETE ON secure_replayed_seal_rows BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=OLD.scope; END",
    "CREATE TRIGGER IF NOT EXISTS secure_base_history_replayed_seal_rows_update AFTER UPDATE ON secure_replayed_seal_rows BEGIN UPDATE secure_base_history SET valid=0 WHERE scope=OLD.scope OR scope=NEW.scope; END",
];

pub(super) struct PreparedHistory {
    token: Commitment,
    pub(super) through: i64,
    pub(super) frontier: BTreeMap<PublicKey, LogPosition>,
    pub(super) operations: u64,
    pub(super) identities: Vec<checkpoint::CoveredMark>,
    pub(super) custody: checkpoint::CustodySummary,
}

pub(super) struct HistoryWork {
    prepared: PreparedHistory,
    last: i64,
    after: i64,
    identities: bool,
    marks: identity::IdentityLedger,
    own: identity::IdentityLedger,
    sole: CommitmentBuilder,
}

impl HistoryWork {
    pub(super) fn finish(mut self) -> PreparedHistory {
        self.prepared.identities = self
            .marks
            .into_marks()
            .into_iter()
            .map(|mark| checkpoint::CoveredMark {
                own: self.own.get(mark.author).map_or(0, |own| own.sequence),
                author: mark.author,
                signer: mark.signer,
                sequence: mark.sequence,
            })
            .collect();
        self.prepared.custody.sole.digest = self.sole.finish();
        self.prepared
    }
}

impl<A: RowApplication> Inner<A> {
    pub(super) async fn begin_base_history(
        &mut self,
        scope: ResourceScope,
        id: i64,
        target: &Finalized,
    ) -> Result<Option<HistoryWork>, Error> {
        if !self.replayed_scope(scope) || self.prepared_base(id, target.seal).await?.is_none() {
            return Ok(None);
        }
        let previous = self.checkpoint_record(id, true).await?;
        let base = previous.as_ref().filter(|record| {
            record
                .signed
                .as_ref()
                .is_some_and(|signed| signed.statement.seal.is_some())
        });
        let position = base.map_or(0, |record| record.position);
        let previous_marks = base
            .and_then(|record| record.signed.as_ref())
            .map_or(&[][..], |signed| signed.statement.identities.as_slice());
        let bound = collect_rows(
            self.conn
                .query(
                    "SELECT position FROM secure_seal_bounds WHERE scope=? AND depth=?",
                    [id, target.depth],
                )
                .await?,
        )
        .await?;
        let through = match bound.first().map(Vec::as_slice) {
            Some([SqlValue::Integer(through)]) => *through,
            None => position,
            _ => return Err(Error::Corrupt("base history bound".into())),
        };
        if through < position {
            return Err(Error::Conflict);
        }
        let last = collect_rows(
            self.conn
                .query(
                    "SELECT COALESCE(MAX(position),0) FROM secure_applied WHERE scope=?",
                    [id],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Integer(last)]) = last.first().map(Vec::as_slice) else {
            return Err(Error::Corrupt("base history cut".into()));
        };
        let last = *last;
        let token = rand::random();
        let mut frontier = BTreeMap::new();
        if let Some(signed) = base.and_then(|record| record.signed.as_ref()) {
            frontier.extend(
                signed
                    .statement
                    .frontier
                    .iter()
                    .map(|log| (log.author, log.clone())),
            );
        }
        let work = HistoryWork {
            prepared: PreparedHistory {
                token,
                through,
                frontier,
                operations: base
                    .and_then(|record| record.signed.as_ref())
                    .map_or(0, |signed| signed.statement.operations),
                identities: Vec::new(),
                custody: checkpoint::CustodySummary {
                    superseded: 0,
                    sole: SetSummary {
                        count: 0,
                        digest: [0; 32],
                    },
                },
            },
            last,
            after: position,
            identities: false,
            marks: identity::IdentityLedger::seeded(
                &previous_marks
                    .iter()
                    .map(checkpoint::CoveredMark::mark)
                    .collect::<Vec<_>>(),
            )?,
            own: identity::IdentityLedger::based(previous_marks)?,
            sole: CommitmentBuilder::new("rocky.checkpoint-sole-copies.v1"),
        };
        transaction!(self, |inner| {
            inner
                .conn
                .execute(
                    "INSERT OR REPLACE INTO secure_base_history VALUES(?,?,?,1)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(token.to_vec()),
                        SqlValue::Integer(last),
                    ]),
                )
                .await?;
            Ok(())
        })?;
        Ok(Some(work))
    }

    pub(super) async fn validate_base_history(
        &self,
        id: i64,
        history: &PreparedHistory,
    ) -> Result<(), Error> {
        let valid = collect_rows(
            self.conn
                .query(
                    "SELECT 1 FROM secure_base_history WHERE scope=? AND token=? AND valid=1",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(history.token.to_vec()),
                    ]),
                )
                .await?,
        )
        .await?;
        if valid.is_empty() {
            return Err(Error::Conflict);
        }
        Ok(())
    }

    /// Disposable summaries restart after process death; the durable row cursor
    /// is independent. No application operation executes in either scan.
    pub(super) async fn base_history_step(
        &self,
        id: i64,
        work: &mut HistoryWork,
    ) -> Result<bool, Error> {
        self.validate_base_history(id, &work.prepared).await?;
        let records = collect_rows(self.conn.query(
            "SELECT a.position,a.hash,o.header,o.body,EXISTS(SELECT 1 FROM secure_ack k WHERE k.hash=a.hash) FROM secure_applied a LEFT JOIN secure_operations o ON o.hash=a.hash WHERE a.scope=? AND a.position>? AND a.position<=? ORDER BY a.position LIMIT ?",
            [id, work.after, if work.identities { work.last } else { work.prepared.through }, base_transition::BASE_ROWS as i64],
        ).await?).await?;
        #[cfg(any(test, feature = "qualification"))]
        self.flusher.measurements.work(
            if work.identities {
                lock::WorkUnit::TransitionIdentityRecord
            } else {
                lock::WorkUnit::TransitionHistoryRecord
            },
            records.len() as u64,
        );
        for record in &records {
            let [
                SqlValue::Integer(position),
                SqlValue::Blob(hash),
                SqlValue::Blob(header),
                SqlValue::Blob(body),
                SqlValue::Integer(ack),
            ] = record.as_slice()
            else {
                return Err(Error::Conflict);
            };
            work.after = *position;
            let decoded: p2panda_core::Header<Extensions> = decode(header)?;
            if !work.identities {
                work.prepared.operations += 1;
                let log = LogPosition {
                    author: decoded.public_key,
                    sequence: decoded.seq_num,
                    hash: checkpoint::hash_of(hash)?,
                    clock: decoded.timestamp,
                };
                if work
                    .prepared
                    .frontier
                    .get(&log.author)
                    .is_none_or(|held| held.sequence < log.sequence)
                {
                    work.prepared.frontier.insert(log.author, log);
                }
                if work.prepared.frontier.len() > checkpoint::MAX_CHECKPOINT_LOGS {
                    return Err(denied("too many checkpoint logs"));
                }
                continue;
            }
            if *position <= work.prepared.through {
                if *ack != 0 {
                    work.prepared.custody.superseded += 1;
                } else {
                    work.prepared.custody.sole.count += 1;
                    work.sole.update(&(hash.len() as u64).to_le_bytes());
                    work.sole.update(hash);
                }
            }
            if work
                .prepared
                .frontier
                .get(&decoded.public_key)
                .is_none_or(|log| log.sequence < decoded.seq_num)
            {
                continue;
            }
            let frame = self.chained_frame(header.clone(), body.clone()).await?;
            for birth in self.birth_marks(&frame)? {
                if !birth.carried {
                    work.own.merge(birth.mark.clone())?;
                }
                work.marks.merge(birth.mark)?;
            }
            for mark in self.replayed_births(frame.id()?).await? {
                work.own.merge(mark.clone())?;
                work.marks.merge(mark)?;
            }
        }
        if records.len() == base_transition::BASE_ROWS {
            return Ok(true);
        }
        if !work.identities {
            work.identities = true;
            work.after = 0;
            return Ok(true);
        }
        Ok(false)
    }
}
