# 5. Foreign keys on partitioned tables

Status: complete; all completion and verification gates passed on 2026-10-09.
Owner: `rocky-secure` constraint resolution and support rows; `rocky-core`
installability. Depends on [phase 4](04-partition-aware-constraints.md).
Wave 2. On the critical path.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read requirement [4](scope-partitioned-constraints.md#4-foreign-keys-on-partitioned-tables)
of the scope, [design rationale](../runtime/existence-logs.md#design-rationale)
and [existence logs](../runtime/existence-logs.md). At the start of this phase,
a reference named exactly one identity column, and a reference to another scope
was accepted only for an unpartitioned logged parent from an unpartitioned child.

## Ordered slices

1. In-group reference: both tables are in one group and the reference
   includes every partition column, so parent and child are in one partition.
   Multi-column identities (partition columns plus `id`) are accepted;
   `items.list → lists` is the case where the partition column is the parent's
   `id`.
2. `CASCADE` and `SET NULL` derived per partition. `SET NULL` nulls only the
   non-partition reference columns. Descriptor validation refuses a `SET NULL`
   key containing a `NOT NULL` or primary-key column today, so
   `(list, category)` needs that rule to exempt partition columns. In
   resolution, `image()` nulls every reference column and `required` is true
   when any reference column is `NOT NULL`; both must ignore partition
   columns, or `items.category` takes the virtual-row path and nulls `list`.
3. Holding: resolution treats a child partition without its parent partition
   as pending, never as orphaned.
4. Partitioned child to an unpartitioned logged parent, for example order
   lines by customer referencing `products`. Puppets and virtual rows use the
   partition-aware identities of phase 4.
5. Lift the native refusals for these two shapes. A reference between
   different groups, to an unlogged parent in another scope, `RESTRICT` and
   natural-key references stay refused with precise messages.

Starting gaps (resolved or bounded below):

- `rocky-secure/src/replica/horizon.rs` resolves a parent through
  `typed.by_name`, which returns the table scope, not the child's partition
  scope. Once this phase allows the reference, a cut would name a scope with
  no logs and could match vacuously. Resolve the parent from the child's
  partition tuple; [phase 6](06-horizon-checkpoints-bootstrap.md) builds on it.
- The original brief described existence record version 2 as partition-only.
  Reconciled phase 1 already makes version 2 the sole format for every log.
  This phase tests it with partitioned children of an unpartitioned logged
  parent; partitioned logs themselves remain deferred.

## Completion gate

- Deleting list A hides A's items on every device holding A; a device holding
  only B is unaffected and receives nothing of A.
- Deleting a category nulls `items.category` and leaves `list` and the item.
- Items arriving before their list stay pending and appear when it arrives.
- A partitioned child shows, hides and restores rows by its unpartitioned
  parent's existence log on a device without the parent's key.
- Two devices holding different partitions derive the same orphans for the
  partitions they share.
- One refusal test per shape that stays refused.

## Implementation and evidence

Native admission accepts complete in-group identities and partitioned children
of unpartitioned logged parents. Resolution maps references to the parent's
concrete partition, propagates changes through complete identity tuples and
preserves partition columns under `SET NULL`. Missing or unheld parent
partitions remain pending. Rebuild and eviction cover the constrained group
partition; an unpartitioned logged dependency also rebuilds its partitioned
children and restores the needed puppets. Horizon dependencies name concrete
parent scopes and refuse a missing parent partition.

`cargo test -p rocky-secure partition_foreign_keys --locked` passes eight
tests covering: isolated cascade, nullable category projection, child-first delivery,
keyless parent liveness and restoration, shared-partition agreement, rebuild,
concrete horizon dependencies, text partition slots, and each remaining refusal.
The fixtures use three independently credentialed replicas and equal child ids
in different partitions. Kotlin/Room admission stays phase 7; partitioned
existence logs and partitioned support tables remain deferred. No persisted or
wire format changes in this phase.

### Full gates on 2026-10-09

- `cargo fmt --all -- --check`, workspace Clippy with `-D warnings`, and
  `./scripts/commitlint --jj`: passed.
- `./scripts/check`: 651 workspace tests passed, 11 ignored;
  the SDK without iroh passed 128 tests, with 4 ignored. Zero failures.
- `./sdks/native/check`: 2 tests passed; formatting, Clippy and the build
  without default features passed.
- `./kotlin check`: 51 JUnit tests passed, zero skipped or failed; all
  qualification and packaging tasks passed.
- Separate Room gate: 12 tests passed. Separate schema qualification: all
  7 hand/import pairs installed and reopened. Separate packaged consumer:
  one clean consumer run passed.
- Android runtime republished to Maven local from this workspace before the
  successful Kotlin gate. Both configured Android ABIs built; no device/UI
  qualification is claimed.

Kotlin/Room partitioned-constraint admission remains phase 7, the complete
partitioned horizon/checkpoint/bootstrap lifecycle remains phase 6, and
partitioned existence logs remain deferred. The Phase 5 native completion gate
is satisfied; these later boundaries are not reported as implemented here.
