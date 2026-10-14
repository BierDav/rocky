# 7. Migrations at any size

Status: implemented (2026-10-14) except issuer-independent `insert` ids,
which are blocked; see [evidence](#evidence).
Owner: `rocky-sdk` `database.rs` migration flow and status; `rocky-secure`
capture, outbox and bundles; `sdks/kotlin`. Depends on
[phase 6](06-no-remnants.md). Follow the
[execution contract](README.md#agent-execution-contract). Takes decision record 0071.

## Inputs and objective

Read [generations](../runtime/generations.md),
[secure sessions](../runtime/secure-sessions.md) and
[SQL sessions](../runtime/sql-sessions.md).

A schema's `migrations` run in one SQL transaction published as one bundle,
so a migration changes at most 2,048 rows (`MAX_BUNDLE_ROWS`). Filling a new
field of a table with 100,000 rows is impossible. After this phase a
migration has no size limit: it still runs as one local transaction, and its
result is published as many bundles.

## Settled

- **Who migrates.** Only an issuer whose grants cover every partition of
  every table its migrations read or write, for reading and writing, may run
  them and sign the generation, as the current check over
  `settings.partitions` already requires. Another device opening that app
  version keeps its database at the household's generation; its app's
  queries on what the new version adds fail, and status says an issuer with
  full access must migrate first.
- **Order.** The issuer signs the generation first; the earliest signing
  time is the [baseline](02-adopting-newer-generations.md#settled). Then, at open and before the network starts, it runs every
  migration step in one local SQL transaction of any size. That transaction
  also stores the net change as a staged publication and marks the migration
  done, so a crash either repeats nothing or loses nothing.
- **Publication.** The staged change leaves as consecutive bundles within
  the bundle limits, parents before children. Each bundle's handle derives
  from the generation and its position, so a retry returns the same frames.
  Publishing resumes after a restart until the stage is empty.
- **Stamps.** Every migrated write carries the stamp of the local commit,
  after the baseline. An edit another device makes while bundles are still
  leaving is newer and wins.
- **Receivers.** Each bundle is ordinary. Until the last arrives, rows not
  reached yet read their default; the hidden-row rules settle references and
  unique values in between. Every device ends with the same rows.
- **Concurrent issuers converge.** Issuers that cannot reach each other may
  each migrate. An `insert` step derives each row's id from the generation,
  the step and the selected row's key, never from the issuer's partition, so
  both write the same rows and they merge. Inside a migration Rocky refuses
  every other id source, such as `next_id` and `rocky_next_id`. Differing
  updates meet by latest wins. No coordinator is needed;
  [avoiding the duplicate work](../optimizations_on_demand/coordinated-migrations.md)
  is deferred.
- **Rows not held.** Rows the issuer did not hold when it migrated keep
  their default. Rocky guarantees convergence, not that a migration saw
  every row.
- **Steps unchanged.** SQL and `insert` steps keep their form and lose their
  size limit. Ordinary application transactions keep theirs.

## Ordered slices

1. Decision record 0071: staged migration publication and its handles.
2. Sign-then-migrate flow at open, with the authority check and the
   durable done mark; deterministic `insert` ids and refused id sources.
3. Capture beyond the bundle limits for migrations only; staging, chunked
   publication, resume.
4. Status: publication progress; "an issuer with full access must migrate".
5. Guides: generations, secure sessions, SQL sessions.

## Completion gate

- An issuer adds a required field to a 100,000-row table and fills it with a
  migration; every member converges on the same values.
- A crash after the local migration commit resumes publishing without
  repeating or duplicating a row; a crash before it repeats the migration.
- An edit a member makes while bundles are still arriving wins on every
  device.
- Two issuers that cannot reach each other both migrate; after they meet,
  every device has the same rows and none twice.
- An issuer without access to one partition cannot sign; a device whose app
  declares the new version keeps syncing on the old generation, and its
  queries on the new field fail with SQL errors.
- Measured publication time and size are recorded with the evidence; the
  large case runs in the tier [testing](../testing.md) assigns it.

## Evidence

Decision record [0071](../decisions/0071-staged-migration-publication.md)
and the guides [generations](../runtime/generations.md#migrations),
[secure SQL](../runtime/secure-sql.md#staged-publications),
[secure sessions](../runtime/secure-sessions.md#bundles) and
[SQL sessions](../runtime/sql-sessions.md#limits).

- Slices 1 to 3. The issuer signs first, then migrates at open in one SQL
  transaction that `SecureSql::commit_publication` commits as consecutive
  bundles, parents first, with handles derived from the generation's
  publication and the bundle's position. The publication record commits in
  that transaction and marks the migration done; `migration.json` keeps
  only the version migrated from. The encrypted replica is
  `rocky.encrypted-replica.v17`.
- Slice 4. Status `migration` reports `publishing` with bundles and those a
  peer holds, or `full_access_required`.
- A 100,000-row table gains a required field that a migration fills, and a
  member whose application stays at version 1 converges on the same values:
  `rocky-sdk` `tests/migrations.rs`
  `a_migration_of_100000_rows_converges_on_every_member`, in the e2e tier.
  Measured once on an Apple Silicon laptop, dev profile, on 2026-10-14:

  | Step | Time | Issuer file |
  | --- | --- | --- |
  | Seeding 100,000 rows by an `insert` migration (49 bundles) | 83 s to open | 0.7 MB to 808 MB |
  | Filling the required field by an `UPDATE` migration (49 bundles) | 120 s to open | 1,185 MB |
  | The member pulling both, in 2 syncs | 276 s | member file 577 MB |

  The whole test took 481 s. A frame of 32 rows weighs about 94 KB, so a
  migration retains about 3 KB per row until it is pruned.
- A crash after the commit repeats nothing; a failure before it repeats the
  migration: `a_failed_migration_runs_again_and_a_committed_one_never`. A
  migration that calls `rocky_next_id` fails after the generation was
  signed; the fixed release runs it once, and a `migration.json` restored
  after the commit runs nothing. Both are durable states of a crash at those
  points; no process was killed.
- An edit made while bundles are still arriving wins on every device:
  `rocky-secure` `tests::sql::publication::a_transaction_of_any_size_leaves_as_consecutive_bundles`,
  which also applies the first bundle alone, tracks progress through a
  peer's frontier and returns the same frames for a repeated commit.
- Two issuers that cannot reach each other both migrate and converge, with
  no row twice in a table keyed by name:
  `issuers_that_cannot_reach_each_other_both_migrate_and_converge`.
- An issuer without a partition's key does not sign; its application is
  `ahead`, status says `full_access_required` and its query on the new field
  fails with `no such column`; once the key arrives it migrates:
  `an_issuer_without_a_partitions_key_keeps_the_households_generation` and
  `enrollment::tests::an_issuer_without_the_key_of_a_known_partition_may_not_migrate`.

Found on the way:

- A projection switch filled a shadow from its table's own scope, so every
  generation lost the visible rows of a partitioned table without unique
  indexes or foreign keys, also before this phase. Schemas with a
  partitioned table now recreate their tables from the stored rows.
- A migration beyond the 64 MiB retained-history bound left the issuer
  refusing every later write, including renewing members into the
  generation, so no member could ever adopt it. A publication's frames no
  longer count toward the bound until they are pruned.

Checks on 2026-10-14: `./scripts/lint`, `./scripts/check` (175 unit and
1,019 integration tests within their budgets),
`sdks/kotlin/gradlew -p sdks/kotlin check` and, against this change's SDK
published to a private Maven repository, `./examples/shoppinglist-app/check`
on a JVM/Android project (`scripts/ci/shopping-project`), whose
`SchemaUpgradeTest` moves a version 6 household to version 7. The e2e test
above ran alone; the e2e suite was not run.

Blocked: an `insert` step's ids still come from the issuer's creation log,
not from the generation, the step and the selected key. A row id is an
author number and a sequence; the publisher's author binding ties the
number to one pseudonym, the creation proof is signed by it, and each
author's births must continue its log. Issuer-independent ids need a new
identity class in `rocky-core` and `rocky-secure`, beyond this brief. Until
then two issuers that insert into a table keyed by `id` add each row twice.
