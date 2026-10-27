# Phase 9 evidence

Status: incomplete (2026-10-27). The latest correctness continuation is
recorded below. Complete single-row receive still takes **83 SQL statements**;
the **70 s issuer gate remains open**, and the owner deferred isolated
performance qualification. The official multi-step 100k migration now passes
its correctness assertions; the earlier publication timeout and missing-schema
bootstrap failure are no longer its final result. A physical iPhone LAN load
and offline restart also passed, with the tested artifact identities below.

The original measurements started on `623ef5e2`, then phase 4 (`d1d5eb13`).
The continuation uses change `qrypvysm`, parent `a56ce100`, in workspace
`lighter-replica`. The replica format remains **39**; row, checkpoint and link
encodings are unchanged. Earlier observations below are retained as history.

## Implementation

- **Shared base.** `secure_checkpoint_base` replaces the base row table, as
  `WITHOUT ROWID`. Its `fields` is NULL while the stored row holds the same
  record. `secure_checkpoint_rows` is now a view that resolves the record;
  inserting through it shares equal rows. Updating or deleting a stored row
  first copies a shared base record back into the base, so the base never
  changes underneath it. Bulk rewrites (rebuild, rewriting import) share the
  base again afterwards. Readers, audit, transfer and finalization read the
  same view; signed bytes and digests are unchanged.

  This keeps `secure_rows` holding every current row, rather than holding
  only the rows that differ from the base as the brief settled. The
  orientation was flipped because `secure_rows` carries the shape, seal
  candidate, kept-history and key triggers and is read directly in about 30
  places; the stored bytes saved are the same.
- **Compact keys.** `secure_row_keys` is keyed by the interned scope id and
  holds a 16-byte key digest. Every lookup confirms the full key from the
  decoded row.
- **Fitting tables.** `secure_creation_headers` is a rowid table with a
  unique digest index. Checkpoint overlay, scratch, rebootstrap and served
  rows are `WITHOUT ROWID`.
- **Fewer statements.** Apply reuses the record it read for the digest
  update, key index and projection; it deletes from `secure_move_rows` only
  when the replaced record was a marked move. Scope-id lookups are cached
  with the record dictionary, which every rollback clears.
- **Replay.** A finalization replay rebuilt the identity ledger from the base
  for every frame (quadratic in frames). Births only accumulate and merges
  are order-independent, so the ledger is extended from the previous step,
  within one transaction. Frame paths depend only on the frame's tip and are
  computed once per tip per step; signer keys are decompressed once.
- **SQLite settings.** Statement cache 512 on every connection; readers map
  64 MiB. `temp_store` stays at SQLite's default: with `MEMORY`, every
  statement end truncated an in-memory statement journal by walking it, which
  made publication 3× slower (`memjrnlTruncate` led its profile). Writers use a 32 MiB page cache (8 MiB on
  Android). The replica checkpoints passively at 32 MiB, cuts the WAL back
  to 32 MiB, and a truncating checkpoint waits up to 50 ms for readers. New
  files use `auto_vacuum=INCREMENTAL`; after each commit, at most 4096 free
  pages are returned once 1024 have collected. `PRAGMA optimize` runs at
  close.
- **Indexed scans.** Placement serving reads headers through
  `secure_position_sources` by author and sequence, and only the length of
  an operation body. A partial index covers the pending log heads.

## 100k fixture

`cargo test -p rocky-sdk --test migrations --release` then
`a_migration_of_100000_rows_converges_on_every_member --exact --nocapture`.
The test now also prints the issuer's `dbstat` and both files' free pages.
The baseline is the base commit in a second workspace, run concurrently with
the optimized build so both saw the same host load (18 cores, load 20–90
from other workspaces). Times are therefore paired observations, not
isolated benchmarks.

