//! Replay of received intents. A partition's transactions run in canonical
//! stamp order, each in its own savepoint; one whose statements fail or
//! whose effects its author may not write is dropped with its reason. An
//! arrival ordered before applied transactions rewinds them through their
//! undo records, applies, then redoes them. A seal ends rewinds behind it.
use super::*;
use rocky_core::TableClass;
use rocky_core::replay::{DropReason, Intent, TransactionOutcome};
use rocky_storage::partition_sql::{TablePartitions, split_intent};

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_replayed(scope INTEGER NOT NULL,clock INTEGER NOT NULL,author INTEGER NOT NULL,sequence INTEGER NOT NULL,hash BLOB NOT NULL UNIQUE,txn BLOB NOT NULL,member INTEGER NOT NULL,dropped TEXT,sealed INTEGER NOT NULL,undo BLOB,PRIMARY KEY(scope,clock,author,sequence)) WITHOUT ROWID",
    "CREATE INDEX IF NOT EXISTS secure_replayed_txn ON secure_replayed(txn)",
];

/// A row as it stood before a replayed transaction changed it. An applied,
/// unsealed member keeps these until a seal covers it; they never leave
/// the device.
pub(super) struct Undo {
    pub(super) table: String,
    pub(super) key: Vec<SqlValue>,
    pub(super) before: Option<Vec<SqlValue>>,
}

/// A rewound transaction's members here, to redo in canonical order.
struct Redo {
    transaction: Commitment,
    members: Vec<(Hash, u16)>,
}

/// One held intent member of a transaction: its frame, statements, its
/// author's grants and its member index.
pub(super) type Replaying<'a> = (
    &'a EncryptedFrame,
    &'a Intent,
    &'a [rocky_core::ResourceGrant],
    u16,
);

fn values_blob(values: &[SqlValue]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in values {
        value.encode(&mut bytes);
    }
    bytes
}

fn blob_values(mut bytes: &[u8]) -> Result<Vec<SqlValue>, Error> {
    let mut values = Vec::new();
    while !bytes.is_empty() {
        values
            .push(SqlValue::decode(&mut bytes).map_err(|_| Error::Corrupt("undo record".into()))?);
    }
    Ok(values)
}

/// A member's undo column: `NULL` when it changed nothing.
fn undo_blob(undo: &[Undo]) -> Result<SqlValue, Error> {
    if undo.is_empty() {
        return Ok(SqlValue::Null);
    }
    let rows: Vec<_> = undo
        .iter()
        .map(|undo| {
            (
                undo.table.as_str(),
                serde_bytes::ByteBuf::from(values_blob(&undo.key)),
                undo.before
                    .as_deref()
                    .map(|before| serde_bytes::ByteBuf::from(values_blob(before))),
            )
        })
        .collect();
    Ok(SqlValue::Blob(encode(&rows)?))
}

fn undo_of(bytes: &[u8]) -> Result<Vec<Undo>, Error> {
    let rows: Vec<(String, serde_bytes::ByteBuf, Option<serde_bytes::ByteBuf>)> =
        decode_bounded(bytes, usize::MAX)?;
    rows.into_iter()
        .map(|(table, key, before)| {
            Ok(Undo {
                table,
                key: blob_values(&key)?,
                before: before.map(|before| blob_values(&before)).transpose()?,
            })
        })
        .collect()
}

fn stamp_of(row: &[SqlValue]) -> Result<Stamp, Error> {
    match row {
        [
            SqlValue::Integer(clock),
            SqlValue::Integer(author),
            SqlValue::Integer(sequence),
            ..,
        ] => Ok(Stamp {
            clock: *clock as u64,
            author: *author as u32,
            sequence: *sequence as u64,
        }),
        _ => Err(Error::Corrupt("replayed order".into())),
    }
}

/// Events one replayed transaction may capture.
const REPLAY_CAPTURE: (usize, usize) = (MAX_BUNDLE_ROWS, MAX_BUNDLE_BYTES);

/// A frame's canonical position in its partition.
pub(super) fn stamp(header: &p2panda_core::Header<Extensions>) -> Stamp {
    Stamp {
        clock: header.timestamp,
        author: header.extensions.author.unwrap_or(0),
        sequence: header.extensions.author_sequence,
    }
}

