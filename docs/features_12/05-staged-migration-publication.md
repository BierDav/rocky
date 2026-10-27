# 5. Staged migration publication

Status: planned. Owner: `rocky-secure` (`replica/publication.rs`,
migration sessions in `replica/sql.rs`, schema staging),
`rocky-sdk` (`node/schema_publication.rs`). Depends on
[phase 1](01-see-the-wait.md). Effort: two weeks. Follow the
[execution contract](README.md#agent-execution-contract).

## Objective

A schema migration runs at open. `run_staged_migration` executes the
program in one migration SQL session and calls `commit_publication`.
`stage_publication` then authors every bundle (about 3,100 for the 100k
fixture) inside that same SQLite transaction. The transaction outgrows the
writer page cache, and the fill profile is led by `pwrite` from
`pagerStress`. The page cache can't grow with data size (8 MiB on
Android), so authoring must be bounded instead. After this phase, the
program still runs in one transaction, and its publication is authored in
bounded, resumable transactions.

## Inputs

- The features_11 phase 9 evidence for the 100k fill profile.
- `stage_publication`, `author_publication`, `publication_order`,
  `chunks`, `bundle_handle`, `migration_stage`,
  `build_migration_intent` and `commit_schema_stage` in `rocky-secure`.
- `run_staged_migration`, `withdraw_stage` and `endorse_migration` in
  `rocky-sdk`.
- The test
  `rocky-sdk --test migrations a_migration_of_100000_rows_converges_on_every_member`.

## Settled

- **Scope.** Migration publications only: those with a staged schema,
  where `migration_stage` returns a stage. Application `commit_publication`
  calls are interactive transactions and stay single-transaction.
- **Transaction 1: run, capture, stage.** Do these steps in one
  transaction:
  1. Run the program inside a savepoint, as today.
  2. Take the captured writes.
  3. Roll back to the savepoint.
  4. Insert the writes in publication order into a new staging table,
     with the publication, its bundle count, the stage's authoring time
     and a cursor at zero.
  5. Commit.

  The program's own projection writes don't survive this transaction.
  Projection rows are written only by authoring, together with their
  operations. This keeps the rule that projection, operation, outbox and
  revision changes commit atomically.
- **Transactions 2…n: author in bounded chunks.** Each transaction:
  - takes the next consecutive bundles from the staging table
  - authors them through the ordinary bundle path (`author_bundle`, with
    `strict_projection` off), using the existing
    `bundle_handle(publication, position)` handles and the staged time
  - commits their operations, rows, projection, outbox entries and the
    advanced cursor atomically

  A chunk is at most 16 bundles at first. Slice 1 measures pages per
  bundle, and the final bound is set so a chunk's dirty pages stay under
  half of the Android writer cache. It is recorded as a named constant
  with its evidence.
- **Final transaction: record and release.** After the last chunk commits,
  one transaction does what `stage_publication` does today after
  authoring:
  - insert the `secure_publication_frames` rows
  - insert the `secure_publications` record
  - build the migration intent
  - commit the schema stage
  - delete the staging rows

  `endorse_migration` then proceeds unchanged.
- **Nothing leaves early.** Frames of a staged publication are never
  offered, served or acknowledged before its final transaction commits.
  Open already runs the migration before the network links. The phase
  still adds an explicit gate on staged publications that serving and
  offering check, so this holds no matter when a link starts.
- **Resume.** `publish_schema_at_open` finds a staged publication with a
  cursor and continues from it before doing anything else. Bundle handles
  are deterministic by position, so a chunk replayed after a crash between
  its commit and the cursor read is recognized by its handle and is not
  authored twice.
- **Authority mid-fill.** Each chunk re-checks grants, blockers and the
  writable tip through the ordinary bundle path. A refused chunk fails the
  open with the refusal and its blockers, leaves the stage and staged
  writes in place, and is retried at the next open. A partially authored
  stage is never released. Withdrawing a partially authored stage is not
  supported in this phase; the evidence records this limitation.
- **Format.** The staging table moves the replica format from 39 to 40,
  the sprint's only bump. Files at 39 are refused, as every older format
  is.

## Ordered slices

1. **Equivalence check, before any change.** Using the current
   single-transaction path, write a test-only comparison. Run the 100k
   fixture's programs (both the `Insert` and the `UPDATE` steps) and a
   small fixture through the current path, and through capture → rollback
   → `author_bundle` for the same writes in one transaction. Compare
   projection rows, secure rows, row identities and the LtHash row
   digest. If they differ in anything other than signatures and clocks,
   stop and report. Don't proceed to slice 2. Also record pages per bundle
   and the fill profile.
2. Staging table and transaction 1. Bump the replica format to 40.
3. Bounded chunk authoring with the cursor, then the final transaction.
   Remove the single-transaction authoring path for migration
   publications.
4. Release gate. Test that staged frames are neither offered nor served,
   using a link started between chunks under test control.
5. Crash and fault tests, reusing the qualification kill points and
   `ROCKY_SQL_KILL_AT` style hooks. Kill after transaction 1, between
   chunks, after a chunk's commit and before its cursor is observed, and
   before the final transaction. Each must reopen, resume and converge on
   every member, with no duplicate bundle handles and no frame offered
   before release.
6. Authority test: revoking the issuer's grant between chunks makes open
   fail with a named blocker, and nothing is released.
7. Measurements: the 100k migration test in release, paired with the
   phase-start commit. Record issuer fill, publication to one peer, member
   pull, issuer and member file sizes, peak WAL size and the new fill
   profile.

## Completion gate

- The 100k migration test passes, and every member converges with the
  same row digest as the single-transaction path for the same writes
  (slice 1's comparison).
- Issuer fill is at most 70 s on the reference host, paired. Publication
  to a peer and member pull are within 110% of paired baseline. The issuer
  file is at most 300 MB and the member file at most 80 MB. WAL never
  exceeds 64 MiB during the fill. `pagerStress` no longer leads the fill
  profile.
- All crash, release and authority tests from slices 4–6 pass within
  their tier budgets.
- `./scripts/check` passes.

## Not in scope

Chunking the program's SQL, application publications, interactive
writes, and withdrawing partially authored stages.
