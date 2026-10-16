//! Retain only bases referenced by current checkpoints, staging or proposals.
use super::*;
impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn archived_existence_base(
        &self,
        scope: ResourceScope,
        base: Commitment,
    ) -> Result<Option<ExistenceBaseAttachment>, Error> {
        let mut rows = self
            .conn
            .query(
                "SELECT attachment FROM secure_existence_archive WHERE scope=? AND base=?",
                [encode(&scope)?, base.to_vec()],
            )
            .await?;
        rows.next()
            .await?
            .map(|row| decode(&row.get::<Vec<u8>>(0)?))
            .transpose()
    }
    pub(in crate::replica) async fn archive_existence_bases(&self) -> Result<(), Error> {
        for row in rows(
            &self.conn,
            "SELECT scope,attachment FROM secure_existence_base",
        )
        .await?
        {
            let [SqlValue::Blob(scope), SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(corrupt());
            };
            let base: ExistenceBaseAttachment = decode(bytes)?;
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO secure_existence_archive VALUES(?,?,?)",
                    [base.id()?.to_vec(), scope.clone(), bytes.clone()],
                )
                .await?;
            self.conn.execute("INSERT OR IGNORE INTO secure_existence_archive_rows SELECT ?,id,created,deleted FROM secure_existence_base_rows WHERE scope=?", [base.id()?.to_vec(), scope.clone()]).await?;
        }
        Ok(())
    }
    pub(in crate::replica) async fn collect_existence_archives(&self) -> Result<(), Error> {
        let mut keep = BTreeSet::new();
        for row in rows(&self.conn, "SELECT attachment FROM secure_existence_base").await? {
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(corrupt());
            };
            keep.insert(decode::<ExistenceBaseAttachment>(bytes)?.id()?);
        }
        for row in rows(&self.conn, "SELECT signed FROM secure_checkpoints WHERE signed IS NOT NULL UNION ALL SELECT signed FROM secure_rebootstrap").await? {
            let [SqlValue::Blob(bytes)] = row.as_slice() else { return Err(corrupt()); };
            let signed = SignedCheckpoint::from_bytes(bytes)?;
            if let Some(horizon) = signed.statement.finalized {
                for descriptor in &horizon.statement.bases {
                    keep.insert(ExistenceBaseAttachment { basis: ExistenceBasis::Horizon(horizon.clone()), scope: descriptor.scope }.id()?);
                }
            }
        }
        for row in rows(&self.conn, "SELECT attachment FROM secure_existence_stage").await? {
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(corrupt());
            };
            keep.insert(decode::<ExistenceBaseAttachment>(bytes)?.id()?);
        }
        for row in rows(&self.conn, "SELECT statement FROM secure_mergeable_horizon").await? {
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(corrupt());
            };
            let horizon =
                rocky_credentials::SignedHorizon::from_bytes(bytes).map_err(credential)?;
            for descriptor in &horizon.statement.bases {
                keep.insert(
                    ExistenceBaseAttachment {
                        basis: ExistenceBasis::Horizon(Box::new(horizon.clone())),
                        scope: descriptor.scope,
                    }
                    .id()?,
                );
            }
        }
        for row in rows(&self.conn, "SELECT base FROM secure_existence_archive").await? {
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(corrupt());
            };
            if !keep.contains(&checkpoint::row_of(bytes)?) {
                for table in ["secure_existence_archive_rows", "secure_existence_archive"] {
                    self.conn
                        .execute(format!("DELETE FROM {table} WHERE base=?"), [bytes.clone()])
                        .await?;
                }
            }
        }
        Ok(())
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn existence_base_at(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        expected: Option<Commitment>,
    ) -> Result<Option<ExistenceBaseAttachment>, Error> {
        let mut inner = self.inner.lock().await;
        inner.authorize_domain_peer(peer, credentials, scope)?;
        let base = match expected {
            Some(id) => inner.archived_existence_base(scope, id).await?,
            None => inner.existence_base(scope).await?,
        };
        inner.flush()?;
        Ok(base)
    }
}
