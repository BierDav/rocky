# SQL driver connections

`rocky_sdk::Database` connections and the typed handles of `sdks/native`: the
layer a database driver such as an `androidx.sqlite.SQLiteDriver` sits on. It
builds on [secure SQL](secure-sql.md) and
[queries and observation](queries-and-observation.md).

## Connections

```rust
let writer = database.connect(ConnectionOptions { read_only: false, ..options }).await?;
let statement = writer.statement();
let page = writer.execute(statement, "SELECT id, title FROM items".into(), vec![]).await?;
let more = writer.next(statement).await?; // while !page.done
writer.close().await;
```

| Connection | Runs on | Sees |
| --- | --- | --- |
| Writer | One secure SQL session over every granted table | Its own open transaction |
| Reader | [Snapshot cursors](queries-and-observation.md#snapshot-cursors) | The latest committed snapshot |

`PRAGMA query_only` reads and sets the kind per connection, as in SQLite:
`= 1` makes a writer a reader and `= 0` a writer again, outside a
transaction. An idle writer holds nothing, so switching costs nothing.

A statement's result arrives in pages of at most 256 rows and 1 MiB; columns
come with the first page, before any row. A statement number keeps the
undelivered rest until the next page, `finish` or the next `execute` under
the same number; a connection holds at most 64 such results. Closing a
connection rolls an open transaction back and ends its cursors; closing the
database closes every connection first.

## The writer

The writer is a [secure SQL session](secure-sql.md): one transaction is one
commit and one signed [bundle](secure-sessions.md#bundles), and capture stays
in the engine's row hook. Transaction control follows
[SQL sessions](sql-sessions.md#transactions), with two differences:

- `BEGIN` takes the replica only when the first statement of the
  transaction runs. A transaction that only began and ended never takes it
  and publishes nothing.
- A transaction the runtime ended (`Aborted`) still counts as open for
  `in_transaction` until the driver sends `ROLLBACK` or `COMMIT`.

An open write transaction holds the replica: another connection's statement
waits until it ends, at most for its `transaction_timeout`, 5 s by default.
Readers never delay the writer. A caller that awaits a reader inside its own
write transaction waits for that timeout; an ORM uses the transaction's own
connection for nested calls instead.

| Limit | Value | When exceeded |
| --- | --- | --- |
| Rows of one bundle | 2,048 | The commit fails, code 18; nothing is published or split |
| Encoded writes of one bundle | 4 MiB | The commit fails, code 18 |
| `transaction_timeout` | 5 s, at most 60 s | Code 4 for every statement until `ROLLBACK` |

A bundle records the net change: a row inserted and deleted in one
transaction publishes nothing. Under the `changed`
[update policy](sql-sessions.md#what-a-commit-publishes) an update publishes
only the columns whose values differ, so writing a whole entity is safe.

## Row ids

Every typed table has Rocky's `id` and `row_kind` columns
([row identity](logical-schema.md#row-identity)); an insert
names the next `id` of its table author, which an issuer assigns the device
as a table pseudonym. A writer obtains it before the insert, inside the
transaction that inserts the row:

```sql
BEGIN IMMEDIATE TRANSACTION;
SELECT rocky_next_id('items');           -- or rocky_next_id(?) with the name bound
INSERT INTO items(key, title, id) VALUES('i1', 'Milk', ?);
COMMIT;
```

A [partitioned](partitioned-scopes.md#ids-and-transactions) table takes its
partition values after the name: `rocky_next_id('items', ?)`. Arguments are
string literals, non-negative integer literals or `?`. Negative values must be
bound through `?`. The call must be a standalone `SELECT` without aliases,
other result columns or expressions; unsupported shapes return a driver error.
The statement returns one integer column. Further reservations of the
transaction continue the same author's sequence, so a bulk insert may use the
ids that follow one reservation; a rollback to a savepoint or of the
transaction releases what it reserved. It fails with code 8 on a reader, and
with code 1 outside a write transaction, for an unknown table and without the
device's table pseudonym or insert grant. The commit refuses an `id` that is not the author's next
sequence or a `row_kind` other than 0.

## Readers

A statement outside a transaction opens its own cursor and pages it. Inside
`BEGIN DEFERRED`, every statement reads one snapshot, taken by the first of
them, and its result is read whole, at most 16 MiB. Savepoints of a read
transaction only nest names. A write, or `BEGIN IMMEDIATE`, fails with code 8.
Cursor bounds, availability and revocation are those of
[snapshot cursors](queries-and-observation.md#snapshot-cursors); after an
authority change a reader opens a new session outside a transaction.

## Values and errors

Values are SQLite's five storage classes; a NaN binds as NULL, as SQLite
stores it. A failure carries SQLite's result code and a message:

| Code | Cause |
| --- | --- |
| Engine code, such as 1555 or 2067 | A constraint or another engine error; the transaction continues |
| 23 `SQLITE_AUTH` | `Denied`: pragmas, the engine catalog, protocol tables, schema changes |
| 4 `SQLITE_ABORT` | The runtime ended the transaction |
| 8 `SQLITE_READONLY` | A write on a reader |
| 18 `SQLITE_TOOBIG` | A quota, such as the bundle limits |
| 21 `SQLITE_MISUSE` | A closed connection or a statement without further rows |

## Room 2.8.4

With `profile: Some(Profile::Room284)` (`"room-2.8.4"` natively) a
connection answers the housekeeping statements of the pinned Room 2.8.4 and
androidx.sqlite 2.6.2 itself; every other statement reaches the session,
which still denies pragmas and schema changes. Statements are matched as Room
writes them; another version needs a new profile and fixture.

| Room sends | Answer |
| --- | --- |
| `PRAGMA journal_mode = …`, `synchronous = NORMAL\|FULL`, `temp_store = MEMORY`, `recursive_triggers = 1`, `foreign_keys = ON` | Accepted: the file is always in WAL mode and every commit flushes |
| `PRAGMA busy_timeout [= n]` | The transaction timeout in milliseconds; setting it changes nothing |
| `PRAGMA user_version` | The installed schema's version, which Room's import makes Room's database version |
| `PRAGMA user_version = n` | Accepted when it is that version; otherwise an error naming both |
| `PRAGMA table_info`, `index_list`, `index_info`, `index_xinfo`, `foreign_key_list` | Read from the file for a table or index of the session's catalog, so Room validates the real keys and indexes |
| `CREATE TABLE IF NOT EXISTS`, `CREATE [UNIQUE] INDEX IF NOT EXISTS` | Nothing, for an installed table or index, as SQLite leaves an existing one; otherwise an error. An entity declaring `AUTOINCREMENT` (`autoGenerate = true`) or lacking the `id` or `row_kind` field fails with code 1 |
| `room_master_table` statements | The identity hash Room writes, kept by the connection; Room validates the schema once per process |
| `sqlite_master` counts and lists | From the catalog; no ORM trigger or view is listed |
| The invalidation tracker's log and triggers | Emulated from [table events](#table-events) |

### Invalidation

Room's tracker keeps a temporary modification log that triggers on its own
connection set; sync writes never fire them. The profile instead remembers
which log id each tracked table's triggers would set. When Room reads the log,
it marks the ids of every table a committed [table event](#table-events)
named since the last read: local commits, sync, rebuilds and authority
changes. Room refreshes after its own transactions; `followRocky` in
[Room integration](room-integration.md) refreshes it after every other commit,
so DAO observers see sync writes. A rolled-back transaction marks nothing.

Rocky creates the tables, so Room never runs its `onCreate` and finds the
version it expects. A Room migration or a destructive fallback fails at its
first schema change; the replicated schema changes through Rocky's
[generations](generations.md) instead.

## Table events

`Database::observe_tables` returns the committed
[table events](queries-and-observation.md#table-events) of every table or
of named ones: local commits, sync, rebuilds and authority changes. A
driver uses them to invalidate what it read.

## Native handles

`sdks/native` exports `NativeDatabase.connect`, `NativeConnection`,
`NativeStatement` (`execute`, `next`, `finish`) and `NativeTableEvents`.
Values cross as the `NativeValue` enum and pages as `NativePage`; one call
executes a statement or delivers a page, never a JSON call per step. A
statement failure is `NativeError.Sql` with its code.

## The Kotlin driver

`sdks/kotlin/rocky-sqlite` implements `androidx.sqlite.SQLiteDriver` 2.6.2 on
these handles, with no Room dependency, so any ORM on that interface can use
it:

```kotlin
val driver = RockySQLiteDriver(database) // a RockyDatabase
val connection = driver.open("shopping")  // the name names no file
```

- The data is the database's. Room on the JVM still keeps a `<name>.lck`
  lock file beside the name it is built with, so give it a path in the
  application's directory.
- `hasConnectionPool` is false: the ORM pools connections and uses each from
  one caller at a time. `PRAGMA query_only = 1` makes a connection a
  reader, as ORMs mark theirs, and `= 0` a writer again.
- A statement runs at its first `step` or column query, after its
  parameters are bound, and then steps through its pages. `reset` drops the
  rest; the next `step` runs it again with the same bindings.
- Getters convert as SQLite's column functions do, qualified against the
  bundled driver. Rocky's scalar codec has one zero: `-0.0` reads back as
  `0.0`.
- A failure is an `androidx.sqlite.SQLiteException` with
  `Error code: <code>, message: <message>`, as the bundled driver words it.

Run `./kotlin test -m rocky-sqlite`.

## Qualification

- `cargo test -p rocky-sdk --test driver` and
  `cargo test -p rocky-storage --lib cursor`: `rocky_next_id` reservation,
  release and refusals; a parent and two children as one bundle; nested
  savepoints; net-change and empty transactions publish nothing; 2,049 rows
  and 5 MiB fail with code 18 while exactly 2,048 rows commit as one bundle of
  64 frames; a 200 ms lease aborts with code 4 until `ROLLBACK`; cursors and
  read transactions keep their snapshot while the writer commits; one table
  event covers both tables of a bundle on a peer.
- `cargo test -p rocky-sdk --test driver room_`: every housekeeping answer,
  refusal of entities without identity fields or with an autoincrement key,
  and schema pragmas compared with ordinary SQLite on the file.
- `./kotlin test -m rocky-sqlite`: a JVM `SQLiteDriver` transaction with a
  nested savepoint, a reader's snapshot across a commit, 700 rows over pages
  and a rerun after `reset`, codes 18, 1555, 23 and 4, and getter parity with
  `BundledSQLiteDriver` over 16 values.
- `./kotlin test -m room-qualification` is the acceptance reference for the
  pinned Room version; see [Room integration](room-integration.md#build-and-verification).
  The 49 distinct statements Room 2.8.4 sends are pinned in
  `sdks/kotlin/room-qualification/fixtures/room-2.8.4.statements`
  (`ROCKY_RECORD_ROOM=1` rewrites it after an intended upgrade) and replayed
  through the Rocky driver: no housekeeping statement fails and every DAO
  statement returns the bundled engine's rows and codes; only Room's upsert
  probe fails, with the bundled engine's code.
