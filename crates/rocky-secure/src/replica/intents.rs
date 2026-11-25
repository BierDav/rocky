use super::*;
use rocky_core::TableClass;
use rocky_core::replay::{BaseMark, IntentStatement};
use rocky_storage::partition_sql::{IntentRefusal, SplitError, split_intent_within, written_table};

#[derive(Clone, Debug)]
pub(crate) struct Recorded {
    pub scope: ResourceScope,
    pub statement: IntentStatement,
}

impl<A: RowApplication> Inner<A> {
    pub(super) fn generated_tables(
        &self,
    ) -> Option<(
        Vec<rocky_core::LogicalView>,
        BTreeMap<String, rocky_storage::partition_sql::GeneratedTable>,
    )> {
        let schema = self.workspace.typed.as_ref()?.rows.schema();
        let generated = schema
            .tables
            .iter()
            .filter(|table| table.class == TableClass::Replayed && table.generated_id())
            .map(|table| {
                (
                    table.sql_name.to_ascii_lowercase(),
                    rocky_storage::partition_sql::GeneratedTable {
                        partition_by: table.partition_by.clone(),
                        insertable: table
                            .columns
                            .iter()
                            .filter(|column| {
                                column.class == rocky_core::ColumnClass::Replicated
                                    && !column
                                        .sql_name
                                        .eq_ignore_ascii_case(rocky_core::GENERATED_ID)
                            })
                            .map(|column| column.sql_name.clone())
                            .collect(),
                    },
                )
            })
            .collect();
        Some((schema.views.clone(), generated))
    }
    pub(super) fn generates_ids(&self, sql: &str, params: &[SqlValue]) -> Result<bool, Error> {
        let Some((views, generated)) = self.generated_tables() else {
            return Ok(false);
        };
        match rocky_storage::partition_sql::fill_generated_ids(
            sql,
            params,
            &views,
            |name| generated.get(name).cloned(),
            |_| Ok::<_, Error>(Vec::new()),
        ) {
            Ok(filled) => Ok(filled.is_some()),
            Err(SplitError::Refused(refusal)) => Err(Error::Sql(SqlError::Refused(refusal))),
            Err(SplitError::Evaluate(error)) => Err(error),
        }
    }
    pub(super) fn check_key_writes(&self, sql: &str) -> Result<(), IntentRefusal> {
        let Some(typed) = &self.workspace.typed else {
            return Ok(());
        };
        let schema = typed.rows.schema();
        rocky_storage::partition_sql::check_key_writes(sql, &schema.views, |name| {
            schema
                .table(name)
                .filter(|table| table.class == TableClass::Replayed)
                .map(|table| table.primary_key.clone())
        })
    }
    pub(super) async fn intent_copies(
        &self,
        sql: &str,
        params: &[SqlValue],
        status: &SessionStatus,
        limit: usize,
        parents: &BTreeMap<String, String>,
    ) -> Result<Option<Vec<Recorded>>, Error> {
        let Some(typed) = &self.workspace.typed else {
            return Ok(None);
        };
        let schema = typed.rows.schema();
        let replayed = |name: &str| {
            schema
                .table(name)
                .is_some_and(|table| table.class == TableClass::Replayed)
        };
        let name = match written_table(sql, &schema.views) {
            Some(name) if replayed(&name) => name,
            Some(_) => return Ok(None),
            None => match crate::statement::analyze(sql, |_| None) {
                Ok(analyzed)
                    if analyzed.kind != crate::statement::Kind::Read
                        && analyzed.tables.iter().any(|name| replayed(name)) =>
                {
                    String::new()
                }
                _ => return Ok(None),
            },
        };
        self.check_key_writes(sql)
            .map_err(|refusal| Error::Sql(SqlError::Refused(refusal)))?;
        let rules = self.partition_rules(None, status).await?;
        let conn = self.conn.connection();
        let split = split_intent_within(sql, params, &schema.views, &rules, limit, |read| {
            let sql = match parents.is_empty() {
                true => read.sql.clone(),
                false => {
                    crate::statement::analyze_migration(&read.sql, |name| {
                        parents.get(name).cloned()
                    })
                    .map_err(|rejection| Error::from(crate::sqlite::Error::from(rejection)))?
                    .sql
                }
            };
            conn.rows(&sql, storage::bindings(read.params.clone()))?
                .into_iter()
                .map(|row| row.into_iter().map(storage::sql_value).collect())
                .collect::<Result<Vec<Vec<_>>, Error>>()
        });
        let copies = match split {
            Ok(copies) => copies,
            Err(SplitError::Evaluate(error)) => return Err(error),
            Err(SplitError::Refused(refusal)) => {
                if let IntentRefusal::Unavailable {
                    table,
                    reason: crate::sql::Unavailable::Incomplete { .. },
                } = refusal
                {
                    return Err(self.incomplete(typed, table).await);
                }
                return Err(Error::Sql(SqlError::Refused(refusal)));
            }
        };
        let logical = schema
            .table(&name)
            .ok_or_else(|| Error::Corrupt("replayed write target".into()))?;
        let projected = typed
            .by_name(&logical.sql_name)
            .ok_or_else(|| Error::Corrupt(format!("{} is not typed", logical.sql_name)))?;
        let mut scopes = Vec::new();
        if logical.partition_by.is_empty() {
            scopes.push((Vec::new(), projected.scope));
        } else {
            for (scope, tuple) in self.partitions.iter().filter(|(scope, _)| {
                scope.table == projected.table.id && scope.domain == typed.domain
            }) {
                scopes.push((
                    super::partition::partition_values(schema, logical, tuple)?,
                    *scope,
                ));
            }
        }
        copies
            .into_iter()
            .map(|copy| {
                let scope = scopes
                    .iter()
                    .find(|(values, _)| *values == copy.partition)
                    .map(|(_, scope)| *scope)
                    .ok_or_else(|| Error::Corrupt("split partition has no scope".into()))?;
                if !status.writable.contains(&scope) {
                    return Err(Error::Sql(SqlError::Denied));
                }
                let statement = IntentStatement {
                    sql: copy.statement.sql,
                    params: copy.statement.params,
                };
                let size = crate::encode(&statement)?.len();
                if size > crate::network::MAX_PAGE_BYTES {
                    return Err(Error::TooLarge {
                        limit: rocky_storage::Limit::Intent,
                        size,
                        max: crate::network::MAX_PAGE_BYTES,
                    });
                }
                Ok(Recorded { scope, statement })
            })
            .collect::<Result<_, _>>()
            .map(Some)
    }