| Measure | Base | This phase | Gate |
| --- | --- | --- | --- |
| Member file (main) | 166,494,208 B | 67,411,968 B | ≤ 80 MB, met |
| Issuer file | 719,355,904 B (123,293 free pages) | 125,304,832 B (976 free) | ≤ 300 MB, met |
| Issuer fill (open running both migrations) | 735.9 s | 239.6 s | |
| Publication to one peer | 74.4 s | 85.8 s | |
| Closing compaction | 32.3 s | 37.7 s | |
| Member pull | 13.6 s | 15.2 s | |

An earlier pair at heavier load gave 168.6 → 69.0 MB, 719.3 → 115.5 MB and
a fill of 954.7 → 262.9 s. Publication and the member pull are within the
spread of these runs; the fill is not.

Member tables (bytes on disk):

| Table | Base | This phase |
| --- | --- | --- |
| `secure_row_keys` + index | 76,697,600 | 12,517,376 |
| base rows (+ autoindex) | 34,033,664 | 4,509,696 |
| `secure_creation_headers` | 14,659,584 | 6,422,528 |
| `secure_rows` | 29,413,376 | 29,413,376 |

Row and checkpoint encodings are unchanged, the closing checkpoint verified
on both devices and the member took it in one transfer, and finalization
and rebuild equality tests pass. The fixture's LtHash digest was not compared
with a phase-start value: the fixture signs fresh keys each run.

## Statements per applied row

`tests::scaling::an_ordinary_update_applies_in_few_statements_per_row` counts
statements prepared on the applying thread (`rocky_storage::sqlite::statements`,
test support only).

| Frame | Base | This phase |
| --- | --- | --- |
| 32 updated rows | 8.66 per row | 6.34 per row |
| 1 updated row | 94 | 82 |

The gate holds for batched updates. A single-row frame still runs about 80
statements, mostly per-frame reads of subscriptions (11), the roles
configuration (10) and the newest checkpoint (8); caching those is left open.

## Outstanding at first landing

- Held refresh still scans applied and pending operations and runs on every
  request, not only after a graph, key or grant generation change.
- The issuer's author and publish (fill plus publication, 325 s here) is far
  above 70 s on this host. Before this phase, finalization replay led the fill
  profile: frame-path walks re-encoding seals, Ed25519 key decompression and
  the per-frame ledger query. After it, the fill is led by `pwrite` from
  `pagerStress`: the migration's single transaction outgrows the 32 MiB page
  cache and SQLite spills pages to the WAL mid-transaction.
- `Checkpoint pages insert in key order` was not changed.
- `./scripts/check` passes all 200 unit and 1,404 integration tests but is
  not green on time budgets. Before the rebase,
  `tips races::revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner`
  took 2.20–2.24 s alone (the base took 2.12–2.29 s on this host, as phase 6
  also recorded). After it, `live_links::iroh_tie_gate` took 4.13 s alone;
  reruns took 1.12, 1.17 and 4.12 s, a network-timing split this phase
  does not touch. Both budgets are 2 s.
- Physical-device runs were not part of this phase's gates and were not done.


## Continuation on main (2026-10-26)

Workspace: `~/AgentWorkspaces/rocky-db-lighter-replica`, change
`qrypvysm`, based on `main` at `a56ce100`. The existing SDK workspace
was not changed. This continuation changes no persisted or wire encoding;
the replica format remains 39.

### Implementation and invariants

Held refresh filters operation metadata by the requested resource scope
and checks the joined position index. Local held-change listing enumerates scope metadata, then refreshes
only changed scopes. Each scope remembers its graph shape and revocations,
selected tip and manifest, installed key epochs, effective grants and
readability. An unchanged request does not scan operation history. A failed transaction attempt clears the markers, so a rolled-back
classification is retried. Ordinary SQL commits and general context-cache
resets preserve the markers; graph, key and grant changes are detected from
their scope-specific context. Incoming closed frames still use the existing admission path.

Checkpoint page storage sorts opened rows by their stored row key before
building the insert batches. The signed page order and signatures are
unchanged.

Subscriptions, role configuration and newest checkpoint records are cached,
including missing records. SQLite's existing watched-write hook now exposes
table-specific counters. Writes invalidate the corresponding metadata inside
the transaction; context restoration and rollback clear the cache. The
checkpoint cache shares the same latest record across callers requesting
newest or verified metadata; verification still runs in the existing
callers. Test-only statement traces collect SQL text without bound values.

