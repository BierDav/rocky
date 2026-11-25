use super::*;

pub(in crate::replica) const FRAME_RECORDS: [&str; 12] = [
    "secure_migration_frames",
    "secure_publication_frames",
    "secure_publication_unsent",
    "secure_replayed",
    "secure_kept_outcomes",
    "secure_unoffered",
    "secure_held_frames",
    "secure_voids",
    "secure_outbox",
    "secure_ack",
    "secure_applied",
    "secure_operations",
];

const OUTCOME_RECORDS: [&str; 3] = [
    "secure_outcomes",
    "secure_outcome_members",
    "secure_outcome_divergence",
];

const WRITTEN_SCOPE_RECORDS: [&str; 10] = [
    "secure_identity_partitions",
    "secure_identity_ledger",
    "secure_identity_summary",
    "secure_identity_own",
    "secure_identity_marks",
    "secure_position_sources",
    "secure_position_ranges",
    "secure_log_ranges",
    "secure_move_rows",
    "secure_own_logs",
];

const SCOPE_STATE_KEYED_TABLES: &str = "SELECT m.name FROM sqlite_schema m JOIN pragma_table_info(m.name) c WHERE m.type='table' AND m.name GLOB 'secure_*' AND c.name='scope' AND c.type='INTEGER' ORDER BY m.name";

