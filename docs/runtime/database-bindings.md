# Database bindings, placement groups and migration

A `rocky_core::DatabaseBinding` lets one [SQL session](sql-sessions.md) of
the trusted runtime name the tables of several domains in one file. Rust API
only; the SDK, native ABI and `EncryptedReplica` do not use it. It needs
[explicit placement](wide-schemas.md#explicit-placement).

## The identities involved

| Concept | Where it lives |
| --- | --- |
| Application definition | The installed app and its manifest |
| Invariant domain | `Scope`; one worker, author sequence, revision and policy |
| Logical database binding | `rocky_core::DatabaseBinding`: visible table name to one table of one domain |
| Atomic placement group | The domains of one binding; they share one file slot |
| File slot | `DomainPlacement::file`; private placement, not identity and not a grant |

A domain with several tables is one domain. A binding grants nothing: every
operation is still admitted by its own domain's installed policy.

## Describe a binding

```rust
// The generated default: one tenant, every table under its own name.
let tenant = DatabaseBinding::single("shopping", scope, &schema)?;
// Two tenants of one schema in one session need generated aliases.
let both = DatabaseBinding::new("both")
    .domain_as("a", first, &schema)?   // a_shopping_items, ...
    .domain_as("b", second, &schema)?; // b_shopping_items, ...
// Or exact names, table by table.
let exact = DatabaseBinding::new("exact").table("items", first, "shopping_items")?;
```

Names compare without case. A name bound twice, or one table under two names,
is `BindingError::Ambiguous`; `domain` on two domains of one schema therefore
fails and nothing is unioned. Names beginning `rocky_` or `sqlite_` are
refused. A binding holds at most 4,096 tables of 64 domains.

Reusing a schema for another tenant is another binding with the same
statements. Repositories pass no domain to a statement, and a caller's
`WHERE` clause never selects one.

## Open and use

```rust
let runtime = Runtime::open_bound(path, installations, placement, vec![both], options).await?;
let database = runtime.database("both")?;
let session = database.sql_session(SqlOptions::default()).await?;
session.run("BEGIN", vec![]).await?;
session.run("INSERT INTO a_shopping_items(id,title,completed) VALUES('x','Milk',0)", vec![]).await?;
session.run("UPDATE b_shopping_items SET completed=1", vec![]).await?;
let outcome = session.commit(token).await?.committed;
```

`open_bound` is `open_placed` with bindings. Before any data file is opened
or created it refuses a binding that

- names a domain this process does not install, or a table its domain lacks;
- names a domain without a placement, or domains in different file slots;
- differs from the recorded binding of that name without the next generation.

The session is an ordinary `SqlSession`. It reads and joins every bound table
and writes the `mergeable` and `local_only` ones. A table of the file that is
not bound, another tenant's included, is `SqlError::Denied` under any name; an
unbound logical name is "no such table". `Replica::sql_session` remains the
unrecorded session on one domain's own tables.

## One transaction over several domains

A transaction leases the one writer of the binding's file, as any session
does. At commit each domain seals its own rows in scope order: its
operations, typed rows, revision and token record. One engine commit then
covers all of them. If any domain refuses, by its policy, its reducer, a
capture failure or an injected fault, the engine transaction is rolled back
and no domain keeps a row, an operation, a counter or a notification.

`TransactionOutcome::domains` has one `DomainOutcome` per bound domain, with
its scope, intent, operations and revision. `operations` lists all of them
and `intent` commits to every domain's scope and intent. The token is
recorded in every bound domain; `Database::sql_outcome(token)` returns the
combined outcome or None.

Co-location makes the local commit atomic and nothing else. Each domain keeps
its own author sequence, revision, history, checkpoint and delivery, and a
peer applies each domain's operations independently.

## Recorded bindings and generations

The placement catalog stores each binding under its name with its generation
and canonical bytes, in the transaction that records assignments and before
any use. A binding passed again unchanged is accepted. Changed tables or
domains need exactly the next generation; anything else is refused. Bindings
that a process does not pass stay recorded and closed, and still constrain
[migration](#move-domains-between-file-slots).

A runtime's bindings are fixed while it is open, so no transaction can switch
tenant or domain. Each prepared `SqlStatement` is stamped with the domain, or
the binding and generation, of the session that compiled it. Executing it in
another session fails with `SqlError::Stale`; prepare it again.

## Move domains between file slots

Assignments are append-only at open. To co-locate domains, run a migration
while no runtime has the affected files open:

```rust
let recorded = migrate_placement(path, vec![DomainPlacement { scope, file: 0 }]).await?;
```

It returns the complete recorded assignment set, which the next open must
pass. Three durable steps make it resumable:

| Step | State afterwards |
| --- | --- |
| Record the intent in the catalog | The source file is still the domain |
| Copy the domain's tables, rows and indexes into the target in one transaction, then switch the assignment | The target file is the domain |
| Drop the domain's tables from the source, then clear the intent | Done |

While an intent is recorded every open of the catalog is refused; call
`migrate_placement(path, vec![])` to finish it. Another move is refused until
then. A migration is refused, with nothing recorded, when a runtime holds
either file, the target belongs to another catalog, a domain's installation
is unfinished or the result would split a recorded binding. A domain that
was assigned but never installed is only reassigned.

The moved domain keeps its identity, history, counters, outbox, token records
and checkpoint; unrelated domains and assignments are untouched. The source
file keeps its size. Keep the catalog and every file together in backups.

## What works across files

| Capability | Status |
| --- | --- |
| Sessions on different files at once, each with its own transactions | Supported |
| Processes serving disjoint files of one catalog; a second writer of a file is refused | Supported |
| Opening selected files of 4,096 domains over 256 slots | Supported |
| Moving a domain to another slot, offline | Supported |
| One transaction, statement, join or relation load over tables in different files | Blocked at `open_bound` |
| A cursor or join over the bound tables of one file, in one snapshot | Supported; see [snapshot cursors](queries-and-observation.md#snapshot-cursors) |
| Federated reads over several files with a snapshot contract | Not implemented |

## Limits

- Bindings need explicit placement; the automatic layout records none.
- A binding changes only by reopening the runtime, and a recorded binding
  cannot be removed.
- A multi-domain transaction delays every write of its file, as any lease.
- Observers of a database receive changes under the bound names.
- Commit and migration cost are not measured.

## Qualification

`cargo test -p rocky-sdk --test domains` and
`cargo test -p rocky-runtime --lib -- binding_tests`:

- Three tenant handles with the same table, key and statement text, two of
  them in one file, keep separate rows through joins, aggregates, updates and
  prepared statements, and cannot name each other's physical tables.
- A two-domain transaction commits its rows, operations and token once and
  is recovered as one outcome. A domain whose policy denies the write, a
  failing commit step and a failing engine commit keep nothing in either
  domain. A process killed before the commit leaves neither domain changed;
  one killed after it is recovered in both.
- Bindings are refused across slots, for absent domains or tables and for a
  change without the next generation, before a data file exists.
- A migration keeps rows, history and token records, lets the binding open,
  and resumes after a kill at each of its four steps.
- 4,096 assignments over 256 slots: two runtimes serve disjoint slots, a
  second writer is refused and only the two used files exist.
- Every domain's checkpoint replay reproduces its tables afterwards.

## Design rationale

- One binding is one atomic placement group in one file: WAL mode has no
  super-journal, so two files cannot commit together. A binding across slots
  is refused, never degraded to sequential commits.
- Rejected: `ATTACH` for cross-file transactions or joins (no atomic commit
  across attached WAL files, and far fewer than 256 attachments); two-phase
  intent records with compensations (a raw reader would see a partly applied
  projection); silently moving a domain when a binding needs it (placement is
  durable and must not change as a side effect of a statement); recording
  bindings in each data file (a selected-file open would see part of the
  truth; the catalog owns assignments).
- No row-dependent routing: a table belongs to one domain. Mixing domains in
  one table would need an immutable routing column and a checked domain set in
  the replicated definition; it is absent rather than approximated by a
  callback or filter. Domain identity never comes from a file name, a key or
  a statement.
- Migration is offline with the catalog as commit point, not a cross-file
  transaction: exactly one file is the domain at every catalog state, and
  opens are refused while an intent is recorded.
- Local atomicity over several domains does not make peer application atomic
  or grant cross-domain access; each domain delivers independently.
