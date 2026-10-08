# 5. Foreign keys on partitioned tables

Status: not started.
Owner: `rocky-secure` constraint resolution and support rows; `rocky-core`
installability. Depends on [phase 4](04-partition-aware-constraints.md).
Wave 2. On the critical path.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read requirement [4](scope-partitioned-constraints.md#4-foreign-keys-on-partitioned-tables)
of the scope, [design rationale](../runtime/existence-logs.md#design-rationale)
and [existence logs](../runtime/existence-logs.md). A reference names exactly
one identity column today, and a reference to another scope is accepted only
for an unpartitioned logged parent from an unpartitioned child.

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
