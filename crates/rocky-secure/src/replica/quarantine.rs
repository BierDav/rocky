use super::*;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_quarantine(hash BLOB PRIMARY KEY,reason TEXT NOT NULL,attempts INTEGER NOT NULL,first_seen INTEGER NOT NULL,last_seen INTEGER NOT NULL,context INTEGER NOT NULL) WITHOUT ROWID",
];

const MAX_BACKOFF: u64 = 3600;
pub(super) const UNDECRYPTABLE: &str = "undecryptable";

#[derive(Clone, Copy)]
pub(super) struct Quarantine {
    attempts: i64,
    last_seen: i64,
    context: i64,
}
impl Quarantine {
    pub(super) fn due(&self, time: u64, context: i64) -> bool {
        let wait = 1u64 << self.attempts.clamp(0, 12);
        context != self.context
            || time >= (self.last_seen.max(0) as u64).saturating_add(wait.min(MAX_BACKOFF))
    }
}

pub(super) fn quarantines(error: &Error) -> bool {
    !retryable(error)
        && !identity::receipts::semantic_refusal(error)
        && !matches!(
            error,
            Error::Io(_)
                | Error::Storage(_)
                | Error::Closed
                | Error::Durability(_)
                | Error::Rollback { .. }
        )
        && !injected(error)
}
fn injected(error: &Error) -> bool {
    matches!(error, Error::InjectedFailure)
}

impl<A: RowApplication> Inner<A> {
    pub(super) fn quarantine_context(&self) -> i64 {
        let mut bytes = self.authority.to_le_bytes().to_vec();
        bytes.extend_from_slice(&self.workspace.tip.0);
        for key in self.opening_keys() {
            bytes.extend(encode(&(key.scope, key.generation)).unwrap_or_default());
        }
        for manifest in self.catalog.keys() {
            bytes.extend_from_slice(manifest);
        }
        bytes.extend_from_slice(&self.manifest);
        for (domain, graph) in &self.graphs {
            bytes.extend(encode(domain).unwrap_or_default());
            for count in [
                graph.seals.len(),
                graph.openings.len(),
                graph.moves.len(),
                graph.checkpoints.len(),
                graph.revoked.len(),
            ] {
                bytes.extend_from_slice(&(count as u64).to_le_bytes());
            }
        }
        let hash = rocky_core::commitment("rocky.quarantine-context.v1", &bytes);
        let mut head = [0; 8];
        head.copy_from_slice(&hash[..8]);
        i64::from_le_bytes(head)
    }
    pub(super) async fn quarantined_frame(&self, hash: Hash) -> Result<Option<Quarantine>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT attempts,last_seen,context FROM secure_quarantine WHERE hash=?",
                    [hash.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?;
        match records.first().map(Vec::as_slice) {
            Some(
                [
                    SqlValue::Integer(attempts),
                    SqlValue::Integer(last_seen),
                    SqlValue::Integer(context),
                ],
            ) => Ok(Some(Quarantine {
                attempts: *attempts,
                last_seen: *last_seen,
                context: *context,
            })),
            None => Ok(None),
            _ => Err(Error::Corrupt("quarantined frame".into())),
        }
    }
    pub(super) async fn quarantine(
        &self,
        hash: Hash,
        reason: &str,
        time: u64,
        context: i64,
    ) -> Result<(), Error> {
        let time = time as i64;
        self.conn
            .execute(
                "INSERT INTO secure_quarantine VALUES(?,?,1,?,?,?) ON CONFLICT(hash) DO UPDATE SET reason=excluded.reason,attempts=attempts+1,last_seen=excluded.last_seen,context=excluded.context",
                bindings(vec![
                    SqlValue::Blob(hash.as_bytes().to_vec()),
                    SqlValue::Text(reason.into()),
                    SqlValue::Integer(time),
                    SqlValue::Integer(time),
                    SqlValue::Integer(context),
                ]),
            )
            .await?;
        Ok(())
    }
    pub(super) async fn release_quarantine(&self, hash: Hash) -> Result<(), Error> {
        self.conn
            .execute(
                "DELETE FROM secure_quarantine WHERE hash=?",
                [hash.as_bytes().to_vec()],
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn quarantine_backoff_doubles_up_to_an_hour_and_a_new_context_is_due_at_once() {
        let at = |attempts| Quarantine {
            attempts,
            last_seen: 1_000,
            context: 7,
        };
        assert!(!at(3).due(1_007, 7));
        assert!(at(3).due(1_008, 7));
        assert!(!at(12).due(1_000 + MAX_BACKOFF - 1, 7));
        assert!(at(40).due(1_000 + MAX_BACKOFF, 7));
        assert!(at(40).due(1_000, 8));
    }
}
