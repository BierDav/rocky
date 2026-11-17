use super::*;
use crate::replica::checkpoint::{
    CheckpointStatement, MAX_SEALED_OUTCOMES, MAX_SEALED_VIOLATION, SealedOutcome, SealedOutcomes,
    SealedTransaction,
};

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
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
    pub(in crate::replica) async fn leaves_open(&self, id: i64, depth: i64) -> Result<bool, Error> {
        let mut scopes = self.linked_scopes(id).await?;
        scopes.push(id);
        for scope in scopes {
            if !collect_rows(
                self.conn
                    .query(
                        "SELECT 1 FROM secure_replayed r JOIN secure_applied a ON a.hash=r.hash JOIN secure_seal_bounds b ON b.scope=r.scope AND b.depth=?2 WHERE r.scope=?1 AND r.sealed=0 AND a.position<=b.position LIMIT 1",
                        [scope, depth],
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

    pub(in crate::replica) async fn sealed_outcomes(
        &self,
        id: i64,
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
            .map(|listed| (listed.stamp.clone(), listed))
            .collect();
        for row in collect_rows(
            self.conn
                .query(
                    "SELECT clock,author,sequence,txn,dropped,violated FROM secure_replayed WHERE scope=? AND sealed=1 AND dropped IS NOT NULL",
                    [id],
                )
                .await?,
        )
        .await?
        {
            let [
                SqlValue::Integer(clock),
                SqlValue::Blob(author),
                SqlValue::Integer(sequence),
                SqlValue::Blob(txn),
                SqlValue::Text(reason),
                violated,
            ] = row.as_slice()
            else {
                return Err(corrupt());
            };
            let stamp = Stamp {
                clock: stored_clock(*clock)?,
                author: stored_writer(author)?,
                sequence: *sequence as u64,
            };
            let outcome = SealedOutcome::Dropped {
                reason: reason_named(reason)?,
                violation: match violated {
                    SqlValue::Text(violation) => Some(bounded(violation)),
                    _ => None,
                },
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

    pub(in crate::replica) async fn keep_covered(
        &self,
        id: i64,
        hash: &[u8],
        outcomes: &SealedOutcomes,
    ) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("replayed member".into());
        let Some(author) = self.node_author.as_ref().map(|author| author.writer) else {
            return Ok(());
        };
        let rows = collect_rows(
            self.conn
                .query(
                    "SELECT txn,clock,sequence,dropped,sealed FROM secure_replayed WHERE hash=? AND scope=? AND author=?",
                    bindings(vec![
                        SqlValue::Blob(hash.to_vec()),
                        SqlValue::Integer(id),
                        writer_value(author),
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
            clock: stored_clock(*clock)?,
            author,
            sequence: *sequence as u64,
        };
        let listed = outcomes.of(&stamp, &transaction);
        let sealed_here = *sealed != 0;
        if sealed_here {
            let recorded = match dropped {
                SqlValue::Text(reason) => Some(reason_named(reason)?),
                _ => None,
            };
            let stated = match listed {
                Some(Some(SealedOutcome::Dropped { reason, .. })) => Some(Some(*reason)),
                Some(None) => Some(None),
                None => None,
            };
            if stated.is_some_and(|stated| stated != recorded) {
                return Err(denied("checkpoint outcomes contradict a sealed outcome"));
            }
        }
        let (dropped, sealed, violated) = self.based_outcome(listed, transaction);
        self.conn
            .execute(
                "INSERT OR REPLACE INTO secure_kept_outcomes VALUES(?,?,?,?,?,?,?,?,1)",
                bindings(vec![
                    SqlValue::Blob(hash.to_vec()),
                    SqlValue::Blob(transaction.to_vec()),
                    SqlValue::Integer(id),
                    SqlValue::Integer(*clock),
                    SqlValue::Integer(*sequence),
                    dropped,
                    SqlValue::Integer(sealed),
                    violated,
                ]),
            )
            .await?;
        Ok(())
    }

    pub(in crate::replica) async fn cover_kept(
        &self,
        scope: ResourceScope,
        id: i64,
        statement: &CheckpointStatement,
    ) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("kept outcome".into());
        let (Some(author), Some(seal)) = (
            self.node_author.as_ref().map(|author| author.writer),
            statement.seal,
        ) else {
            return Ok(());
        };
        let Ok(seal) = self.graph_for(scope.domain)?.seal_by_id(seal) else {
            return Ok(());
        };
        let Some(through) = seal
            .statement
            .frontier
            .iter()
            .find(|mark| mark.author == author)
            .map(|mark| mark.sequence)
        else {
            return Ok(());
        };
        for row in collect_rows(
            self.conn
                .query(
                    "SELECT hash,txn,clock,sequence FROM secure_kept_outcomes WHERE scope=? AND based=0 AND sealed=0 AND sequence<=?",
                    [id, i64::try_from(through).map_err(|_| corrupt())?],
                )
                .await?,
        )
        .await?
        {
            let [
                SqlValue::Blob(hash),
                SqlValue::Blob(txn),
                SqlValue::Integer(clock),
                SqlValue::Integer(sequence),
            ] = row.as_slice()
            else {
                return Err(corrupt());
            };
            let transaction: Commitment = txn.as_slice().try_into().map_err(|_| corrupt())?;
            let stamp = Stamp {
                clock: stored_clock(*clock)?,
                author,
                sequence: *sequence as u64,
            };
            let listed = statement.outcomes.of(&stamp, &transaction);
            let (dropped, sealed, violated) = self.based_outcome(listed, transaction);
            self.conn
                .execute(
                    "UPDATE secure_kept_outcomes SET dropped=?,sealed=?,violated=?,based=1 WHERE hash=?",
                    bindings(vec![
                        dropped,
                        SqlValue::Integer(sealed),
                        violated,
                        SqlValue::Blob(hash.clone()),
                    ]),
                )
                .await?;
        }
        Ok(())
    }

    fn based_outcome(
        &self,
        listed: Option<Option<&SealedOutcome>>,
        transaction: Commitment,
    ) -> (SqlValue, i64, SqlValue) {
        let decided = match listed {
            Some(Some(SealedOutcome::Dropped { reason, violation })) => (
                SqlValue::Text(reason_name(*reason).into()),
                1,
                violation.clone().map_or(SqlValue::Null, SqlValue::Text),
            ),
            Some(None) => (SqlValue::Null, 1, SqlValue::Null),
            None => (SqlValue::Null, 0, SqlValue::Null),
        };
        if decided.1 == 1 {
            self.touch_sealed(transaction);
        }
        decided
    }
}

fn bounded(violation: &str) -> String {
    let mut end = violation.len().min(MAX_SEALED_VIOLATION);
    while !violation.is_char_boundary(end) {
        end -= 1;
    }
    violation[..end].to_owned()
}

#[cfg(test)]
mod violation_tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_listed_violation_is_cut_to_its_bound_at_a_character_boundary() {
        let short = "fk_task_owner";
        assert_eq!(bounded(short), short);
        let wide = "é".repeat(MAX_SEALED_VIOLATION);
        let cut = bounded(&wide);
        assert_eq!(cut.len(), MAX_SEALED_VIOLATION);
        assert!(wide.starts_with(&cut));
        let odd = format!("a{wide}");
        let cut = bounded(&odd);
        assert_eq!(cut.len(), MAX_SEALED_VIOLATION - 1);
        assert!(odd.starts_with(&cut));
    }
}