Regressions exercise repeated metadata reads, writes within one transaction,
rollback, unchanged held refresh as history grows, and a key change affecting
only one of two scopes. Direct SQL corruption tests invalidate the refresh
context before forcing another scan; filtering by the operation's scope keeps
damaged index coordinates visible to the existing corruption checks.
The ordinary-update test applies identical frames to
cached and uncached replicas, compares the resulting rows and checks at most
one read of each cached table per frame.

The measured comparison on the same code and identical frames is:

| Frame | Metadata cache disabled | Enabled |
| --- | --- | --- |
| 32 updated rows | 6.78125 statements/row | 6.40625 statements/row |
| 1 updated row | 96 statements | 84 statements |

The batched gate passes and all three metadata tables are read at most once
per frame. The complete single-row receive still exceeds eight statements:
its trace includes operation admission/storage, pending and quarantine
passes, identity and seal checks, revision signing, transactions and
post-commit recovery. The earlier 82-statement number preceded phase 7;
the disabled-cache control here isolates this continuation's 12-statement
saving. This does not close the strict single-row receive gate.

### Repeatable LtHash comparison

The new `lthash_100000_matches_phase_start` corpus fixes the publisher,
signer, row keys, creation identity, field values and clocks. It runs 100,000
records through local record encoding and canonical expansion, checks every
expanded record against its canonical input, then computes the row-set
LtHash. This isolates record-format equality from fresh operation signatures
and wall-clock stamps.

The exact same helper and corpus were run against `d1d5eb13`, the phase-4
prerequisite immediately before phase 9, in a temporary Jujutsu workspace,
and against this continuation. Both produced:

```text
c88490677cc3bfa8212b1c99e9c43def1f8049d6335636531383edad5544e389
```

The regression pins that measured phase-start value. This is a fixed
100k canonical-record corpus, not a claim that two independently authored
migration runs with fresh identities have equal digests. It runs in the
e2e tier; no existing budget was relaxed.

### Desktop and device qualification

The isolated frozen JVM load consumer ran with a freshly created 100k
source and a second desktop publication peer. Both checked all 100,000
generated values. Source identity:
`3779aed8840a2b279425f74a59fa074dad8fe9b091d0418dab421f63e15c0d68`;
publication peer:
`26a770f5f715cbf129dba62ada804875d91c7e7d2e5dbf2c657b26c83582884c`.
Authoring took **186.040 s** and publication **65.286 s**. Source file after
closing compaction: **103,559,168 B**. The publication peer retained
**192,544,768 B** of unpruned history; this is not the returning-member
checkpoint fixture used for the 80 MB gate.

These are observations under concurrent builds and other workspace load
(starting load averages 48.3 / 67.7 / 53.1 on 18 cores), not an isolated
speedup comparison. A five-second sampled profile still shows SQLite
`pagerStress` and `pwrite` dominating active work: 1,319 top-of-stack
`pwrite` samples, versus 39 in the leading field-arithmetic function.
The unchanged 70 s author-and-publish gate is not met.

The physical Pixel 11 Pro (`66121FDKX001MF`, Android 17) initially
reported device-support timeouts. After the user refreshed Devices, the final
isolated APK installed and opened successfully. The first instrumented retry
joined the phone in 0.805 s, then lost its control stream during sync; the
USB device disappeared. No load-app crash was present in the recovered crash
log. A wireless retry opened the app and returned
`unreachable: dial timed out` from phone join. Neither attempt verified
phone bootstrap, 100k row equality or one-row latency.

The final desktop control in the wireless attempt joined in 0.301 s,
bootstrapped in 7.991 s and verified all 100,000 generated values; its closed
file was 65,597,440 B. This fresh publication peer is not the official
returning-member migration fixture. Logs and results are in
`/tmp/rocky-p09-device/device-final2` and `device-final3`, with adjacent
`.log` files. The user then requested iPhone for subsequent device tests.
The available target is the iPhone 18 Pro Max simulator on iOS 27.0;
physical iPhone qualification is not implied.

