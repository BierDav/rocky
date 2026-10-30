# 7. Intent migrations: evidence

Phase start: `dd9a8a35` (phase 6). Host: `david-mb`, macOS 27 arm64, Apple
M5 Pro, 48 GiB.

## Prerequisites the code lacked

Three gaps blocked any migration of a replayed table. Each is fixed here and
covered by the replayed migration tests.

- **Replayed rows were lost across a migration's close.** A close and opening
  is not a forward tip switch, so `rebuild_tip_workspace` reprojected every
  scope from row records. Replayed scopes keep none (phase 5), so the issuer's
  tables were empty before its program ran, and so were every member's.
  `replayed_on_path` keeps a replayed scope's rows when every transaction it
  replayed stays on the new path, as across a close. A switch that leaves
  some replayed transaction behind still rebuilds as before. Rewinding those
  transactions is not part of this phase.
- **Partitioned replayed tables had no partition grants.** `whole_partitions`
  and `partition_address` accepted only mergeable tables, so the SDK could not
  create or share a replayed partition.
- **An intent frame could be larger than any peer can pull.** Authoring allowed
  frames up to `MAX_SECURE_FRAME` (256 KiB), but a range page asks for at most
  half of that, so a larger frame stalled sync at "byte quota smaller than one
  frame". Intent authoring now refuses frames over `MAX_PAGE_BYTES` with a
  quota error.

## Slice 1: equivalence

