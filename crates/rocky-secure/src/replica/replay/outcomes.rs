//! Outcome records: how a replica replayed the members of a transaction it
//! holds. A replica holding only some members of a transaction applies
//! them provisionally and drops them while a record naming a member it
//! does not hold says the transaction dropped. Records of the same members
//! after the same prefixes that disagree are divergence.
use super::*;
use rocky_core::replay::{BaseMark, OutcomeRecord};

pub(in crate::replica) const SCHEMA: &[&str] = &[
    // The latest record of each publisher per transaction.
    "CREATE TABLE IF NOT EXISTS secure_outcomes(txn BLOB NOT NULL,publisher BLOB NOT NULL,sequence INTEGER NOT NULL,record BLOB NOT NULL,PRIMARY KEY(txn,publisher)) WITHOUT ROWID",
    // Members some record names: a seal covers a transaction of several
    // members once each has one.
    "CREATE TABLE IF NOT EXISTS secure_outcome_members(txn BLOB NOT NULL,member INTEGER NOT NULL,PRIMARY KEY(txn,member)) WITHOUT ROWID",
];

/// This build's engine as records name it.
fn engine() -> String {
    format!(
        "rocky-secure {} {}",
        env!("CARGO_PKG_VERSION"),
        sqlite::engine()
    )
}

impl<A: RowApplication> Inner<A> {
    /// What the partition applied before `stamp`: the last sequence per
    /// author.
    async fn prefix(&self, scope: i64, stamp: &Stamp) -> Result<Vec<BaseMark>, Error> {
        let rows = collect_rows(
            self.conn
                .query(
                    "SELECT author,MAX(sequence) FROM secure_replayed WHERE scope=? AND (clock,author,sequence)<(?,?,?) GROUP BY author ORDER BY author",
                    bindings(vec![
                        SqlValue::Integer(scope),
                        SqlValue::Integer(stamp.clock as i64),
                        SqlValue::Integer(i64::from(stamp.author)),
                        SqlValue::Integer(stamp.sequence as i64),
                    ]),
                )
                .await?,
        )
        .await?;
        if rows.len() > rocky_core::replay::MAX_OUTCOME_MEMBERS {
            return Err(denied("outcome prefix quota"));
        }
        rows.iter()
            .map(|row| match row.as_slice() {
                [SqlValue::Integer(author), SqlValue::Integer(sequence)] => Ok(BaseMark {
                    author: *author as u32,
                    sequence: *sequence as u64,
                }),
                _ => Err(Error::Corrupt("replayed prefix".into())),
            })
            .collect()
    }

    /// Every stored record of `transaction`, by publisher.
    async fn outcome_records(
        &self,
        transaction: Commitment,
    ) -> Result<Vec<(Vec<u8>, i64, OutcomeRecord)>, Error> {
        collect_rows(
            self.conn
                .query(
                    "SELECT publisher,sequence,record FROM secure_outcomes WHERE txn=?",
                    [transaction.to_vec()],
                )
                .await?,
        )
        .await?
        .into_iter()
        .map(|row| match row.as_slice() {
            [
                SqlValue::Blob(publisher),
                SqlValue::Integer(sequence),
                SqlValue::Blob(record),
            ] => Ok((publisher.clone(), *sequence, decode(record)?)),
            _ => Err(Error::Corrupt("outcome record".into())),
        })
        .collect()
    }