Release builds for macOS and Android arm64 passed. Frozen final artifacts:

| Artifact | SHA-256 |
| --- | --- |
| `final.dylib` | `cdda13e1db23129c0800fe5ad41028b08d85af70671b4b120ab9668f7d88d367` |
| `final.so` (debug sections stripped) | `bf847f26de725d2f9136877634791b06b241c63b24d77104959c6aa8c3e09868` |

The desktop load above used the initial build before table-specific cache
invalidation; its observations are not relabeled as final-build timings.
The final APK was repackaged after both final native builds and installed
and opened on the Pixel. Its SHA-256 is
`2ea63f0f3c67cca65efae615c855d14c2354176a114c5ea7938540aac7726165`.

Raw load results, sampled profile, prepared APK and build logs are under
`/tmp/rocky-p09-device`. The phase-start digest run is recorded in
`/tmp/rocky-p09-baseline-digest.log`.

### Verification observations

The final focused nextest run passes all 17 refresh, SQL and scaling tests,
including the golden digest, cache-disabled control, rollback, scoped refresh
and direct index-corruption checks. The SQL regression confirms an ordinary
commit does not restart held-history scans. The same release migration binary's eight-row two-step migration
passes, including closing-base equality and a one-transfer returning member.

The first official 100k migration attempt completed seed open in
148.692 s, the additive switch in 8.726 s and fill open in 208.097 s,
then failed with `Unreachable("exchange timed out")` during publication
(398.74 s total). Concurrent workspace builds were running; this observation
does not establish their causality and is not a successful full-fixture
qualification. Its log is `/tmp/rocky-p09-migrations.log`.

The final `./scripts/lint` and Conventional Commit check pass. The last
`./scripts/check` passed 203 unit tests and 1,409 of 1,410 integration cases,
with integration within its 300 s budget (234.70 s). One SDK driver case,
`a_connection_bound_to_a_kept_seal_reads_it_and_never_writes`, returned
`submission identity reused with different input`. Both candidate driver
binaries passed 50 isolated repetitions each. An original-workspace driver
binary without these replica changes reproduced the same error once in 50
repetitions. This is evidence of an existing intermittent failure, not a
green final full check. Logs: `/tmp/rocky-p09-check-complete.log` and
`/tmp/rocky-p09-kept-driver-comparison.log`.

A final isolated run of the rebuilt release migration binary also failed
in publication with `Unreachable("exchange timed out")`, after 230.29 s.
It measured seed open **72.081 s**, additive open **7.869 s** and fill open
**118.878 s**. The fill produced 6,251 frames / 77,236,456 body bytes
(12,355 B average, 16,112 B maximum). No builds or other test workloads
ran alongside this retry. Consequently concurrent builds alone cannot
explain the publication timeout. No returning-member file or full-fixture
digest result is claimed from this run. Log:
`/tmp/rocky-p09-migrations-isolated.log`.

### Final iPhone simulator qualification

At the user's request, subsequent device tests used only the iPhone 18 Pro
Max simulator (`14A05E5A-6C89-4EC4-9B05-D94C43C7CA94`, iOS 27.0).
A final-source `aarch64-apple-ios-sim` release library built successfully
in 2 min 9 s, then linked into the dedicated
`org.example.rocky.phase9load` Swift/UniFFI consumer. The initial probe
lacked iOS 27's required scene lifecycle and stopped before database work;
the harness was corrected before the measured run. No production code
changed for the probe.

The same sealed 100k source was copied to a fresh run. Both desktop
processes loaded the final macOS library above; the simulator loaded the
final iOS library. All three verified all 100,000 generated values before
and after the update sequence. Final identities:

