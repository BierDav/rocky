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
    /// wrote: forget stale pulled states and quarantine records, then
    /// finalize at most one seal of their domains. The group is already
    /// committed, so a failure is kept for the status and backs off.
    pub(crate) async fn maintain(&mut self, scopes: &BTreeSet<ResourceScope>) {
        if scopes.is_empty() {
            return;
        }
        let Ok(time) = now() else { return };
        if self
            .maintenance_backoff
            .is_some_and(|(until, _)| time < until)
        {
            return;
        }
        match self.maintain_domains(scopes).await {
            Ok(()) => {
                self.maintenance_error = None;
                self.maintenance_backoff = None;
            }
            Err(error) => {
                self.maintenance_error = Some(error.to_string());
                let failures = self.maintenance_backoff.map_or(0, |(_, failures)| failures);
                let wait = (1u64 << failures.min(6)).min(60);
                self.maintenance_backoff = Some((time.saturating_add(wait), failures + 1));
            }
        }
    }
    async fn maintain_domains(&mut self, scopes: &BTreeSet<ResourceScope>) -> Result<(), Error> {
        transaction!(self, |inner| inner.forget_stale().await)?;
        // A scope being imported waits: its own checkpoint would only
        // repeat the one it imports.
        let mut domains = BTreeSet::new();
        for scope in scopes {
            if self.scope_key(*scope).is_ok() && !self.importing(*scope).await? {
                domains.insert(scope.domain);
            }
        }
        for domain in domains {
            self.graph_for(domain)?;
            if self.finalize_at_most(domain, 1).await?.0 > 0 {
                return Ok(());
            }
        }
        Ok(())
    }
    /// Inside the caller's transaction: pulled states that no longer pass
    /// the import checks or that their scope's base already holds, and
    /// quarantine records of frames that are no longer pending.
    async fn forget_stale(&mut self) -> Result<(), Error> {
        self.revalidate_pulled().await?;
        for record in rows(&self.conn, "SELECT scope,seal FROM secure_pulled").await? {
            let [SqlValue::Integer(id), SqlValue::Blob(seal)] = record.as_slice() else {
                return Err(Error::Corrupt("pulled checkpoint".into()));
            };
            if self.base_seal(*id).await?.map(|base| base.to_vec()) == Some(seal.clone()) {
                self.drop_pulled(*id).await?;
            }
        }
        self.conn
            .execute(
                "DELETE FROM secure_quarantine WHERE hash NOT IN(SELECT hash FROM secure_operations WHERE status='pending')",
                (),
            )
            .await?;
        Ok(())
    }
}
