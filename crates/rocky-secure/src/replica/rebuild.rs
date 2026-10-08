use super::*;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_rows_shadow(scope BLOB NOT NULL,row BLOB NOT NULL,fields BLOB NOT NULL,stamp BLOB NOT NULL,PRIMARY KEY(scope,row))",
    "CREATE INDEX IF NOT EXISTS secure_operation_scope_hash ON secure_operations(scope,hash)",
    "CREATE INDEX IF NOT EXISTS secure_operation_status_hash ON secure_operations(status,hash)",
    // Serves the pending check of every read and the replay scan.
    "CREATE INDEX IF NOT EXISTS secure_operation_scope_status ON secure_operations(scope,status,hash)",
];

impl<A: RowApplication> EncryptedReplica<A> {
    /// Rebuilds one local row projection from the verified checkpoint and the
    /// original signed bytes after it. This does not migrate application
    /// generations or reinterpret a confirmed strong head.
    pub async fn rebuild_projection(&self, scope: ResourceScope) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        if !inner.can_read_key(scope, &inner.local_grants(now()?)?) {
            return Err(denied("projection rebuild denied"));
        }
        if !inner.config.keys.iter().any(|key| key.scope == scope) {
            return Err(denied("projection rebuild key unavailable"));
        }
        if inner.has_pending(scope).await? {
            return Err(denied("projection rebuild has unresolved inputs"));
        }
        // The checkpoint rows plus a replay of the operations after them, in
        // application order. Nothing but one batch is held in memory.
        inner.settle(scope).await?;
        transaction!(inner, |inner| {
            let id = inner.scope_id(scope).await?;
            let (mut cursor, mut sets) = match inner.checkpoint_record(id, true).await? {
                Some(record) => {
                    inner.verify_record(scope, &record)?;
                    (record.position, record.sets)
                }
                None => (0, checkpoint::Sets::new()),
            };
            inner
                .conn
                .execute("DELETE FROM secure_checkpoint_scratch WHERE scope=?", [id])
                .await?;
            while let Some(last) = inner
                .replay(
                    scope,
                    id,
                    checkpoint::Overlay::Scratch,
                    (cursor, i64::MAX),
                    checkpoint::STEP_OPERATIONS,
                    &mut sets,
                )
                .await?
            {
                cursor = last;
            }
            let blob = encode(&scope)?;
            inner
                .conn
                .execute("DELETE FROM secure_rows WHERE scope=?", [blob.clone()])
                .await?;
            for table in ["secure_checkpoint_rows", "secure_checkpoint_scratch"] {
                inner
                    .conn
                    .execute(
                        format!("INSERT OR REPLACE INTO secure_rows SELECT ?,row,fields,X'' FROM {table} WHERE scope=?"),
                        bindings(vec![SqlValue::Blob(blob.clone()), SqlValue::Integer(id)]),
                    )
                    .await?;
            }
            inner.reproject(scope).await?;
            inner
                .conn
                .execute("DELETE FROM secure_checkpoint_scratch WHERE scope=?", [id])
                .await?;
            let applied = inner.scope_state(id).await?.3;
            inner
                .store_scope_state(id, sets.rows, sets.tombstones, &sets.digest, applied)
                .await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
            #[cfg(test)]
            if inner.fail_before_commit > 0 {
                inner.fail_before_commit -= 1;
                return Err(Error::InjectedFailure);
            }
            Ok(())
        })?;
        inner.publish_revision().await
    }
}
