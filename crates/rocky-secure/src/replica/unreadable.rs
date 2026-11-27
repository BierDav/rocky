use super::staging::FRAME_RECORDS;
use super::typed::{Typed, quoted};
use super::*;
use rocky_core::TableClass;

const SCOPE_NUMBERED_TABLES: &str = "SELECT m.name FROM sqlite_schema m JOIN pragma_table_info(m.name) c WHERE m.type='table' AND m.name GLOB 'secure_*' AND c.name='scope' AND c.type='INTEGER' AND m.name<>'secure_scope_state' ORDER BY m.name<>'secure_rows',m.name";

const SCOPE_RECORDS: [&str; 7] = [
    "secure_held_frames",
    "secure_keys",
    "secure_key_epochs",
    "secure_peer_frontiers",
    "secure_retention",
    "secure_subscriptions",
    "secure_rebuild_scopes",
];

const UNSCOPED_RECORDS: [&str; 7] = [
    "secure_held",
    "secure_held_resolved",
    "secure_outcomes",
    "secure_outcome_members",
    "secure_outcome_divergence",
    "secure_kept_outcomes",
    "secure_schema_drops",
];

impl<A: RowApplication> Inner<A> {
    fn proven_readable_scopes(&self) -> Option<BTreeSet<ResourceScope>> {
        let grants = self.local_grants(now().ok()?).ok()?;
        Some(
            grants
                .iter()
                .filter(|grant| {
                    grant.rows == RowSelection::All && grant.rights.contains(&Right::Read)
                })
                .map(|grant| grant.scope)
                .collect(),
        )
    }
    async fn keyed_scopes(&self) -> Result<BTreeSet<ResourceScope>, Error> {
        rows(
            &self.conn,
            "SELECT scope FROM secure_keys UNION SELECT scope FROM secure_key_epochs",
        )
        .await?
        .iter()
        .map(|record| match record.as_slice() {
            [SqlValue::Blob(scope)] => decode(scope),
            _ => Err(Error::Corrupt("key scope".into())),
        })
        .collect()
    }
    async fn no_longer_readable(&self) -> Result<Vec<ResourceScope>, Error> {
        let Some(readable) = self.proven_readable_scopes() else {
            return Ok(Vec::new());
        };
        let declared = |scope: &ResourceScope| {
            self.workspace
                .typed
                .as_ref()
                .is_some_and(|typed| typed.declared_by_scope(*scope).is_some())
        };
        Ok(self
            .keyed_scopes()
            .await?
            .into_iter()
            .filter(|scope| declared(scope) && !readable.contains(scope))
            .collect())
    }
    async fn forget_scope_data(&mut self, scope: ResourceScope) -> Result<Option<i64>, Error> {
        let blob = encode(&scope)?;
        let id = self.find_scope_id(scope).await?;
        if let Some(id) = id
            && self.log(id, self.config.node.public_key()).await?.is_some()
        {
            self.save_restoration_guard(scope).await?;
        }
        if let Some(id) = id {
            self.conn
                .execute(
                    "INSERT INTO secure_kept_outcomes SELECT hash,txn,scope,clock,sequence,dropped,sealed,violated,0 FROM secure_replayed WHERE scope=? ON CONFLICT(hash) DO UPDATE SET dropped=excluded.dropped,sealed=excluded.sealed,violated=excluded.violated WHERE secure_kept_outcomes.based=0",
                    [id],
                )
                .await?;
            let own = self.config.node.public_key().as_bytes().to_vec();
            let unsynced = collect_rows(
                self.conn
                    .query(
                        "SELECT r.hash,r.txn,r.clock FROM secure_replayed r JOIN secure_operations o ON o.hash=r.hash WHERE r.scope=?1 AND (r.author=?2 OR r.author IN (SELECT node FROM secure_line_nodes)) AND r.dropped IS NULL AND NOT EXISTS(SELECT 1 FROM secure_ack a WHERE a.hash=r.hash) AND NOT EXISTS(SELECT 1 FROM secure_peer_frontiers f WHERE f.scope=?3 AND f.author=o.author AND f.held>o.sequence)",
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(own.clone()),
                            SqlValue::Blob(blob.clone()),
                        ]),
                    )
                    .await?,
            )
            .await?;
            for row in unsynced {
                let [
                    SqlValue::Blob(hash),
                    SqlValue::Blob(txn),
                    SqlValue::Integer(clock),
                ] = row.as_slice()
                else {
                    return Err(Error::Corrupt("unsynced revoked write".into()));
                };
                self.conn
                    .execute(
                        "UPDATE secure_kept_outcomes SET dropped='admission',sealed=1,violated=NULL,based=1 WHERE hash=?",
                        [hash.clone()],
                    )
                    .await?;
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO secure_schema_drops VALUES(?,?,?,'admission')",
                        bindings(vec![
                            SqlValue::Blob(txn.clone()),
                            SqlValue::Blob(own.clone()),
                            SqlValue::Integer(*clock),
                        ]),
                    )
                    .await?;
                if let Ok(commitment) = txn.as_slice().try_into() {
                    self.touch_dropped(commitment, rocky_core::replay::DropReason::Admission);
                }
            }
        }
        self.replace_ledger(scope, super::identity::IdentityLedger::default())
            .await?;
        if self.replayed_scope(scope) {
            self.clear_replayed(scope).await?;
        }
        let operations = collect_rows(
            self.conn
                .query(
                    "SELECT hash,LENGTH(header)+LENGTH(body) FROM secure_operations WHERE scope=?",
                    [blob.clone()],
                )
                .await?,
        )
        .await?;
        let mut bytes = 0;
        for record in &operations {
            let [SqlValue::Blob(hash), SqlValue::Integer(size)] = record.as_slice() else {
                return Err(Error::Corrupt("retained operation".into()));
            };
            for table in FRAME_RECORDS {
                if table == "secure_kept_outcomes" {
                    continue;
                }
                self.conn
                    .execute(format!("DELETE FROM {table} WHERE hash=?"), [hash.clone()])
                    .await?;
            }
            bytes += *size;
        }
        let records =
            i64::try_from(operations.len()).map_err(|_| Error::Corrupt("frame count".into()))?;
        self.conn
            .execute(
                "UPDATE secure_retained SET records=records-?,bytes=bytes-?",
                [records, bytes],
            )
            .await?;
        self.clear_applied_cache();
        self.sweep_chains().await?;
        if let Some(id) = id {
            for record in rows(&self.conn, SCOPE_NUMBERED_TABLES).await? {
                let [SqlValue::Text(table)] = record.as_slice() else {
                    return Err(Error::Corrupt("scope numbered table".into()));
                };
                if table == "secure_kept_outcomes" {
                    continue;
                }
                self.conn
                    .execute(format!("DELETE FROM {} WHERE scope=?", quoted(table)), [id])
                    .await?;
            }
            self.store_scope_state(id, 0, 0, &super::checkpoint::LtHash16::new(), 0)
                .await?;
        }
        for table in SCOPE_RECORDS {
            self.conn
                .execute(format!("DELETE FROM {table} WHERE scope=?"), [blob.clone()])
                .await?;
        }
        Ok(id)
    }
    async fn settle_forgotten(
        &mut self,
        scopes: &[ResourceScope],
        ids: Vec<i64>,
    ) -> Result<(), Error> {
        for id in ids {
            self.verified_at.remove(&id);
        }
        self.lose_keys(|key| scopes.contains(&key.scope));
        self.publish_revision().await?;
        self.load_direct().await?;
        let domains: BTreeSet<_> = scopes.iter().map(|scope| scope.domain).collect();
        for domain in domains {
            self.retry_rederive(domain).await?;
        }
        self.scrub().await
    }
    async fn narrowed_projection(
        &self,
        allowed: &BTreeSet<String>,
    ) -> Result<Option<Typed>, Error> {
        let Some(typed) = self.workspace.typed.as_ref() else {
            return Ok(None);
        };
        let kept: BTreeSet<String> = typed
            .tables
            .iter()
            .map(|table| table.table.sql_name.clone())
            .filter(|name| allowed.contains(name))
            .collect();
        if kept.len() == typed.tables.len() {
            return Ok(None);
        }
        let Some((schema, domain, readers)) = self.installed_typed().await? else {
            return Ok(None);
        };
        Ok(Some(Typed::new(schema, domain, &readers)?.restrict(&kept)))
    }
    async fn drop_unreadable_tables(&mut self, narrowed: &Typed) -> Result<(), Error> {
        let Some(typed) = self.workspace.typed.clone() else {
            return Ok(());
        };
        let schema = typed.rows.schema();
        let dropped: Vec<&str> = super::replay::leaf_first(schema.tables.iter().filter(|table| {
            table.class == TableClass::Replayed
                && typed.by_name(&table.sql_name).is_some()
                && narrowed.by_name(&table.sql_name).is_none()
        }))
        .into_iter()
        .map(|table| table.sql_name.as_str())
        .collect();
        for table in dropped {
            self.touch(table);
            self.conn
                .execute(format!("DROP TABLE IF EXISTS {}", quoted(table)), ())
                .await?;
        }
        self.record_projection_access(narrowed).await?;
        self.install_delete_triggers(narrowed).await
    }
    fn leaf_first_scopes(&self, mut scopes: Vec<ResourceScope>) -> Vec<ResourceScope> {
        if let Some(typed) = self.workspace.typed.as_ref() {
            let ranks = super::replay::leaf_first_ranks(typed.rows.schema());
            scopes.sort_by_key(|scope| {
                typed
                    .by_scope(*scope)
                    .and_then(|projected| ranks.get(&projected.table.sql_name.to_ascii_lowercase()))
                    .copied()
                    .unwrap_or(usize::MAX)
            });
        }
        scopes
    }
    pub(super) async fn forget_unreadable(
        &mut self,
        allowed: Option<&BTreeSet<String>>,
    ) -> Result<(), Error> {
        let scopes = self.leaf_first_scopes(self.no_longer_readable().await?);
        let narrowed = match allowed {
            Some(allowed) => self.narrowed_projection(allowed).await?,
            None => None,
        };
        if scopes.is_empty() && narrowed.is_none() {
            return Ok(());
        }
        self.secure_delete(true).await?;
        let forgotten = transaction!(self, |inner| {
            let mut ids = Vec::new();
            for scope in &scopes {
                ids.extend(inner.forget_scope_data(*scope).await?);
            }
            if let Some(narrowed) = &narrowed {
                inner.drop_unreadable_tables(narrowed).await?;
            }
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
            inner.cause(crate::observe::Cause::Authority);
            Ok(ids)
        });
        self.secure_delete(false).await?;
        let ids = forgotten?;
        if let Some(narrowed) = narrowed {
            self.workspace.typed = Some(Arc::new(narrowed));
            self.start_events();
        }
        self.settle_forgotten(&scopes, ids).await
    }
    pub(super) async fn forget_unreadable_by_credentials(&mut self) -> Result<(), Error> {
        let allowed = match self.workspace.typed.clone() {
            Some(typed) => self.readable_names(&typed).ok(),
            None => None,
        };
        self.forget_unreadable(allowed.as_ref()).await
    }
    async fn forget_everything(&mut self) -> Result<(), Error> {
        let scopes: Vec<ResourceScope> = rows(&self.conn, "SELECT scope FROM secure_scope_state")
            .await?
            .iter()
            .map(|record| match record.as_slice() {
                [SqlValue::Blob(scope)] => decode(scope),
                _ => Err(Error::Corrupt("scope state".into())),
            })
            .collect::<Result<_, _>>()?;
        let scopes = self.leaf_first_scopes(scopes);
        let mut ids = Vec::new();
        for scope in &scopes {
            ids.extend(self.forget_scope_data(*scope).await?);
        }
        for table in FRAME_RECORDS.iter().chain(&UNSCOPED_RECORDS) {
            self.conn
                .execute(format!("DELETE FROM {table}"), ())
                .await?;
        }
        for table in ["secure_keys", "secure_key_epochs", "secure_partitions"] {
            self.conn
                .execute(format!("DELETE FROM {table}"), ())
                .await?;
        }
        self.partitions.clear();
        self.partition_names.clear();
        self.conn
            .execute("UPDATE secure_retained SET records=0,bytes=0", ())
            .await?;
        if let Some(typed) = self.workspace.typed.clone() {
            for table in super::replay::leaf_first(&typed.rows.schema().tables) {
                if typed.installs(&table.sql_name) {
                    self.touch(&table.sql_name);
                    self.conn
                        .execute(format!("DELETE FROM {}", quoted(&table.sql_name)), ())
                        .await?;
                }
            }
        }
        self.conn
            .execute(
                "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                (),
            )
            .await?;
        self.cause(crate::observe::Cause::Authority);
        for id in ids {
            self.verified_at.remove(&id);
        }
        Ok(())
    }
    async fn secure_delete(&self, on: bool) -> Result<(), Error> {
        let mode = if on { "ON" } else { "OFF" };
        rows(&self.conn, &format!("PRAGMA secure_delete={mode}")).await?;
        Ok(())
    }
    async fn scrub(&mut self) -> Result<(), Error> {
        rows(&self.conn, "PRAGMA incremental_vacuum").await?;
        self.truncate_wal(SCRUB_CHECKPOINT_WAIT).await;
        Ok(())
    }
}

const SCRUB_CHECKPOINT_WAIT: std::time::Duration = std::time::Duration::from_millis(200);

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn forget_everything(&self) -> Result<(), Error> {
        self.forget_everything_and_record(&[], 0).await
    }
    pub async fn forget_everything_and_record(
        &self,
        entries: &[(&str, Option<&[u8]>)],
        updated_at: u64,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.lose_keys(|_| true);
        inner.secure_delete(true).await?;
        let forgotten = transaction!(inner, |inner| {
            inner.forget_everything().await?;
            if !entries.is_empty() {
                super::authority::apply_entries(&inner.conn, entries, updated_at).await?;
            }
            Ok(())
        });
        inner.secure_delete(false).await?;
        forgotten?;
        inner.publish_revision().await?;
        inner.load_direct().await?;
        inner.scrub().await
    }
}