fn reason_name(reason: DropReason) -> &'static str {
    match reason {
        DropReason::Constraint => "constraint",
        DropReason::Admission => "admission",
        DropReason::Schema => "schema",
        DropReason::Engine => "engine",
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// How a transaction with replayed writes stands here: `None` until a
    /// member of it has replayed. A drop is final once a seal covers it.
    pub async fn transaction_outcome(
        &self,
        transaction: Commitment,
    ) -> Result<Option<TransactionOutcome>, Error> {
        let inner = self.inner.lock(Class::Interactive).await;
        let rows = collect_rows(
            inner
                .conn
                .query(
                    "SELECT dropped,sealed FROM secure_replayed WHERE txn=? ORDER BY dropped IS NULL,sealed LIMIT 1",
                    [transaction.to_vec()],
                )
                .await?,
        )
        .await?;
        rows.first()
            .map(|row| match row.as_slice() {
                [SqlValue::Null, _] => Ok(TransactionOutcome::Applied),
                [SqlValue::Text(name), SqlValue::Integer(sealed)] => {
                    Ok(TransactionOutcome::Dropped {
                        reason: reason_named(name)?,
                        sealed: *sealed != 0,
                    })
                }
                _ => Err(Error::Corrupt("replayed outcome".into())),
            })
            .transpose()
    }
}

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    /// Seals everything `scope` applied so far, as a finalized seal would.
    pub(crate) async fn seal_replayed_for_test(&self, scope: ResourceScope) {
        let inner = self.inner.lock(Class::Background).await;
        let id = inner.find_scope_id(scope).await.unwrap().unwrap();
        inner.seal_replayed(id, i64::MAX).await.unwrap();
    }
}

fn reason_named(name: &str) -> Result<DropReason, Error> {
    Ok(match name {
        "constraint" => DropReason::Constraint,
        "admission" => DropReason::Admission,
        "schema" => DropReason::Schema,
        "engine" => DropReason::Engine,
        _ => return Err(Error::Corrupt("replayed drop reason".into())),
    })
}

