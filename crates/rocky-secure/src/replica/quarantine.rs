//! Frames whose application failed for a reason that is neither retryable
//! nor a semantic refusal. A quarantined frame is rolled back alone and its
//! log waits behind it; other logs go on. It is tried again when keys,
//! grants, schemas or the tip graph change, and otherwise after a backoff.
//! Quarantine never admits or deletes a frame.
use super::*;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_quarantine(hash BLOB PRIMARY KEY,reason TEXT NOT NULL,attempts INTEGER NOT NULL,first_seen INTEGER NOT NULL,last_seen INTEGER NOT NULL,context INTEGER NOT NULL) WITHOUT ROWID",
];

/// The longest wait between two attempts while nothing changes.
const MAX_BACKOFF: u64 = 3600;
/// The reason of a frame that does not decrypt under the key its header names.
pub(super) const UNDECRYPTABLE: &str = "undecryptable";

/// One quarantined frame's attempts.
#[derive(Clone, Copy)]
pub(super) struct Quarantine {
    attempts: i64,
    last_seen: i64,
    context: i64,
}
impl Quarantine {
    /// Whether another attempt is due at `time` under `context`.
    pub(super) fn due(&self, time: u64, context: i64) -> bool {
        let wait = 1u64 << self.attempts.clamp(0, 12);
        context != self.context
            || time >= (self.last_seen.max(0) as u64).saturating_add(wait.min(MAX_BACKOFF))
    }
}

/// Whether `error` is the frame's own failure rather than the storage's,
/// which ends the pass instead.
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
#[cfg(any(test, feature = "qualification"))]
fn injected(error: &Error) -> bool {
    matches!(error, Error::InjectedFailure)
}
#[cfg(not(any(test, feature = "qualification")))]
fn injected(_: &Error) -> bool {
    false
}

impl<A: RowApplication> Inner<A> {
    /// What a quarantined frame's retry depends on: keys, grants, schemas
    /// and the tip graph. A change makes every quarantined frame due.
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
                graph.finalities.len(),
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
    /// Inside the caller's transaction, records another failed attempt.
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
    /// Inside the caller's transaction: the frame applied, was rejected or
    /// waits for a known reason now.
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