    pub(super) async fn author_intent_frame(
        &self,
        log: (ResourceScope, Option<checkpoint::Log>),
        time: u64,
        payload: impl FnOnce(u64, Vec<BaseMark>) -> Result<Vec<u8>, Error>,
    ) -> Result<EncryptedFrame, Error> {
        let heads = self.applied_heads(log.0).await?;
        self.author_log_frame(log, time, heads, payload).await
    }

    pub(super) async fn author_outcome_frame(
        &self,
        log: (ResourceScope, Option<checkpoint::Log>),
        time: u64,
        record: &rocky_core::replay::OutcomeRecord,
    ) -> Result<EncryptedFrame, Error> {
        let names_only_its_own_log = Vec::new();
        self.author_log_frame(log, time, names_only_its_own_log, |_, _| {
            RowPayload::outcome_record(record.clone())
        })
        .await
    }

    async fn author_log_frame(
        &self,
        (scope, tip): (ResourceScope, Option<checkpoint::Log>),
        time: u64,
        heads: Vec<(Hash, p2panda_core::Header<Extensions>)>,
        payload: impl FnOnce(u64, Vec<BaseMark>) -> Result<Vec<u8>, Error>,
    ) -> Result<EncryptedFrame, Error> {
        self.ensure_tip_writable(scope.domain)?;
        let partition = self.authenticated_partition(scope)?.cloned();
        let signer = self
            .node_author
            .as_ref()
            .ok_or_else(|| denied("a replayed table's log needs a table author"))?;
        let signing_key = &signer.key;
        let verified = self.verified_chain(&signer.credentials, &self.config.revocations, time)?;
        match &partition {
            Some(tuple) => verified
                .partition_author(
                    signing_key.public_key(),
                    scope,
                    tuple,
                    &self.config.revocations,
                    time,
                )
                .map_err(credential)?,
            None => verified
                .author(
                    signing_key.public_key(),
                    scope.table,
                    &self.config.revocations,
                    time,
                )
                .map_err(credential)?,
        };
        let key = self
            .config
            .keys
            .iter()
            .find(|key| key.scope == scope)
            .ok_or_else(|| denied("author needs an authorized row key"))?;
        self.admit_author(scope, signing_key.public_key()).await?;
        let (sequence, backlink, mut clock) = match tip {
            Some(log) => (
                (log.sequence as u64)
                    .checked_add(1)
                    .filter(|seq| *seq <= i64::MAX as u64)
                    .ok_or_else(|| denied("author counter exhausted"))?,
                Some(log.hash),
                log.clock,
            ),
            None => (0, None, 0),
        };
        if heads.len() > 64 {
            return Err(denied("dependency quota"));
        }
        let mut base = Vec::with_capacity(heads.len());
        let mut previous = BTreeSet::new();
        for (hash, header) in &heads {
            base.push(BaseMark {
                author: header
                    .extensions
                    .author(&header.public_key)
                    .ok_or_else(|| Error::Corrupt("intent base author".into()))?,
                sequence: header.extensions.author_sequence,
            });
            clock = clock.max(header.timestamp);
            if Some(*hash) != backlink {
                previous.insert(*hash);
            }
        }
        base.sort();
        let latest_replayed_clock = {
            let cached = *self
                .latest_replayed_clock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match cached {
                Some(clock) => clock,
                None => {
                    let queried = match collect_rows(
                        self.conn
                            .query("SELECT COALESCE(MAX(clock),0) FROM secure_replayed", ())
                            .await?,
                    )
                    .await?
                    .first()
                    .map(Vec::as_slice)
                    {
                        Some([SqlValue::Integer(clock)]) => u64::try_from(*clock).ok(),
                        _ => None,
                    }
                    .ok_or_else(|| Error::Corrupt("replayed clock".into()))?;
                    *self
                        .latest_replayed_clock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(queried);
                    queried
                }
            }
        };
        let clock = next_clock(clock.max(latest_replayed_clock))?;
        let author_sequence = self
            .reserve_author_sequence(signing_key.public_key())
            .await?;
        let encoded = payload(sequence, base)?;
        if encoded.len() > crate::network::MAX_PAGE_BYTES {
            return Err(Error::TooLarge {
                limit: rocky_storage::Limit::Intent,
                size: encoded.len(),
                max: crate::network::MAX_PAGE_BYTES,
            });
        }
        let witnessed = RowPayload::witness(encoded, Vec::new())?;
        let compacted = RowPayload::compact(&witnessed, &self.definition, scope)?;
        let opened_payload = RowPayload::decode_source(&compacted, &self.definition, scope)?;
        let frame = author(
            signing_key,
            crate::wire::FrameDraft {
                extensions: Extensions {
                    authored: true,
                    author_sequence,
                    row_format: ROW_FORMAT,
                    births: Vec::new(),
                    tip: self.workspace.tip,
                    scope,
                    generation: key.generation,
                    chain: signer.chain.digest(),
                },
                chain: signer.chain.clone(),
                encryption_key: &key.key,
                log: (sequence, backlink, clock),
                previous: previous.into_iter().collect(),
                partition: partition.as_ref(),
                plaintext: &compacted,
            },
        )?;
        if frame.weight() > crate::network::MAX_PAGE_BYTES {
            return Err(Error::TooLarge {
                limit: rocky_storage::Limit::Intent,
                size: frame.weight(),
                max: crate::network::MAX_PAGE_BYTES,
            });
        }
        self.verify_frame(&frame, time)?;
        let operation = frame.operation_ref()?;
        self.check_budget(
            usize::try_from(frame.stored_bytes()?).map_err(|_| denied("stored frame size"))?,
        )
        .await?;
        self.keep_chain(frame.chain()?).await?;
        self.no_frames_intake(&operation.header).await?;
        let inserted = TransactionStore(self.conn.clone())
            .insert_applied_operation(&frame, &scope)
            .await?;
        self.write_materialized_status(
            &frame,
            Vec::new(),
            Default::default(),
            Some(&opened_payload),
            inserted,
        )
        .await?;
        self.conn
            .execute(
                "INSERT INTO secure_outbox VALUES(?)",
                [operation.hash.as_bytes().to_vec()],
            )
            .await?;
        self.conn.execute("INSERT INTO secure_node_tips VALUES(?,?,?) ON CONFLICT(scope,node) DO UPDATE SET tip=excluded.tip", [encode(&scope.domain)?, signing_key.public_key().as_bytes().to_vec(), encode(&self.workspace.tip)?]).await?;
        self.record_key(key).await?;
        Ok(frame)
    }

