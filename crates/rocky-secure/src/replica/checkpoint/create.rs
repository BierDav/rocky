//! Checkpoint summaries and the maintenance step after a write.
use super::*;

pub(super) fn chain(label: &str, items: &[Vec<SqlValue>]) -> Result<SetSummary, Error> {
    let mut bytes = Vec::new();
    for item in items {
        for value in item {
            match value {
                SqlValue::Blob(value) => {
                    bytes.extend((value.len() as u64).to_le_bytes());
                    bytes.extend(value);
                }
                SqlValue::Integer(value) => bytes.extend(value.to_le_bytes()),
                _ => return Err(Error::Corrupt("checkpoint summary column".into())),
            }
        }
    }
    Ok(SetSummary {
        count: items.len() as u64,
        digest: rocky_core::commitment(label, &bytes),
    })
}

/// Covered custody of a state with no frames: nothing at all.
pub(super) fn no_custody() -> Result<CustodySummary, Error> {
    Ok(CustodySummary {
        superseded: 0,
        sole: chain("rocky.checkpoint-sole-copies.v1", &[])?,
    })
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The scope's verified replay base. A base sits only at a seal:
    /// `compact` forms it, an import installs one.
    pub async fn checkpoint(&self, scope: ResourceScope) -> Result<SignedCheckpoint, Error> {
        let inner = self.inner.lock().await;
        if let Some(id) = inner.find_scope_id(scope).await?
            && let Some(record) = inner.checkpoint_record(id, true).await?
            && let Some(signed) = record.signed
            && signed.statement.seal.is_some()
        {
            return Ok(signed);
        }
        Err(denied("a checkpoint sits at a seal; compact instead"))
    }
}

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn custody_summary(
        &self,
        id: i64,
        position: i64,
    ) -> Result<CustodySummary, Error> {
        let superseded = collect_rows(
            self.conn
                .query(
                    "SELECT COUNT(*) FROM secure_applied a WHERE a.scope=? AND a.position<=? AND EXISTS(SELECT 1 FROM secure_ack k WHERE k.hash=a.hash)",
                    [id, position],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Integer(superseded)]) = superseded.first().map(Vec::as_slice) else {
            return Err(Error::Corrupt("custody summary".into()));
        };
        let sole = collect_rows(
            self.conn
                .query(
                    "SELECT a.hash FROM secure_applied a WHERE a.scope=? AND a.position<=? AND NOT EXISTS(SELECT 1 FROM secure_ack k WHERE k.hash=a.hash) ORDER BY a.position",
                    [id, position],
                )
                .await?,
        )
        .await?;
        Ok(CustodySummary {
            superseded: *superseded as u64,
            sole: chain("rocky.checkpoint-sole-copies.v1", &sole)?,
        })
    }
    /// One bounded step of automatic maintenance for the scopes a group
    /// wrote: continue or start a checkpoint, else prune one batch. The
    /// group is already committed, so a failure is kept for the status.
    pub(crate) async fn maintain(&mut self, scopes: &BTreeSet<ResourceScope>) {
        for scope in scopes {
            match self.maintain_scope(*scope).await {
                Ok(true) => {
                    self.maintenance_error = None;
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    self.maintenance_error = Some(error.to_string());
                    return;
                }
            }
        }
    }
    /// True when a step ran. A scope being imported waits: its own
    /// checkpoint would only repeat the one it imports.
    async fn maintain_scope(&mut self, scope: ResourceScope) -> Result<bool, Error> {
        if self.scope_key(scope).is_err() || self.importing(scope).await? {
            return Ok(false);
        }
        self.graph_for(scope.domain)?;
        Ok(self.finalize_ready(scope.domain).await?.0 > 0)
    }
}
