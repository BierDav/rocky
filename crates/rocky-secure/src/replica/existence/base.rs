//! Issuer-authenticated live existence bases and bounded, resumable transfer.
use super::*;
use rocky_credentials::{
    ExistenceBase, ExistenceBaseStatement, ExistenceIdentity, HorizonCut, SignedExistenceBase,
    SignedHorizon,
};
const LIVE: &str = "rocky.existence-base-live.v1";

/// What authenticates an existence base.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExistenceBasis {
    Horizon(Box<SignedHorizon>),
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
            ExistenceBasis::Horizon(_) => None,
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
            (ExistenceBasis::Horizon(horizon), _) => horizon
                .statement
                .bases
                .iter()
                .find(|base| base.scope == self.scope)
                .ok_or_else(|| denied("horizon has no authenticated existence base")),
            (_, Some(statement)) if statement.base.scope == self.scope => Ok(&statement.base),
            _ => Err(denied("existence base names another scope")),
        }
    }
    pub fn cut(&self) -> Result<&HorizonCut, Error> {
        match (&self.basis, self.statement()) {
            (ExistenceBasis::Horizon(horizon), _) => horizon
                .statement
                .existence
                .iter()
                .find(|cut| cut.scope == self.scope)
                .ok_or_else(|| denied("horizon has no existence cut")),
            (_, Some(statement)) if statement.cut.scope == self.scope => Ok(&statement.cut),
            _ => Err(denied("existence base names another scope")),
        }
    }
    pub fn covers(&self, previous: &Self) -> Result<bool, Error> {
        if self.cut()?.logs == previous.cut()?.logs {
            let same = self.descriptor()? == previous.descriptor()?
                && self.cut()?.state == previous.cut()?.state;
            let authoritative = match (&self.basis, &previous.basis) {
                (ExistenceBasis::Horizon(new), ExistenceBasis::Horizon(old)) => {
                    new.statement.scope == self.scope
                        && (old.statement.scope != self.scope
                            || new.statement.sequence > old.statement.sequence)
                }
                _ => false,
            };
            if !same && !authoritative {
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
            (ExistenceBasis::Horizon(horizon), _) => Ok(rocky_core::commitment(
                "rocky.existence-base.id.v1",
                &encode(&(
                    horizon.statement.tip,
                    horizon.statement.revocations,
                    self.cut()?,
                    self.descriptor()?,
                ))?,
            )),
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
            ExistenceBasis::Horizon(horizon) => {
                horizon.verify(self.config.publisher).map_err(credential)?;
                horizon.statement.tip
            }
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
    pub(in crate::replica) async fn horizon_existence_rows(
        &self,
        scope: ResourceScope,
        finalizing: Option<ResourceScope>,
        after: i64,
    ) -> Result<Vec<(i64, Option<Commitment>, Option<Commitment>)>, Error> {
        let found = rows_with(&self.conn, "SELECT id,created,deleted FROM secure_existence WHERE scope=? AND (deleted IS NULL OR created IS NULL) AND id>? ORDER BY id LIMIT ?", vec![SqlValue::Blob(encode(&scope)?),SqlValue::Integer(after),SqlValue::Integer(MAX_EXISTENCE_PAGE as i64)]).await?;
        let mut rows = Vec::new();
        for row in found {
            let [SqlValue::Integer(id), created, deleted] = row.as_slice() else {
                return Err(corrupt());
            };
            rows.push((*id, nullable_hash(created)?, nullable_hash(deleted)?));
        }
        // Keep the scan cursor independent of the survivor count.
        if finalizing == Some(scope) {
            for (id, created, _) in &mut rows {
                if created.is_some() && !self.horizon_row_survives(scope, *id).await? {
                    *created = None;
                }
            }
        }
        Ok(rows)
    }
    async fn horizon_row_survives(&self, scope: ResourceScope, id: i64) -> Result<bool, Error> {
        let key = rocky_core::commitment("rocky.row-incarnation.v2", &id.to_le_bytes());
        let Some(record) = self.row(scope, key).await? else {
            let identity = rocky_core::RowIdentity::from_integer(id)
                .map_err(|error| denied(&error.to_string()))?;
            if self.identity_marks(scope).await?.iter().any(|mark| {
                mark.author == identity.author() && mark.sequence >= identity.sequence()
            }) {
                return Ok(false);
            }
            return Err(denied(
                "sync row creations before declaring a logged horizon",
            ));
        };
        if record.deleted {
            return Ok(false);
        }
        let typed = self
            .typed
            .as_ref()
            .ok_or_else(|| denied("horizon requires typed rows"))?;
        let table = typed
            .by_scope(scope)
            .ok_or_else(|| denied("horizon table missing"))?;
        let values = typed
            .rows
            .values(&table.table, &record.fields)
            .map_err(Error::Corrupt)?;
        let parameters = table
            .table
            .key
            .iter()
            .map(|index| values[*index].clone())
            .collect();
        let id_column = typed
            .rows
            .schema()
            .table(&table.table.sql_name)
            .ok_or_else(|| denied("horizon table missing"))?
            .columns
            .iter()
            .position(|column| column.sql_name.eq_ignore_ascii_case("id"))
            .ok_or_else(|| denied("horizon requires row incarnations"))?;
        let select = format!("{} AND row_kind=0", table.select);
        Ok(
            collect_rows(self.conn.query(select, bindings(parameters)).await?)
                .await?
                .iter()
                .any(|values| values.get(id_column) == Some(&SqlValue::Integer(id))),
        )
    }
    pub(in crate::replica) async fn capture_horizon_existence_base(
        &self,
        scope: ResourceScope,
        finalizing: Option<ResourceScope>,
    ) -> Result<ExistenceBase, Error> {
        let mut marks: BTreeMap<u32, ExistenceIdentity> = self
            .existence_base(scope)
            .await?
            .map(|base| {
                base.descriptor().map(|descriptor| {
                    descriptor
                        .identities
                        .iter()
                        .map(|mark| (mark.author, mark.clone()))
                        .collect()
                })
            })
            .transpose()?
            .unwrap_or_default();
        let mut after = None;
        loop {
            let page = self
                .existence_scan(scope, after, MAX_EXISTENCE_PAGE)
                .await?;
            for record in page.records {
                if record.event() == ExistenceEvent::Created {
                    let identity = record.statement.id;
                    match marks.get_mut(&identity.author()) {
                        Some(mark) if mark.signer != record.signer => {
                            return Err(denied("existence author changed signer"));
                        }
                        Some(mark) => mark.sequence = mark.sequence.max(identity.sequence()),
                        None => {
                            marks.insert(
                                identity.author(),
                                ExistenceIdentity {
                                    author: identity.author(),
                                    signer: record.signer,
                                    sequence: identity.sequence(),
                                },
                            );
                        }
                    }
                }
            }
            after = page.next;
            if after.is_none() {
                break;
            }
        }
        if finalizing == Some(scope) {
            for held in self.identity_marks(scope).await? {
                match marks.get_mut(&held.author) {
                    Some(mark) if mark.signer != held.signer => {
                        return Err(denied("row and existence authors disagree"));
                    }
                    Some(mark) => mark.sequence = mark.sequence.max(held.sequence),
                    None => {
                        marks.insert(
                            held.author,
                            ExistenceIdentity {
                                author: held.author,
                                signer: held.signer,
                                sequence: held.sequence,
                            },
                        );
                    }
                }
            }
        }
        if marks.len() > MAX_CHECKPOINT_LOGS {
            return Err(denied("existence base author quota"));
        }
        let (mut count, mut digest, mut cursor) = (0, checkpoint::Lt::new(), 0i64);
        loop {
            let rows = self
                .horizon_existence_rows(scope, finalizing, cursor)
                .await?;
            if rows.is_empty() {
                break;
            }
            for (id, created, deleted) in rows {
                cursor = id;
                if created.is_none() && deleted.is_none() {
                    continue;
                }
                digest.add(LIVE, &item(id, &created, &deleted)?);
                count += 1;
            }
        }
        Ok(ExistenceBase {
            scope,
            rows: count,
            digest: digest.digest(),
            identities: marks.into_values().collect(),
        })
    }
    /// Captured roots are rechecked in the row-finalization transaction.
    pub(in crate::replica) async fn seal_existence_bases(
        &self,
        horizon: &SignedHorizon,
    ) -> Result<(), Error> {
        for cut in &horizon.statement.existence {
            let attachment = ExistenceBaseAttachment {
                basis: ExistenceBasis::Horizon(Box::new(horizon.clone())),
                scope: cut.scope,
            };
            self.verify_existence_base(&attachment)?;
            if self
                .capture_horizon_existence_base(cut.scope, Some(horizon.statement.scope))
                .await?
                != *attachment.descriptor()?
            {
                return Err(denied("existence base does not reproduce the declared cut"));
            }
            let blob = encode(&cut.scope)?;
            self.conn
                .execute(
                    "DELETE FROM secure_existence_base_rows WHERE scope=?",
                    [blob.clone()],
                )
                .await?;
            let mut cursor = 0;
            loop {
                let rows = self
                    .horizon_existence_rows(cut.scope, Some(horizon.statement.scope), cursor)
                    .await?;
                if rows.is_empty() {
                    break;
                }
                for (id, created, deleted) in rows {
                    cursor = id;
                    if created.is_none() && deleted.is_none() {
                        continue;
                    }
                    self.conn
                        .execute(
                            "INSERT INTO secure_existence_base_rows VALUES(?,?,?,?)",
                            bindings(vec![
                                SqlValue::Blob(blob.clone()),
                                SqlValue::Integer(id),
                                hash_value(&created),
                                hash_value(&deleted),
                            ]),
                        )
                        .await?;
                }
            }
            if cut.scope == horizon.statement.scope {
                self.conn.execute("DELETE FROM secure_existence WHERE scope=? AND created IS NOT NULL AND NOT EXISTS(SELECT 1 FROM secure_existence_base_rows b WHERE b.scope=secure_existence.scope AND b.id=secure_existence.id)", [blob.clone()]).await?;
            }
            self.conn.execute("INSERT INTO secure_existence_base VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET attachment=excluded.attachment", [blob, encode(&attachment)?]).await?;
        }
        self.archive_existence_bases().await?;
        Ok(())
    }
    pub(in crate::replica) async fn compact_existence_base(
        &self,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        let base = self
            .existence_base(scope)
            .await?
            .ok_or_else(|| denied("no signed existence base"))?;
        for log in &base.cut()?.logs {
            self.conn
                .execute(
                    "DELETE FROM secure_existence_log WHERE scope=? AND signer=? AND sequence<=?",
                    bindings(vec![
                        SqlValue::Blob(encode(&scope)?),
                        SqlValue::Blob(log.signer.as_bytes().to_vec()),
                        SqlValue::Integer(log.sequence as i64),
                    ]),
                )
                .await?;
        }
        self.conn.execute("DELETE FROM secure_existence WHERE scope=? AND created IS NOT NULL AND deleted IS NOT NULL", [encode(&scope)?]).await?;
        Ok(())
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn existence_base_offer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<Option<ExistenceBaseAttachment>, Error> {
        let mut inner = self.inner.lock().await;
        inner.authorize_domain_peer(peer, credentials, scope)?;
        let base = inner
            .existence_base(scope)
            .await?
            .filter(|base| !matches!(base.basis, ExistenceBasis::Local(_)));
        inner.flush()?;
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
        let mut inner = self.inner.lock().await;
        inner.authorize_domain_peer(peer, credentials, scope)?;
        if inner.archived_existence_base(scope, base).await?.is_none() {
            return Err(denied("existence base changed; restart transfer"));
        }
        let limit = limit.clamp(1, MAX_EXISTENCE_PAGE);
        let found = rows_with(&inner.conn, "SELECT id,created,deleted FROM secure_existence_archive_rows WHERE base=? AND id>? ORDER BY id LIMIT ?", vec![SqlValue::Blob(base.to_vec()),SqlValue::Integer(after.unwrap_or(0)),SqlValue::Integer(limit as i64)]).await?;
        let rows = found
            .iter()
            .map(|row| {
                let [SqlValue::Integer(id), created, deleted] = row.as_slice() else {
                    return Err(corrupt());
                };
                Ok((*id, nullable_hash(created)?, nullable_hash(deleted)?))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let next = (rows.len() == limit)
            .then(|| rows.last().map(|row| row.0))
            .flatten();
        inner.flush()?;
        Ok(ExistenceBasePage { base, rows, next })
    }
    /// Returns a durable cursor, resuming only the same authenticated base.
    pub async fn begin_existence_base(
        &self,
        base: ExistenceBaseAttachment,
    ) -> Result<(Option<i64>, bool), Error> {
        let mut inner = self.inner.lock().await;
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
        let mut inner = self.inner.lock().await;
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
        let mut inner = self.inner.lock().await;
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
        let mut inner = self.inner.lock().await;
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
        inner.flush()?;
        inner.publish_revision().await
    }
}
