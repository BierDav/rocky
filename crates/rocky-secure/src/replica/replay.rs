//! Replay of received intents. A partition's transactions run in canonical
//! stamp order, each in its own savepoint; one whose statements fail or
//! whose effects its author may not write is dropped with its reason.
use super::*;
use rocky_core::TableClass;
use rocky_core::replay::{DropReason, Intent, TransactionOutcome};
use rocky_storage::partition_sql::{TablePartitions, split_intent};

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_replayed(scope INTEGER NOT NULL,clock INTEGER NOT NULL,author INTEGER NOT NULL,sequence INTEGER NOT NULL,hash BLOB NOT NULL UNIQUE,txn BLOB NOT NULL,member INTEGER NOT NULL,dropped TEXT,PRIMARY KEY(scope,clock,author,sequence)) WITHOUT ROWID",
];

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
    /// member of it has replayed. Drops stay provisional until a seal
    /// covers them, which later slices record.
    pub async fn transaction_outcome(
        &self,
        transaction: Commitment,
    ) -> Result<Option<TransactionOutcome>, Error> {
        let inner = self.inner.lock(Class::Interactive).await;
        let rows = collect_rows(
            inner
                .conn
                .query(
                    "SELECT dropped FROM secure_replayed WHERE txn=? ORDER BY dropped IS NULL LIMIT 1",
                    [transaction.to_vec()],
                )
                .await?,
        )
        .await?;
        rows.first()
            .map(|row| match row.as_slice() {
                [SqlValue::Null] => Ok(TransactionOutcome::Applied),
                [SqlValue::Text(name)] => Ok(TransactionOutcome::Dropped {
                    reason: reason_named(name)?,
                    sealed: false,
                }),
                _ => Err(Error::Corrupt("replayed outcome".into())),
            })
            .transpose()
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
    /// Whether `header` orders after every transaction its partition applied.
    pub(super) async fn replays_in_order(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<bool, Error> {
        let id = self.scope_id(header.extensions.scope).await?;
        let last = collect_rows(
            self.conn
                .query(
                    "SELECT clock,author,sequence FROM secure_replayed WHERE scope=? ORDER BY clock DESC,author DESC,sequence DESC LIMIT 1",
                    [id],
                )
                .await?,
        )
        .await?;
        Ok(match last.first().map(Vec::as_slice) {
            None => true,
            Some(
                [
                    SqlValue::Integer(clock),
                    SqlValue::Integer(author),
                    SqlValue::Integer(sequence),
                ],
            ) => {
                stamp(header)
                    > Stamp {
                        clock: *clock as u64,
                        author: *author as u32,
                        sequence: *sequence as u64,
                    }
            }
            Some(_) => return Err(Error::Corrupt("replayed order".into())),
        })
    }

    /// Records where a transaction's member stands in its partition's order
    /// and how it replayed; `None` when it applied.
    pub(super) async fn record_replayed(
        &self,
        frame: &EncryptedFrame,
        membership: &Membership,
        dropped: Option<DropReason>,
    ) -> Result<(), Error> {
        let operation = frame.operation_ref()?;
        let header = &operation.header;
        let stamp = stamp(header);
        let id = self.scope_id(header.extensions.scope).await?;
        self.conn
            .execute(
                "INSERT INTO secure_replayed VALUES(?,?,?,?,?,?,?,?)",
                bindings(vec![
                    SqlValue::Integer(id),
                    SqlValue::Integer(stamp.clock as i64),
                    SqlValue::Integer(i64::from(stamp.author)),
                    SqlValue::Integer(stamp.sequence as i64),
                    SqlValue::Blob(operation.hash.as_bytes().to_vec()),
                    SqlValue::Blob(membership.transaction.to_vec()),
                    SqlValue::Integer(i64::from(membership.index)),
                    dropped.map_or(SqlValue::Null, |reason| {
                        SqlValue::Text(reason_name(reason).into())
                    }),
                ]),
            )
            .await?;
        Ok(())
    }

    /// Runs one member's statements on its partition inside the caller's
    /// savepoint and admits their effects against the author's grants.
    /// `Some` names why the transaction drops; the caller rolls back.
    pub(super) fn replay_intent(
        &self,
        frame: &EncryptedFrame,
        intent: &Intent,
        grants: &[rocky_core::ResourceGrant],
    ) -> Result<Option<DropReason>, Error> {
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
                _ => return Ok(Some(DropReason::Schema)),
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
            return Ok(Some(reason));
        }
        // Admission runs on the rows this replay produced.
        for touched in touched? {
            let captured = &plan.tables[touched.table];
            if !captured.name.eq_ignore_ascii_case(&logical.sql_name) {
                return Ok(Some(DropReason::Engine));
            }
            let Some(change) = replayed_change(conn, typed, captured, &touched)? else {
                continue;
            };
            if change.scope != scope || !change.permitted_by(grants) {
                return Ok(Some(DropReason::Admission));
            }
        }
        Ok(None)
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