    async fn store_outcome(
        &self,
        publisher: &[u8],
        sequence: u64,
        record: &OutcomeRecord,
    ) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO secure_outcomes VALUES(?,?,?,?)",
                bindings(vec![
                    SqlValue::Blob(record.transaction.to_vec()),
                    SqlValue::Blob(publisher.to_vec()),
                    SqlValue::Integer(sequence as i64),
                    SqlValue::Blob(encode(record)?),
                ]),
            )
            .await?;
        for (member, _) in &record.replayed {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO secure_outcome_members VALUES(?,?)",
                    bindings(vec![
                        SqlValue::Blob(record.transaction.to_vec()),
                        SqlValue::Integer(i64::from(*member)),
                    ]),
                )
                .await?;
        }
        Ok(())
    }

    /// Why another replica's record drops `transaction`, when one that
    /// names a member not `held` here says so.
    pub(super) async fn remote_drop(
        &self,
        transaction: Commitment,
        held: &[u16],
    ) -> Result<Option<DropReason>, Error> {
        let own = self.config.node.public_key();
        Ok(self
            .outcome_records(transaction)
            .await?
            .into_iter()
            .filter(|(publisher, ..)| publisher.as_slice() != own.as_bytes())
            .filter(|(.., record)| {
                record
                    .replayed
                    .iter()
                    .any(|(member, _)| !held.contains(member))
            })
            .find_map(|(.., record)| record.dropped))
    }

    /// Signs how this replica replayed its held members of a transaction
    /// of several members into each of their logs it may write: after
    /// replaying them, and as their author at commit.
    pub(in crate::replica) async fn publish_outcome(
        &self,
        transaction: Commitment,
        held: &[(&EncryptedFrame, u16)],
        dropped: Option<DropReason>,
        time: u64,
    ) -> Result<(), Error> {
        let Some(node) = self
            .node_author
            .as_ref()
            .map(|author| author.key.public_key())
        else {
            return Ok(());
        };
        let grants = self.local_grants(time)?;
        let mut replayed = Vec::with_capacity(held.len());
        let mut logs = Vec::new();
        for (frame, member) in held {
            let header = &frame.operation_ref()?.header;
            let scope = header.extensions.scope;
            let id = self.scope_id(scope).await?;
            replayed.push((*member, self.prefix(id, &stamp(header)).await?));
            let writes = grants.iter().any(|grant| {
                grant.scope == scope
                    && [Right::Insert, Right::Update, Right::Delete]
                        .iter()
                        .any(|right| grant.rights.contains(right))
            });
            if writes {
                logs.push((scope, id));
            }
        }
        if logs.is_empty() {
            return Ok(());
        }
        replayed.sort_by_key(|(member, _)| *member);
        let record = OutcomeRecord {
            transaction,
            replayed,
            dropped,
            engine: engine(),
        };
        record.check().map_err(denied)?;
        let mut sequence = 0;
        for (scope, id) in logs {
            let tip = self.author_tip(scope, id, node).await?;
            let frame = self
                .author_log_frame((scope, tip), time, |_, _| {
                    RowPayload::outcome_record(record.clone())
                })
                .await?;
            sequence = frame.operation_ref()?.header.extensions.author_sequence;
        }
        self.store_outcome(node.as_bytes(), sequence, &record).await
    }

    /// Takes in another replica's record. A record that disagrees with one
    /// of the same members after the same prefixes is divergence, named
    /// with both engines. One that changes whether this replica's held
    /// members of the transaction drop rewinds and redoes them.
    pub(in crate::replica) async fn apply_outcome(
        &self,
        frame: &EncryptedFrame,
        record: &OutcomeRecord,
        time: u64,
    ) -> Result<Result<(), String>, Error> {
        let header = &frame.operation_ref()?.header;
        let publisher = header.public_key;
        let sequence = header.extensions.author_sequence;
        let stored = self.outcome_records(record.transaction).await?;
        if stored.iter().any(|(stored, at, _)| {
            stored.as_slice() == publisher.as_bytes() && *at as u64 >= sequence
        }) {
            return Ok(Ok(()));
        }
        if let Some((.., other)) = stored.iter().find(|(stored, _, other)| {
            stored.as_slice() != publisher.as_bytes()
                && other.replayed == record.replayed
                && other.dropped != record.dropped
        }) {
            return Ok(Err(format!(
                "divergent outcome records for one transaction: {} and {}",
                other.engine, record.engine
            )));
        }
        self.store_outcome(publisher.as_bytes(), sequence, record)
            .await?;
        let held = collect_rows(
            self.conn
                .query(
                    "SELECT clock,author,sequence,scope,member,dropped FROM secure_replayed WHERE txn=? AND sealed=0",
                    [record.transaction.to_vec()],
                )
                .await?,
        )
        .await?;
        let mut starts = Vec::with_capacity(held.len());
        let mut members = Vec::with_capacity(held.len());
        let mut dropped = false;
        for row in &held {
            let [
                ..,
                SqlValue::Integer(scope),
                SqlValue::Integer(member),
                outcome,
            ] = row.as_slice()
            else {
                return Err(Error::Corrupt("replayed member".into()));
            };
            starts.push((*scope, stamp_of(row)?));
            members.push(*member as u16);
            dropped |= !matches!(outcome, SqlValue::Null);
        }
        if held.is_empty()
            || self
                .remote_drop(record.transaction, &members)
                .await?
                .is_some()
                == dropped
        {
            return Ok(Ok(()));
        }
        let redo = self.rewind(starts).await?;
        self.redo(redo, time).await?;
        Ok(Ok(()))
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Publishes this replica's record for every open transaction of
    /// several members it holds whole that some member still lacks one
    /// for, as when its author is gone. The owner runs this before it
    /// seals, so the seal can cover them.
    pub async fn publish_missing_outcomes(&self) -> Result<(), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        let time = now()?;
        transaction!(inner, |inner| {
            inner.publish_missing_outcomes(time).await
        })
    }

    /// Replayed transactions a finalized seal reached that stay open
    /// because some member has no outcome record: no replica holding it
    /// whole has published one. Their drops stay provisional.
    pub async fn awaiting_outcomes(&self) -> Result<Vec<Commitment>, Error> {
        let inner = self.inner.lock(Class::Background).await;
        collect_rows(
            inner
                .conn
                .query(
                    "SELECT DISTINCT r.txn FROM secure_replayed r JOIN secure_applied a ON a.hash=r.hash JOIN secure_seal_bounds b ON b.scope=r.scope AND a.position<=b.position WHERE r.sealed=0 ORDER BY r.txn",
                    (),
                )
                .await?,
        )
        .await?
        .into_iter()
        .map(|row| match row.as_slice() {
            [SqlValue::Blob(txn)] => txn
                .as_slice()
                .try_into()
                .map_err(|_| Error::Corrupt("replayed transaction".into())),
            _ => Err(Error::Corrupt("replayed transaction".into())),
        })
        .collect()
    }
}

