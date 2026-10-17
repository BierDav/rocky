# Secure SQL

SQL over the typed tables of an `EncryptedReplica`, exposed by SDK SQL
requests, Kotlin client wrappers and the [SQL driver](sql-driver.md).
Transaction semantics follow the trusted [SQL sessions](sql-sessions.md); a
commit is one [secure bundle](secure-sessions.md#bundles).

`commit_reauthoring(handle, held_id)` commits a mapped [held change](held-changes.md)
with fresh stamps and resolves its ID in the same transaction. Kotlin exposes
this as `RockySession.reauthor(id) { ... }`.

A [row move](row-moves.md) uses `SecureSql::move_row` as a dedicated atomic
mutation, carrying children, identity proofs and field stamps.

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

The SDK calls `install_readable_tables`: unreadable replicated tables are physically
absent, including their constraint candidates. Read selectors allow a partitioned
table even before its first partition key arrives. Referenced existence support
and local-only tables remain installed. The full descriptor is still checked
against the signed manifest; reopening preserves the selected projection.
The lower-level `install_tables` API installs all declared tables.
See [the projection and custody decision](../decisions/0061-readable-projections-and-checkpoint-custody.md)
for format and access-change rules.

## Row identities

The identity contract is in [row identity](logical-schema.md#row-identity).
Secure specifics:

- A numbered node credential is installed with `SecureConfig`. Its node key
  signs all granted tables; no table key registration is needed after open.
- `SecureSql::next_id(table)` (`next_id_partition` for partitioned tables)
  reserves a `RowIdentity` inside an active transaction; insert its
  `integer()` in `id`. Local inserts must continue the own creation log
  without gaps. Shared SQL capture enforces the identity rules on local-only
  tables too.
- Frames bind the author number and row format in the signed extension and
  encryption AAD. A bundle seals each member's signing key with its opening,
  and every member is signed by the node.
- An insert must bind its `id`'s author to the frame's authenticated node number
  and exceed that author's own-birth mark in that partition; carries never
  raise it. Exact
  previously admitted births and carries use the
  [admission evidence rules](admission-evidence.md). Duplicates
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
| Key deleted | Permanent tombstones of the visible and other held incarnations at that key |
| Equal rows | Nothing |

Deletion identifies each live incarnation by its validated numeric `id` and
stored key binding. It emits the visible delete once, even when added nullable
or defaulted columns make the SQL image differ from the sparse stored image.
Other live IDs at that concrete scope/key retain their original before images
and empty deletion masks; indexed tombstones add no delete.

The typed table shows the live incarnation with the greatest creation stamp at
each natural key; a private key-to-incarnation index, maintained in the same
transaction and backfilled at open and restore, narrows that lookup. A commit
that changed only local tables publishes no frame. Committing again under a
used handle rolls the new transaction back and returns the recorded frames
when the rows are the same, and conflicts otherwise; this also holds after a
flush failed once the engine committed.

Partition-complete `LatestWins` unique indexes resolve per partition, with the
real index enforcing local writes. Rebuild and eviction preserve other
partitions' visible and hidden rows. Unique indexes with a conflict policy and
identity foreign keys are in
[mergeable constraints](mergeable-constraints.md).

## Leases and limits

A transaction holds the replica until it commits, rolls back, is cancelled,
times out or its session is dropped. Every other call of the replica waits,
receipt and reads outside the transaction included. `SqlOptions` bounds it as
in [SQL sessions](sql-sessions.md#limits). A caller that awaits another call of
the same replica inside its own transaction waits for its timeout.

`acknowledge` decides what a commit waits for. `Flushed`, the default, flushes
the device and runs the scope's maintenance step before it answers, so its
frames may be sent anywhere. `Buffered` answers once the engine committed: the
frames must not leave the device except through a call that flushes first,
such as a transfer page, and `EncryptedReplica::flush` and `maintain` finish
the commit afterwards. Power loss can drop a buffered commit that nothing
exposed ([durability](write-concurrency.md#durability-and-exposure)).

A policy change or credential renewal waits for an open transaction. That
transaction commits under the authority it began with; later statements are
refused.

## Consumer requests

The language-neutral session accepts `sql` requests (a statement and tagged
`values`) and `next_id` with a table name. Bind an identity as an `integer`
cell whose `value` is the decimal string. The same cached SQL connection owns
the lease. A `COMMIT` request may carry a 32-byte `handle` for durable recovery
through `mutation_progress`; retries must keep the same transaction input.
A session has one connection and so at most one open transaction, shared by
every caller of that session: a concurrent `BEGIN` is refused, and any
caller's `COMMIT` or `ROLLBACK` ends it. Serialize transactions per session,
or open a session per concurrent writer.
Kotlin `RockyClient.sql` and `nextId` wrap these. Integer values in rows and
checkpoint incarnation IDs travel as strings; numeric JSON identities are
rejected.

## Staged publications

`SecureSql::commit_publication(publication)` commits the open transaction,
of any size, as consecutive ordinary bundles within the
[bundle bounds](secure-sessions.md#bundles): rows of referenced tables
first, new identities in order, deletions of referencing rows before their
parents. Every write carries the commit's stamp. Bundle `n`'s handle is
`rocky.secure-publication.v1` over `publication` and `n`, and the record of
the publication commits in the same transaction; committing it again returns
the recorded frames and rolls the new transaction back. A transaction that
changed nothing is recorded too. Raise `SqlOptions::captured_rows` and
`captured_bytes` for it; the transaction timeout still applies.

`EncryptedReplica::publications()` lists each record with its bundles and how
many leading ones some peer's frontier holds. Generic publications remain
subject to `RetentionLimits::hard_bytes`. Only an authenticated migration stage
can journal an issuer-endorsed descriptor and exact frame inclusion proofs for
the separate migration-history account. Endorsement happens after SQL commit,
outside the transaction, before any private candidate is exposed. The SDK
commits [migrations](generations.md#migrations) this way; see
[decision 0071](../decisions/0071-staged-migration-publication.md).

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
- A transaction beyond one bundle commits as a staged publication: the
  first bundle applies alone, an edit made before the second arrives wins,
  progress follows a peer's frontier and a repeated commit returns the same
  frames (`tests::sql::publication`).
- Identity: three replicas with reversed delivery, an N:M pair unlinked and
  relinked in one transaction, concurrent deletion, a late edit to the old
  `id` and checkpoint bootstrap agree; a wrong author number and a reused
  sequence are refused. The SDK consumer regression covers node credential
  installation, allocation, `RETURNING` and reopen with the next sequence.

Not qualified: row-level read policies and expiry while a transaction is open.
