# Scope: constraints on partitioned tables

Status: agreed 2026-10-08; sequenced in [the roadmap](README.md). Blocks the shopping
qualification; the evidence is in [the audit](../runtime/shopping-qualification.md).
Owner: `rocky-core` descriptors, `rocky-secure` constraint resolution,
existence logs and horizon, the Kotlin builder and Room import.

## The schema that must install

Phase 16 requires this definition. Today it fails before installation:

```kotlin
schema("shopping", version = 3) {
    table("lists") { text("title"); primaryKey("id"); partitionBy("id") }
    table("items") {
        integer("list"); text("key"); text("title"); boolean("completed")
        primaryKey("list", "key"); partitionBy("list")
        unique("list", "title", onConflict = Conflict.LatestWins)
        reference("list", "lists", onDelete = Delete.Cascade)
    }
}
```

Decided 2026-10-08: the shopping app also gains `categories`, partitioned by
`list` in the same group, and a nullable `items.category` whose reference
`(list, category) → categories` uses `Delete.SetNull`. It qualifies the
multi-column in-group reference and `SET NULL`.

`./kotlin run -m requiredSchema` installs it, with groups and categories, since
[phase 7](07-builder-room-and-shopping-schema.md). Before, it failed with
`items: a partitioned table takes no conflict policy or reference`.

## What exists today

Native admission accepts partition-complete `LatestWins` indexes and phase 5
foreign keys within a partition group or to an unpartitioned logged parent.
Resolution uses partition-aware identities. The Kotlin builder and Room import
admit the same shapes since phase 7. The starting refusals and gaps below explain
the roadmap boundary; current behavior is in
[mergeable constraints](../runtime/mergeable-constraints.md).

| Refusal | Where |
| --- | --- |
| Partitioned table with a conflict policy or foreign key | Kotlin `SchemaBuilder` (`build`); native `LogicalTable::installable` |
| Foreign key to a partitioned parent | `LogicalSchema::installable` requires `target.partition_by.is_empty()` |
| Existence log or support row on a partitioned table | Kotlin `existenceLog` check; `Incompatibility::SupportRow` |
| Partitioned command-controlled table | `LogicalTable::installable` |

The refusals guard real limits in `rocky-secure/src/replica/constraints.rs`:

- A `Node` holds one `ResourceScope` per table. Candidates, `Seeds` and
  `support_seeds` identify a row as `(node, id)`. The bookkeeping table is
  `secure_support(tab, id)`.
- The private candidate table's primary key is the table key, without the scope.
- A reference must name exactly one identity column.
- A reference to another scope is accepted only for a logged parent, and
  logged parents must be unpartitioned.
- Horizon finalization (`horizon_dependencies`) resolves dependencies per
  installed table scope, not per partition.

