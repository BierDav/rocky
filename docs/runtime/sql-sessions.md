# SQL sessions

Ordinary SQL on the typed tables of a [logical schema](logical-schema.md) in
the trusted runtime (`Replica::sql_session`, a Rust API). `EncryptedReplica`
has [secure SQL](secure-sql.md) on the same capture, which the SDK, the
[driver](sql-driver.md) and Room use; transaction control, capture and limits
described here apply to both.

## Open a session and run statements

```rust
let session = replica.sql_session(SqlOptions::default()).await?;
session.run("BEGIN IMMEDIATE", vec![]).await?;
let insert = session
    .prepare("INSERT INTO shopping_items(id,title,completed) VALUES(?,?,?)")
    .await?;
session.execute(&insert, vec![id, title, SqlValue::Integer(0)]).await?;
let rows = session.run("SELECT id,title FROM shopping_items", vec![]).await?;
let committed = session.commit(token).await?.committed;
```

| Handle | Meaning |
| --- | --- |
| `SqlSession` | One connection. Dropping it rolls an open transaction back |
| `SqlStatement` | Columns, parameter count and `read_only` of compiled text. Preparing admits and changes nothing |
| `SqlRows` | A statement's whole result, with `changes`, `last_insert_rowid`, the session state and, on the committing call, the `TransactionOutcome` |
| `SqlCursor` | `SqlRows::cursor()`: an iterator over that buffered result |

The application must be installed with a logical schema and implement
`StoredApplication::sql_writes`; `rocky_sdk::TypedApp` does. Other
applications refuse `sql_session` with `Incompatible`.

