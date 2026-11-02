//! What a seal decided for a replayed scope's transactions. A base at a
//! seal carries it, so a replica that takes the base and loses the frames
//! still knows how its own covered transactions ended.
use super::*;
use crate::replica::checkpoint::{
    MAX_SEALED_OUTCOMES, MAX_SEALED_VIOLATION, SealedOutcome, SealedOutcomes, SealedTransaction,
};

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    /// Records `outcomes` for every member of `scope` this node authored,
    /// as an import of a base that covers them does.
    pub(crate) async fn cover_for_test(
        &self,
        scope: ResourceScope,
        outcomes: &SealedOutcomes,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        let id = inner.find_scope_id(scope).await?.ok_or(Error::Closed)?;
        let outcomes = outcomes.clone();
        transaction!(inner, |inner| {
            let members = rows(
                &inner.conn,
                &format!("SELECT hash FROM secure_replayed WHERE scope={id}"),
            )
            .await?;
            for row in members {
                let [SqlValue::Blob(hash)] = row.as_slice() else {
                    return Err(Error::Corrupt("replayed member".into()));
                };
                inner.keep_covered(id, hash, &outcomes).await?;
            }
            Ok(())
        })
    }
}

impl<A: RowApplication> Inner<A> {
    /// Scope `id`'s outcomes at a seal through applied position `through`,
    /// just finalized: the current base's, every drop sealed since, and
    /// each member the seal covers but leaves open.
    pub(in crate::replica) async fn sealed_outcomes(
        &self,
        id: i64,
        through: i64,
    ) -> Result<SealedOutcomes, Error> {
        let corrupt = || Error::Corrupt("sealed outcome".into());
        let base = self
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed)
            .filter(|signed| signed.statement.seal.is_some())
            .map(|signed| signed.statement.outcomes)
            .unwrap_or_default();
        let mut horizon = base.horizon;
        let mut listed: BTreeMap<Stamp, SealedTransaction> = base
            .listed
            .into_iter()
            .filter(|listed| matches!(listed.outcome, SealedOutcome::Dropped { .. }))
            .map(|listed| (listed.stamp.clone(), listed))
            .collect();
        for row in collect_rows(
            self.conn
                .query(
                    "SELECT clock,author,sequence,txn,dropped,violated,sealed FROM secure_replayed WHERE scope=?1 AND ((sealed=1 AND dropped IS NOT NULL) OR (sealed=0 AND hash IN(SELECT hash FROM secure_applied WHERE scope=?1 AND position<=?2)))",
                    [id, through],
                )
                .await?,
        )
        .await?
        {
            let [
                SqlValue::Integer(clock),
                SqlValue::Integer(author),
                SqlValue::Integer(sequence),
                SqlValue::Blob(txn),
                dropped,
                violated,
                SqlValue::Integer(sealed),
            ] = row.as_slice()
            else {
                return Err(corrupt());
            };
            let stamp = Stamp {
                clock: *clock as u64,
                author: *author as u32,
                sequence: *sequence as u64,
            };
            let outcome = match (*sealed != 0, dropped) {
                (false, _) => SealedOutcome::Open,
                (true, SqlValue::Text(reason)) => SealedOutcome::Dropped {
                    reason: reason_named(reason)?,
                    violation: match violated {
                        SqlValue::Text(violation) => Some(bounded(violation)),
                        _ => None,
                    },
                },
                _ => return Err(corrupt()),
            };
            listed.insert(
                stamp.clone(),
                SealedTransaction {
                    stamp,
                    transaction: txn.as_slice().try_into().map_err(|_| corrupt())?,
                    outcome,
                },
            );
        }
        while listed.len() > MAX_SEALED_OUTCOMES {
            if let Some((stamp, _)) = listed.pop_first() {
                horizon = horizon.max(Some(stamp));
            }
        }
        Ok(SealedOutcomes {
            listed: listed.into_values().collect(),
            horizon,
        })
    }

    /// Records how base `outcomes` says this node's member `hash` of scope
    /// `id`, whose frame the base covers, ended. A listed drop or an
    /// unlisted member is final; an open one keeps what its record said; one
    /// past the list's horizon reads as unknown. Records never overwrite it.
    pub(in crate::replica) async fn keep_covered(
        &self,
        id: i64,
        hash: &[u8],
        outcomes: &SealedOutcomes,
    ) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("replayed member".into());
        let Some(author) = self.node_author.as_ref().map(|author| author.number) else {
            return Ok(());
        };
        let rows = collect_rows(
            self.conn
                .query(
                    "SELECT txn,clock,sequence,dropped,sealed FROM secure_replayed WHERE hash=? AND scope=? AND author=?",
                    bindings(vec![
                        SqlValue::Blob(hash.to_vec()),
                        SqlValue::Integer(id),
                        SqlValue::Integer(i64::from(author)),
                    ]),
                )
                .await?,
        )
        .await?;
        let Some(
            [
                SqlValue::Blob(txn),
                SqlValue::Integer(clock),
                SqlValue::Integer(sequence),
                dropped,
                SqlValue::Integer(sealed),
            ],
        ) = rows.first().map(Vec::as_slice)
        else {
            return Ok(());
        };
        let transaction: Commitment = txn.as_slice().try_into().map_err(|_| corrupt())?;
        let stamp = Stamp {
            clock: *clock as u64,
            author,
            sequence: *sequence as u64,
        };
        let listed = outcomes.of(&stamp, &transaction);
        // A final record here and the base decide the same seal.
        if *sealed != 0 {
            let recorded = match dropped {
                SqlValue::Text(reason) => Some(reason_named(reason)?),
                _ => None,
            };
            let stated = match listed {
                Some(Some(SealedOutcome::Dropped { reason, .. })) => Some(Some(*reason)),
                Some(None) => Some(None),
                Some(Some(SealedOutcome::Open)) | None => None,
            };
            if stated.is_some_and(|stated| stated != recorded) {
                return Err(denied("checkpoint outcomes contradict a sealed outcome"));
            }
        }
        let (dropped, sealed, violated) = match listed {
            Some(Some(SealedOutcome::Open)) => {
                self.conn
                    .execute(
                        "INSERT INTO secure_kept_outcomes SELECT hash,txn,scope,clock,dropped,sealed,violated,0 FROM secure_replayed WHERE hash=? ON CONFLICT(hash) DO NOTHING",
                        [hash.to_vec()],
                    )
                    .await?;
                return Ok(());
            }
            Some(Some(SealedOutcome::Dropped { reason, violation })) => (
                SqlValue::Text(reason_name(*reason).into()),
                1,
                violation.clone().map_or(SqlValue::Null, SqlValue::Text),
            ),
            Some(None) => (SqlValue::Null, 1, SqlValue::Null),
            None => (SqlValue::Null, 0, SqlValue::Null),
        };
        self.conn
            .execute(
                "INSERT OR REPLACE INTO secure_kept_outcomes VALUES(?,?,?,?,?,?,?,1)",
                bindings(vec![
                    SqlValue::Blob(hash.to_vec()),
                    SqlValue::Blob(transaction.to_vec()),
                    SqlValue::Integer(id),
                    SqlValue::Integer(*clock),
                    dropped,
                    SqlValue::Integer(sealed),
                    violated,
                ]),
            )
            .await?;
        Ok(())
    }
}

/// `violation` within the listed bound, cut at a character boundary.
fn bounded(violation: &str) -> String {
    let mut end = violation.len().min(MAX_SEALED_VIOLATION);
    while !violation.is_char_boundary(end) {
        end -= 1;
    }
    violation[..end].to_owned()
}
