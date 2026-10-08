# Queries, snapshot cursors and table events

The SQL surface, catalogs, snapshot cursors, availability and table events
shared by [SQL sessions](sql-sessions.md), [bound databases](database-bindings.md)
and [secure SQL](secure-sql.md). The native ABI reaches cursors and table
events through [driver connections](sql-driver.md); catalogs are Rust-only.

## Supported SQL

Every application statement is parsed in the SQLite dialect before the engine
sees it, and its table names are resolved by position, never by replacing text.

| Statement | Status |
| --- | --- |
| `SELECT` with aliases, joins, subqueries, CTEs (also recursive), window functions, aggregates | Supported |
| `INSERT`, `REPLACE`, `UPDATE`, `DELETE` with `WHERE`, conflict clauses, upserts, `RETURNING` | Supported on writable tables |
| `BEGIN`, `COMMIT`, `ROLLBACK`, savepoints | Session transaction control |
| `json_each`, `json_tree` | Supported table functions |
| `PRAGMA`, `ATTACH`, `DETACH`, `CREATE`, `DROP`, `ALTER`, triggers, views, virtual tables | `SqlError::Denied` |
| `main.t`, `rocky_*`, `secure_*`, `sqlite_*`, `pragma_*`, `load_extension`, file functions | `SqlError::Denied` |
| Several statements, `EXPLAIN`, `INDEXED BY`, `VACUUM`, a CTE named like a table | `SqlError::Unsupported` |
| Anything the parser does not accept, for example `x IS y` with a column operand | `SqlError::Unsupported` |

Nothing compiles or runs for a refused statement. `Denied` ends an open
transaction; `Unsupported` leaves it open, as a syntax error does. No compatibility with
every SQLite program or ORM is claimed; an ORM's statements must stay inside
this surface.

## Catalogs

`SqlSession::catalog()` and `SecureSql::catalog()` list the tables a session
may name, under the names it uses: columns with affinity, nullability,
collation and key position, indexes and whether the session may write the
table. Protocol tables, other tenants, other scopes and physical names never
appear. Engine catalogs (`sqlite_schema`, pragmas) stay denied.

## Snapshot cursors

```rust
let mut cursor = session.cursor(sql, params, CursorLimits::default()).await?;
cursor.columns();            // names and declared types, fixed for its life
cursor.revision();           // the snapshot's revision (one-domain sessions)
let page = cursor.next().await?; // rows, done
cursor.reset().await?;       // the same snapshot from its first row
drop(cursor);                // ends the read transaction now
```

A cursor reads the latest committed snapshot on its own read-only connection.
It does not see the session's open transaction and holds no writer lease, so
writes and sync commit meanwhile without changing its pages. Inside a
transaction, `run` reads with the transaction's own snapshot.

| Limit | Default | When exceeded |
| --- | --- | --- |
| `page_rows` | 256 | The next page continues |
| `page_bytes` | 1 MiB | The row opens the next page; a larger single row is `Quota` |
| `idle` | 30 s | `Aborted(Timeout)` |
| `lifetime` | 5 min, at most 5 min | `Aborted(Timeout)` |
| Open cursors per session | 64 | `Quota` |

While a cursor is open the WAL cannot be reset, so the lifetime bounds WAL
growth; the writer's truncating checkpoint is retried later. `cancel` ends a
running step with `Aborted(Cancelled)`. Closing the replica or file ends every
cursor and waits for its connection. A secure cursor checks its session's
authority and credential expiry before stepping each page and before
delivering it: after a revocation, key change, logout or expiry, the stepped
page is discarded and the cursor ends with `Unavailable(Revoked)`.

Cursors and joins stay inside one file. Bound tables of different files are
refused at `open_bound`; remote reads keep their explicit
[replica role](replica-roles.md) API outside local transactions.

## Availability

A secure read is refused with `SqlError::Unavailable { table, reason }`
before it runs when a named typed table cannot be answered locally:

| Reason | Cause |
| --- | --- |
| `KeyMissing` | The reader key of the table's scope is not installed |
| `NotHeld` | The scope is not held: unsubscribed, evicting or evicted |
| `Incomplete` | Admitted operations of the scope wait to be applied, such as part of a bundle |
| `Revoked` | The cursor's session lost its authority |

The check covers every table the statement names, the nullable side of a
`LEFT JOIN` included, so missing coverage never appears as NULLs. A held,
complete scope without a matching row, a tombstone and a permitted NULL
companion are ordinary SQL results. A complete scope does not prove that every
peer's newest write arrived. Trusted domains are whole-domain replicas;
their dependency backlog is reported as `Completeness`.

## Table events

```rust
let mut observer = replica.observe_tables(Some(&["notes"]))?; // or database., secure.
let registered = observer.registered();   // revisions when it registered
// ... read the initial snapshot ...
let change = observer.next().await?;      // tables, revisions, causes
```

Register before reading the initial snapshot: every later commit that changes
an observed table is reported. A change names the observed tables, each
domain's revision after it and its causes: `Local`, `Sync`, `Confirmed`,
`Rebuild` or `Authority`. A read started after the change observes at least
those revisions. Rolled back or open transactions are never reported.

Changes coalesce: an observer holds one pending change however many commits
arrive. Publication visits only the observers of the changed tables. Authority
changes, key loss and eviction wake every observer with `Authority`; closing
the domain ends observers with `Error::Closed` after their last change. A bound
database reports changes under the bound names.

## Qualification

`cargo test -p rocky-sdk --test queries --test domains`,
`cargo test -p rocky-secure --lib tests::sql` and
`cargo test -p rocky-storage --lib -- statement cursor observe`:

- A definition-built schema commits a two-table transaction with nested
  savepoints, answers an indexed join through a cursor and reopens; a changed
  definition is refused.
- Denied and unsupported statements, catalogs and table functions fail before
  any effect; catalogs list no protocol table or other tenant.
- Cursors keep their snapshot through concurrent deletes and inserts, page by
  rows and bytes, reset, expire, cancel, end on revocation and on close.
- A secure session granted a scope without its key gets `Unavailable` for a
  `LEFT JOIN`; a partial bundle is `Incomplete`; an absent companion is NULL.
- Local commits, incoming sync and eviction wake only the right observers, with
  revisions covering the commit; rollbacks wake none.

Not measured: cursor throughput and observer fan-out.

## Design rationale

- Statements are parsed (`sqlparser`, SQLite dialect) and table names resolved
  by position in the tree. The earlier token rewriter guessed which words were
  tables and could misread a column, alias or literal; do not return to text
  replacement. A reference without an alias becomes `"physical" AS name`;
  `RETURNING` qualifiers name the physical table because the engine resolves
  no alias there; a CTE shadowing a bound table is unsupported, not guessed.
- The surface is declared, not universal: what the parser rejects fails even
  where SQLite would run it. The engine's authorizer still decides every
  access.
- A cursor uses its own read-only connection and thread and never a writer
  lease. A pinned snapshot keeps the WAL from being reset, which is why the
  lifetime is capped.
- Missing coverage is an error (`Unavailable`), never an empty table or NULLs:
  a reader could not otherwise tell absence from a missing key or a partial
  bundle.
- Observers hold one merged pending change and register before reading the
  revision, so a commit is either inside the registered revision or reported;
  publication happens only after the file's group commit.