| Role | Node identity |
| --- | --- |
| Source desktop | `3779aed8840a2b279425f74a59fa074dad8fe9b091d0418dab421f63e15c0d68` |
| New desktop issuer | `c8aba8fe31e80f9436c61d4e2dc7ed34e916f5d11625b6bdd286db0eebae2418` |
| iPhone simulator writer | `eb026ecd2364db942f0759db8feff156b088995f8e87b9385558fc662fe2761f` |

| Measurement | Result |
| --- | --- |
| Desktop join / bootstrap | 0.300 s / 8.009 s |
| Simulator join / bootstrap | 0.254 s / 9.604 s |
| Five empty simulator syncs | 152.7–196.5 ms |
| 20 alternating simulator ↔ source one-row updates, p95 | 71.34 ms |
| All updates also visible on the third desktop | Passed |
| Simulator closed file after updates | 65,122,304 B |
| New desktop closed file after updates | 65,617,920 B |
| Simulator app restart, offline reopen and all-row verification | Passed |

One-row latency includes SQL begin/update/commit and polling the receiving
peer until the new value appears. Every update was also checked on the
third desktop. Restart verification checked 100,000 rows, all unchanged
generated values, and the final updated value `phase9-final-19` without a
network. These are simulator results on one host, not physical phone/LAN
measurements or the official multi-step returning-member fixture. Source
shutdown exceeded the probe's two-second cleanup timeout, so its final
file size is not claimed from this run.

| Final iOS artifact | SHA-256 |
| --- | --- |
| `ios-final.a` | `6f60fc9485bd37968c034f8d2fc94fb20b230c7506fc204610b665c0369b5488` |
| Signed `RockyLoad.app/RockyLoad` | `d05f77304cfa60584f0a4034abc43ad3a1c35bf75fb7b093f99f9369dbf7e355` |

Raw results: `/tmp/rocky-p09-device/ios-final2/result.json`,
`/tmp/rocky-p09-device/ios-final2.log` and
`/tmp/rocky-p09-device/ios-restart.json`. Build logs and isolated consumer
sources are alongside them. The shopping application and physical iPhone
were not tested. The issuer-time and strict single-row statement gates,
and the official 100k migration publication timeout, remain unresolved.


## Correctness continuation and physical iPhone (2026-10-26)

This continuation does not complete phase 9. Following an unrelated test run
in the original workspace, the owner requested **correctness checks only** and
deferred the quiet-window performance rerun. No time, statement, storage,
admission, durability, retention or test-tier budget was increased.

### Final changes

- Selected-row replay walks ordered applied positions and checks membership
  with `EXISTS`, preserving the operation `LEFT JOIN`. It no longer rebuilds
  the remaining position set for every bounded page. Seal-drop listing uses
  the same cursor-first approach for candidates, tombstones and missing
  fields; its existing indexed presence check keeps quiet seals from scanning
  every row. Two regressions demand fewer than 2,000 SQLite VM steps for an
  eight-result page with 10,000 remaining entries. A missing operation stays
  visible to the corruption check.
- Pending-head queries constrain the outer operation to `status='pending'`,
  allowing the existing partial index to cover both sides of the lookup.
- Apply and finalization reuse decoded records and tombstone flags for
  canonical digest updates. The old encoded-row copy and its private wrapper
  are no longer retained. Ordinary updates preserve an unchanged key-index
  entry; moves and changed keys keep their existing paths.
- Buffered SQL commits queue maintenance before announcing the committed
  revision. Initialization drains it and finalizes the locally ready path
  before opening networking, avoiding migration seal finalization inside
  enrollment. Flushed commits retain synchronous post-flush maintenance:
  deferring it broke kept-checkpoint availability and let later rejected
  requests trigger pending finalization. Both existing regressions reproduced
  in isolation; no refusal or checkpoint assertion was relaxed.
- Bootstrap acquires the graph's required, verified historical schemas before
  importing an ancestor checkpoint. A new eight-row regression failed before
  this change with `Incompatible("tip schema is not installed")` and zero
  imported rows, then passed with all eight rows and migration values.
