# 4. Partition-aware constraint identities

Status: not started.
Owner: `rocky-secure` constraint resolution; `rocky-core` installability.
Depends on [phase 2](02-partition-groups.md). Wave 1. On the critical path.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [what exists today](scope-partitioned-constraints.md#what-exists-today),
requirements [2](scope-partitioned-constraints.md#2-partition-aware-constraint-identities)
and [3](scope-partitioned-constraints.md#3-unique-indexes-with-a-policy-on-partitioned-tables)
of the scope, and
[design rationale](../runtime/mergeable-constraints.md).
Resolution holds one scope per table and identifies a row by `(table, id)`.
An `id` is unique only within its partition, so lifting the refusals without
changing these identities would merge rows of different partitions.

## Ordered slices

1. Identities: candidates, seeds, support bookkeeping and puppets are keyed by
   partition and `id`. The private candidate table's key includes the
   partition columns.
2. Resolution per partition: resolve, rebuild and evict one partition without
   reading or changing another. No step needs a key the device does not hold.
3. Unique indexes with a conflict policy on a partitioned table. The index
   must include every partition column; rows clash only within one partition;
   the later stamp wins as in [mergeable constraints](../runtime/mergeable-constraints.md).
4. Lift the native refusal for this shape in `LogicalTable::installable`;
   replace `partitioned_tables_refuse_constraints_and_command_control` by
   tests of the shapes that stay refused.
5. Reopen and rebuild derive the same winners as live application.

Known gaps in `rocky-secure/src/replica/constraints.rs` beyond the scope's
list; the search is not complete:

- `Constraints::node(scope)` matches scopes exactly while `Typed::by_scope`
  matches partitions, so `seed_existence`, `reproject` and `clear_typed`
  silently skip partition scopes today. Make a miss an error, not `Ok(())`.
- The candidate table also has a unique index on `id` alone.
- Puppet and virtual-row SQL (`row_kind`, `set_live`, `secure_support`
  probes) selects by `id` alone.
- `rebuild_constraints` is whole-installation and clears all of
  `secure_support`; `follow` reads winners by table scope.
- A unique index's `SetNull` policy would null partition columns; refuse it
  on partitioned tables or exempt them.
- The loss cache, seeds and `secure_support(tab, id)` are keyed by
  `(node, id)`, and reference selects match the parent `id` alone; the
  candidate primary key already holds the partition columns.
- The equal-stamp tie breaks on `id` since 2026-10-08. With partitions it
  must break on partition and `id`.
- The secure profile never compares the engine catalog at open, so a dropped
  unique or identity index goes unnoticed and resolution loses its backstop.
  Decide here whether partitioned indexes are verified.

Foreign keys stay refused until [phase 5](05-partitioned-foreign-keys.md).

## Completion gate

- Two offline devices add the same title to list A; both converge under
  `LatestWins`. The same title in A and B stays visible in both.
- A device with A only resolves A's index without B's key and receives
  nothing of B.
- Evicting and rebuilding A leaves B's winners and hidden rows unchanged,
  checked by row and by hidden state.
- Equal `id`s in two partitions never share a candidate, seed or support row.
- A unique index that omits a partition column is refused; uniqueness across
  partitions stays refused.