struct FirstDiscarded {
    sequence: u64,
    backlink: Option<Hash>,
    clock: u64,
}

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn discard_tip_writes(&self, tip: TipId) -> Result<(), Error> {
        let written = self.frames_on_tip(tip).await?;
        self.forget_handles_of(&written).await?;
        let transactions = self.transactions_of(&written).await?;
        let logs = self.forget_written_frames(&written).await?;
        self.forget_outcomes(&transactions).await?;
        for ((scope, author), first) in logs {
            if let Some(id) = self.find_scope_id(scope).await? {
                self.rewind_log(id, author, first).await?;
            }
        }
        Ok(())
    }
    pub(in crate::replica) async fn forget_undeclared_scopes(
        &self,
        domain: Scope,
    ) -> Result<(), Error> {
        for record in rows(
            &self.conn,
            "SELECT id,scope FROM secure_scope_state ORDER BY id",
        )
        .await?
        {
            let [SqlValue::Integer(id), SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("scope state".into()));
            };
            let scope: ResourceScope = decode(bytes)?;
            if scope.domain != domain
                || self.declares(scope)
                || self.scope_keeps_history(*id, bytes).await?
            {
                continue;
            }
            self.forget_scope(*id, bytes).await?;
        }
        self.invalidate_context();
        Ok(())
    }
    fn declares(&self, scope: ResourceScope) -> bool {
        self.tables.contains(&scope.table)
            && self
                .workspace
                .typed
                .as_ref()
                .is_none_or(|typed| typed.declared_by_scope(scope).is_some())
    }
    async fn scope_keeps_history(&self, id: i64, scope: &[u8]) -> Result<bool, Error> {
        Ok(!collect_rows(
            self.conn
                .query(
                    "SELECT 1 FROM secure_operations WHERE scope=?1 UNION ALL SELECT 1 FROM secure_stubs WHERE scope=?2 UNION ALL SELECT 1 FROM secure_checkpoints WHERE scope=?2 LIMIT 1",
                    bindings(vec![SqlValue::Blob(scope.to_vec()), SqlValue::Integer(id)]),
                )
                .await?,
        )
        .await?
        .is_empty())
    }
    async fn forget_scope(&self, id: i64, scope: &[u8]) -> Result<(), Error> {
        for record in rows(&self.conn, SCOPE_STATE_KEYED_TABLES).await? {
            let [SqlValue::Text(table)] = record.as_slice() else {
                return Err(Error::Corrupt("scope keyed table".into()));
            };
            self.conn
                .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                .await?;
        }
        for table in WRITTEN_SCOPE_RECORDS {
            self.conn
                .execute(
                    format!("DELETE FROM {table} WHERE scope=?"),
                    [scope.to_vec()],
                )
                .await?;
        }
        self.conn
            .execute(
                "DELETE FROM secure_identity_tables WHERE scope=? AND NOT EXISTS(SELECT 1 FROM secure_identity_partitions p WHERE p.allocation=secure_identity_tables.id)",
                [scope.to_vec()],
            )
            .await?;
        self.conn
            .execute("DELETE FROM secure_scope_state WHERE id=?", [id])
            .await?;
        Ok(())
    }
    async fn frames_on_tip(&self, tip: TipId) -> Result<Vec<Vec<u8>>, Error> {
        let mut found = Vec::new();
        let mut after = Vec::new();
        loop {
            let page = collect_rows(
                self.conn
                    .query(
                        "SELECT hash,header FROM secure_operations WHERE hash>? ORDER BY hash LIMIT 256",
                        [after.clone()],
                    )
                    .await?,
            )
            .await?;
            for record in &page {
                let [SqlValue::Blob(hash), SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(Error::Corrupt("tip operation".into()));
                };
                after = hash.clone();
                if decode::<p2panda_core::Header<Extensions>>(bytes)?
                    .extensions
                    .tip
                    == tip
                {
                    found.push(hash.clone());
                }
            }
            if page.len() < 256 {
                return Ok(found);
            }
        }
    }
    async fn transactions_of(&self, hashes: &[Vec<u8>]) -> Result<BTreeSet<Vec<u8>>, Error> {
        let mut transactions = BTreeSet::new();
        for hash in hashes {
            for record in collect_rows(
                self.conn
                    .query(
                        "SELECT txn FROM secure_replayed WHERE hash=?1 UNION SELECT txn FROM secure_kept_outcomes WHERE hash=?1",
                        [hash.clone()],
                    )
                    .await?,
            )
            .await?
            {
                let [SqlValue::Blob(transaction)] = record.as_slice() else {
                    return Err(Error::Corrupt("replayed transaction".into()));
                };
                transactions.insert(transaction.clone());
            }
        }
        Ok(transactions)
    }
    async fn forget_outcomes(&self, transactions: &BTreeSet<Vec<u8>>) -> Result<(), Error> {
        for transaction in transactions {
            let kept = !collect_rows(
                self.conn
                    .query(
                        "SELECT 1 FROM secure_replayed WHERE txn=?1 UNION ALL SELECT 1 FROM secure_kept_outcomes WHERE txn=?1 LIMIT 1",
                        [transaction.clone()],
                    )
                    .await?,
            )
            .await?
            .is_empty();
            if kept {
                continue;
            }
            for table in OUTCOME_RECORDS {
                self.conn
                    .execute(
                        format!("DELETE FROM {table} WHERE txn=?"),
                        [transaction.clone()],
                    )
                    .await?;
            }
        }
        Ok(())
    }
    async fn forget_written_frames(
        &self,
        hashes: &[Vec<u8>],
    ) -> Result<BTreeMap<(ResourceScope, PublicKey), FirstDiscarded>, Error> {
        let mut logs: BTreeMap<(ResourceScope, PublicKey), FirstDiscarded> = BTreeMap::new();
        let mut bytes = 0;
        for hash in hashes {
            let found = collect_rows(
                self.conn
                    .query(
                        "SELECT header,LENGTH(header)+LENGTH(body) FROM secure_operations WHERE hash=?",
                        [hash.clone()],
                    )
                    .await?,
            )
            .await?;
            let Some([SqlValue::Blob(header), SqlValue::Integer(size)]) =
                found.first().map(Vec::as_slice)
            else {
                return Err(Error::Corrupt("discarded frame".into()));
            };
            bytes += size;
            let header: p2panda_core::Header<Extensions> = decode(header)?;
            let discarded = FirstDiscarded {
                sequence: header.seq_num,
                backlink: header.backlink,
                clock: header.timestamp,
            };
            match logs.entry((header.extensions.scope, header.public_key)) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(discarded);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if discarded.sequence < entry.get().sequence {
                        entry.insert(discarded);
                    }
                }
            }
            for table in FRAME_RECORDS {
                self.conn
                    .execute(format!("DELETE FROM {table} WHERE hash=?"), [hash.clone()])
                    .await?;
            }
        }
        self.conn
            .execute(
                "UPDATE secure_retained SET records=records-?,bytes=bytes-?",
                [
                    i64::try_from(hashes.len()).map_err(|_| denied("discarded frames"))?,
                    bytes,
                ],
            )
            .await?;
        Ok(logs)
    }
    async fn rewind_log(
        &self,
        id: i64,
        author: PublicKey,
        first: FirstDiscarded,
    ) -> Result<(), Error> {
        let author = author.as_bytes().to_vec();
        let sequence = SqlValue::Integer(
            i64::try_from(first.sequence).map_err(|_| Error::Corrupt("log sequence".into()))?,
        );
        let Some(backlink) = first.backlink else {
            self.conn
                .execute(
                    "DELETE FROM secure_logs WHERE scope=? AND author=? AND sequence>=?",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(author),
                        sequence,
                    ]),
                )
                .await?;
            return Ok(());
        };
        let clock = match self.frame(backlink).await? {
            Some(frame) => frame.operation_ref()?.header.timestamp,
            None => first.clock,
        };
        self.conn
            .execute(
                "UPDATE secure_logs SET sequence=?-1,hash=?,clock=? WHERE scope=? AND author=? AND sequence>=?",
                bindings(vec![
                    sequence.clone(),
                    SqlValue::Blob(backlink.as_bytes().to_vec()),
                    clock_value(clock)?,
                    SqlValue::Integer(id),
                    SqlValue::Blob(author),
                    sequence,
                ]),
            )
            .await?;
        Ok(())
    }
}