- The main writer is WAL-backed, but the spill profile followed SQLite's
  non-WAL pager, consistent with temporary history tables. The previous
  setting affected only the main cache; SQLite's default temporary cache
  setting was zero. The same total configured budget is now divided between
  main and temporary caches: **16 + 16 MiB** on desktops/iOS and **4 + 4 MiB**
  on Android. A regression reads both actual pragma values and checks their
  sum. Only cache allocation changes; `temp_store` and durability settings
  remain unchanged, and spilling is not disabled.
  This is not a claimed isolated speedup measurement.

The paired statement-count control on identical frames now reports:

| Ordinary update | Metadata cache disabled | Enabled |
| --- | --- | --- |
| 32 rows | 5.78125 statements/row | 5.40625 statements/row |
| 1 row, complete receive | 95 statements | 83 statements |

The three cached metadata tables are each read at most once per frame. The
unchanged-key regression forbids the redundant key-index insertion. The
strict eight-statement single-row gate is **not met**. The full trace is
available with `ROCKY_TRACE_RECEIVE=1` on the scaling test; it includes
admission, operation and position storage, quarantine/pending passes,
revision signing, transactions and post-commit recovery. No assertion was
changed to treat the batched result as satisfying the single-row gate.

### Migration diagnosis and correctness result

The original `Unreachable("exchange timed out")` came from enrollment, before
the fixture's publication sync. Registry read, bootstrap construction and
signing finished in approximately 8 ms; the registry commit entered automatic
seal finalization and outlasted the unchanged 30 s exchange deadline.
`/tmp/rocky-p09-enrollment-diagnosis.log` and
`/tmp/rocky-p09-pair-commit.sample` record that path.

After enrollment progressed, the first checkpoint bootstrap still failed.
A preserved-fixture activity diagnostic showed reconciliation completing in
115 ms, zero of 100,000 rows imported, and backoff with a pending scope.
The exact scope error was `Incompatible("tip schema is not installed")`:
its offered checkpoint used an earlier migration schema. The small
regression reproduces this without a 100k authoring run. Logs:
`/tmp/rocky-p09-publication-error.log`,
`/tmp/rocky-p09-historical-schema-before.log` and
`/tmp/rocky-p09-historical-schema-after.log`.

The official
`a_migration_of_100000_rows_converges_on_every_member` then **passed in
553.26 s**, within its unchanged 600 s budget. This is a correctness run,
not the deferred isolated issuer-performance qualification. It checked all
100,000 keys and values, seed and filled generations, matching closing bases,
retention accounting and exactly one returning-member sync. Its log is
`/tmp/rocky-p09-ready-path-migration.log`. This run preceded the final cleanup
that stopped retaining an unused encoded-row copy; final-source checks are
listed below.

| Fixture observation | Result |
| --- | --- |
| Final frames / body bytes | 6,251 / 77,235,448 B |
| Issuer main file at measurement | 111,476,736 B |
| Returning-member main file at measurement | 68,562,944 B |
| Returning-member WAL at measurement | 33,554,432 B |
| Returning-member syncs | 1 |
| Returning-member open / pull | 0.104 s / 34.438 s |
| Matching closing seal | `d6117dfd428acc2fbe8036d40960e1544c5bf874689f8e426acb97647595235f` |
| Matching closing checkpoint | `fa66f3f6849735c0aec54cb3c320e9bad23e8df79e472d191957a798d9fac378` |

The table reports the fixture's **main file and WAL separately**, before the
normal final closes. It does not relabel the 553.26 s full multi-step test as
an issuer-authoring timing. The isolated 70 s gate still requires its own
qualification. The fixed canonical-record corpus continues to produce
`c88490677cc3bfa8212b1c99e9c43def1f8049d6335636531383edad5544e389`.

### Physical phone result

T3 discovery still omitted the physical phone, but Xcode reported the exact
UDID connected and paired with Developer Mode enabled. A valid provisioning
profile included that UDID. Xcode installed and launched the signed isolated
`org.example.rocky.phase9physical` consumer on **david-iphone, iPhone 15,
iOS 27.0, UDID `00008120-001E28413ED8A01E`**. No shopping data or application
was used.

