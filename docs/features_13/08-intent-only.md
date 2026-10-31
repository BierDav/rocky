# 8. Intent only

Status: planned. Owner: `rocky-core`, `rocky-secure`, `rocky-sdk`,
`sdks/uniffi`, `sdks/kotlin` and the examples. Depends on
[phase 5](05-replayed-constraints.md), [phase 6](06-symbolic-fast-path.md)
(merged or rejected) and [phase 7](07-intent-migrations.md). Effort: three
weeks, one of them for replayed checkpoints. Follow the [execution contract](README.md#agent-execution-contract).

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
  meaning for replayed tables is unchanged, except for the three items
  below that phase 10's planning added (2026-10-31).
- **Replayed checkpoints.** Without register records a checkpoint carries
  no rows, `prune` refuses every scope and each bootstrap replays the
  whole history. A checkpoint of a replayed partition instead carries its
  typed rows at a finalized seal, with the seal's drop map, sealed
  outcomes and an LtHash commitment over the rows. A member installs one
  into a partition that is still `waiting`, through the existing direct
  page import; frames the seal covers then prune as before. Installing
  one into a live partition is phase 10's kind I. The checkpoint format
  bumps once with the row format.
- **Replayed scopes never rebuild from records.** `typed.rs` `reproject`
  empties the table and refills it from row records, which replayed
  scopes keep none of. `rebuild_rows` (`rebuild_projection`,
  `rebuild_tip_workspace`, `withdraw_schema_switch`) and
  `switch_to_staged` reach it. For a replayed scope they restore from the
  base checkpoint and replay the frames after it instead. Tests: a
  non-forward tip switch and a schema switch withdrawal over a replayed
  table keep its rows and equal a reference replay.
- **Compaction commits its finalized-seal bookkeeping atomically.**
  `compact_to` updates and deletes `secure_finalized` in autocommit
  statements after its per-scope transactions; both move into the last
  one.
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
3. Replayed checkpoints, record-free rebuilds of replayed scopes and the
   `compact_to` fix. Tests: a member joining after compaction installs
   the checkpoint, replays only the frames after it and reaches the
   issuer's rows, drop map and LtHash; pruning removes the covered
   frames; a crash after each checkpoint step resumes.
4. SDK and Kotlin DSL cleanup; docs.
5. Record the line count of `rocky-secure` non-test sources against the
   phase-start count, and the files removed.
6. Rerun the desktop sync non-regression rows, including the member
   file after a 100k sync, and the phase 4 statement budgets.

## Completion gate

- `rocky-secure` non-test source lines are at least 2,500 fewer than at
  the features_13 start. If not, report what remains and why.
- No code refers to registers, conflict policies, support rows or the
  fan-out exception.
- Statement budgets and sync non-regression rows hold.
- `./scripts/check`, the Kotlin check and the shopping app check pass.

## Not in scope

New features; changes to checkpoints, compaction or coordinated logs
beyond removing register fields and the three items above.