    pub(super) async fn refuse_intent_blockers(&self, scope: ResourceScope) -> Result<(), Error> {
        let mut blocked = self.scope_blockers(scope).await?;
        blocked.extend(self.line_node_blockers()?);
        blockers::refuse(blocked)
    }

    pub(super) fn note_applied_head(
        &self,
        scope: ResourceScope,
        hash: Hash,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<(), Error> {
        let mut cache = self
            .applied_heads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((tip, heads)) = cache.get_mut(&scope) else {
            return Ok(());
        };
        if *tip != self.workspace.tip {
            cache.remove(&scope);
            return Ok(());
        }
        let Ok(graph) = self.graph_for(scope.domain) else {
            cache.remove(&scope);
            return Ok(());
        };
        let extension = &header.extensions;
        let visible = match extension.author(&header.public_key) {
            Some(author) => match self.visible_frame(
                graph,
                extension.tip,
                author,
                extension.author_sequence,
                self.workspace.tip,
            ) {
                Ok(visible) => visible,
                Err(_) => {
                    cache.remove(&scope);
                    return Ok(());
                }
            },
            None => false,
        };
        if !visible {
            cache.remove(&scope);
            return Ok(());
        }
        if let Some(entry) = heads
            .iter_mut()
            .find(|(_, existing)| existing.public_key == header.public_key)
        {
            if header.seq_num >= entry.1.seq_num {
                *entry = (hash, header.clone());
            }
        } else {
            heads.push((hash, header.clone()));
        }
        Ok(())
    }

    async fn applied_heads(
        &self,
        scope: ResourceScope,
    ) -> Result<Vec<(Hash, p2panda_core::Header<Extensions>)>, Error> {
        if let Some((tip, heads)) = self
            .applied_heads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&scope)
            && *tip == self.workspace.tip
        {
            return Ok(heads.clone());
        }
        let corrupt = || Error::Corrupt("applied head".into());
        let decoded =
            |row: &[SqlValue]| -> Result<(Hash, p2panda_core::Header<Extensions>), Error> {
                let [SqlValue::Blob(hash), SqlValue::Blob(header)] = row else {
                    return Err(corrupt());
                };
                Ok((
                    Hash::try_from(hash.as_slice()).map_err(|_| corrupt())?,
                    decode(header)?,
                ))
            };
        let graph = self.graph_for(scope.domain)?;
        let visible = |header: &p2panda_core::Header<Extensions>| -> Result<bool, Error> {
            let extension = &header.extensions;
            self.visible_frame(
                graph,
                extension.tip,
                extension.author(&header.public_key).ok_or_else(corrupt)?,
                extension.author_sequence,
                self.workspace.tip,
            )
        };
        let mut heads = Vec::new();
        for row in collect_rows(
            self.conn
                .query(
                    "SELECT o.hash,o.header FROM secure_operations o WHERE o.scope=? AND o.status='applied' AND o.sequence=(SELECT MAX(sequence) FROM secure_operations WHERE scope=o.scope AND author=o.author AND status='applied')",
                    [encode(&scope)?],
                )
                .await?,
        )
        .await?
        {
            let (hash, header) = decoded(&row)?;
            if visible(&header)? {
                heads.push((hash, header));
                continue;
            }
            let mut before = header.seq_num as i64;
            'author: loop {
                let older = collect_rows(
                    self.conn
                        .query(
                            "SELECT hash,header FROM secure_operations WHERE scope=? AND author=? AND status='applied' AND sequence<? ORDER BY sequence DESC LIMIT 64",
                            bindings(vec![
                                SqlValue::Blob(encode(&scope)?),
                                SqlValue::Blob(header.public_key.as_bytes().to_vec()),
                                SqlValue::Integer(before),
                            ]),
                        )
                        .await?,
                )
                .await?;
                if older.is_empty() {
                    break;
                }
                for row in &older {
                    let (hash, header) = decoded(row)?;
                    before = header.seq_num as i64;
                    if visible(&header)? {
                        heads.push((hash, header));
                        break 'author;
                    }
                }
            }
        }
        self.applied_heads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(scope, (self.workspace.tip, heads.clone()));
        Ok(heads)
    }
}

pub(super) fn grouped(
    recorded: &[Recorded],
) -> Result<BTreeMap<ResourceScope, Vec<IntentStatement>>, Error> {
    let mut intents = BTreeMap::<ResourceScope, Vec<IntentStatement>>::new();
    for recorded in recorded {
        intents
            .entry(recorded.scope)
            .or_default()
            .push(recorded.statement.clone());
    }
    if intents
        .values()
        .any(|statements| statements.len() > rocky_core::replay::MAX_INTENT_STATEMENTS)
    {
        return Err(Error::Quota {
            resource: "intent statements",
            limit: rocky_core::replay::MAX_INTENT_STATEMENTS,
        });
    }
    Ok(intents)
}
