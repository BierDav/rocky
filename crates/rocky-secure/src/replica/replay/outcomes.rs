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
    // Refused records that disagreed with a stored one, with both engines.
    "CREATE TABLE IF NOT EXISTS secure_outcome_divergence(txn BLOB NOT NULL,publisher BLOB NOT NULL,stored TEXT NOT NULL,refused TEXT NOT NULL,PRIMARY KEY(txn,publisher)) WITHOUT ROWID",
];

/// Two replicas replayed the same members of a transaction after the same
/// prefixes and disagree on its outcome. The later record was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutcomeDivergence {
    pub transaction: Commitment,
    /// The publisher of the refused record.
    pub publisher: PublicKey,
    /// The engine of the record this replica kept.
    pub stored: String,
    /// The engine of the refused record.
    pub refused: String,
}

/// This build's engine as records name it.
fn engine() -> String {
    format!(
        "rocky-secure {} {}",
        env!("CARGO_PKG_VERSION"),
        sqlite::engine()
    )
}

impl<A: RowApplication> Inner<A> {
    /// What the partition applied before the recorded member `hash`: the
    /// last sequence per author.
    async fn prefix(&self, scope: i64, hash: Hash) -> Result<Vec<BaseMark>, Error> {
        let rows = collect_rows(
            self.conn
                .query(
                    "SELECT author,MAX(sequence) FROM secure_replayed WHERE scope=? AND (era,clock,author,sequence)<(SELECT era,clock,author,sequence FROM secure_replayed WHERE hash=?) GROUP BY author ORDER BY author",
                    bindings(vec![
                        SqlValue::Integer(scope),
                        SqlValue::Blob(hash.as_bytes().to_vec()),
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
    /// replaying them, and as their author at commit; again only when the
    /// outcome changes.
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
            let operation = frame.operation_ref()?;
            let scope = operation.header.extensions.scope;
            let id = self.scope_id(scope).await?;
            replayed.push((*member, self.prefix(id, operation.hash).await?));
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
        // A redo publishes again only when the outcome changed; a moved
        // prefix alone would make records grow with every rewind.
        let unchanged =
            self.outcome_records(transaction)
                .await?
                .iter()
                .any(|(publisher, _, own)| {
                    publisher.as_slice() == node.as_bytes()
                        && own.dropped == dropped
                        && own
                            .replayed
                            .iter()
                            .map(|(member, _)| member)
                            .eq(replayed.iter().map(|(member, _)| member))
                });
        if unchanged {
            return Ok(());
        }
        let record = OutcomeRecord {
            transaction,
            replayed,
            dropped,
            engine: engine(),
        };
        self.sign_outcome(&record, &logs, node, time).await
    }

    /// Appends `record` to each of `logs` as this node and stores it.
    async fn sign_outcome(
        &self,
        record: &OutcomeRecord,
        logs: &[(ResourceScope, i64)],
        node: PublicKey,
        time: u64,
    ) -> Result<(), Error> {
        record.check().map_err(denied)?;
        let mut sequence = 0;
        for (scope, id) in logs {
            let tip = self.author_tip(*scope, *id, node).await?;
            let frame = self
                .author_log_frame((*scope, tip), time, |_, _| {
                    RowPayload::outcome_record(record.clone())
                })
                .await?;
            sequence = frame.operation_ref()?.header.extensions.author_sequence;
        }
        self.store_outcome(node.as_bytes(), sequence, record).await
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
            tracing::warn!(
                stored = %other.engine,
                refused = %record.engine,
                "divergent outcome records"
            );
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO secure_outcome_divergence VALUES(?,?,?,?)",
                    bindings(vec![
                        SqlValue::Blob(record.transaction.to_vec()),
                        SqlValue::Blob(publisher.as_bytes().to_vec()),
                        SqlValue::Text(other.engine.clone()),
                        SqlValue::Text(record.engine.clone()),
                    ]),
                )
                .await?;
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
                    "SELECT era,clock,author,sequence,scope,member,dropped FROM secure_replayed WHERE txn=? AND sealed=0",
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
            starts.push((*scope, order_of(row)?));
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

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    /// Signs this replica's stored record of `transaction` again, with its
    /// outcome and engine replaced, as a divergent replica would.
    pub(crate) async fn forge_outcome_for_test(
        &self,
        transaction: Commitment,
        dropped: Option<DropReason>,
        engine: &str,
        scopes: &[ResourceScope],
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        let time = now()?;
        let engine = engine.to_owned();
        let scopes = scopes.to_vec();
        transaction!(inner, |inner| {
            let node = inner.config.node.public_key();
            let (.., mut record) = inner
                .outcome_records(transaction)
                .await?
                .into_iter()
                .find(|(publisher, ..)| publisher.as_slice() == node.as_bytes())
                .ok_or(Error::Closed)?;
            record.dropped = dropped;
            record.engine = engine.clone();
            let mut logs = Vec::new();
            for scope in &scopes {
                logs.push((*scope, inner.scope_id(*scope).await?));
            }
            inner.sign_outcome(&record, &logs, node, time).await
        })
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Outcome records this replica refused as divergence.
    pub async fn outcome_divergences(&self) -> Result<Vec<OutcomeDivergence>, Error> {
        let inner = self.inner.lock(Class::Background).await;
        let corrupt = || Error::Corrupt("outcome divergence".into());
        collect_rows(
            inner
                .conn
                .query(
                    "SELECT txn,publisher,stored,refused FROM secure_outcome_divergence ORDER BY txn,publisher",
                    (),
                )
                .await?,
        )
        .await?
        .into_iter()
        .map(|row| match row.as_slice() {
            [
                SqlValue::Blob(txn),
                SqlValue::Blob(publisher),
                SqlValue::Text(stored),
                SqlValue::Text(refused),
            ] => Ok(OutcomeDivergence {
                transaction: txn.as_slice().try_into().map_err(|_| corrupt())?,
                publisher: PublicKey::from_bytes(publisher.as_slice().try_into().map_err(|_| corrupt())?)
                    .map_err(|_| corrupt())?,
                stored: stored.clone(),
                refused: refused.clone(),
            }),
            _ => Err(corrupt()),
        })
        .collect()
    }

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
