use super::*;

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_checkpoint_archive(scope BLOB NOT NULL,checkpoint BLOB NOT NULL,sequence INTEGER NOT NULL,signed BLOB NOT NULL,cursor BLOB,complete INTEGER NOT NULL,PRIMARY KEY(scope,checkpoint)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_checkpoint_archive_pages(scope BLOB NOT NULL,checkpoint BLOB NOT NULL,cursor BLOB NOT NULL,page BLOB NOT NULL,vouch BLOB NOT NULL,PRIMARY KEY(scope,checkpoint,cursor)) WITHOUT ROWID",
];
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct CheckpointVouch {
    signer: PublicKey,
    chain: CredentialChain,
    signed_at: u64,
    signature: Signature,
}
fn vouch_bytes(
    page: &CheckpointPage,
    signer: PublicKey,
    chain: &CredentialChain,
    signed_at: u64,
) -> Result<Vec<u8>, Error> {
    signing_bytes(
        "rocky.custody-checkpoint-page.v1",
        &(page, signer, chain, signed_at),
    )
}
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn vouch_checkpoint_page(
        &self,
        page: &CheckpointPage,
    ) -> Result<CheckpointVouch, Error> {
        let inner = self.inner.lock(Class::Background).await;
        let signer = inner.config.node.public_key();
        let chain = inner.config.credentials.clone();
        let signed_at = now()?;
        inner.scope_key(page.scope)?;
        let signature = inner
            .config
            .node
            .sign(&vouch_bytes(page, signer, &chain, signed_at)?);
        Ok(CheckpointVouch {
            signer,
            chain,
            signed_at,
            signature,
        })
    }
    pub(crate) async fn begin_checkpoint_archive(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        signed: SignedCheckpoint,
    ) -> Result<(Option<Commitment>, bool), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let scope = signed.statement.scope;
        inner.authorize_peer(peer, credentials, scope)?;
        signed.verify_authority(&inner, now()?)?;
        if !crate::replica::roles::holds_whole(&inner.local_grants(now()?)?, scope) {
            return Err(denied("local checkpoint custody denied"));
        }
        if signed.statement.seal.is_none() {
            return Err(denied("custody requires a finalized checkpoint"));
        }
        if signed.statement.no_frames() {
            return Ok((None, true));
        }
        let checkpoint = signed.id()?;
        let existing = collect_rows(
            inner
                .conn
                .query(
                    "SELECT 1 FROM secure_checkpoint_archive WHERE scope=? AND checkpoint=?",
                    [encode(&scope)?, checkpoint.to_vec()],
                )
                .await?,
        )
        .await?;
        if !existing.is_empty() {
            return inner.archive_progress(scope, checkpoint).await;
        }
        let sequence = i64::try_from(signed.statement.sequence)
            .map_err(|_| denied("checkpoint sequence overflow"))?;
        transaction!(inner, |inner| {
            let count = rows(
                &inner.conn,
                "SELECT COUNT(*) FROM secure_checkpoint_archive",
            )
            .await?;
            if count != vec![vec![SqlValue::Integer(0)]] {
                let Some([SqlValue::Integer(count)]) = count.first().map(Vec::as_slice) else {
                    return Err(Error::Corrupt("custody archive count".into()));
                };
                if *count >= MAX_SECURE_RECORDS as i64 {
                    return Err(Error::Quota {
                        resource: "custody checkpoints",
                        limit: MAX_SECURE_RECORDS,
                    });
                }
            }
            inner.check_archive_quota(signed.to_bytes()?.len()).await?;
            inner
                .conn
                .execute(
                    "INSERT INTO secure_checkpoint_archive VALUES(?,?,?,?,NULL,0)",
                    bindings(vec![
                        SqlValue::Blob(encode(&scope)?),
                        SqlValue::Blob(checkpoint.to_vec()),
                        SqlValue::Integer(sequence),
                        SqlValue::Blob(signed.to_bytes()?),
                    ]),
                )
                .await?;
            Ok(())
        })?;
        drop(flush_guard(inner, &self.inner).await?);
        Ok((None, false))
    }
    pub(crate) async fn archive_checkpoint_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        page: CheckpointPage,
        vouch: CheckpointVouch,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let scope = page.scope;
        inner.authorize_peer(peer, credentials, scope)?;
        if vouch.signed_at > now()?.saturating_add(MAX_CLOCK_AHEAD.as_secs())
            || !vouch.signer.verify(
                &vouch_bytes(&page, vouch.signer, &vouch.chain, vouch.signed_at)?,
                &vouch.signature,
            )
        {
            return Err(denied("invalid custody checkpoint page vouch"));
        }
        let grants =
            inner.verify_chain_scopes(&vouch.chain, vouch.signer, vouch.signed_at, &[scope])?;
        if !grants.iter().any(|grant| {
            grant.scope == scope
                && grant.rows == RowSelection::All
                && grant.rights.contains(&Right::Read)
        }) {
            return Err(denied("custody checkpoint page needs a reader vouch"));
        }
        let signed = inner.check_archive(scope, page.checkpoint).await?;
        if page.rows.kind != PackageKind::Checkpoint
            || page.rows.scope != scope
            || Some(page.rows.generation) != signed.statement.key_epoch()
            || page
                .next
                .is_some_and(|next| page.after.is_some_and(|after| next <= after))
        {
            return Err(denied("custody checkpoint page outside archive"));
        }
        let cursor = page.after.map(|after| after.to_vec()).unwrap_or_default();
        let bytes = encode(&page)?;
        transaction!(inner, |inner| {
            let held = collect_rows(inner.conn.query("SELECT page FROM secure_checkpoint_archive_pages WHERE scope=? AND checkpoint=? AND cursor=?", [encode(&scope)?, page.checkpoint.to_vec(), cursor.clone()]).await?).await?;
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
                        "SELECT cursor,complete FROM secure_checkpoint_archive WHERE scope=? AND checkpoint=?",
                        [encode(&scope)?, page.checkpoint.to_vec()],
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
            inner
                .check_archive_quota(bytes.len() + encode(&vouch)?.len())
                .await?;
            inner
                .conn
                .execute(
                    "INSERT INTO secure_checkpoint_archive_pages VALUES(?,?,?,?,?)",
                    [
                        encode(&scope)?,
                        page.checkpoint.to_vec(),
                        cursor.clone(),
                        bytes.clone(),
                        encode(&vouch)?,
                    ],
                )
                .await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_checkpoint_archive SET cursor=?,complete=? WHERE scope=? AND checkpoint=?",
                    bindings(vec![
                        page.next
                            .map_or(SqlValue::Null, |next| SqlValue::Blob(next.to_vec())),
                        SqlValue::Integer(i64::from(page.next.is_none())),
                        SqlValue::Blob(encode(&scope)?),
                        SqlValue::Blob(page.checkpoint.to_vec()),
                    ]),
                )
                .await?;
            Ok(())
        })?;
        drop(flush_guard(inner, &self.inner).await?);
        Ok(())
    }
}
impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn check_archive_quota(
        &self,
        additional: usize,
    ) -> Result<(), Error> {
        let mut result = self.conn.query("SELECT (SELECT COALESCE(SUM(LENGTH(page)+LENGTH(vouch)),0) FROM secure_checkpoint_archive_pages)+(SELECT COALESCE(SUM(LENGTH(signed)),0) FROM secure_checkpoint_archive)+(SELECT COALESCE(SUM(LENGTH(record)),0) FROM secure_escrow)+(SELECT COALESCE(SUM(LENGTH(record)),0) FROM secure_custody_metadata)+(SELECT COALESCE(SUM(LENGTH(manifest)),0) FROM secure_custody_bootstrap)+(SELECT bytes FROM secure_retained WHERE id=1)", ()).await?;
        let used = result
            .next()
            .await?
            .ok_or_else(|| Error::Corrupt("custody retained bytes".into()))?
            .get::<i64>(0)?;
        let used =
            usize::try_from(used).map_err(|_| Error::Corrupt("custody retained bytes".into()))?;
        if used.saturating_add(additional) > self.limits.hard_bytes {
            return Err(Error::Quota {
                resource: "custody checkpoint archive",
                limit: self.limits.hard_bytes,
            });
        }
        Ok(())
    }
    pub(in crate::replica) async fn archived_checkpoint(
        &self,
        scope: ResourceScope,
        complete: bool,
    ) -> Result<Option<SignedCheckpoint>, Error> {
        let mut rows = self.conn.query("SELECT signed FROM secure_checkpoint_archive WHERE scope=? AND (complete=1 OR ?=0) ORDER BY sequence DESC,checkpoint DESC LIMIT 1", bindings(vec![SqlValue::Blob(encode(&scope)?), SqlValue::Integer(i64::from(complete))])).await?;
        rows.next()
            .await?
            .map(|row| SignedCheckpoint::from_bytes(&row.get::<Vec<u8>>(0)?))
            .transpose()
    }
    async fn archive_progress(
        &self,
        scope: ResourceScope,
        checkpoint: Commitment,
    ) -> Result<(Option<Commitment>, bool), Error> {
        let mut rows = self
            .conn
            .query(
                "SELECT cursor,complete FROM secure_checkpoint_archive WHERE scope=? AND checkpoint=?",
                [encode(&scope)?, checkpoint.to_vec()],
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
        let mut result = self
            .conn
            .query(
                "SELECT signed FROM secure_checkpoint_archive WHERE scope=? AND checkpoint=?",
                [encode(&scope)?, checkpoint.to_vec()],
            )
            .await?;
        let row = result
            .next()
            .await?
            .ok_or_else(|| denied("custody checkpoint outside archive"))?;
        SignedCheckpoint::from_bytes(&row.get::<Vec<u8>>(0)?)
    }

    pub(super) async fn archived_page(
        &self,
        scope: ResourceScope,
        checkpoint: Commitment,
        after: Option<Commitment>,
        quota: usize,
    ) -> Result<CheckpointPage, Error> {
        let complete = collect_rows(self.conn.query("SELECT 1 FROM secure_checkpoint_archive WHERE scope=? AND checkpoint=? AND complete=1", [encode(&scope)?, checkpoint.to_vec()]).await?).await?;
        if complete.is_empty() {
            return Err(denied("custody checkpoint archive incomplete"));
        }
        let mut rows = self
            .conn
            .query(
                "SELECT page FROM secure_checkpoint_archive_pages WHERE scope=? AND checkpoint=? AND cursor=?",
                [
                    encode(&scope)?,
                    checkpoint.to_vec(),
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
