//! Failed checkpoint imports, counted per scope and checkpoint so that a
//! checkpoint that keeps failing is given up across restarts.
use super::*;

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_import_failures(scope BLOB NOT NULL,checkpoint BLOB NOT NULL,failures INTEGER NOT NULL,PRIMARY KEY(scope,checkpoint)) WITHOUT ROWID",
];
/// Failed imports of one checkpoint before its staging is cancelled and
/// the scope falls back to frames.
pub const IMPORT_ATTEMPTS: u8 = 3;

impl<A: RowApplication> EncryptedReplica<A> {
    /// How often importing `checkpoint` into `scope` failed.
    pub async fn import_failures(
        &self,
        scope: ResourceScope,
        checkpoint: Commitment,
    ) -> Result<u8, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let found = collect_rows(
            inner
                .conn
                .query(
                    "SELECT failures FROM secure_import_failures WHERE scope=? AND checkpoint=?",
                    [encode(&scope)?, checkpoint.to_vec()],
                )
                .await?,
        )
        .await?;
        Ok(match found.first().map(Vec::as_slice) {
            Some([SqlValue::Integer(failures)]) => (*failures).clamp(0, 255) as u8,
            None => 0,
            Some(_) => return Err(Error::Corrupt("import failure count".into())),
        })
    }
    /// Counts another failed import of `checkpoint` into `scope`. The
    /// `IMPORT_ATTEMPTS`th cancels its staged pages, so the scope falls
    /// back to frames. Returns the new count.
    pub async fn import_failed(
        &self,
        scope: ResourceScope,
        checkpoint: Commitment,
    ) -> Result<u8, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "INSERT INTO secure_import_failures VALUES(?,?,1) ON CONFLICT(scope,checkpoint) DO UPDATE SET failures=failures+1",
                    [encode(&scope)?, checkpoint.to_vec()],
                )
                .await?;
            Ok(())
        })?;
        drop(flush_guard(inner, &self.inner).await?);
        let failures = self.import_failures(scope, checkpoint).await?;
        if failures >= IMPORT_ATTEMPTS {
            self.cancel_rebootstrap(scope, checkpoint).await?;
        }
        Ok(failures)
    }
}

impl<A: RowApplication> Inner<A> {
    /// Inside the caller's transaction: an import into `scope` finished.
    pub(super) async fn forget_import_failures(&self, scope: ResourceScope) -> Result<(), Error> {
        self.conn
            .execute(
                "DELETE FROM secure_import_failures WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        Ok(())
    }
}