The measured three-node run used direct LAN control and direct iroh paths,
with relay and lookup disabled. The selected phone paths were
`192.168.0.196` to desktop `192.168.0.36`; network snapshots are in the result.
Every node checked all 100,000 values before and after 20 alternating
source/phone updates, and the third desktop received every update.

| Physical qualification observation | Result |
| --- | --- |
| Desktop join / bootstrap | 0.277 s / 8.053 s |
| Phone join / bootstrap | 0.332 s / 16.601 s |
| Five empty phone syncs | 831.7–1,045.6 ms |
| 20 alternating LAN updates, p95 | 128.625 ms |
| Closed source / new desktop files | 103,559,168 B / 65,601,536 B |
| Closed physical phone file | 65,122,304 B |
| Clean close failures | None |
| App restart, offline reopen and all-row verification | Passed |
| Offline final value | `phase9-final-19` |

Identities were source
`3779aed8840a2b279425f74a59fa074dad8fe9b091d0418dab421f63e15c0d68`,
new desktop
`c270ed7bbe9b9faf5c489c6e92bc7f44ed4c30fec9ca89dbec515ee38f4e14dc`
and physical phone
`f13b56b995daa679e0b07e1f21e08b6f45ea7828405fcf379e7d711615ae81a3`.

These measurements used the artifacts below, before the subsequent cache
split and historical-schema/startup refinements. They are physical LAN
results, not simulator results, and are not silently relabeled as timings
of later artifacts. The owner's correctness-only instruction defers another
performance run.

| Physical-run artifact | SHA-256 |
| --- | --- |
| macOS `bounded.dylib` | `3ab7dc6009a59c032408180b59bce4e54513bd6f4ac2941a455d9ac93d76c8b6` |
| Physical iOS Rust static library | `d3b9899f6a55ae674e2c89e89530579ad6ee67916a308327f365159f2ab773dd` |
| Signed instrumented physical app executable | `436c47cf451853b61720ad32bd8a8c637e45756056bce6dab888ce06c467a7f2` |

Raw result: `/tmp/rocky-p09-device/physical-20261026-2048/result.json`.
Logs: `/tmp/rocky-p09-device/physical-probe2.log` and
`/tmp/rocky-p09-physical-console2.log`. Offline restart result:
`/tmp/rocky-p09-device/physical-restart.json`. All owned desktop processes
were stopped, and the isolated phone app was terminated after verification.

### Final branch-source checks before merge (2026-10-27)

The final-source scaling suite passed all 12 tests, including the pinned
100k digest, ordered replay bounds, cache invalidation and rollback. Counts
remain 83 / 95 statements for cached / disabled-cache single-row receive,
and 5.40625 / 5.78125 per row for the 32-row receive. Log:
`/tmp/rocky-p09-final-scaling.log`.

Final release builds passed for native macOS, Android arm64 and physical
arm64 iOS. Logs are `/tmp/rocky-p09-corrected-host-build.log`,
`/tmp/rocky-p09-corrected-android-build.log` and
`/tmp/rocky-p09-corrected-ios-build.log`. The signed final iPhone consumer
was installed, reopened the existing isolated dataset offline and verified
all 100,000 values plus `phase9-final-19`; closed SQLite remained
65,122,304 B. This is correctness verification, not a new LAN timing run.
Result: `/tmp/rocky-p09-device/physical-corrected-offline.json`; console:
`/tmp/rocky-p09-physical-corrected-console.log`. The app was then terminated.

| Final-source artifact | SHA-256 |
| --- | --- |
| macOS dynamic library | `06e3831361cd99ee118622b7ce19bea76d2ac1313005d2a0c4a6e9a4a1e87d5e` |
| Physical iOS static library | `d53f2ce39585e3decf71a36aedf678bba952fe26f65d3856ea495e8617d386f4` |
| Android arm64 library | `43fbded3511d92af9ba145e6ec9f3658bfa55fb0ad04e968e7da3d2137443777` |
| Signed physical app executable | `59c6e3fd7a5751a00f8a9da57722ab4bb7ef051790d4797c36f9efed7f0ec8b0` |