impl<A: RowApplication> Inner<A> {
    /// Undoes every unsealed transaction ordered at or after `starts` in
    /// its partition, and every transaction sharing a member with one,
    /// latest first. Returns them for redo in canonical order.
    async fn rewind(&self, starts: Vec<(i64, Stamp)>) -> Result<Vec<Redo>, Error> {
        let corrupt = || Error::Corrupt("replayed member".into());
        let mut work = starts;
        let mut rewound = BTreeMap::new();
        while let Some((scope, from)) = work.pop() {
            let later = collect_rows(
                self.conn
                    .query(
                        "SELECT clock,author,sequence,hash,txn,member,undo FROM secure_replayed WHERE scope=? AND sealed=0 AND (clock,author,sequence)>=(?,?,?)",
                        bindings(vec![
                            SqlValue::Integer(scope),
                            SqlValue::Integer(from.clock as i64),
                            SqlValue::Integer(i64::from(from.author)),
                            SqlValue::Integer(from.sequence as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            for row in later {
                let stamp = stamp_of(&row)?;
                let [
                    ..,
                    SqlValue::Blob(hash),
                    SqlValue::Blob(txn),
                    SqlValue::Integer(member),
                    undo,
                ] = row.as_slice()
                else {
                    return Err(corrupt());
                };
                let hash = Hash::try_from(hash.as_slice()).map_err(|_| corrupt())?;
                let transaction: Commitment = txn.as_slice().try_into().map_err(|_| corrupt())?;
                let undo = match undo {
                    SqlValue::Null => Vec::new(),
                    SqlValue::Blob(undo) => undo_of(undo)?,
                    _ => return Err(corrupt()),
                };
                if rewound
                    .insert((stamp, scope), (hash, transaction, *member as u16, undo))
                    .is_some()
                {
                    continue;
                }
                // Its other members rewind with it, and what follows them.
                for other in collect_rows(
                    self.conn
                        .query(
                            "SELECT clock,author,sequence,scope FROM secure_replayed WHERE txn=? AND sealed=0",
                            [transaction.to_vec()],
                        )
                        .await?,
                )
                .await?
                {
                    let [.., SqlValue::Integer(other_scope)] = other.as_slice() else {
                        return Err(corrupt());
                    };
                    let stamp = stamp_of(&other)?;
                    if !rewound.contains_key(&(stamp.clone(), *other_scope)) {
                        work.push((*other_scope, stamp));
                    }
                }
            }
        }
        for (hash, _, _, undo) in rewound.values().rev() {
            self.undo(undo).await?;
            self.conn
                .execute(
                    "DELETE FROM secure_replayed WHERE hash=?",
                    [hash.as_bytes().to_vec()],
                )
                .await?;
        }
        let mut redo: Vec<Redo> = Vec::new();
        for (hash, transaction, member, _) in rewound.into_values() {
            match redo.iter_mut().find(|redo| redo.transaction == transaction) {
                Some(redo) => redo.members.push((hash, member)),
                None => redo.push(Redo {
                    transaction,
                    members: vec![(hash, member)],
                }),
            }
        }
        Ok(redo)
    }

    /// Restores the rows one member changed to their state before it.
    async fn undo(&self, undo: &[Undo]) -> Result<(), Error> {
        if undo.is_empty() {
            return Ok(());
        }
        let corrupt = || Error::Corrupt("undo record".into());
        let typed = self.workspace.typed.as_ref().ok_or_else(corrupt)?;
        let mut tables = Vec::with_capacity(undo.len());
        for undo in undo {
            let projected = typed.by_name(&undo.table).ok_or_else(corrupt)?;
            tables.push(
                typed
                    .plan(&BTreeSet::from([projected.scope]))?
                    .tables
                    .into_iter()
                    .find(|table| table.name.eq_ignore_ascii_case(&undo.table))
                    .ok_or_else(corrupt)?,
            );
        }
        // Every changed row goes first, so a restored image never collides
        // with a row the member moved; references settle at commit.
        self.conn
            .execute("PRAGMA defer_foreign_keys=ON", ())
            .await?;
        for (table, undo) in tables.iter().zip(undo) {
            self.conn
                .execute(&table.delete_by_key(), bindings(undo.key.clone()))
                .await?;
        }
        for (table, undo) in tables.iter().zip(undo) {
            if let Some(before) = &undo.before {
                self.conn
                    .execute(&table.insert_image(), bindings(before.clone()))
                    .await?;
            }
        }
        self.conn
            .execute("PRAGMA defer_foreign_keys=OFF", ())
            .await?;
        Ok(())
    }

    /// A seal finalized `scope` through `through`: the transactions it
    /// covers never rewind again, their drops are final and their undo
    /// records go.
    pub(super) async fn seal_replayed(&self, scope: i64, through: i64) -> Result<(), Error> {
        self.conn
            .execute(
                "UPDATE secure_replayed SET sealed=1,undo=NULL WHERE scope=? AND sealed=0 AND hash IN (SELECT hash FROM secure_applied WHERE scope=? AND position<=?)",
                [scope, scope, through],
            )
            .await?;
        Ok(())
    }

    /// Replays one transaction's held intent members together: one that
    /// drops drops the transaction in every partition held here. Records
    /// the outcome and, when it applied, the undo of each member.
    async fn replay_transaction(
        &self,
        transaction: Commitment,
        intents: &[Replaying<'_>],
    ) -> Result<(), Error> {
        self.conn.execute("SAVEPOINT intents", ()).await?;
        let mut undo = Vec::with_capacity(intents.len());
        let mut dropped = None;
        for (frame, intent, grants, _) in intents {
            match self.replay_intent(frame, intent, grants)? {
                Ok(changed) => undo.push(changed),
                Err(reason) => {
                    dropped = Some(reason);
                    break;
                }
            }
        }
        if dropped.is_some() {
            self.conn.execute("ROLLBACK TO intents", ()).await?;
            undo.clear();
        }
        self.conn.execute("RELEASE intents", ()).await?;
        for (index, (frame, _, _, member)) in intents.iter().enumerate() {
            let undo = undo.get(index).map_or(&[][..], Vec::as_slice);
            self.record_replayed(frame, (transaction, *member), dropped, undo)
                .await?;
            if dropped.is_none()
                && let Some(table) = self
                    .workspace
                    .typed
                    .as_ref()
                    .and_then(|typed| typed.rows.table(&frame.scope().ok()?.table))
            {
                self.touch(&table.sql_name);
            }
        }
        Ok(())
    }

    /// Replays an arrival's intent members. Unsealed transactions of their
    /// partitions ordered after them rewind first and redo after, so every
    /// partition holds its canonical order; a late arrival behind a seal
    /// orders after the seal's members.
    pub(super) async fn replay_arrival(
        &self,
        transaction: Commitment,
        intents: &[Replaying<'_>],
        time: u64,
    ) -> Result<(), Error> {
        let mut starts = Vec::with_capacity(intents.len());
        for (frame, ..) in intents {
            let header = &frame.operation_ref()?.header;
            starts.push((self.scope_id(header.extensions.scope).await?, stamp(header)));
        }
        let redo = self.rewind(starts).await?;
        self.replay_transaction(transaction, intents).await?;
        for redo in redo {
            let mut opened = Vec::with_capacity(redo.members.len());
            for (hash, member) in redo.members {
                let frame = self
                    .frame(hash)
                    .await?
                    .ok_or_else(|| Error::Corrupt("rewound frame is missing".into()))?;
                let payload = self.row_payload(&frame)?;
                // Admission is never carried across a grant change.
                let grants = self.verify_frame(&frame, time).ok();
                opened.push((frame, payload, grants, member));
            }
            let mut intents = Vec::with_capacity(opened.len());
            for (frame, payload, grants, member) in &opened {
                let intent = payload
                    .intent()
                    .ok_or_else(|| Error::Corrupt("rewound frame carries no intent".into()))?;
                intents.push((frame, intent, grants, *member));
            }
            if intents.iter().any(|(_, _, grants, _)| grants.is_none()) {
                for (frame, _, _, member) in &intents {
                    self.record_replayed(
                        frame,
                        (redo.transaction, *member),
                        Some(DropReason::Admission),
                        &[],
                    )
                    .await?;
                }
                continue;
            }
            let intents: Vec<Replaying<'_>> = intents
                .iter()
                .map(|(frame, intent, grants, member)| {
                    (*frame, *intent, grants.as_deref().unwrap_or(&[]), *member)
                })
                .collect();
            self.replay_transaction(redo.transaction, &intents).await?;
        }
        Ok(())
    }

    /// Records where a transaction's member stands in its partition's order
    /// and how it replayed: `None` with its undo when it applied.
    pub(super) async fn record_replayed(
        &self,
        frame: &EncryptedFrame,
        (transaction, member): (Commitment, u16),
        dropped: Option<DropReason>,
        undo: &[Undo],
    ) -> Result<(), Error> {
        let operation = frame.operation_ref()?;
        let header = &operation.header;
        let stamp = stamp(header);
        let id = self.scope_id(header.extensions.scope).await?;
        self.conn
            .execute(
                "INSERT INTO secure_replayed VALUES(?,?,?,?,?,?,?,?,0,?)",
                bindings(vec![
                    SqlValue::Integer(id),
                    SqlValue::Integer(stamp.clock as i64),
                    SqlValue::Integer(i64::from(stamp.author)),
                    SqlValue::Integer(stamp.sequence as i64),
                    SqlValue::Blob(operation.hash.as_bytes().to_vec()),
                    SqlValue::Blob(transaction.to_vec()),
                    SqlValue::Integer(i64::from(member)),
                    dropped.map_or(SqlValue::Null, |reason| {
                        SqlValue::Text(reason_name(reason).into())
                    }),
                    undo_blob(undo)?,
                ]),
            )
            .await?;
        Ok(())
    }

    /// Runs one member's statements on its partition inside the caller's
    /// savepoint and admits their effects against the author's grants.
    /// Returns the changed rows' prior images, or why the transaction
    /// drops; the caller rolls back then.
    fn replay_intent(
        &self,
        frame: &EncryptedFrame,
        intent: &Intent,
        grants: &[rocky_core::ResourceGrant],
    ) -> Result<Result<Vec<Undo>, DropReason>, Error> {
        let header = &frame.operation_ref()?.header;
        let scope = header.extensions.scope;
        let typed = self
            .workspace
            .typed
            .as_ref()
            .ok_or_else(|| denied("no typed tables"))?;
        let schema = typed.rows.schema();
        let table = typed
            .rows
            .table(&scope.table)
            .ok_or_else(|| denied("an intent names an unknown table"))?;
        let logical = schema
            .table(&table.sql_name)
            .filter(|logical| logical.class == TableClass::Replayed)
            .ok_or_else(|| denied("an intent names a table that is not replayed"))?;
        // Each statement is checked and confined to the frame's own
        // partition again: a receiver runs only what its own split yields.
        let held = match &header.extensions.partition {
            Some(tuple) => vec![super::partition::partition_values(schema, logical, tuple)?],
            None => Vec::new(),
        };
        let rules = BTreeMap::from([(
            logical.sql_name.to_ascii_lowercase(),
            TablePartitions {
                columns: super::partition::partition_columns(logical)?,
                complete: held.is_empty(),
                held,
                waiting: Vec::new(),
                support: false,
            },
        )]);
        let mut copies = Vec::with_capacity(intent.statements.len());
        for statement in &intent.statements {
            match split_intent(
                &statement.sql,
                &statement.params,
                &schema.views,
                &rules,
                |_| Err::<Vec<Vec<SqlValue>>, _>(()),
            ) {
                Ok(split) if split.len() <= 1 => copies.extend(split),
                _ => return Ok(Err(DropReason::Schema)),
            }
        }
        let mut plan = typed.plan(&BTreeSet::from([scope]))?;
        let conn = self.conn.connection();
        plan.bind_physical_columns(conn)?;
        let plan = Arc::new(plan);
        let capture = conn.capture()?;
        let state = conn.session()?;
        let poisoned = || Error::Corrupt("capture poisoned".into());
        capture
            .lock()
            .map_err(|_| poisoned())?
            .arm(plan.clone(), REPLAY_CAPTURE);
        let ran = (|| {
            for copy in copies {
                state
                    .gate
                    .lock()
                    .map_err(|_| poisoned())?
                    .arm(Some(plan.clone()));
                capture.lock().map_err(|_| poisoned())?.begin_statement();
                let result = conn.session_run(
                    &copy.statement.sql,
                    storage::bindings(copy.statement.params),
                    0,
                    true,
                );
                let assigned = {
                    let mut gate = state.gate.lock().map_err(|_| poisoned())?;
                    let assigned = gate.assigned();
                    gate.arm(None);
                    assigned
                };
                let failed = !matches!(result, Ok(sqlite::Run::Done(_)));
                capture
                    .lock()
                    .map_err(|_| poisoned())?
                    .end_statement(failed, &assigned);
                match result {
                    Ok(sqlite::Run::Done(_)) => {}
                    Err(error) if error.is_constraint() => {
                        return Ok(Some(DropReason::Constraint));
                    }
                    Err(sqlite::Error::Denied | sqlite::Error::Unsupported(_)) => {
                        return Ok(Some(DropReason::Schema));
                    }
                    _ => return Ok(Some(DropReason::Engine)),
                }
            }
            Ok::<_, Error>(None)
        })();
        let touched = {
            let mut sink = capture.lock().map_err(|_| poisoned())?;
            let touched = match (&ran, sink.failure()) {
                (Ok(None), None) => sink
                    .freeze()
                    .map_err(|_| Error::Corrupt("replay capture".into())),
                _ => Ok(Vec::new()),
            };
            sink.disarm();
            touched
        };
        if let Some(reason) = ran? {
            return Ok(Err(reason));
        }
        // Admission runs on the rows this replay produced.
        let mut undo = Vec::new();
        for touched in touched? {
            let captured = &plan.tables[touched.table];
            if !captured.name.eq_ignore_ascii_case(&logical.sql_name) {
                return Ok(Err(DropReason::Engine));
            }
            let Some(change) = replayed_change(conn, typed, captured, &touched)? else {
                continue;
            };
            if change.scope != scope || !change.permitted_by(grants) {
                return Ok(Err(DropReason::Admission));
            }
            undo.push(Undo {
                table: captured.name.clone(),
                key: touched.key,
                before: touched.before,
            });
        }
        Ok(Ok(undo))
    }
}

/// The change a replayed table's captured row went through, as grants see
/// it; `None` when it changed nothing.
pub(super) fn replayed_change(
    conn: &sqlite::Connection,
    typed: &typed::Typed,
    captured: &rocky_storage::capture::Table,
    touched: &rocky_storage::capture::Touched,
) -> Result<Option<RowChange>, Error> {
    let corrupt = |what: &str| Error::Corrupt(format!("replayed row {what}"));
    let projected = typed
        .by_name(&captured.name)
        .ok_or_else(|| corrupt("table"))?;
    let after = match conn
        .rows(&captured.select, storage::bindings(touched.key.clone()))?
        .first()
    {
        Some(values) => Some(captured.image(values).map_err(|_| corrupt("image"))?),
        None => None,
    };
    let Some(anchor) = after.as_ref().or(touched.before.as_ref()) else {
        return Ok(None);
    };
    let fields = |image: &Vec<SqlValue>| {
        typed
            .rows
            .fields(&projected.table, image)
            .map_err(|_| corrupt("fields"))
    };
    let change = RowChange {
        scope: typed.row_scope(projected, anchor)?,
        row: typed
            .rows
            .row_id(typed.domain.domain, &projected.table, anchor)
            .map_err(|_| corrupt("key"))?,
        before: touched.before.as_ref().map(fields).transpose()?,
        after: after.as_ref().map(fields).transpose()?,
    };
    Ok(apply::changes(&change).then_some(change))
}