The fixture is `crates/rocky-sdk/tests/migrations/replayed.rs`: `items`
replayed and partitioned by `list`, an issuer, and a member holding every
partition. Version 2's `Insert` step seeds `items` from the `WITH RECURSIVE`
select, version 3 adds a nullable column in place, and version 4's `UPDATE
items SET position = title` fills. The member opens version 4 and syncs. The
comparison is `local_table_digests`: row count and LtHash over every column
of every row, including `id`, with columns in name order.

- `a_member_replays_each_partition_of_the_issuers_rows` (64 rows, 128
  partitions): issuer and member digests are equal.
- `fill_of_100000_replayed_rows` (100,000 rows, release): digests are equal.
- Physical column order differs: the issuer added `tag` and then `position`
  in place, while the member installed version 4 at once. The values are
  identical. Nothing else differed, and signatures and clocks do not appear in
  the projection.

How the seed's rows travel: an `Insert` step into a replayed table evaluates
its select on the issuer and inserts up to 1,024 rows per statement with
bound values. Each partition's copy keeps its own rows, so the seed intent
grows with the partition (about 113 bytes per row here). A partition's intent
must fit one pulled frame (`MAX_PAGE_BYTES`, 128 KiB), about 1,100 rows of
this shape. The 100k fixture therefore uses 128 partitions. The fill's
`UPDATE` is one short statement per partition, whatever the partition's size.

## Slice 2: staging, transaction 1, pages per bundle

The migration session's commit (`SecureSql::commit_publication`) applies the
determinism, partition, grant and constraint checks of any replayed
transaction. When the transaction wrote replayed tables, `stage_intents`
stores, in the same transaction:

- each partition's members in publication order in
  `secure_publication_intents`: per scope, the statements and the undo of the
  issuer's execution. All unpartitioned scopes form one partition, ordered
  first.
- a cursor at zero in `secure_publication_cursors`.

The issuer's projection keeps the program's result. A migration session
splits each statement across every partition it reaches
(`split_intent_within`), because each partition's copy replays as its own
transaction rather than as one frame of a 64-frame bundle. A refused
statement fails the open and names the construct; there is no row fallback.
A program that writes both replayed and mergeable tables is refused, as in
any transaction.

Pages per bundle: `ROCKY_MEASURE_PUBLICATION_PAGES=1 cargo test --release -p
rocky-sdk --test migrations replayed::fill_of_100000 -- --ignored
--nocapture` empties the WAL before each authoring transaction and reads its
frame count after the commit (qualification builds only). It covers 128
bundles each for the seed and the fill, with a 4 KiB page:

| chunk | transactions | pages per chunk (min–max) |
|---|---|---|
| 16 bundles | 16 | 585–758 |
| 8 bundles | 32 | ≤ 471 |

Fitted: about 184 pages per transaction plus 36 per bundle. Android's writer
cache is `CACHE_KIB / 2` = 4 MiB = 1,024 pages, and half of it is 512.
`PUBLICATION_CHUNK_BUNDLES = 8` is the largest chunk under that bound (9 would
be about 507).

Format: the persisted format gains `secure_publication_intents`,
`secure_publication_cursors` and `secure_publication_unsent`. A file without
them is refused as missing secure storage tables. No release separates this
from phase 4's bump, so they share it. The wire format is unchanged: a
migration's frames are ordinary intent frames under the existing migration
publication record. `ROW_FORMAT` stays 14.

## Slice 3: bounded chunks, then the final transaction

`EncryptedReplica::author_publication_step` runs one transaction per call:
the next `PUBLICATION_CHUNK_BUNDLES` staged positions, each authored through
the ordinary bundle path under `bundle_handle(publication, position)`, with
their operations, outbox entries and the advanced cursor. Once the cursor
reaches the total, one last transaction records the publication and its
frames, commits the schema stage, publishes outcome records of bundles with
several members and deletes the staging rows. `endorse_migration` follows
unchanged. `publish_schema_at_open` resumes a staged publication before
anything else and never runs its program again. A position whose handle
committed before is recognized by `lookup_bundle` and not authored twice.

- `each_authoring_transaction_writes_at_most_one_chunk` (`rocky-secure`,
  17 positions: the unpartitioned scopes, then 16 `tags` partitions): the
  steps author 8, 16 and 17 bundles, then record; every frame hash is
  distinct and the record names 17 bundles.
- `the_execution_commits_staged_and_authoring_records_it_once`: the issuer's
  projection holds the program's result before any bundle exists; a second
  pass authors and records nothing more.

## Slice 4: release gate

Frames of positions already authored are listed in
`secure_publication_unsent` until the final transaction. Range pages,
transfer pages and placements skip them, and `refuse_staged` refuses any
offer or send that names one.

`a_staged_publication_is_neither_offered_nor_served_before_its_record`
starts a link after each of the three chunks: the member is admitted on the
issuer's own tip, so serving is not refused for a tip mismatch, and pages
every scope by range and by transfer. No authored frame is served and an
offer of them is refused as "still being authored". After the record the
member admits the endorsed publication, receives the frames, and its
`items` and `tags` equal the issuer's. Mutation check: with the
`secure_publication_unsent` filter removed from `unsent()`, the test fails.

## Slice 5: crash and resume

`an_interrupted_fill_resumes_and_converges` (`rocky-sdk`, 256 rows in 17
partitions: three chunks per publication) runs the fill in a child process
armed with `ROCKY_SQL_KILL_AT`, then reopens. Each point leaves:

| kill point | when | cursor | authored = unsent frames |
|---|---|---|---|
| `after` | transaction 1 committed | 0 / 17 | 0 |
| `publication-staged` | before the first chunk | 0 / 17 | 0 |
| `publication-chunk` | a chunk committed, cursor not yet read | 8 / 17 | 8 |
| `publication-between` | before the second chunk | 8 / 17 | 8 |
| `publication-final` | before the final transaction | 17 / 17 | 17 |

After each, the reopened issuer resumes, the member converges to the
issuer's LtHash digest, and the issuer holds two publications of 17 bundles
and 34 frames, so no handle was authored twice. Every authored frame was
still unsent when the process died.

## Slice 6: authority mid-fill

Each chunk calls the bundle path's grant, blocker and writable-tip checks
again. A registered line node whose chain no longer verifies, for example
after a revocation, is now reported as the `LineNode` blocker, clearing by
the application. Before this change it reported no blocker, and a revoked
writer failed only at frame verification with `blockers: []`.

`a_revoked_writer_between_chunks_names_its_blocker_and_releases_nothing`
revokes the migration writer's capability after the first chunk. The next
call fails with "the staged publication stopped at bundle 8 of 17: … ;
blockers: [<LineNode blocker>]". Retries fail in the same way. The cursor
and the eight authored frames stay as they were, all unsent; the schema stays
private and uncommitted; there is no migration intent and no publication.

## Slice 7: measurements

Host `david-mb` (above). Release builds, run one after another with no
other build or test running. Load average: 3.7 when the current run
started, 2.5 when the phase-start run ended.

- **Current:** change `yonmmlnr` (on `dd9a8a35`).
  `ROCKY_PROFILE_FILL=<file> cargo test --release -p rocky-sdk --test
  migrations replayed::fill_of_100000 -- --ignored --nocapture`. This is the
  replayed fixture: 100,000 rows in 128 partitions, seed, additive switch
  and fill. 3 trials.
- **Phase start:** `dd9a8a35` in a separate workspace. `cargo test --release
  -p rocky-sdk --test migrations prepared_pull::member_pull -- --ignored
  --nocapture`. This is the mergeable 100k fixture, the one the 239.6 s
  baseline came from. It reports one fill, then the first pull. The second
  pull trial is never reached: the test fails after the first one with
  `transfers.iter().all(|transfer| transfer["checkpoint"].is_null())`
  (`migrations.rs:647`). That failure is the same at the current change
  (`migrations.rs:649`), so it predates this phase. One sample each.

| measure | phase start (mergeable) | current (replayed), 3 trials | target |
|---|---|---|---|
| issuer fill open | 81.90 s | 3.86 / 3.89 / 4.62 s | ≤ 70 s |
| issuer seed open | 71.37 s | not separated (seed open is in setup) | — |
| publication to one peer | 55.48 s, then 29.64 s closing compaction | inside member pull below | — |
| member apply / pull | 10.12 s pull | 9.68–9.92 s open, pull and replay (≤ 98% of paired pull) | ≤ 110% of paired pull |
| issuer file | not reported | 29.9 MB | ≤ 300 MB |
| member file | not reported | 42.2–42.3 MB | ≤ 80 MB |
| peak WAL, seed / fill / pull | not reported | 46.2 / 43.8 / 40.1 MB max | ≤ 64 MiB (67.1 MB) |

For each trial, the member's `items` digest equals the issuer's. The current
change still runs the mergeable path without regression: fill 81.65 s, pull
10.04 s, 6,251 frames, on the same host.

Fill profile (macOS `sample`, 8 s at 1 ms, first fill): the leading on-CPU
frames are CBOR decoding (`ciborium` header and integer reads). `pagerStress`
appears in 8 samples. Most samples are idle waits of the test's other
threads.

**WAL fix found while measuring.** The first runs peaked at 67.4 MB (seed)
and 77.8 MB (fill), over the bound. Traces showed every truncating
checkpoint between chunks returning `(busy=1, -1, -1)`. Other callers'
`checkpoint_over(WAL_LIMIT_BYTES)` had scheduled a passive checkpoint once
the WAL passed 32 MiB. While that passive checkpoint ran, each step's
truncation failed, and the WAL grew about 1.7 MB per chunk.
`author_publication_step` now waits for a running passive checkpoint before
taking the lock. After the fix, every step's truncation succeeds on the first
attempt and the peaks are the ones in the table. Transaction 1 is the largest
writer: about 6,150 WAL pages (25 MB) on a cut WAL.

## Verification

`./scripts/lint` and `./scripts/check` pass: unit 246, integration 1525,
including the SDK built without default features. `sdks/kotlin/gradlew -p
sdks/kotlin check -x iosSimulatorArm64Test` and
`./examples/shoppinglist-app/check` pass. With the iOS Simulator test tasks
included, the Kotlin check stalled for 30 minutes in
`iosSimulatorArm64Test`; those tasks and physical devices are not verified.
