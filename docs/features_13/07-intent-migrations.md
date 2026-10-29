# 7. Intent migrations

Status: planned. Owner: `rocky-secure` (`replica/publication.rs`,
migration sessions in `replica/sql.rs`, schema staging), `rocky-sdk`
(`node/schema_publication.rs`). Depends on [phase 4](04-intent-replay.md).
Effort: two weeks. Carried over from features_12 phase 5. Follow the
[execution contract](README.md#agent-execution-contract).

## Objective

Today a schema migration runs its program, then `stage_publication`
authors about 3,100 bundles of captured rows for the 100k fixture inside
the same SQLite transaction. That transaction outgrows the writer page
cache; `pagerStress` leads the fill profile (239.6 s paired). With
intents the issuer publishes the program, not its rows. Members replay
it per partition. The issuer's publication becomes small, and its
remaining work stays bounded and resumable.

## Settled

- **Scope.** Migration publications of `Replayed` tables: those with a
  staged schema, where `migration_stage` returns a stage. Migrations of
  `Mergeable` tables keep today's path until phase 8 deletes it.
  Application `commit_publication` calls are ordinary interactive
  transactions.
- **The program is an intent.** Migration statements pass the same
  determinism and partition checks as any replayed transaction. A
  refused program fails the open with the refusing construct. There is
  no fallback to row publication.
- **One transaction per partition.** The program is split per partition
  like any statement and replays as the first transaction of the new tip
  in each partition. A partition's migration transaction is bounded by
  that partition's size, not the table's. The program's SQL is never
  chunked inside a partition.
- **Transaction 1: run, check, stage.** In one transaction the issuer
  runs the program on its own state, checks its statements, and stores
  the per-partition intents in publication order in a staging table with
  a cursor at zero. The issuer's own projection keeps the result, because
  for intents the local execution is the local result.
- **Transactions 2…n: author in bounded chunks.** Each takes the next
  consecutive per-partition intents, authors them as bundles of at most
  `MAX_BUNDLE_FRAMES` through the ordinary bundle path, using the
  existing deterministic `bundle_handle(publication, position)`, and
  commits operations, outbox entries and the advanced cursor atomically.
  A chunk is at most 16 bundles at first. Slice 2 measures pages per
  bundle and sets the final bound so a chunk's dirty pages stay under
  half of the Android writer cache. It is a named constant with its
  evidence.
- **Final transaction: record and release.** After the last chunk, one
  transaction inserts the publication records, builds the migration
  intent record, commits the schema stage and deletes the staging rows.
  `endorse_migration` then proceeds unchanged.
- **Nothing leaves early.** Frames of a staged publication are never
  offered, served or acknowledged before its final transaction commits.
  Serving and offering check an explicit gate on staged publications, no
  matter when a link starts.
- **Resume.** `publish_schema_at_open` continues a staged publication
  from its cursor before doing anything else. A chunk replayed after a
  crash between its commit and the cursor read is recognized by its
  handle and is not authored twice.
- **Authority mid-fill.** Each chunk re-checks grants, blockers and the
  writable tip through the bundle path. A refused chunk fails the open
  with the refusal and its blockers, leaves the stage in place, and is
  retried at the next open. A partially authored stage is never
  released, and withdrawing one is not supported.
- **Members.** A member applies the migration by replaying the program
  for each held partition, one transaction per partition, as quiet-gate
  steps (phase 2). Its cost is measured against the paired member pull.

## Ordered slices

1. **Equivalence check first.** For the 100k fixture's programs (the
   `Insert` and `UPDATE` steps) and a small fixture, compare the issuer's
   result with a member's per-partition replay: projection rows, row
   identities and the LtHash row digest. If they differ in anything but
   signatures and clocks, stop and report.
2. Staging table, transaction 1 and pages per bundle. Format bump (shared
   with phase 4's if they land in one release).
3. Bounded chunk authoring with the cursor, then the final transaction.
4. Release gate test: staged frames are neither offered nor served,
   with a link started between chunks under test control.
5. Crash tests with the qualification kill points and
   `ROCKY_SQL_KILL_AT` hooks: after transaction 1, between chunks, after
   a chunk's commit and before its cursor is observed, before the final
   transaction. Each reopens, resumes and converges on every member,
   with no duplicate bundle handles and no frame offered early.
6. Authority test: revoking the issuer's grant between chunks fails the
   open with a named blocker; nothing is released.
7. Measurements in release, paired with the phase-start commit: issuer
   fill, publication to one peer, member apply, file sizes, peak WAL and
   the fill profile.

## Completion gate

- The 100k migration test passes on a replayed fixture, and every member
  reaches the issuer's row digest.
- Issuer fill ≤ 70 s on the reference host, paired. Member apply ≤ 110%
  of paired member pull. Issuer file ≤ 300 MB, member file ≤ 80 MB. WAL
  never exceeds 64 MiB. `pagerStress` no longer leads the fill profile.
- Crash, release and authority tests enforce their correctness invariants.
- `./scripts/check` passes.

## Not in scope

Chunking a partition's migration SQL, migrations of `Mergeable` tables,
and withdrawing partially authored stages.
