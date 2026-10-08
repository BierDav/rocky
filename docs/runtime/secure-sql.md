# Secure SQL

SQL over the typed tables of an `EncryptedReplica`, exposed by SDK SQL
requests, Kotlin client wrappers and the [SQL driver](sql-driver.md).
Transaction semantics follow the trusted [SQL sessions](sql-sessions.md); a
commit is one [secure bundle](secure-sessions.md#bundles).

## Install typed tables

The replica's application is `rocky_core::TypedRows` over a
[logical schema](logical-schema.md), and its signed manifest is the schema's
manifest for the replica's domains.

```rust
let app = TypedRows::new(schema.clone(), vec![domain.domain])?;
let replica = EncryptedReplica::open(path, app, config).await?;
replica
    .install_tables(schema, domain, [("items".into(), public), ("salaries".into(), payroll)].into())
    .await?;
```

Every mergeable table names its reader scope; the resource scope of its rows
is that domain, reader scope and the table's id. A row's grant identity is a
commitment to the domain, table and canonical business key; its numeric `id`
names one incarnation, and every column is one field. The installation is
recorded in the file and loaded at open; another schema is refused.
Local-only tables are installed and never replicated.
[Command-controlled tables](confirmed-tables.md) are installed read-only.
Derived columns, `confirmed_read_only` tables and names beginning `secure_` are
refused.

Typed rows follow every stored row change in the same transaction: authoring,
receipt, rebuild, rebootstrap and eviction. Authorized typed tables are
plaintext at rest, as stored rows are.

## Row identities

The identity contract is in [row identity](logical-schema.md#row-identity).
Secure specifics:

- After each open a writer registers the externally held table pseudonym with
  `EncryptedReplica::register_row_author(key, credentials)`. The chain binds
  the signing key to one table and author number within the device's grants.
  A reopened SDK installs only pseudonyms valid for its current manifest,
  policy and period; refresh the pseudonym after a generation or policy change.
- `SecureSql::next_id(table)` (`next_id_partition` for partitioned tables)
  reserves a `RowIdentity` inside an active transaction; insert its
  `integer()` in `id`. Local inserts must continue the own creation log
  without gaps. Shared SQL capture enforces the identity rules on local-only
  tables too.
- Frames bind the author number and row format in the signed extension and
  encryption AAD. A bundle seals each member's signing key with its opening,
  so one transaction may use a different pseudonym per table.
- An insert must bind its `id`'s author to the frame's authenticated pseudonym
  and exceed the permanent sequence mark for that author and scope. Duplicates
  settle first; refused frames and rolled-back transactions never advance the
  mark. Marks survive pruning, eviction, rebuild and reopen, and checkpoints
  sign them with the incarnation rows and creation stamps.

## Run statements

```rust
let sql = replica.session(scopes).await?.sql(SqlOptions::default()).await?;
sql.run("BEGIN IMMEDIATE", vec![]).await?;
let id = sql.next_id("items").await?;
sql.run("INSERT INTO items(key,title,id) VALUES(?,?,?)",
    vec![key, title, SqlValue::Integer(id.integer())]).await?;
let committed = sql.commit(handle).await?.committed;
```

Transaction control, savepoints, conflict clauses, per-session counts and the
`Aborted` protocol follow [SQL sessions](sql-sessions.md#transactions). A
statement outside a transaction commits by itself; `commit` names the handle,
and plain `COMMIT` chooses a random one.

| A session may | Tables |
| --- | --- |
| Read | Typed tables of its readable scopes, and local tables |
| Write | Mergeable tables of its writable scopes, and local tables |

Anything else is `SqlError::Denied`; a statement that writes a table the
session may not write ends its transaction. Each statement checks the
session's authority and credentials. A read of a typed table whose key is
missing, whose scope is not held or whose operations wait to be applied fails
with [`SqlError::Unavailable`](queries-and-observation.md#availability).

Partitioned tables need an execution-time proof per reference: every partition
column constrained to held equality/`IN` values (possibly through an inner
join), or `row_kind = 0` asking for held real rows. `OR` supplies no proof;
reserved row kinds are refused; views are expanded and checked; a `LEFT JOIN`
proof for its right side may appear in `ON`. Updates and deletes use the same
proof. See [partitioned scopes](partitioned-scopes.md#sql-and-completeness).

## What a commit publishes

Each changed row becomes one `RowWrite` with its exact before and after rows.
All of them commit as one bundle in the engine transaction that holds the
statements, or none do. The commit admits every changed row and field: a table
writable for updates still refuses an insert without the insert right, and the
whole transaction rolls back.

| Change | Write |
| --- | --- |
| New key | Insert of the whole row |
| Changed columns | Update; under `assigned` also every assigned column |
| Key deleted and inserted again | Delete of the old `id` and insert of a new `id` |
| Key deleted | Delete of that incarnation, a permanent tombstone |
| Equal rows | Nothing |

The typed table shows the live incarnation with the greatest creation stamp at
each natural key; a private key-to-incarnation index, maintained in the same
transaction and backfilled at open and restore, narrows that lookup. A commit
that changed only local tables publishes no frame. Committing again under a
used handle rolls the new transaction back and returns the recorded frames
when the rows are the same, and conflicts otherwise; this also holds after a
flush failed once the engine committed.

Unique indexes with a conflict policy and identity foreign keys are in
[mergeable constraints](mergeable-constraints.md).

## Leases and limits

A transaction holds the replica until it commits, rolls back, is cancelled,
times out or its session is dropped. Every other call of the replica waits,
receipt and reads outside the transaction included. `SqlOptions` bounds it as
in [SQL sessions](sql-sessions.md#limits); `acknowledge` is ignored because a
commit always flushes before it answers. A caller that awaits another call of
the same replica inside its own transaction waits for its timeout.

A policy change or credential renewal waits for an open transaction. That
transaction commits under the authority it began with; later statements are
refused.

## Consumer requests

The language-neutral session accepts `sql` requests (a statement and tagged
`values`) and `next_id` with a table name. Bind an identity as an `integer`
cell whose `value` is the decimal string. The same cached SQL connection owns
the lease. A `COMMIT` request may carry a 32-byte `handle` for durable recovery
through `mutation_progress`; retries must keep the same transaction input.
Kotlin `RockyClient.sql` and `nextId` wrap these. Integer values in rows and
checkpoint incarnation IDs travel as strings; numeric JSON identities are
rejected.

## Qualification

`cargo test -p rocky-secure --lib tests::sql` and
`cargo test -p rocky-core typed_rows`:

- A transaction over two reader scopes and a local table commits one bundle;
  a public reader projects and reads its table only; a full reader shows
  neither table until the whole bundle arrived.
- Protocol tables, the schema catalog, pragmas, schema changes and
  attachments are refused; read-only and field-limited principals cannot
  write, and a refused commit leaves no row.
- A learned policy waits for the lease and ends the session afterwards;
  renewed credentials open a new one; logout leaves nothing readable.
- Cancel, timeout and a dropped session roll back and free the replica.
- Handles recover a commit across a restart; both update policies; concurrent
  edits converge.
- Identity: three replicas with reversed delivery, an N:M pair unlinked and
  relinked in one transaction, concurrent deletion, a late edit to the old
  `id` and checkpoint bootstrap agree; a wrong author number and a reused
  sequence are refused. The SDK consumer regression covers pseudonym
  assignment, allocation, `RETURNING` and reopen with the next sequence.

Not qualified: row-level read policies and expiry while a transaction is open.
