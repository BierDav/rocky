//! Bounded transfer of the creation proofs retained across a legacy upgrade.
use super::*;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointWitnessPage {
    pub scope: ResourceScope,
    pub checkpoint: Commitment,
    pub after: Option<Commitment>,
    pub next: Option<Commitment>,
    pub proofs: EncryptedPackage,
}
#[derive(Serialize, Deserialize)]
struct Plain {
    checkpoint: Commitment,
    after: Option<Commitment>,
    next: Option<Commitment>,
    witnesses: Vec<(Commitment, u64)>,
}
impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn capture_checkpoint_witnesses(
        &self,
        scope: ResourceScope,
        id: i64,
        table: &str,
    ) -> Result<(), Error> {
        self.conn
            .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
            .await?;
        self.conn.execute(format!("INSERT INTO {table} SELECT scope,hash,clock FROM secure_stubs WHERE scope=? AND creation=1"), [id]).await?;
        let mut after = Vec::new();
        loop {
            let rows = collect_rows(self.conn.query("SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?", bindings(vec![SqlValue::Blob(encode(&scope)?),SqlValue::Blob(after.clone()),SqlValue::Integer(STEP_ROWS as i64)])).await?).await?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                let [SqlValue::Blob(key), SqlValue::Blob(bytes)] = row.as_slice() else {
                    return Err(Error::Corrupt("creation witness row".into()));
                };
                after = key.clone();
                let record: Materialized = decode(bytes)?;
                let clock = match self.stub(record.creation).await? {
                    Some((_, clock)) => clock,
                    None => {
                        self.frame(record.creation)
                            .await?
                            .ok_or_else(|| denied("checkpoint creation witness missing"))?
                            .operation()?
                            .header
                            .timestamp
                    }
                };
                self.conn
                    .execute(
                        format!("INSERT OR IGNORE INTO {table} VALUES(?,?,?)"),
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(record.creation.as_bytes().to_vec()),
                            integer(clock),
                        ]),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn checkpoint_witness_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        checkpoint: Commitment,
        after: Option<Commitment>,
    ) -> Result<CheckpointWitnessPage, Error> {
        let mut inner = self.inner.lock().await;
        inner.authorize_peer(peer, credentials, scope)?;
        let (_, signed) = inner
            .served_checkpoint(scope)
            .await?
            .ok_or_else(|| denied("checkpoint unavailable"))?;
        if signed.id()? != checkpoint || signed.statement.format != 6 {
            return Err(denied("creation proof checkpoint changed"));
        }
        let id = inner.scope_id(scope).await?;
        let records = collect_rows(inner.conn.query("SELECT hash,clock FROM secure_checkpoint_stubs WHERE scope=? AND hash>? ORDER BY hash LIMIT ?", bindings(vec![SqlValue::Integer(id),SqlValue::Blob(after.map(|hash|hash.to_vec()).unwrap_or_default()),SqlValue::Integer(STEP_ROWS as i64)])).await?).await?;
        let witnesses = records
            .iter()
            .map(|row| {
                let [SqlValue::Blob(hash), SqlValue::Integer(clock)] = row.as_slice() else {
                    return Err(Error::Corrupt("creation proof".into()));
                };
                Ok((row_of(hash)?, *clock as u64))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let next = (witnesses.len() == STEP_ROWS)
            .then(|| witnesses.last().map(|(hash, _)| *hash))
            .flatten();
        let (generation, key) = inner.scope_key(scope)?;
        let proofs = EncryptedPackage::seal(
            scope,
            generation,
            PackageKind::Checkpoint,
            &key,
            &encode(&Plain {
                checkpoint,
                after,
                next,
                witnesses,
            })?,
        )?;
        inner.flush()?;
        Ok(CheckpointWitnessPage {
            scope,
            checkpoint,
            after,
            next,
            proofs,
        })
    }
    pub async fn checkpoint_witness_progress(
        &self,
        scope: ResourceScope,
    ) -> Result<(Option<Commitment>, bool), Error> {
        let inner = self.inner.lock().await;
        let Some(id) = inner.find_scope_id(scope).await? else {
            return Ok((None, false));
        };
        let mut rows = inner
            .conn
            .query(
                "SELECT cursor,complete FROM secure_rebootstrap_witnesses WHERE scope=?",
                [id],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok((None, false));
        };
        let cursor = match row.get_value(0)? {
            crate::sqlite::Value::Blob(bytes) => Some(row_of(&bytes)?),
            _ => None,
        };
        Ok((cursor, row.get::<i64>(1)? != 0))
    }
    pub async fn stage_checkpoint_witnesses(
        &self,
        page: CheckpointWitnessPage,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        let scope = page.scope;
        let (generation, key) = inner.scope_key(scope)?;
        if page.proofs.scope != scope
            || page.proofs.generation != generation
            || page.proofs.kind != PackageKind::Checkpoint
        {
            return Err(denied("creation proof package context"));
        }
        let plain: Plain = decode(&page.proofs.open(&key)?)?;
        if plain.checkpoint != page.checkpoint
            || plain.after != page.after
            || plain.next != page.next
            || plain.witnesses.len() > STEP_ROWS
            || plain
                .witnesses
                .windows(2)
                .any(|pair| pair[0].0 >= pair[1].0)
            || plain
                .witnesses
                .first()
                .is_some_and(|(first, _)| page.after.is_some_and(|after| *first <= after))
            || page
                .next
                .is_some_and(|next| plain.witnesses.last().is_none_or(|(hash, _)| *hash != next))
        {
            return Err(denied("noncanonical creation proof page"));
        }
        transaction!(inner, |inner| {
            let id = inner.scope_id(scope).await?;
            let mut staging = inner
                .staging(id)
                .await?
                .ok_or_else(|| denied("no checkpoint staging"))?;
            if staging.signed.id()? != page.checkpoint || staging.signed.statement.format != 6 {
                return Err(denied("creation proof checkpoint context"));
            }
            let mut progress = inner
                .conn
                .query(
                    "SELECT cursor,complete FROM secure_rebootstrap_witnesses WHERE scope=?",
                    [id],
                )
                .await?;
            let (cursor, complete) = match progress.next().await? {
                None => (None, false),
                Some(row) => (
                    match row.get_value(0)? {
                        crate::sqlite::Value::Blob(bytes) => Some(row_of(&bytes)?),
                        _ => None,
                    },
                    row.get::<i64>(1)? != 0,
                ),
            };
            if complete || page.after != cursor {
                return Err(denied("creation proof page outside durable cursor"));
            }
            for (hash, clock) in &plain.witnesses {
                if *clock > staging.signed.statement.clock {
                    return Err(denied("creation proof clock outside checkpoint"));
                }
                let mut old = inner
                    .conn
                    .query(
                        "SELECT clock FROM secure_rebootstrap_stubs WHERE scope=? AND hash=?",
                        bindings(vec![SqlValue::Integer(id), SqlValue::Blob(hash.to_vec())]),
                    )
                    .await?;
                if let Some(old) = old.next().await? {
                    if old.get::<i64>(0)? as u64 != *clock {
                        return Err(denied("creation proof clocks disagree"));
                    }
                } else {
                    staging.sets.create(hash_of(hash)?, *clock);
                    if staging.sets.creations > staging.signed.statement.creations.count {
                        return Err(denied("creation proof quota"));
                    }
                    inner
                        .conn
                        .execute(
                            "INSERT INTO secure_rebootstrap_stubs VALUES(?,?,?)",
                            bindings(vec![
                                SqlValue::Integer(id),
                                SqlValue::Blob(hash.to_vec()),
                                integer(*clock),
                            ]),
                        )
                        .await?;
                }
            }
            let cursor = plain.witnesses.last().map(|(hash, _)| *hash).or(page.after);
            inner.conn.execute("INSERT INTO secure_rebootstrap_witnesses VALUES(?,?,?) ON CONFLICT(scope) DO UPDATE SET cursor=excluded.cursor,complete=excluded.complete", bindings(vec![SqlValue::Integer(id),cursor.map_or(SqlValue::Null,|hash|SqlValue::Blob(hash.to_vec())),SqlValue::Integer(i64::from(page.next.is_none()))])).await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_rebootstrap SET sets=? WHERE scope=?",
                    bindings(vec![
                        SqlValue::Blob(staging.sets.to_bytes()),
                        SqlValue::Integer(id),
                    ]),
                )
                .await?;
            Ok(())
        })
    }
}
