use super::*;

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_import_failures(scope BLOB NOT NULL,checkpoint BLOB NOT NULL,failures INTEGER NOT NULL,PRIMARY KEY(scope,checkpoint)) WITHOUT ROWID",
];
pub const IMPORT_ATTEMPTS: u8 = 3;

impl<A: RowApplication> EncryptedReplica<A> {
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
