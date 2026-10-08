# 4. Partition-aware constraint identities

Status: complete on the assigned base; all completion gates passed on 2026-10-09.
Owner: `rocky-secure` constraint resolution; `rocky-core` installability.
Depends on [phase 2](02-partition-groups.md). Wave 1. On the critical path.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [what exists today](scope-partitioned-constraints.md#what-exists-today),
requirements [2](scope-partitioned-constraints.md#2-partition-aware-constraint-identities)
and [3](scope-partitioned-constraints.md#3-unique-indexes-with-a-policy-on-partitioned-tables)
of the scope, and
[design rationale](../runtime/mergeable-constraints.md).
Before this phase, resolution held one scope per table and identified a row by
`(table, id)`.
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

Starting gaps in `rocky-secure/src/replica/constraints.rs`, beyond the scope's
list (addressed below):

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

## Implementation and decisions

`rocky-secure` candidates, seeds, loss/liveness/visibility caches and natural-key
slots carry the concrete partition scope. Candidate identity indexes and SQL
liveness updates bind partition columns plus id. Support bookkeeping uses
`(tab, scope, id)`; support-row probes bind the identity. Partitioned references
and support tables remain refused, so no parent is guessed from a bare id across
partitions. The existing unpartitioned reference graph remains supported.

Rebuild selects one concrete partition without reading or rewriting another.
Full installation rebuild enumerates registered partition scopes. Live candidate
updates read incarnation winners in the incoming concrete scope. SQL commit
verification compares the same scope as its captured write. Missing existence
constraint nodes and unknown rebuild/eviction table scopes are errors.

Native installation admits only partition-complete `LatestWins` policy indexes.
Partitioned `SetNull` is refused, as permitted by the brief, so policy projection
cannot null partition columns. Cross-partition uniqueness, partitioned foreign
keys and command control remain refused.

[Decision 0039](../decisions/0041-partition-constraint-state.md): verify constrained installations against SQLite's catalog at open,
including typed identity/policy indexes, private candidate tables/indexes and
support bookkeeping. The current private support format has one shape,
`secure_support(tab, scope, id)`. Earlier shapes and missing or altered indexes
are refused; no migration or compatibility reader is introduced. Rationale and
consumer boundaries are in [mergeable constraints](../runtime/mergeable-constraints.md).

The Kotlin builder's partition-policy refusal belongs to phase 7; this phase
qualifies the native definition boundary and preserves existing Kotlin/Room
regressions.

## Qualification on the assigned base

All commands ran in the assigned `rocky-db-p04-partition-aware-constraints` jj
workspace on parent `kuymzpky` / `1a918842`, without fetching, pushing or moving
bookmarks. The current change is described with a Conventional Commit.

- `cargo check -p rocky-secure --locked`: passed.
- `cargo test -p rocky-core --locked`: 61 passed. The revised partition refusal
  regression admits complete `LatestWins`, and refuses an incomplete index,
  policy-free uniqueness, partitioned `SetNull`, references and command control.
- `cargo test -p rocky-secure --lib tests::constraints --locked`: 3 passed.
- `cargo test -p rocky-secure --lib tests::existence --locked`: 20 passed.
- `cargo test -p rocky-secure --lib partitioned_ --locked`: 4 passed, also passing
  inside the final workspace run. The gate covers two offline writers, NOCASE
  titles, equal ids across partitions in one bundle, an A-only reader without
  B's key, direct hidden-state checks, rebuild, actual custody-backed eviction,
  reopen, equal stamps, SQL duplicate refusal/rollback and four dropped indexes.
- `cargo test -p rocky-sdk --test bootstrap --locked`: 8 passed after boxing
  constraint pass futures at replay boundaries; the normal SDK stack is retained.
- `./sdks/native/check`: formatting, Clippy, two tests and the ABI build without
  default features passed.
- `./kotlin check`: 50 tests, both qualification plugins and JVM/Android builds
  passed. The final run used `ANDROID_HOME`/`ANDROID_SDK_ROOT` pointing to the
  existing licensed SDK and `KOTLIN_CLI_JAVA_OPTIONS=-Duser.home=$PWD/local/phase04-java-home`.
  Its Maven artifact was copied from this workspace's rebuilt Android publication
  and its recorded source digest was independently compared to the current tree.
- `./kotlin test -m room-qualification`: 12 passed.
- `./kotlin check qualify -m schema-qualification`: seven hand/import descriptor
  and fingerprint pairs passed; all installed and reopened.
- `./kotlin check packaged -m packaged-consumer`: artifact-only JVM consumer passed.
- `sdks/kotlin/android-runtime/gradlew -p sdks/kotlin/android-runtime publishToMavenLocal`:
  release builds for arm64-v8a and x86_64 passed.
- `./scripts/check`: formatting and Clippy passed; 734 passing test executions,
  15 ignored, including the SDK without default features.
- `cargo test -p rocky-secure --lib measure_200_partitions --locked -- --ignored --nocapture`:
  one passed. Final debug timings: initialize 7.616 s, authenticated catch-up
  14.562 s, 200 checkpoints 5.285 s, reopen 1.068 s; empty partition metadata
  409.6 B per partition at 4 KiB/page. Every existing review budget passed.
  The initial run amid compilation took 15.233 s to initialize and missed the
  15 s review budget; the budget was retained and the final rerun passed.
- `./scripts/commitlint --jj`: passed.

Initial compilation and fixture issues were corrected. The required workspace gate exposed an SDK
bootstrap stack overflow; heap-allocated constraint futures fixed it. Android
source-digest and isolated-SDK license prerequisites were repaired using rebuilt
artifacts and the already licensed SDK. No admission, durability or atomicity gate
was weakened.

No phones, emulator, real-network distribution or post-rebase chain is qualified
here. The shopping Kotlin/Room shape, foreign keys, group secrets, partition
horizon and moves remain their owning phases. The single-table measurement is
not phase 9's four-table group-sync qualification.
