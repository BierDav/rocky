//! Incremental cleanup; deleting a pulled header first makes its payload unreachable.
use super::*;

/// One stored record per cleanup acquisition. Cost calibration is still part
/// of phase 2 qualification; see docs/features_12/02-bounded-holds-evidence.md.
const CLEANUP_ROWS: usize = 1;
const PAYLOADS: [(&str, &str); 3] = [
    ("secure_pulled_rows", "row"),
    ("secure_pulled_stubs", "hash"),
    ("secure_pulled_pages", "after"),
];

#[derive(Default)]
enum Phase {
    #[default]
    Pulled,
    Payload,
    Quarantine,
}
#[derive(Default)]
pub(super) struct Pass {
    phase: Phase,
    pulled: i64,
    payload: usize,
    scope: i64,
    deleting: Option<i64>,
    quarantine: Vec<u8>,
    pub(super) commit: u64,
}
impl<A: RowApplication> Inner<A> {
    /// One independently valid cleanup step. Cursors need no persisted shape:
    /// missing headers and orphan payloads reconstruct the remaining work.
    pub(super) async fn forget_stale_step(&mut self) -> Result<bool, Error> {
        let mut pass = std::mem::take(&mut self.maintenance);
        let result = self.cleanup_step(&mut pass).await;
        if matches!(result, Ok(true)) {
            self.maintenance = pass;
        }
        result
    }
    async fn cleanup_step(&self, pass: &mut Pass) -> Result<bool, Error> {
        match pass.phase {
            Phase::Pulled => {
                if pass.pulled == 0 {
                    let mut empty = true;
                    for table in [
                        "secure_pulled",
                        "secure_pulled_rows",
                        "secure_pulled_stubs",
                        "secure_pulled_pages",
                        "secure_quarantine",
                    ] {
                        if !rows(&self.conn, &format!("SELECT 1 FROM {table} LIMIT 1"))
                            .await?
                            .is_empty()
                        {
                            empty = false;
                            break;
                        }
                    }
                    if empty {
                        return Ok(false);
                    }
                }
                let found = collect_rows(
                    self.conn
                        .query(
                            "SELECT scope FROM secure_pulled WHERE scope>? ORDER BY scope LIMIT ?",
                            [pass.pulled, CLEANUP_ROWS as i64],
                        )
                        .await?,
                )
                .await?;
                let Some(record) = found.first() else {
                    pass.phase = Phase::Payload;
                    return Ok(true);
                };
                let [SqlValue::Integer(id)] = record.as_slice() else {
                    return Err(Error::Corrupt("pulled cleanup scope".into()));
                };
                self.cleanup_unit();
                pass.pulled = *id;
                if self.stale_pulled(*id).await? {
                    self.conn
                        .execute("DELETE FROM secure_pulled WHERE scope=?", [*id])
                        .await?;
                }
            }
            Phase::Payload => {
                let (table, key) = PAYLOADS[pass.payload];
                if let Some(id) = pass.deleting {
                    // A new completed pull may have replaced the garbage since
                    // the last step; its header protects all of its payload.
                    let header = collect_rows(
                        self.conn
                            .query("SELECT 1 FROM secure_pulled WHERE scope=?", [id])
                            .await?,
                    )
                    .await?;
                    if header.is_empty() {
                        let records = collect_rows(self.conn.query(
                            format!("SELECT {key} FROM {table} WHERE scope=? ORDER BY {key} LIMIT ?"),
                            [id, CLEANUP_ROWS as i64],
                        ).await?).await?;
                        if let Some(record) = records.first() {
                            let [SqlValue::Blob(value)] = record.as_slice() else {
                                return Err(Error::Corrupt("pulled cleanup key".into()));
                            };
                            self.cleanup_unit();
                            self.conn
                                .execute(
                                    format!("DELETE FROM {table} WHERE scope=? AND {key}=?"),
                                    bindings(vec![
                                        SqlValue::Integer(id),
                                        SqlValue::Blob(value.clone()),
                                    ]),
                                )
                                .await?;
                            return Ok(true);
                        }
                    }
                    pass.scope = id;
                    pass.deleting = None;
                    return Ok(true);
                }
                let found = collect_rows(
                    self.conn
                        .query(
                            format!(
                                "SELECT scope FROM {table} WHERE scope>? ORDER BY scope LIMIT ?"
                            ),
                            [pass.scope, CLEANUP_ROWS as i64],
                        )
                        .await?,
                )
                .await?;
                if let Some(record) = found.first() {
                    let [SqlValue::Integer(id)] = record.as_slice() else {
                        return Err(Error::Corrupt("pulled cleanup scope".into()));
                    };
                    self.cleanup_unit();
                    let header = collect_rows(
                        self.conn
                            .query("SELECT 1 FROM secure_pulled WHERE scope=?", [*id])
                            .await?,
                    )
                    .await?;
                    if header.is_empty() {
                        pass.deleting = Some(*id);
                    } else {
                        pass.scope = *id;
                    }
                } else {
                    pass.payload += 1;
                    pass.scope = 0;
                    if pass.payload == PAYLOADS.len() {
                        pass.phase = Phase::Quarantine;
                    }
                }
            }
            Phase::Quarantine => {
                let found = collect_rows(
                    self.conn
                        .query(
                            "SELECT hash FROM secure_quarantine WHERE hash>? ORDER BY hash LIMIT ?",
                            bindings(vec![
                                SqlValue::Blob(pass.quarantine.clone()),
                                SqlValue::Integer(CLEANUP_ROWS as i64),
                            ]),
                        )
                        .await?,
                )
                .await?;
                let Some(record) = found.first() else {
                    return Ok(false);
                };
                let [SqlValue::Blob(hash)] = record.as_slice() else {
                    return Err(Error::Corrupt("quarantine cleanup hash".into()));
                };
                self.cleanup_unit();
                pass.quarantine = hash.clone();
                let id = Hash::from_bytes(
                    hash.as_slice()
                        .try_into()
                        .map_err(|_| Error::Corrupt("quarantine cleanup hash".into()))?,
                );
                if self.stored_status(id).await?.as_deref() != Some("pending") {
                    self.conn
                        .execute("DELETE FROM secure_quarantine WHERE hash=?", [hash.clone()])
                        .await?;
                }
            }
        }
        Ok(true)
    }
    fn cleanup_unit(&self) {
        #[cfg(any(test, feature = "qualification"))]
        self.flusher
            .measurements
            .work(lock::WorkUnit::MaintenanceRecord, 1);
    }
}

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn maintenance_step_for_test(&self) -> Result<bool, Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        transaction!(@commit inner, |inner| inner.forget_stale_step().await)
    }
}