A session may read every table of its domain's schema and write the tables
classified `mergeable` or `local_only`. A statement that names anything else
fails with `SqlError::Denied`: protocol tables, another domain, `sqlite_*`
tables, `PRAGMA`, `ATTACH`, schema changes and temporary objects. Each call
takes one statement, inside the [supported SQL surface](queries-and-observation.md#supported-sql);
others fail with `SqlError::Unsupported` before they run. `cursor` and
`catalog` are in [queries and observation](queries-and-observation.md).

## Transactions

| Statement | Behavior |
| --- | --- |
| `BEGIN [DEFERRED\|IMMEDIATE\|EXCLUSIVE] [TRANSACTION]` | Starts a transaction in that engine mode |
| `COMMIT`, `END [TRANSACTION]` | Seals and commits; the reply carries the outcome |
| `ROLLBACK [TRANSACTION]` | Undoes the transaction |
| `SAVEPOINT name`, `RELEASE [SAVEPOINT] name`, `ROLLBACK [TRANSACTION] TO [SAVEPOINT] name` | Nested as in SQLite, up to 64 deep. Names compare without case |
| `SAVEPOINT` outside a transaction | Begins one; releasing that savepoint commits |
| Any other statement outside a transaction | Commits by itself and is reported after its commit |

The session state is `Idle` or `Active`. Committing is not observable: a
commit call returns the outcome, an error, or nothing if the replica closed.

Inside a transaction the session reads its own writes. Bound reads,
`Replica::query` and other sessions see committed rows only. Row counts, row
ids and `RETURNING` rows are provisional until the commit.

A failed statement follows its conflict clause. `ABORT`, the default, undoes
the statement and the transaction continues. `FAIL` keeps the rows written
before the conflict. `IGNORE` and `REPLACE` do not fail. `ROLLBACK` ends the
transaction in the engine. A write of equal values reports its native row
count and publishes nothing.

When the runtime ends a transaction itself, every later statement fails with
`SqlError::Aborted` and its reason until the session sends `ROLLBACK`, which
succeeds, or `COMMIT`, which fails once more. This stops the rest of a
transaction body from running as separate statements.

| Reason | Cause |
| --- | --- |
| `Timeout` | The transaction outlived `transaction_timeout` |
| `Cancelled` | `SqlSession::cancel` |
| `Poisoned` | A denied statement, a writing statement over its result bound or a capture failure |
| `Engine` | `OR ROLLBACK`, or an interrupted writing statement |

## What a commit publishes

Each changed row becomes at most one operation, by comparing the row when the
transaction began with the final row:

| Change | Operation |
| --- | --- |
| New key | Insert |
| Key gone | Delete |
| Same key, columns differ | One update holding every differing column |
| Deleted and inserted again, `INSERT OR REPLACE`, or a respelled `NOCASE` key | A new row replacing the old one |
| Key value changed | A delete and an insert |
| Inserted and deleted in the transaction, or equal rows | Nothing |

All operations of a transaction, the typed rows and one local revision commit
together. Under the `assigned` update policy an update also publishes columns
an `UPDATE` assigned to an equal value; `changed` does not. The policy is set
when the schema is generated.

Register columns hold values. `SET count = count + 1` publishes the number
the author computed, so two devices incrementing concurrently keep one of the
two results. Writes that must not race, such as a counter or a sequential
number, are [coordinated writes](coordinated-writes.md): a secure SQL
transaction begun on a coordinated slot and committed with
`commit_coordinated` (SDK: `"coordinated": true` on `BEGIN` and `COMMIT`)
becomes true only once the domain's trusted nodes stored it. Clock and random
functions run once, on the author.

A derived column takes its installed rule's value at commit whatever a
statement wrote. A value outside a column's declared type, such as text in an
`INTEGER` column, cannot be published and the commit fails with
`SqlError::Capture`.

## Limits

`SqlOptions` bounds a session. Exceeding a bound is reported; nothing is
truncated.

| Option | Default | Effect when exceeded |
| --- | --- | --- |
| `transaction_timeout` | 5 s, at most 60 s | Rollback, `Aborted(Timeout)` |
| `statement_budget` | 2 s | The statement is interrupted, code 9 |
| `captured_rows`, `captured_bytes` | 65,536 and 32 MiB | Rollback, `SqlError::Capture` |
| `result_bytes` | 16 MiB | `Quota`; a writing statement's transaction is rolled back |
| `acknowledge` | `Flushed` | A commit is reported after a full flush |

A transaction holds its file's writer. Every other write of the file, in any
domain, waits until it ends, so a long transaction delays local writes and
incoming sync by up to its timeout. Reads are answered meanwhile.
`WriterStats` reports `leases`, `lease_micros` and `lease_max_micros`.

## Errors and recovery

`Error::sql_certainty` classifies a failed call:

| Certainty | Meaning | Examples |
| --- | --- | --- |
| `Retryable` | Nothing happened; send the call again | A busy engine at `BEGIN`, `Backpressure` |
| `Definite` | The statement or transaction had no effect | Constraint errors, `Denied`, `Aborted`, `Capture`, `Conflict`, `Stale` |
| `Uncertain` | A commit may have happened | `Closed`, a failed rollback |

`SqlError::Statement` carries SQLite's extended result code, for example 1555
for a primary key conflict. `SqlError::Stale` refuses a statement that a
session of another domain or [binding](database-bindings.md) prepared.

To make a commit recoverable, choose a `TransactionToken` and pass it to
`SqlSession::commit` or `execute_with_token`. After an uncertain result or a
restart, `Replica::sql_outcome(token)` returns the committed outcome, or None
if the transaction did not happen and its body may run again. Committing
again under a used token rolls the new transaction back: it returns the
recorded outcome when the statements are the same and `Conflict` otherwise.
`Replica::lookup_mutation(operation_handle(id))` returns an operation's bytes
until it is pruned.

A token record is kept as long as a mutation handle of the same commit: until
its operations are behind the [pruning horizon](checkpoints.md) and
`RetentionPolicy::handles` newer local operations exist, 65,536 by default.
After that the token is unknown: `sql_outcome` returns None and a commit
under it is a new commit. Ask for an uncertain outcome before then.

The runtime never reruns a transaction body.

On the encrypted profile, [held changes](held-changes.md) retain transactions
that a later removal makes incompatible before they were offered to peers.
The application maps a held change to current SQL, then uses
`SecureSql::commit_reauthoring(handle, held_id)` (Kotlin `reauthor(id) { ... }`).
Its fresh writes and removal from the held list commit atomically; a failure
leaves both unchanged. The handle binds the held ID as well as the writes.
Dropping a held change is a separate explicit operation.

## Qualification

`cargo test -p rocky-sdk --test sql` and
`cargo test -p rocky-runtime --lib -- sql_tests` and
`cargo test -p rocky-storage capture`:

- A bulk insert and a multi-column update commit as one revision, read back
  inside the session before the commit and by nobody else. Peers that receive
  the operations in order, reversed and twice hold the same rows, and
  applying them creates no local operation.
- Nested savepoints, every conflict clause, no-op writes, key changes,
  `RETURNING`, engine-assigned ids and per-session counts follow SQLite.
- Both update policies, against a concurrent edit on another device.
- Injected failures of protocol insertion, of the engine's commit and of a
  diverging projection publish nothing. Token records are pruned with the
  mutation handles of their time. A process killed with the transaction
  open, inside the commit step and before the engine's commit leaves no
  trace; one killed after the commit is recovered once by its token.
- The row hook captures trigger and cascade writes, `WITHOUT ROWID` tables
  and replacements on a raw connection.
- Every replica's checkpoint replay reproduces its tables afterwards.

A session over several domains of one file, or over one tenant under a
recorded binding, is a [bound database](database-bindings.md). Its outcome
lists each domain in `TransactionOutcome::domains`; a domain's own session
has one entry.

## Design rationale

- A transaction is a *lease* of the file's single write connection, not a
  second connection: one row hook per connection feeds both checkpoint digests
  and capture, and no lock is held while the caller decides its next
  statement. The cost is that a lease stops every write of its file, incoming
  sync included, for up to its timeout, and shares no group commit.
- Peers receive row values and identities, never SQL text, predicates, clock
  or random results; nothing is re-executed elsewhere and the runtime never
  replays a transaction body.
- The final row is read from the table at commit, never taken from a captured
  event, and the digest change is computed from the first and final images, so
  rolled-back statements cannot enter an operation or a checkpoint digest.
- Table classification comes from the installed descriptor and the engine's
  authorizer decides every access; name prefixes are never trusted. Session
  statements are compiled on every execution so a cached runtime statement is
  never reused unauthorized.
- `changes()`, `total_changes()` and `last_insert_rowid()` are replaced by
  per-session values that protocol inserts never touch.
- A failed `OR FAIL` statement keeps its rows, but they publish as changed
  values only: no assignment mask and no delete-and-reinsert evidence.
- A trusted peer applies a transaction's operations one at a time and may show
  part of it between deliveries; atomic peer application is the secure
  profile's [bundle](secure-sessions.md#bundles).