impl<A: RowApplication> Inner<A> {
    async fn publish_missing_outcomes(&self, time: u64) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("replayed member".into());
        let open = collect_rows(
            self.conn
                .query(
                    "SELECT txn FROM secure_replayed r WHERE sealed=0 AND members>1 GROUP BY txn HAVING COUNT(*)=MAX(members) AND (SELECT COUNT(*) FROM secure_outcome_members m WHERE m.txn=r.txn)<MAX(members)",
                    (),
                )
                .await?,
        )
        .await?;
        for row in open {
            let [SqlValue::Blob(txn)] = row.as_slice() else {
                return Err(corrupt());
            };
            let transaction: Commitment = txn.as_slice().try_into().map_err(|_| corrupt())?;
            let mut frames = Vec::new();
            let mut dropped = None;
            for member in collect_rows(
                self.conn
                    .query(
                        "SELECT hash,member,dropped FROM secure_replayed WHERE txn=? ORDER BY member",
                        [txn.clone()],
                    )
                    .await?,
            )
            .await?
            {
                let [SqlValue::Blob(hash), SqlValue::Integer(index), reason] = member.as_slice()
                else {
                    return Err(corrupt());
                };
                let hash = Hash::try_from(hash.as_slice()).map_err(|_| corrupt())?;
                let frame = self.frame(hash).await?.ok_or_else(corrupt)?;
                if let SqlValue::Text(reason) = reason {
                    dropped = Some(reason_named(reason)?);
                }
                frames.push((frame, *index as u16));
            }
            let held: Vec<_> = frames
                .iter()
                .map(|(frame, index)| (frame, *index))
                .collect();
            self.publish_outcome(transaction, &held, dropped, time)
                .await?;
        }
        Ok(())
    }
}