The first full workspace run after these refinements was not green:
203 unit tests passed; integration reported 1,416 passes, two semantic
failures, and two isolated budget overruns (2.14 s and 2.11 s against 2 s).
Log: `/tmp/rocky-p09-final-check.log`. The two semantic failures reproduced
alone in `/tmp/rocky-p09-kept-repro.log` and
`/tmp/rocky-p09-restoration-repro.log`. Preserving synchronous maintenance
for flushed SQL commits fixed both, without changing their tests; corrected
runs are `/tmp/rocky-p09-kept-fixed.log` and
`/tmp/rocky-p09-restoration-fixed.log`.

On that correction, all three finalization/audit/rollback tests passed,
including 100k equality with full rebuild and the original timing assertions
(quiet 5.512 ms; 300-row cut 152.975 ms). All 12 scaling regressions also
passed with unchanged counts and pinned digest. Logs:
`/tmp/rocky-p09-corrected-finalization.log` and
`/tmp/rocky-p09-corrected-scaling.log`. These are correctness regressions,
not isolated issuer or LAN performance qualification.

Corrected lint and `scripts/commitlint --jj` passed. Lint log:
`/tmp/rocky-p09-corrected-lint.log`. The corrected full check ran from rebuilt archives using
`CARGO_BUILD_JOBS=2 NEXTEST_TEST_THREADS=4 ./scripts/check --from /tmp/rocky-p09-corrected-check-archives`.
Clippy passed during `--build`; all **203 unit and 1,417 integration tests
passed**, including both corrected compaction cases and the earlier
intermittent kept-seal driver case. Every per-test budget passed the
script's isolated verification. However, the integration suite took
**336.89 s against 300 s**, so the required full check is **not green**.
No budget or test classification was changed to accept that result.
Log: `/tmp/rocky-p09-corrected-check.log`; build log:
`/tmp/rocky-p09-corrected-check-build.log`.

The owner requested correctness checks only while a separate original-workspace
run was active. No new isolated issuer or LAN timing run was started. This
records the observed suite-budget failure without attributing the earlier
isolated migration timeout to host load. The phase remains incomplete:
83 is above eight, issuer timing is deferred, and the final full check
exceeds its suite budget.


## Merge to main (2026-10-27)

At the owner's request, merge change `wpworzkl` combines main `9faf1ce7`
with lighter-replica `eec2c71d` (`qrypvysm`), preserving both histories.
Jujutsu reported no conflicts. Main's SDK lifecycle, Rust application API,
SQL changes and `sdks/native` to `sdks/uniffi` rename are retained. The
original workspace was not edited. No remote push was requested or made.

Merged validation passed:

- `./scripts/lint` and `scripts/commitlint --jj`.
- Clippy via `CARGO_BUILD_JOBS=2 NEXTEST_TEST_THREADS=4 ./scripts/check --build /tmp/rocky-p09-merge-check-archives`.
- `CARGO_BUILD_JOBS=2 NEXTEST_TEST_THREADS=4 ./scripts/check --from /tmp/rocky-p09-merge-check-archives`: **203 unit and 1,435 integration tests passed**;
  integration **287.52 s against 300 s**, with all per-test budgets accepted
  by the existing verification script. Both metadata/bootstrap regressions
  and the corrected compaction regressions passed on the combined source.

Logs: `/tmp/rocky-p09-merge-lint.log`,
`/tmp/rocky-p09-merge-check-build.log`, `/tmp/rocky-p09-merge-check.log`.
This green run supersedes the pre-merge full-check status above without
removing the earlier 336.89 s observation. Main contributes additional SDK
cases; this is not an isolated replica performance comparison.

No new issuer timing or device run was performed for the merge. Device
artifact hashes and 100k observations above identify their pre-merge source,
not freshly built merged artifacts. Phase 9 remains incomplete: the strict
single-row statement gate is open and isolated issuer qualification remains
deferred. No budget, admission, durability or atomicity contract was relaxed.