An `id` is unique only within its partition
([row identity](../runtime/logical-schema.md#row-identity)). Lifting
the checks without changing these identities would merge rows of different
partitions.

### The shopping schema is cross-scope today

The earlier agreed scope assumed "a list
and its items share one scope per list". The implementation differs.
[Design rationale](../runtime/partitioned-scopes.md#design-rationale) hashes the domain,
**table** and tuple into a scope (`rocky.partition-scope.v1`). So `lists`/X and
`items`/X are different scopes, with separate keys, grants and envelopes.
`items.list → lists` is therefore a cross-scope reference, and the existence-log rule permits
those only to unpartitioned logged parents.

## What I need

### 1. Partition groups

Decided 2026-10-08, to be fixed in decision record 0037:

- **Explicit groups.** A table names its group in the builder and the Room
  overlay: `partitionBy("list", group = "list")`. Nothing is derived from a
  foreign key. The group maps partition columns by position, so `items.list`
  corresponds to `lists.id`; affinity and collation must agree. A partitioned
  table without a group stays a group of its own.
- **In-group references.** A reference between two tables of one group that
  includes every partition column stays inside one partition and is accepted.
  A reference between partitioned tables of different groups stays refused
  until [partitioned existence logs](../ideas/partitioned_existence_logs.md).
- **Holding rule.** The SDK subscribes, evicts and bootstraps a partition for
  the whole group, unless the application leaves a table out explicitly. A
  child is never resolved without its parent partition.
- **Group secret.** One secret per group partition and key epoch. Each
  table's key is derived from it and the table's identity, so depth adds no
  envelope and no rotation. Table scopes, grants, pseudonyms, logs and
  checkpoints stay per table.

Accepted consequences of the group secret:

- Read access is all-or-nothing per group partition: a holder of the secret
  can derive every table's key. An envelope goes only to a recipient with
  whole-partition read on every table of the group. Write rights stay per
  table.
- A table that only some readers of the list may read, such as an owner-only
  `budget`, cannot be in the group. It keeps its own key and cannot reference
  the group's tables until partitioned existence logs exist.
- Adding a new table to a group in a later generation needs no new key.
  Removing a table from a group or renaming the group would need new keys,
  so generation admission refuses both, as it refuses changing partition
  columns.
- Sync, logs and checkpoints still cost one scope per table and partition.
  [Phase 9](09-group-sync-measurement.md) measures a four-table group.

Option B of the original request, a keyless existence log per partition, is
deferred to the idea document above, together with its liveness grant.

### 2. Partition-aware constraint identities

- Key candidates, seeds, support bookkeeping and puppets by
  `(scope or tuple, id)`, never by bare `id`.
- Give each partition its own candidate rows; include the partition columns
  in the candidate table's key.
- Resolve, rebuild and evict per partition. Evicting or rebootstrapping list A
  must not touch list B's winners or hidden rows.
- A device must never need another partition's key to resolve its own rows.

### 3. Unique indexes with a policy on partitioned tables

- Builder and native validation: the index includes every partition column.
- Rows clash only within one partition; the later stamp wins, as today.
- `SetNull` nulls only non-partition columns.
- Uniqueness across partitions stays a strong command and stays refused here.

### 4. Foreign keys on partitioned tables

- a. **In-group reference**: `items.list → lists`. Multi-column identities (partition columns plus
  `id`) are accepted where the partition column is not the `id` itself.
- b. **Partitioned child → unpartitioned logged parent**: for example, ERP order
  lines partitioned by customer referencing `products`. Phase 14's existence log
  already makes the parent's liveness readable; only the child-side refusal and
  the per-partition identities block it.
- `CASCADE` and `SET NULL` are derived per partition. Deleting list A hides
  only A's items, finally at the horizon. A device holding only B is unaffected
  and receives nothing of A.

### 5. Horizon, checkpoints and bootstrap

- Hidden rows and orphans finalize per partition scope at the agreed horizon.
- Horizon dependencies follow per-partition (or per-family) scopes.
- Candidates stay derived and are never replicated. Bootstrap from a
  checkpoint rebuilds them per partition. Any persisted or wire change gets a
  decision record. [No compatibility](README.md#no-backward-compatibility)
  with earlier formats is kept.

### 6. Kotlin builder and Room import

- Lift the builder `require` only for the shapes above. Keep precise messages
  for the refusals that remain.
- A Room export with partitioned entities and these constraints imports to
  the same definition as the hand-written schema.

## Not requested

Uniqueness across partitions (strong command), `RESTRICT`, natural-key
references, partitioned command-controlled tables and confirmed rows of
partitioned tables stay refused.

## Acceptance

Deterministic, multi-process where noted, all in CI:

1. `./kotlin run -m requiredSchema` passes. The definition installs natively and from Room.
2. Two offline devices add the same title to list A. Both converge under
   `LatestWins`. The same title in lists A and B stays visible in both.
3. A device with a grant on A only never receives or decrypts B's rows, and
   resolves A's constraints without B's key.
4. Deleting list A cascades to A's items on every device holding A. This is
   final after the horizon; B is untouched. Deleting a category nulls only
   `items.category`, never `list`.
5. A partition is evicted and rebootstrapped from a checkpoint; its winners
   and hidden rows are rebuilt; other partitions are unchanged.
6. ERP fixture: 200 partitions with partitioned children referencing an
   unpartitioned logged parent. Measured against
   [the 200-partition budget](../runtime/partitioned-scopes.md#limits-and-budget).
7. Refusal regressions for every shape listed under "Not requested".

## Resolved questions

- **Scenario 3** is deleting and adding the same item again in one list while
  another device edits the old incarnation offline. Deleting a category
  exercises `SET NULL`. Moving an item to another list is
  [its own scope](scope-moving-rows.md).
- **Dead-partition retirement** is not part of this scope.
