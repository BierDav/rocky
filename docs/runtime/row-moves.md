# Moving a row between partitions

A move keeps the row's numeric `id`, creation proof and original field stamps.
It commits a deletion in the source and an insertion in the destination in
one [secure bundle](../decisions/0018-secure-transaction-bundles.md). A reader
holding both partitions observes both changes together. A reader holding one
side verifies that side with its own key and grants.

Use `SecureSession::move_row` or `SecureSql::move_row` in Rust. The native
request is `move_row`, with a 32-byte stable `handle`, `table`, decimal-string
`id`, and `from`/`to` arrays of typed cells in partition declaration order.
This is a dedicated commit; end an existing SQL transaction first. Retrying
the same handle and inputs returns the original frames. Reusing the handle
for different inputs is refused.

Kotlin exposes the same operation without requiring Rust code:

```kotlin
database.moveRow(handle, "items", item.id, listOf(sourceList), listOf(targetList))
```

With Room, call outside a Room transaction:

```kotlin
room.moveRockyRow(database, handle, "items", item.id,
    listOf(sourceList), listOf(targetList))
```

Room refreshes invalidation after commit. DAOs read the kept ID in the
destination. A move is not an ordinary SQL update of partition columns, and
a delete/insert pair creates a new incarnation instead of preserving one.
The Trusted `TypedApp::move_row` returns a precise mutation for its admitted
schema; its runtime issues original-author follows through the stored adapter.
Trusted's existing schema capability restrictions still apply.

## Admission and related rows

The mover needs source delete and destination insert rights on every moved
table, including every destination field. It must hold both partitions of
every in-group child table reached through `CASCADE`, even when that child
table is empty. The move includes the transitive child closure automatically.
A missing table, denied field, unsupported reference or bundle limit refuses
the complete mutation and names the cause.

References composed only of partition columns resolve in the destination.
References to collected parents move with them. Nullable `SET NULL` references
to parents staying in the source lose their nonpartition reference fields.
References to unpartitioned parents retain their values. A table partitioned
by its own `id` cannot move.

## Offline updates and recovery

Only the device that authored a missed update issues it again. It must hold
the destination and have current update rights. It carries the original field
stamp, so a later destination edit wins. Other devices do not sign on its
behalf. A source-only author issues no follow. The source link is encrypted
with the source key and is not a cross-partition causal dependency.

Links permit successive moves while the author can open the intervening
source links. Original-operation/field/destination markers commit with the
follow frames and outbox. Repeated sync and reopening do not duplicate the
follow. Source finalization ends following; the destination horizon also
fences old carried stamps. Checkpoints authenticate the links, birth proofs
and stamps with the row records, plus an additive `moves` summary in format 6.

A consumed birth behind a finalized horizon advances the operation log but
has no materialized row. `RebootstrapReport.rejected` reports its hash and
reason; it is not counted in `reapplied`.

The proof establishes historical authorship, not globally exclusive custody.
Authorized concurrent moves to different destinations can form distinct
branches when readers hold disjoint keys. See [decision 0038](../decisions/0038-row-moves.md)
for the proof, privacy and format rules.
