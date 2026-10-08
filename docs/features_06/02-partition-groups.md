# 2. Partition groups in the schema

Status: not started.
Owner: `rocky-core` descriptor and definition; the Kotlin builder and Room
overlay. Depends on nothing; lands after [phase 1](01-remove-compatibility-code.md)
on the descriptor. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [partition groups](scope-partitioned-constraints.md#1-partition-groups),
[design rationale](../runtime/partitioned-scopes.md#design-rationale) and
[partitioned scopes](../runtime/partitioned-scopes.md). A partition address is
built from each table's own column identities today, so `lists.id` and
`items.list` never match. A group gives several tables one address.

## Ordered slices

1. Decision record 0037: the group declaration, the group address (domain,
   group name and slot values, in declaration order), the group secret and
   key derivation of [phase 3](03-group-secrets.md), the holding rule, and
   the accepted consequences listed in the scope.
2. Descriptor and definition: a table names its group and maps its partition
   columns to the group's slots by position. A partitioned table without a
   group is a group of its own.
3. Validation at build and installation: every table of a group has the same
   number of partition columns, with equal affinity and collation per slot;
   a group name is unique in the schema. An ungrouped partitioned table's
   own group cannot be named by any other table.
4. Kotlin: `partitionBy("list", group = "list")` and the same in the Room
   import overlay.
5. `LogicalSchema` exposes groups and their tables to the runtime and SDK.
6. Changing an existing table's group or a group's name is refused by
   generation admission, as changing partition columns is. A table new in a
   generation may join an existing group.

Constraints on partitioned tables stay refused in this phase.

## Completion gate

- `lists` (by `id`) and `items` (by `list`) in one group resolve to the same
  group address for equal values; SQL-equal values of different spelling too.
- Mismatched slot counts, affinities or collations are refused with a message
  naming both tables.
- A hand-written and a Room-imported schema with groups build the same
  descriptor.
- [Partitioned scopes](../runtime/partitioned-scopes.md) describes groups.
