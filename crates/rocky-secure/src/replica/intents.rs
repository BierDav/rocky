//! Authoring writes to replayed tables: each statement is split per held
//! partition on the snapshot it executes on, and the transaction publishes
//! the copies as intents instead of row images.
use super::*;
use rocky_core::TableClass;
use rocky_core::replay::{BaseMark, Intent, IntentStatement};
use rocky_storage::partition_sql::{SplitError, split_intent, written_table};

/// One partition's copy of a statement the transaction ran.
#[derive(Clone, Debug)]
pub(crate) struct Recorded {
    pub scope: ResourceScope,
    pub statement: IntentStatement,
}

impl<A: RowApplication> Inner<A> {
    /// The copies of a write to a replayed table, one per held partition it
    /// reaches, split before it executes. `None` when it writes no replayed
    /// table; empty when it reaches no held partition.
    pub(super) async fn intent_copies(
        &self,
        sql: &str,
        params: &[SqlValue],
        status: &SessionStatus,
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
            // A write the splitter cannot name gets its refusal below when it
            // names a replayed table.
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
        let rules = self.partition_rules(None, status).await?;
        let conn = self.conn.connection();
        let copies = split_intent(sql, params, &schema.views, &rules, |read| {
            conn.rows(&read.sql, storage::bindings(read.params.clone()))?
                .into_iter()
                .map(|row| row.into_iter().map(storage::sql_value).collect())
                .collect::<Result<Vec<Vec<_>>, Error>>()
        })
        .map_err(|error| match error {
            SplitError::Refused(refusal) => Error::Sql(SqlError::Unsupported(format!(
                "a replayed write was refused: {refusal}"
            ))),
            SplitError::Evaluate(error) => error,
        })?;
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
                Ok(Recorded {
                    scope,
                    statement: IntentStatement {
                        sql: copy.statement.sql,
                        params: copy.statement.params,
                    },
                })
            })
            .collect::<Result<_, _>>()
            .map(Some)
    }

    /// Signs and stores one bundle member carrying `statements` for `scope`.
    /// The rows are already in the SQL table: the author's execution is its
    /// local result. The frame follows every author's last applied frame in
    /// the scope, which is the base it names; `tip` is the author's own log
    /// tip there.
    pub(super) async fn author_intent_frame(
        &self,
        (scope, tip): (ResourceScope, Option<checkpoint::Log>),
        statements: Vec<IntentStatement>,
        time: u64,
        payload: impl FnOnce(u64, Intent) -> Result<Vec<u8>, Error>,
    ) -> Result<EncryptedFrame, Error> {
        self.ensure_tip_writable(scope.domain)?;
        let mut blocked = self.scope_blockers(scope).await?;
        blocked.extend(self.dropped_tip_blockers().await?);
        blocked.extend(self.line_node_blockers()?);
        blockers::refuse(blocked)?;
        let partition = self.partitions.get(&scope).cloned();
        let signer = self
            .node_author
            .as_ref()
            .ok_or_else(|| denied("a replayed write needs a table author"))?;
        let signing_key = &signer.key;
        match &partition {
            Some(tuple) => signer
                .credentials
                .verify_partition_author(
                    self.config.publisher,
                    signing_key.public_key(),
                    signer.number,
                    scope,
                    tuple,
                    &self.config.revocations,
                    time,
                )
                .map_err(credential)?,
            None => signer
                .credentials
                .verify_author(
                    self.config.publisher,
                    signing_key.public_key(),
                    signer.number,
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
        self.admit_author(scope, tip.as_ref().map(|log| log.sequence))
            .await?;
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
        let heads = self.applied_heads(scope).await?;
        if heads.len() > 64 {
            return Err(denied("dependency quota"));
        }
        let mut base = Vec::with_capacity(heads.len());
        let mut previous = BTreeSet::new();
        for (hash, header) in &heads {
            base.push(BaseMark {
                author: header
                    .extensions
                    .author
                    .ok_or_else(|| Error::Corrupt("intent base author".into()))?,
                sequence: header.extensions.author_sequence,
            });
            clock = clock.max(header.timestamp);
            if Some(*hash) != backlink {
                previous.insert(*hash);
            }
        }
        base.sort();
        // Canonical order puts the transaction after everything it read.
        let clock = clock
            .checked_add(1)
            .ok_or_else(|| denied("logical clock exhausted"))?
            .max(micros()?);
        let author_sequence = self
            .reserve_author_sequence(signing_key.public_key())
            .await?;
        let intent = Intent { base, statements };
        intent.check().map_err(denied)?;
        let frame = author(
            signing_key,
            Extensions {
                existence: 0,
                author: Some(signer.number),
                author_sequence,
                row_format: ROW_FORMAT,
                births: Vec::new(),
                partition,
                tip: self.workspace.tip,
                scope,
                generation: key.generation,
                chain: signer.chain.digest(),
            },
            signer.chain.clone(),
            &key.key,
            (sequence, backlink, clock),
            previous.into_iter().collect(),
            &RowPayload::compact(
                &RowPayload::witness(payload(sequence, intent)?, Vec::new())?,
                &self.definition,
                scope,
            )?,
        )?;
        self.verify_frame(&frame, time)?;
        let operation = frame.operation_ref()?;
        self.check_budget(
            usize::try_from(frame.stored_bytes()?).map_err(|_| denied("stored frame size"))?,
        )
        .await?;
        self.keep_chain(frame.chain()?).await?;
        self.no_frames_intake(&operation.header).await?;
        TransactionStore(self.conn.clone())
            .insert_operation(
                operation.hash,
                &operation.header,
                operation.body.as_ref(),
                frame.header_bytes(),
                &scope,
            )
            .await?;
        self.write_materialized(&frame, Vec::new(), Default::default(), None)
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

    /// Per author, the last applied frame of the scope's log.
    async fn applied_heads(
        &self,
        scope: ResourceScope,
    ) -> Result<Vec<(Hash, p2panda_core::Header<Extensions>)>, Error> {
        let rows = collect_rows(
            self.conn
                .query(
                    "SELECT o.hash,o.header FROM secure_operations o WHERE o.scope=? AND o.status='applied' AND o.sequence=(SELECT MAX(sequence) FROM secure_operations WHERE scope=o.scope AND author=o.author AND status='applied')",
                    [encode(&scope)?],
                )
                .await?,
        )
        .await?;
        rows.into_iter()
            .map(|row| {
                let [SqlValue::Blob(hash), SqlValue::Blob(header)] = row.as_slice() else {
                    return Err(Error::Corrupt("applied head".into()));
                };
                Ok((
                    Hash::try_from(hash.as_slice())
                        .map_err(|_| Error::Corrupt("applied head".into()))?,
                    decode(header)?,
                ))
            })
            .collect()
    }
}

/// The transaction's statements grouped per partition scope, in the order
/// they ran.
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
