# 2. Partition groups in the schema

Status: complete on the assigned base; all completion gates passed on 2026-10-09.
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

## Implementation and evidence

The descriptor and portable definition carry `partition_group`; format 4 replaces
the previous partition format. `LogicalSchema::partition_groups` exposes validated
slots and members, and `group_address` maps SQL values positionally without table
or column identities. The Kotlin builder and Room overlay emit the same fields.
[Decision 0037](../decisions/0037-partition-groups.md) records the address and the
phase 3 secret/holding contract. Generation admission keeps existing membership
immutable; constraints on partitioned tables remain refused.

Six native group regressions cover equal addresses, SQL collations and slot
order, mismatch messages at build/install, private implicit groups, generation
changes/new members, and descriptor round-trip/old-format refusal. The generated
Room `GroupDatabase` has real `lists` and `items` entities; qualification compares
its descriptor/fingerprints to hand declarations and installs/reopens it through
the native SDK.

The existing 200-partition probe was rerun on 2026-10-08 in the macOS debug build
while other gates ran: initialization 9.663 s, authenticated catch-up 17.337 s,
200 checkpoints 6.487 s and reopen 2.109 s. Partition metadata added 20 pages,
409.6 B per partition at 4 KiB/page. All existing budgets passed. This remains
a single-table probe, not phase 9's four-table group sync qualification.

The assigned base retains its unpartitioned format-2 path; phase 1 removes it
and lands first. This change adds no migration, legacy reader or alternate group
encoding. Shared-secret distribution, group holding, constraints and row moves
remain later phases; no mobile/device or real-network qualification is claimed.

Verification completed in the assigned jj workspace on parent `357dbbb4`:

- `cargo test -p rocky-core --locked`: 61 passed, including six new group tests.
- `cargo test -p rocky-sdk --lib --locked`: 17 passed.
- `cargo test -p rocky-storage --locked`: 24 passed.
- `./scripts/check`: formatting and Clippy passed; 730 passing test executions,
  15 ignored, including the SDK without default features.
- `./sdks/native/check`: all checks passed; two tests passed, including the
  no-default-feature ABI build.
- `./kotlin test -m schema-qualification`: four tests passed; Room regenerated
  the grouped export.
- `./kotlin check qualify -m schema-qualification`: seven hand/import pairs
  matched, installed and reopened.
- `./kotlin check`: 50 tests and both qualification plugins passed; JVM and
  Android builds passed. The final run used a workspace-local Java `user.home`
  with the actual rebuilt Android publication, avoiding shared Maven state.
- `./kotlin test -m room-qualification`: 12 passed.
- `./kotlin check packaged -m packaged-consumer`: artifact-only consumer passed.
- `sdks/kotlin/android-runtime/gradlew -p sdks/kotlin/android-runtime publishToMavenLocal`:
  release builds passed for arm64-v8a and x86_64.
- `cargo test -p rocky-secure --lib measure_200_partitions --locked -- --ignored --nocapture`:
  one passed; measurements are above.

Initial runs found an actionable Clippy style issue and outdated format-refusal
fixtures; these were corrected. The coordinator latency probe failed while
builds ran concurrently and passed in the final run without changing its budget.
The Android artifact was rebuilt and isolated after source-digest/shared-Maven
mismatches. No admission, durability or atomicity gate was weakened. Requalification
after the orchestrator's phase-1 rebase remains its landing responsibility.
