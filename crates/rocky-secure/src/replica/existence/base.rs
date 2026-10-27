//! Issuer-authenticated live existence bases and bounded, resumable transfer.
use super::*;
use rocky_credentials::{ExistenceBase, ExistenceBaseStatement, ExistenceCut, SignedExistenceBase};
const LIVE: &str = "rocky.existence-base-live.v1";

/// What authenticates an existence base.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExistenceBasis {
    /// This replica's own fold at a seal; never offered to peers.
    Local(Box<ExistenceBaseStatement>),
    /// A fold at a seal signed by the owner, a trusted identity or an issuer.
    Signed(Box<SignedExistenceBase>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistenceBaseAttachment {
    pub basis: ExistenceBasis,
    pub scope: ResourceScope,
}
impl ExistenceBaseAttachment {
    /// The fold at a seal, if this base is one.
    pub fn statement(&self) -> Option<&ExistenceBaseStatement> {
        match &self.basis {
            ExistenceBasis::Local(statement) => Some(statement),
            ExistenceBasis::Signed(signed) => Some(&signed.statement),
        }
    }
    /// The seal a fold sits at.
    pub fn seal(&self) -> Option<Commitment> {
        self.statement().map(|statement| statement.seal)
    }
    pub fn descriptor(&self) -> Result<&ExistenceBase, Error> {
        match (&self.basis, self.statement()) {
            (_, Some(statement)) if statement.base.scope == self.scope => Ok(&statement.base),
            _ => Err(denied("existence base names another scope")),
        }
    }
    pub fn cut(&self) -> Result<&ExistenceCut, Error> {
        match (&self.basis, self.statement()) {
            (_, Some(statement)) if statement.cut.scope == self.scope => Ok(&statement.cut),
            _ => Err(denied("existence base names another scope")),
        }
    }
    pub fn covers(&self, previous: &Self) -> Result<bool, Error> {
        if self.cut()?.logs == previous.cut()?.logs {
            let same = self.descriptor()? == previous.descriptor()?
                && self.cut()?.state == previous.cut()?.state;
            if !same {
                return Ok(false);
            }
        }
        Ok(self.scope == previous.scope
            && previous.cut()?.logs.iter().all(|old| {
                self.cut().is_ok_and(|cut| {
                    cut.logs.iter().any(|new| {
                        new.signer == old.signer
                            && new.sequence >= old.sequence
                            && (new.sequence != old.sequence || new.head == old.head)
                    })
                })
            })
            && previous.descriptor()?.identities.iter().all(|old| {
                self.descriptor().is_ok_and(|base| {
                    base.identities.iter().any(|new| {
                        new.author == old.author
                            && new.signer == old.signer
                            && new.sequence >= old.sequence
                    })
                })
            }))
    }
    /// Equal for a local fold and its signed copy.
    pub fn id(&self) -> Result<Commitment, Error> {
        match (&self.basis, self.statement()) {
            (_, Some(statement)) => Ok(rocky_core::commitment(
                "rocky.existence-base.id.v2",
                &encode(statement)?,
            )),
            _ => Err(corrupt()),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExistenceBasePage {
    pub base: Commitment,
    pub rows: Vec<(i64, Option<Commitment>, Option<Commitment>)>,
    pub next: Option<i64>,
}
fn item(
    id: i64,
    created: &Option<Commitment>,
    deleted: &Option<Commitment>,
) -> Result<Vec<u8>, Error> {
    encode(&(id, created, deleted))
}
fn nullable_hash(value: &SqlValue) -> Result<Option<Commitment>, Error> {
    match value {
        SqlValue::Null => Ok(None),
        SqlValue::Blob(bytes) => checkpoint::row_of(bytes).map(Some),
        _ => Err(corrupt()),
    }
}
fn hash_value(value: &Option<Commitment>) -> SqlValue {
    value.map_or(SqlValue::Null, |hash| SqlValue::Blob(hash.to_vec()))
}
impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn existence_base(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<ExistenceBaseAttachment>, Error> {
        let mut rows = self
            .conn
            .query(
                "SELECT attachment FROM secure_existence_base WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        rows.next()
            .await?
            .map(|row| decode(&row.get::<Vec<u8>>(0)?))
            .transpose()
    }
    pub(in crate::replica) fn verify_existence_base(
        &self,
        base: &ExistenceBaseAttachment,
    ) -> Result<(), Error> {
        let tip = match &base.basis {
            ExistenceBasis::Local(_) => {
                return Err(denied("a local existence base is never imported"));
            }
            ExistenceBasis::Signed(signed) => {
                let graph = self.graph_for(base.scope.domain)?;
                signed
                    .verify(self.config.publisher, graph)
                    .map_err(credential)?;
                // Only a seal the followed tip's state passes through.
                let path = self.path_seals(base.scope.domain)?;
                let seal = path
                    .iter()
                    .find(|seal| seal.id().is_ok_and(|id| id == signed.statement.seal))
                    .ok_or_else(|| denied("existence base of a seal off the followed path"))?;
                seal.statement.tip
            }
        };
        self.admits(self.manifest_of_tip(tip)?)?;
        if !self.existence_table(base.scope) {
            return Err(denied("existence base context is not admitted"));
        }
        base.descriptor()?;
        base.cut()?;
        Ok(())
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn installed_existence_base(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<ExistenceBaseAttachment>, Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .existence_base(scope)
            .await
    }
    pub async fn existence_base_offer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<Option<ExistenceBaseAttachment>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.authorize_domain_peer(peer, credentials, scope)?;
        let base = inner
            .existence_base(scope)
            .await?
            .filter(|base| !matches!(base.basis, ExistenceBasis::Local(_)));
        drop(flush_guard(inner, &self.inner).await?);
        Ok(base)
    }
    pub async fn existence_base_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        base: Commitment,
        after: Option<i64>,
        limit: usize,
    ) -> Result<ExistenceBasePage, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.authorize_domain_peer(peer, credentials, scope)?;
        if inner.archived_existence_base(scope, base).await?.is_none() {
            return Err(denied("existence base changed; restart transfer"));
        }
        let limit = limit.clamp(1, MAX_EXISTENCE_PAGE);
        let found = rows_with(&inner.conn, "SELECT id,created,deleted FROM secure_existence_archive_rows WHERE base=? AND id>? ORDER BY id LIMIT ?", vec![SqlValue::Blob(base.to_vec()),SqlValue::Integer(after.unwrap_or(0)),SqlValue::Integer(limit as i64)]).await?;
        let mut rows = found
            .iter()
            .map(|row| {
                let [SqlValue::Integer(id), created, deleted] = row.as_slice() else {
                    return Err(corrupt());
                };
                Ok((*id, nullable_hash(created)?, nullable_hash(deleted)?))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let count = page_len(&rows, limit)?;
        let more = count < rows.len() || rows.len() == limit;
        rows.truncate(count);
        let next = more.then(|| rows.last().map(|row| row.0)).flatten();
        drop(flush_guard(inner, &self.inner).await?);
        Ok(ExistenceBasePage { base, rows, next })
    }
    /// Returns a durable cursor, resuming only the same authenticated base.
    pub async fn begin_existence_base(
        &self,
        base: ExistenceBaseAttachment,
    ) -> Result<(Option<i64>, bool), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.verify_existence_base(&base)?;
        let scope = encode(&base.scope)?;
        transaction!(inner, |inner| {
            let found = rows_with(
                &inner.conn,
                "SELECT attachment,cursor,complete FROM secure_existence_stage WHERE scope=?",
                vec![SqlValue::Blob(scope.clone())],
            )
            .await?;
            if let Some(
                [
                    SqlValue::Blob(bytes),
                    SqlValue::Integer(cursor),
                    SqlValue::Integer(complete),
                ],
            ) = found.first().map(Vec::as_slice)
                && decode::<ExistenceBaseAttachment>(bytes)?.id()? == base.id()?
            {
                return Ok(((*cursor != 0).then_some(*cursor), *complete != 0));
            }
            inner
                .conn
                .execute(
                    "DELETE FROM secure_existence_stage_rows WHERE scope=?",
                    [scope.clone()],
                )
                .await?;
            inner.conn.execute("INSERT INTO secure_existence_stage VALUES(?,?,0,0,0,?) ON CONFLICT(scope) DO UPDATE SET attachment=excluded.attachment,cursor=0,complete=0,count=0,digest=excluded.digest", [scope.clone(), encode(&base)?, checkpoint::Lt::new().to_bytes()]).await?;
            Ok((None, false))
        })
    }
    pub async fn stage_existence_base(
        &self,
        scope: ResourceScope,
        page: ExistenceBasePage,
    ) -> Result<(), Error> {
        if page.rows.len() > MAX_EXISTENCE_PAGE {
            return Err(denied("existence base page quota"));
        }
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(inner, |inner| {
            let blob = encode(&scope)?;
            let held = rows_with(&inner.conn, "SELECT attachment,cursor,count,digest,complete FROM secure_existence_stage WHERE scope=?", vec![SqlValue::Blob(blob.clone())]).await?;
            let Some(
                [
                    SqlValue::Blob(bytes),
                    SqlValue::Integer(cursor),
                    SqlValue::Integer(count),
                    SqlValue::Blob(digest),
                    SqlValue::Integer(complete),
                ],
            ) = held.first().map(Vec::as_slice)
            else {
                return Err(denied("no existence base import"));
            };
            let base: ExistenceBaseAttachment = decode(bytes)?;
            if base.id()? != page.base
                || *complete != 0
                || page.rows.windows(2).any(|rows| rows[0].0 >= rows[1].0)
                || page.rows.first().is_some_and(|row| row.0 <= *cursor)
                || page
                    .next
                    .is_some_and(|next| page.rows.last().is_none_or(|row| row.0 != next))
            {
                return Err(denied("existence base page order/context"));
            }
            let mut digest = checkpoint::Lt::from_bytes(digest)?;
            for (id, created, deleted) in &page.rows {
                let identity =
                    RowIdentity::from_integer(*id).map_err(|error| denied(&error.to_string()))?;
                if created.is_some()
                    && !base.descriptor()?.identities.iter().any(|mark| {
                        mark.author == identity.author() && mark.sequence >= identity.sequence()
                    })
                {
                    return Err(denied("existence row exceeds signed identity mark"));
                }
                if created.is_some() && deleted.is_some() || created.is_none() && deleted.is_none()
                {
                    return Err(denied("noncompact existence row"));
                }
                digest.add(LIVE, &item(*id, created, deleted)?);
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_existence_stage_rows VALUES(?,?,?,?)",
                        bindings(vec![
                            SqlValue::Blob(blob.clone()),
                            SqlValue::Integer(*id),
                            hash_value(created),
                            hash_value(deleted),
                        ]),
                    )
                    .await?;
            }
            inner.conn.execute("UPDATE secure_existence_stage SET cursor=?,complete=?,count=?,digest=? WHERE scope=?", bindings(vec![SqlValue::Integer(page.rows.last().map_or(*cursor, |row| row.0)),SqlValue::Integer(i64::from(page.next.is_none())),SqlValue::Integer(*count+page.rows.len() as i64),SqlValue::Blob(digest.to_bytes()),SqlValue::Blob(blob)])).await?;
            Ok(())
        })
    }
    /// Discards transfer progress without touching the installed state.
    pub async fn abort_existence_base(&self, scope: ResourceScope) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(inner, |inner| {
            for table in ["secure_existence_stage", "secure_existence_stage_rows"] {
                inner
                    .conn
                    .execute(
                        format!("DELETE FROM {table} WHERE scope=?"),
                        [encode(&scope)?],
                    )
                    .await?;
            }
            Ok(())
        })
    }
    pub async fn finish_existence_base(&self, scope: ResourceScope) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(inner, |inner| {
            let blob = encode(&scope)?;
            let held = rows_with(
                &inner.conn,
                "SELECT attachment,complete,count,digest FROM secure_existence_stage WHERE scope=?",
                vec![SqlValue::Blob(blob.clone())],
            )
            .await?;
            let Some(
                [
                    SqlValue::Blob(bytes),
                    SqlValue::Integer(complete),
                    SqlValue::Integer(count),
                    SqlValue::Blob(digest),
                ],
            ) = held.first().map(Vec::as_slice)
            else {
                return Err(denied("no existence base import"));
            };
            let base: ExistenceBaseAttachment = decode(bytes)?;
            inner.verify_existence_base(&base)?;
            if *complete != 1
                || *count as u64 != base.descriptor()?.rows
                || checkpoint::Lt::from_bytes(digest)?.digest() != base.descriptor()?.digest
            {
                return Err(denied("existence base rows differ from issuer signature"));
            }
            if let Some(previous) = inner.existence_base(scope).await?
                && !inner.fold_off_path(scope).await?
                && !base.covers(&previous)?
            {
                return Err(denied("existence base would move backward"));
            }
            // Replay verified suffix records in bounded pages from transactional scratch storage.
            inner
                .conn
                .execute(
                    "DELETE FROM secure_existence_replay WHERE scope=?",
                    [blob.clone()],
                )
                .await?;
            inner.conn.execute("INSERT INTO secure_existence_replay SELECT scope,signer,sequence,record FROM secure_existence_log WHERE scope=?", [blob.clone()]).await?;
            for log in &base.cut()?.logs {
                inner.conn.execute("DELETE FROM secure_existence_replay WHERE scope=? AND signer=? AND sequence<=?", bindings(vec![SqlValue::Blob(blob.clone()),SqlValue::Blob(log.signer.as_bytes().to_vec()),SqlValue::Integer(log.sequence as i64)])).await?;
            }
            inner
                .conn
                .execute("DELETE FROM secure_existence WHERE scope=?", [blob.clone()])
                .await?;
            inner.conn.execute("INSERT INTO secure_existence SELECT * FROM secure_existence_stage_rows WHERE scope=?", [blob.clone()]).await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_existence_log WHERE scope=?",
                    [blob.clone()],
                )
                .await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_existence_base_rows WHERE scope=?",
                    [blob.clone()],
                )
                .await?;
            inner.conn.execute("INSERT INTO secure_existence_base_rows SELECT * FROM secure_existence_stage_rows WHERE scope=?", [blob.clone()]).await?;
            inner.conn.execute("INSERT INTO secure_existence_base VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET attachment=excluded.attachment", [blob.clone(), bytes.clone()]).await?;
            loop {
                let records = rows_with(&inner.conn,"SELECT signer,sequence,record FROM secure_existence_replay WHERE scope=? ORDER BY signer,sequence LIMIT ?",vec![SqlValue::Blob(blob.clone()),SqlValue::Integer(MAX_EXISTENCE_PAGE as i64)]).await?;
                if records.is_empty() {
                    break;
                }
                for row in records {
                    let [
                        SqlValue::Blob(signer),
                        SqlValue::Integer(sequence),
                        SqlValue::Blob(bytes),
                    ] = row.as_slice()
                    else {
                        return Err(corrupt());
                    };
                    let record = ExistenceRecord::from_bytes(bytes)?;
                    if inner.store_existence(&record).await? != ReceiveStatus::Applied {
                        return Err(denied("existence suffix has a gap after base import"));
                    }
                    inner.conn.execute("DELETE FROM secure_existence_replay WHERE scope=? AND signer=? AND sequence=?", bindings(vec![SqlValue::Blob(blob.clone()),SqlValue::Blob(signer.clone()),SqlValue::Integer(*sequence)])).await?;
                }
            }
            inner.archive_existence_bases().await?;
            inner.collect_existence_archives().await?;
            Box::pin(inner.rebuild_constraints(Some(scope), None)).await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_existence_stage WHERE scope=?",
                    [blob.clone()],
                )
                .await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_existence_stage_rows WHERE scope=?",
                    [blob],
                )
                .await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
            Ok(())
        })?;
        inner = flush_guard(inner, &self.inner).await?;
        inner.publish_revision().await
    }
}
