use super::*;

impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn replay_injected_frame_for_test(
        &self,
        scope: ResourceScope,
        frame: &EncryptedFrame,
        filtered: Option<usize>,
    ) -> Result<u64, Error> {
        let inner = self.inner.lock().await;
        let operation = frame.operation_ref()?;
        // The malformed envelope must reach replay after a successful opening.
        let payload = inner.row_payload(frame)?;
        if let Some(ordinal) = filtered {
            let fields = inner.path_fields(scope.domain, operation.header.extensions.tip)?;
            assert!(fields.current(&payload.writes()[ordinal]).is_none());
        }
        inner.conn.execute("SAVEPOINT replay_boundary", ()).await?;
        let result = async {
            let id = inner.scope_id(scope).await?;
            inner.conn.execute(
                "INSERT INTO secure_operations(hash,author,scope,sequence,header,body,status,chain) VALUES(?,?,?,?,?,?,'applied',?)",
                bindings(vec![
                    SqlValue::Blob(operation.hash.as_bytes().to_vec()),
                    SqlValue::Blob(operation.header.public_key.as_bytes().to_vec()),
                    SqlValue::Blob(encode(&operation.header.extensions.scope)?),
                    SqlValue::Integer(operation.header.seq_num as i64),
                    SqlValue::Blob(frame.header_bytes().to_vec()),
                    SqlValue::Blob(frame.body_bytes().to_vec()),
                    SqlValue::Blob(operation.header.extensions.chain.to_vec()),
                ]),
            ).await?;
            inner.conn.execute("INSERT INTO secure_applied(scope,hash) VALUES(?,?)", bindings(vec![SqlValue::Integer(id),SqlValue::Blob(operation.hash.as_bytes().to_vec())])).await?;
            let position = match rows(&inner.conn, "SELECT max(position) FROM secure_applied").await?[0][0] {
                SqlValue::Integer(position) => position,
                _ => panic!("applied position"),
            };
            inner.conn.execute(
                "INSERT INTO secure_checkpoint_scratch SELECT ?,row,fields FROM secure_rows WHERE scope=?",
                bindings(vec![SqlValue::Integer(id), SqlValue::Blob(encode(&scope)?)]),
            ).await?;
            let mut sets = Sets::new();
            for record in rows(&inner.conn, "SELECT row,fields FROM secure_checkpoint_scratch").await? {
                let [SqlValue::Blob(row), SqlValue::Blob(fields)] = record.as_slice() else {
                    panic!("scratch row")
                };
                sets.replace(&row_of(row)?, None, fields)?;
            }
            let tables = [
                "secure_checkpoint_scratch", "secure_rows", "secure_identity_ledger",
                "secure_identity_marks", "secure_identity_own", "secure_identity_summary",
                "secure_admission_context", "secure_admission_entries", "secure_admission_claims",
                "secure_admission_carries", "secure_metadata",
            ];
            let mut before = Vec::new();
            for table in tables {
                before.push(rows(&inner.conn, &format!("SELECT * FROM {table} ORDER BY 1")).await?);
            }
            let original_sets = sets.to_bytes();
            let mut ledger = identity::IdentityLedger::default();
            let result = inner.replay(scope, id, Overlay::Scratch, inner.workspace.tip,
                (position - 1, position), 1, &mut sets, &mut ledger, ReplayMode::Check).await;
            if result.is_err() {
                assert_eq!(sets.to_bytes(), original_sets);
                assert!(ledger.into_marks().is_empty());
                for (table, expected) in tables.into_iter().zip(before) {
                    assert_eq!(rows(&inner.conn, &format!("SELECT * FROM {table} ORDER BY 1")).await?, expected, "{table}");
                }
            }
            assert_eq!(result?, Some(position));
            Ok(rows(&inner.conn, "SELECT row FROM secure_checkpoint_scratch").await?.len() as u64)
        }.await;
        inner
            .conn
            .execute("ROLLBACK TO replay_boundary", ())
            .await?;
        inner.conn.execute("RELEASE replay_boundary", ()).await?;
        result
    }
}
