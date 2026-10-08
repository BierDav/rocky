# Single-copy typed rows

Status: deferred. The current design is recorded in
[design rationale](../runtime/logical-schema.md#design-rationale).

## Trigger

Build this when the file size or write cost of a typed-table application
matters: a device storage quota, or rows with large TEXT or BLOB columns whose
second copy dominates the file.

## Current cost

A mergeable value is stored in two tables of the projection; see
[logical schemas](../runtime/logical-schema.md):

- `rocky_incarnations` holds every incarnation with its key, all column
  values and one stamp per column. Merging reads and writes it.
- The typed table holds the visible incarnation as the ORM declared it.
  Queries read it.

Both are checkpoint tables, so each has a checkpoint copy. A value is in the
file four times, and once more in the operation history until it is pruned.
Every applied operation writes both tables, and replay writes both again.

| Application | Projection copies | With the checkpoint copy |
| --- | --- | --- |
| Compiled adapter with stamp columns, such as todo | 1 | 2 |
| Register application (`sdk_rows` value and registers) | 2 | 4 |
| Typed tables | 2 | 4 |

No measurement exists yet; take one before choosing an option.

## Sketch

### Values only in the typed table

Keep the typed table as the only store of the visible row's values. The
private table keeps the stamps and tombstones of every incarnation, and full
values only for hidden incarnations, which exist after concurrent inserts at
one key and are rare.

- Removes the duplicate in the common case: two copies with the checkpoint.
- The reducer loads its state as rows of one shape across all tables
  (`StoredApplication::touched` and `window`). It would have to read the typed
  row of each table as well, so that contract needs per-table row shapes or a
  second read step.
- Hiding an incarnation moves its values into the private table, and showing
  it moves them back. Both must stay in the one projection transaction.
- A direct SQL write to the typed table changes canonical state. This fits
  [row capture](../runtime/sql-sessions.md),
  and makes an uncaptured write a corruption instead of a stale projection.

### Typed tables outside the checkpoint

Checkpoint only `rocky_incarnations` and rebuild the typed tables from it
after an import. Three copies remain.

- Needs a runtime hook that runs application statements in bounded steps
  after a checkpoint import and before the domain serves reads.
- The audit no longer covers the typed table, so the rebuild or a separate
  comparison must.
- Lifts the reason local-only columns are refused in mergeable tables, if the
  rebuild keeps them by key.

This combines with [lean checkpoints](lean-checkpoints.md), which reduces the
checkpoint copy itself.

## Rejected: the typed table as a view

A view over the private table with `INSTEAD OF` triggers, as PowerSync's
default JSON-backed tables work, would store each value once. It was
considered and not selected:

- Room 2.8.4 compares `PRAGMA table_info`, `index_list` and `foreign_key_list`
  of every entity at open. A view reports no key or NOT NULL flags and no
  indexes.
- Room's invalidation tracker creates `AFTER` triggers on entity tables, and
  SQLite allows only `INSTEAD OF` triggers on a view. Flows would not refresh.
- Declared indexes would become expression indexes over a decode function,
  used only when a query repeats the expression exactly.
- Affinity, key collation and NOT NULL would be emulated instead of enforced
  by the engine.
- Values are a binary blob with stamps and a key can have several
  incarnations, so the view needs a custom function or JSON storage, and a
  stored flag for the visible row.

A view remains an option for a consumer that is not an ORM with schema
validation. `INSTEAD OF` triggers are a capture mechanism and belong to
row capture, where real tables can use the preupdate hook or `AFTER`
triggers instead.

## Must not break

- The typed table must match the descriptor that
  `Replica::logical_schema()` reports, and Room's own validation.
- Replicas with the same applied operations must hold the same rows, in any
  delivery order, including hidden incarnations.
- A late update must not reach a deleted incarnation or its replacement.
- The audit must still detect a projection that differs from its history.
- Rebootstrap must leave typed rows equal to the imported incarnations.
