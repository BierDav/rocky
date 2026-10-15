//! Keyless custody of sealed finalized checkpoint pages.
use super::*;

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_checkpoint_archive(scope BLOB PRIMARY KEY,signed BLOB NOT NULL,cursor BLOB,complete INTEGER NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_checkpoint_archive_pages(scope BLOB NOT NULL,cursor BLOB NOT NULL,page BLOB NOT NULL,PRIMARY KEY(scope,cursor)) WITHOUT ROWID",
];
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn begin_checkpoint_archive(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        signed: SignedCheckpoint,
    ) -> Result<(Option<Commitment>, bool), Error> {
        let mut inner = self.inner.lock().await;
        let scope = signed.statement.scope;
        inner.authorize_peer(peer, credentials, scope)?;
        signed.verify_authority(&inner, now()?)?;
        if !crate::replica::roles::holds_whole(&inner.local_grants(now()?)?, scope) {
            return Err(denied("local checkpoint custody denied"));
        }
        if signed.statement.finalized.is_none() {
            return Err(denied("custody requires a finalized checkpoint"));
        }
        // Readers already retain verified materialized checkpoints.
        if inner.scope_key(scope).is_ok() {
            return Ok((None, true));
        }
        if let Some(held) = inner.archived_checkpoint(scope, false).await? {
            if held.id()? == signed.id()? {
                return inner.archive_progress(scope).await;
            }
            let held_horizon = held.statement.finalized.as_ref().ok_or_else(|| {
                Error::Corrupt("custody checkpoint has no finalized horizon".into())
            })?;
            let incoming = signed
                .statement
                .finalized
                .as_ref()
                .ok_or_else(|| denied("custody requires a finalized checkpoint"))?;
            if incoming.statement.sequence < held_horizon.statement.sequence {
                return Err(denied("custody checkpoint does not advance"));
            }
        }
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "DELETE FROM secure_checkpoint_archive_pages WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            inner.conn.execute("INSERT INTO secure_checkpoint_archive VALUES(?,?,NULL,0) ON CONFLICT(scope) DO UPDATE SET signed=excluded.signed,cursor=NULL,complete=0", [encode(&scope)?, signed.to_bytes()?]).await?;
            Ok(())
        })?;
        inner.flush()?;
        Ok((None, false))
    }
    pub(crate) async fn archive_checkpoint_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        page: CheckpointPage,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        let scope = page.scope;
        inner.authorize_peer(peer, credentials, scope)?;
        if inner.scope_key(scope).is_ok() {
            return Ok(());
        }
        let signed = inner.check_archive(scope, page.checkpoint).await?;
        if page.rows.kind != PackageKind::Checkpoint
            || page.rows.scope != scope
            || page.rows.generation != signed.statement.key_epoch
            || page
                .next
                .is_some_and(|next| page.after.is_some_and(|after| next <= after))
        {
            return Err(denied("custody checkpoint page outside archive"));
        }
        let cursor = page.after.map(|after| after.to_vec()).unwrap_or_default();
        let bytes = encode(&page)?;
        transaction!(inner, |inner| {
            let held = collect_rows(inner.conn.query("SELECT page FROM secure_checkpoint_archive_pages WHERE scope=? AND cursor=?", [encode(&scope)?, cursor.clone()]).await?).await?;
            if let Some(record) = held.first() {
                let [SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(Error::Corrupt("custody checkpoint page".into()));
                };
                let held: CheckpointPage = decode(bytes)?;
                return if held.next == page.next && held.checkpoint == page.checkpoint {
                    Ok(())
                } else {
                    Err(denied("custody checkpoint retry differs"))
                };
            }
            let progress = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT cursor,complete FROM secure_checkpoint_archive WHERE scope=?",
                        [encode(&scope)?],
                    )
                    .await?,
            )
            .await?;
            let expected = page
                .after
                .map_or(SqlValue::Null, |after| SqlValue::Blob(after.to_vec()));
            if progress != vec![vec![expected, SqlValue::Integer(0)]] {
                return Err(denied("custody checkpoint pages out of order"));
            }
            let usage = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT COALESCE(SUM(LENGTH(page)),0) FROM secure_checkpoint_archive_pages",
                        (),
                    )
                    .await?,
            )
            .await?;
            let [SqlValue::Integer(used)] = usage[0].as_slice() else {
                return Err(Error::Corrupt("custody checkpoint archive size".into()));
            };
            if *used as usize + bytes.len() > MAX_SECURE_BYTES {
                return Err(Error::Quota {
                    resource: "custody checkpoint archive",
                    limit: MAX_SECURE_BYTES,
                });
            }
            inner
                .conn
                .execute(
                    "INSERT INTO secure_checkpoint_archive_pages VALUES(?,?,?)",
                    [encode(&scope)?, cursor.clone(), bytes.clone()],
                )
                .await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_checkpoint_archive SET cursor=?,complete=? WHERE scope=?",
                    bindings(vec![
                        page.next
                            .map_or(SqlValue::Null, |next| SqlValue::Blob(next.to_vec())),
                        SqlValue::Integer(i64::from(page.next.is_none())),
                        SqlValue::Blob(encode(&scope)?),
                    ]),
                )
                .await?;
            Ok(())
        })?;
        inner.flush()?;
        Ok(())
    }
}
impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn archived_checkpoint(
        &self,
        scope: ResourceScope,
        complete: bool,
    ) -> Result<Option<SignedCheckpoint>, Error> {
        let mut rows = self.conn.query("SELECT signed FROM secure_checkpoint_archive WHERE scope=? AND (complete=1 OR ?=0)", bindings(vec![SqlValue::Blob(encode(&scope)?), SqlValue::Integer(i64::from(complete))])).await?;
        rows.next()
            .await?
            .map(|row| SignedCheckpoint::from_bytes(&row.get::<Vec<u8>>(0)?))
            .transpose()
    }
    async fn archive_progress(
        &self,
        scope: ResourceScope,
    ) -> Result<(Option<Commitment>, bool), Error> {
        let mut rows = self
            .conn
            .query(
                "SELECT cursor,complete FROM secure_checkpoint_archive WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| Error::Corrupt("custody checkpoint progress".into()))?;
        let cursor = match row.get_value(0)? {
            sqlite::Value::Null => None,
            sqlite::Value::Blob(bytes) => Some(bytes),
            _ => return Err(Error::Corrupt("custody checkpoint cursor".into())),
        };
        Ok((
            cursor
                .map(|bytes| {
                    bytes
                        .try_into()
                        .map_err(|_| Error::Corrupt("custody checkpoint cursor".into()))
                })
                .transpose()?,
            row.get::<i64>(1)? != 0,
        ))
    }
    async fn check_archive(
        &self,
        scope: ResourceScope,
        checkpoint: Commitment,
    ) -> Result<SignedCheckpoint, Error> {
        self.archived_checkpoint(scope, false)
            .await?
            .filter(|signed| signed.id().ok() == Some(checkpoint))
            .ok_or_else(|| denied("custody checkpoint outside archive"))
    }
    pub(super) async fn archived_page(
        &self,
        scope: ResourceScope,
        checkpoint: Commitment,
        after: Option<Commitment>,
        quota: usize,
    ) -> Result<CheckpointPage, Error> {
        let signed = self
            .archived_checkpoint(scope, true)
            .await?
            .ok_or_else(|| denied("custody checkpoint archive incomplete"))?;
        if signed.id()? != checkpoint {
            return Err(denied("checkpoint superseded; restart the rebootstrap"));
        }
        let mut rows = self
            .conn
            .query(
                "SELECT page FROM secure_checkpoint_archive_pages WHERE scope=? AND cursor=?",
                [
                    encode(&scope)?,
                    after.map(|after| after.to_vec()).unwrap_or_default(),
                ],
            )
            .await?;
        let bytes = rows
            .next()
            .await?
            .ok_or_else(|| Error::Corrupt("custody checkpoint page missing".into()))?
            .get::<Vec<u8>>(0)?;
        let page: CheckpointPage = decode(&bytes)?;
        if encode(&page)?.len() > quota {
            return Err(denied("byte quota smaller than archived checkpoint page"));
        }
        Ok(page)
    }
}
