# 6. Horizon, checkpoints and bootstrap

Status: not started.
Owner: `rocky-secure` horizon, checkpoints and rebuild. Depends on phases
[4](04-partition-aware-constraints.md) and [5](05-partitioned-foreign-keys.md).
Wave 3.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read requirement [5](scope-partitioned-constraints.md#5-horizon-checkpoints-and-bootstrap)
of the scope and [design rationale](../runtime/mergeable-horizon.md).
Horizon finalization resolves its dependencies per installed table scope, not
per partition. A child's statement embeds each parent's cut, and an
acknowledger must hold the parent at that cut; for in-group references that
is the parent's same-partition scope. Required peers are checked only
against the finalized scope today, so nothing checks that they hold the
related partitions.

## Ordered slices

1. Horizon dependencies per group partition: a cut of `items` in list A
   depends on `lists` and `categories` in A only.
2. Finalization: hidden rows and orphans of a partition become final at the
   agreed horizon; cascaded children of a deleted parent are dropped with it.
3. Checkpoints: candidates stay derived and are not replicated. A checkpoint
   of one table and partition carries what its rebuild needs and nothing of
   another partition. Checkpoints already carry no candidates, support or
   hidden marks, so this is local rebuild work. Any change to the checkpoint
   is recorded in a decision record.
4. Bootstrap and rebootstrap of one group partition rebuild winners, hidden
   rows and support rows; other partitions are untouched.
5. A horizon in one partition never waits for a device that does not hold it.

Known gaps in `rocky-secure/src/replica`; the search is not complete:

- `typed.rs` and `existence/base.rs` call `rebuild_constraints(None)` inside
  the finalize, rebootstrap and base-import transaction, rebuilding every
  table. Scope the rebuild to the affected partition.
- `horizon.rs`, `pending_horizons`, decodes every stored horizon and its
  checkpoint record on each write and frame admission. Per-partition horizons
  multiply that; persist a pending flag or index.
- Finalize is one transaction proportional to the scope's rows and filters
  `row_kind = 0` only on logged tables; unlogged support parents rely on
  virtual rows always having id 0.
- The checkpoint `format` number encodes which optional parts are present.
  After [phase 1](01-remove-compatibility-code.md) it is one version with
  optional fields; add per-partition state as a field, not a new number.

## Completion gate

- After the horizon, a deleted list's items and a lost duplicate title are
  gone from every device holding the list, and a long-offline device
  rebootstraps to the same rows.
- A partition is evicted and rebootstrapped from a checkpoint; its winners
  and hidden rows are rebuilt; another partition's rows, marks and hidden
  state are byte-equal before and after.
- A device holding only B finalizes B while A's horizon is still waiting.
- An interruption at each step of finalization recovers without a partially
  finalized partition.
