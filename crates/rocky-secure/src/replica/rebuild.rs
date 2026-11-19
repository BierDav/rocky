use super::*;

const OPERATION_SCOPE_STATUS_INDEX: &str = "CREATE INDEX IF NOT EXISTS secure_operation_scope_status ON secure_operations(scope,status,hash)";
const OPERATION_HEADERS_INDEX: &str = "CREATE INDEX IF NOT EXISTS secure_operation_headers ON secure_operations(scope,status,hash,header)";
const OPERATION_SCAN_HEADERS_INDEX: &str = "CREATE INDEX IF NOT EXISTS secure_operation_scan_headers ON secure_operations(status,hash,header)";
const OPERATION_AUTHOR_STATUS_INDEX: &str = "CREATE INDEX IF NOT EXISTS secure_operation_author_status ON secure_operations(author,scope,status,sequence)";
pub(super) const SCHEMA: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS secure_operation_scope_hash ON secure_operations(scope,hash)",
    "CREATE INDEX IF NOT EXISTS secure_operation_status_hash ON secure_operations(status,hash)",
    OPERATION_SCOPE_STATUS_INDEX,
    OPERATION_HEADERS_INDEX,
    OPERATION_SCAN_HEADERS_INDEX,
    OPERATION_AUTHOR_STATUS_INDEX,
];

const DELETE_ROWS_DROPPED_BY_FINALIZED_SEAL: &str = "DELETE FROM secure_row_records WHERE scope=? AND row IN (SELECT row FROM secure_checkpoint_scratch WHERE scope=? AND fields=X'')";

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn rebuild_projection(&self, scope: ResourceScope) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        if !inner.can_read_key(scope, &inner.local_grants(now()?)?) {
            return Err(denied("projection rebuild denied"));
        }
        if !inner.config.keys.iter().any(|key| key.scope == scope) {
            return Err(denied("projection rebuild key unavailable"));
        }
        if inner.has_pending(scope).await? {
            return Err(denied("projection rebuild has unresolved inputs"));
        }
        transaction!(inner, |inner| {
            inner
                .rebuild_rows(scope)
                .await?
                .map_err(|broken| broken.refuses(Rebuilding::Projection))?;
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

impl<A: RowApplication> Inner<A> {
    pub(super) async fn rebuild_rows(
        &self,
        scope: ResourceScope,
    ) -> Result<Result<(), replay::BrokenReference>, Error> {
        if self.replayed_scope(scope) {
            if let Err(broken) = self.replay_from_frames(now()?).await? {
                return Ok(Err(broken));
            }
        } else {
            self.restore_rows(scope, i64::MAX).await?;
            self.reproject(scope).await?;
        }
        self.conn
            .execute(
                "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                (),
            )
            .await?;
        Ok(Ok(()))
    }
    pub(super) async fn restore_rows(
        &self,
        scope: ResourceScope,
        through: i64,
    ) -> Result<(), Error> {
        let id = self.scope_id(scope).await?;
        let base = if self.ignores_base() {
            None
        } else {
            self.checkpoint_record(id, true).await?
        };
        let (mut cursor, mut sets, mut ledger) = match base {
            Some(record) => {
                let signed = self.verify_record(scope, &record)?;
                (
                    record.position,
                    record.sets,
                    identity::IdentityLedger::based(&signed.statement.identities)?,
                )
            }
            None => (
                0,
                checkpoint::Sets::new(),
                identity::IdentityLedger::default(),
            ),
        };
        self.conn
            .execute("DELETE FROM secure_checkpoint_scratch WHERE scope=?", [id])
            .await?;
        sets = self
            .prime_path_base(
                scope,
                id,
                checkpoint::Overlay::Scratch,
                self.workspace.tip,
                sets,
            )
            .await?;
        while let Some(last) = self
            .replay_sealed(
                checkpoint::ReplayRequest {
                    scope,
                    id,
                    overlay: checkpoint::Overlay::Scratch,
                    target: self.workspace.tip,
                    after: cursor,
                    through,
                    limit: checkpoint::STEP_OPERATIONS,
                    mode: checkpoint::ReplayMode::Mint,
                },
                &mut sets,
                &mut ledger,
            )
            .await?
        {
            cursor = last;
        }
        let blob = encode(&scope)?;
        self.in_rows_bulk(scope, async {
            self.conn
                .execute("DELETE FROM secure_row_records WHERE scope=?", [blob.clone()])
                .await?;
            for table in ["secure_checkpoint_rows", "secure_checkpoint_scratch"] {
                if self.ignores_base() && table == "secure_checkpoint_rows" {
                    continue;
                }
                self.conn
                    .execute(
                        format!("INSERT OR REPLACE INTO secure_row_records SELECT ?,row,fields FROM {table} WHERE scope=? AND fields<>X''"),
                        bindings(vec![SqlValue::Blob(blob.clone()), SqlValue::Integer(id)]),
                    )
                    .await?;
            }
            self.conn
                .execute(
                    DELETE_ROWS_DROPPED_BY_FINALIZED_SEAL,
                    bindings(vec![SqlValue::Blob(blob.clone()), SqlValue::Integer(id)]),
                )
                .await?;
            self.conn.execute(checkpoint::SHARE_BASE, [id]).await?;
            Ok(())
        })
        .await?;
        self.conn
            .execute("DELETE FROM secure_checkpoint_scratch WHERE scope=?", [id])
            .await?;
        let applied = self.scope_state(id).await?.3;
        self.store_scope_state(id, sets.rows, sets.tombstones, &sets.digest, applied)
            .await?;
        if through == i64::MAX {
            self.replace_ledger(scope, ledger).await?;
        }
        Ok(())
    }
}
