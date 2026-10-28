# 8. Intent only

Status: planned. Owner: `rocky-core`, `rocky-secure`, `rocky-sdk`,
`sdks/uniffi`, `sdks/kotlin` and the examples. Depends on
[phase 5](05-replayed-constraints.md), [phase 6](06-symbolic-fast-path.md)
(merged or rejected) and [phase 7](07-intent-migrations.md). Effort: two
weeks. Follow the [execution contract](README.md#agent-execution-contract).

## Objective

One replication model. Delete `TableClass::Mergeable` and everything
only it needs. This is where Rocky gets leaner.

## Settled

- **Deleted:**
  - `TableClass::Mergeable`, `ConflictPolicy` and the `on_conflict` field.
  - Per-field register merging in `replica/apply.rs` (`merge_row`'s stamp
    comparison) and the `stamps`/`authors` fields of `Materialized`,
    with their codecs and `stamp_authors.rs` where nothing else uses it.
  - The constraint resolver in `replica/constraints.rs`: candidate
    tables, support rows, virtual rows, `rocky_live`, `rocky_stamps`.
    Also `TablePartitions::support` in `rocky-storage`.
  - The features_12 phase 2 fan-out exception and its induced-work
    counter.
  - Held-change re-authoring for generation removal (`replica/held*`),
    replaced by late transactions replaying in the successor tip. Void
    records stay if anything else still uses them; otherwise they go too.
  - The migration path that authors captured rows (`stage_publication`'s
    row authoring).
  - `SqlWrites::intent`'s row-image translation for replicated tables, if
    nothing but `Mergeable` used it.
- **Kept:** creation proofs and stamps, moves, partitions and keys,
  bundles, coordinated logs, seals, compaction and checkpoints. Their
  meaning for replayed tables is unchanged.
- **`Replayed` becomes the only replicated class.** `LocalOnly` stays.
  Rename only if the name no longer reads clearly; record the decision.
- **No migration.** Existing register databases are refused at open by
  the format bump. Examples, fixtures and test databases are recreated.
- **SDKs.** The Rust schema builder and the Kotlin schema DSL drop the
  conflict policy. Their docs describe intents, drops, outcomes,
  `CHECK`s, assertions, bound timestamps and the read-modify-write rule.
  Every example declares its tables replayed; the shopping app shows a
  drop notification.

## Ordered slices

1. Move every example, SDK test and fixture to `Replayed`; all pass.
2. Delete `Mergeable` and the items above, one module at a time, each
   step passing `./scripts/check`. Format bumps.
3. SDK and Kotlin DSL cleanup; docs.
4. Record the line count of `rocky-secure` non-test sources against the
   phase-start count, and the files removed.
5. Rerun the desktop sync non-regression rows and the phase 4 statement
   budgets.

## Completion gate

- `rocky-secure` non-test source lines are at least 2,500 fewer than at
  the features_13 start. If not, report what remains and why.
- No code refers to registers, conflict policies, support rows or the
  fan-out exception.
- Statement budgets and sync non-regression rows hold.
- `./scripts/check`, the Kotlin check and the shopping app check pass.

## Not in scope

New features; changes to checkpoints, compaction or coordinated logs
beyond removing register fields.
