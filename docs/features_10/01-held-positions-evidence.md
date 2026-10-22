# Phase 1 evidence

Status: complete (2026-10-23). All phase-1 gates passed.

## Indexed coordinates and transactional writers

The file identity is `rocky.encrypted-replica.v33`; earlier identities are
refused before a writer opens. The three mandatory range/count tables and
`secure_position_sources` are created only for new files. There is no backfill.

The brief's proposed log buckets cannot implement the current security
contract on their own. `FramePath::admits` evaluates a domain author and its
domain sequence, including cuts within a source tip. Log sequence and signing
key do not recover that sequence: other scopes consume domain positions too.
Log ranges therefore also retain `domain_author` and
`delta = domain_sequence - log_sequence`. A run is maximal within this
extended bucket. Admission clips the run at the exact domain frontier;
it never admits a whole tip just because some of that tip is visible.

`secure_position_sources` retains authenticated decoded coordinates by
`(hash, kind)`. Insertion passes the already decoded header to `positions.rs`.
Status changes and deletions use triggers owned by that same module. This
covers bulk SQL, checkpoint switches, held replay bases, pruning, eviction,
tip drops and placement deletion without decoding a header for the index.
All index changes run inside the source writer's database transaction and
roll back with it. A source test prohibits writes outside `positions.rs`.

Existence counts retain both `count` (maximum of all retained frame/void
claims at a domain position) and `settled_count` (maximum excluding pending
frames). A single maximum cannot preserve the live pending/key predicate
when claims at one domain position differ. Placements contribute neither.
Readiness sums the selected count with an indexed SQL aggregate; received
existence records are counted through their existing `(scope,author,position)`
index. No signed existence record needs decoding in a normal readiness query.

## Oracle accounting

The brief requires equality scans under `cfg(test)` at every readiness and
frontier computation, but also asks for zero scans outside audit. Those
requirements cannot literally hold for the same counter. Normal-query
`frontier_queries` counts computations and `frontier_decodes` counts their
header decodes. Both remain separate from `position_oracles` (scan count,
actual decode count and elapsed nanoseconds). Equality is always enabled
in unit-test builds. The timing fixture measures the complete query and
subtracts only the separately timed equality block; it does not disable
checks, omit database queries or reuse a cached frontier after invalidation.
Production builds have no equality blocks or oracle counters. Audit rebuilds
both coordinate systems and existence counts independently from headers,
including source metadata, maximality and range boundaries.

Rejection snapshots still allow operation status changes. They now verify the
complete index oracle at each snapshot and compare source coordinates excluding
only the two status-derived bits. Raw-SQL restore fixtures explicitly invoke
the positions writer; production restores retain their transactional triggers.

## Qualification

Host: Apple M5 Pro (Mac17,8), 18 cores, 48 GiB RAM, macOS 27.0.1;
Rust 1.99.0 (`b940084d7`, 2026-09-28), the workspace's configured test profile.
No tier budgets or protocol admission requirements were relaxed.

Passed on the completed implementation:

- `./scripts/lint`: all modules passed (including eight lint-script tests).
- `./scripts/check`: Clippy passed; 177 unit tests passed; all 1,272
  integration tests passed (997 workspace and 275 SDK without defaults).
  Integration wall time was 193.41 s against the unchanged 300 s budget.
- The e2e timing fixture authored 10,000 real frames and checked one domain
  range and one log range. Across ten queries after invalidation, maximum
  `seal_waits` time was **119.792 µs**, and maximum frontier time was
  **65.791 µs**, each below 5 ms. Normal-query header-decode delta was zero.
  The separate equality oracle recorded 21 checks, 10,000 actual decodes
  and 366,737,040 ns. Total fixture time, including authoring, was 83.93 s.
- Corrupt domain ranges, log ranges and source metadata fail full audit with
  `Corrupt`. Missing or altered mandatory position table, index and trigger
  definitions refuse open without backfill; a wrong table shape reports
  `Corrupt` too. Format refusal covers v32 and other past and future
  identities without changing the file.
- Range tests cover sparse arrival, bridging, splitting, duplicate frame/void
  custody, pending versus settled existence maxima, decreasing maxima on
  removal, and rollback. The maximum-sequence gap regression terminates;
  count ranges beyond SQLite's maximum stored sequence are empty. An
  interleaved-scope regression cuts within an ancestor log run and verifies
  both scope frontiers against the scan.
- `NEXTEST_SUCCESS_OUTPUT=immediate NEXTEST_NO_OUTPUT_INDENT=1 ./scripts/check --e2e`:
  **76 passed**: 52 workspace tests and 24 SDK tests without defaults.
  Regular tier wall time was 315.75 s against the unchanged 900 s budget.
  The existing two 100k exemptions added 755.36 s; both tests remained
  within their unchanged 600 s nextest timeout. Total tier elapsed was
  1,071.10 s. No phase-1 gate remains open.

## 100,000-row fixture

Both runs used `rocky-sdk::migrations::a_migration_of_100000_rows_converges_on_every_member`.
Both converged in one member sync and passed the fixture's exhaustive row
checks, publication accounting and additive-switch assertions.

- Default features: seed open **51.887080875 s**; additive open
  **27.426761959 s**; fill open **150.219270167 s**; member pull
  **168.814657292 s**. Member main file: **1,273,307,136 bytes**.
  Complete test: **400.848 s**.
- SDK without default features: seed open **40.465789416 s**; additive open
  **25.018852209 s**; fill open **124.757885292 s**; member pull
  **160.729367708 s**. Member main file: **1,285,672,960 bytes**.
  Complete test: **353.507 s**.

[Raw stage times, accounting and all member dbstat objects](01-held-positions-100k.txt)
retain all 182 table/index measurements for each run, including byte size,
payload bytes and pages. The primary measurements in bytes are:

- Default: `items` 5,054,464; `secure_operations` 626,429,952;
  `secure_rows` 205,307,904. All live dbstat objects total 1,064,394,752.
- Without defaults: `items` 5,054,464; `secure_operations` 637,714,432;
  `secure_rows` 205,307,904. All live dbstat objects total 1,076,760,576.
- Each run: domain ranges 4,096; log ranges 4,096; existence counts
  4,096 (empty for this workload); decoded source coordinates 2,969,600;
  source domain index 2,183,168. The source log index was 2,772,992 with
  defaults and 2,818,048 without defaults. All position objects together
  used **7,938,048** and **7,983,104 bytes**, respectively.

File lengths include free pages; dbstat totals count live pages. These
are observations of the existing in-process Hub fixture, rather than a
controlled before/after comparison or a network throughput measurement.
Kotlin/native platform checks were not run; no Kotlin/native code changed.
The later phases' catch-up, row-size and finalization targets remain their
own gates. Phase 2 has not started.

## Working copy

Edits remain in Jujutsu change `nnrprlwq`, on parent `2ec156e9`.
No commit description, bookmark, push or shared-history rewrite was made.
The pre-existing documentation cleanup and fixture/vendor changes were
preserved. The following is the phase-1 change footprint, excluding that
pre-existing cleanup:

- `.config/nextest.toml`
- `.config/test-tiers.toml`
- `crates/rocky-sdk/tests/migrations.rs`
- `crates/rocky-secure/src/replica/checkpoint/audit.rs`
- `crates/rocky-secure/src/replica/checkpoint/replay_test_support.rs`
- `crates/rocky-secure/src/replica/compaction/anchors.rs`
- `crates/rocky-secure/src/replica/compaction/readiness.rs`
- `crates/rocky-secure/src/replica/compaction.rs`
- `crates/rocky-secure/src/replica/existence/seal.rs`
- `crates/rocky-secure/src/replica/frontiers.rs`
- `crates/rocky-secure/src/replica/generation.rs`
- `crates/rocky-secure/src/replica/held.rs`
- `crates/rocky-secure/src/replica/placements.rs`
- `crates/rocky-secure/src/replica/positions.rs`
- `crates/rocky-secure/src/replica/tips.rs`
- `crates/rocky-secure/src/replica.rs`
- `crates/rocky-secure/src/store.rs`
- `crates/rocky-secure/src/tests/bundles/payload.rs`
- `crates/rocky-secure/src/tests/compaction/receipts.rs`
- `crates/rocky-secure/src/tests/existence/scan_order.rs`
- `crates/rocky-secure/src/tests/formats.rs`
- `crates/rocky-secure/src/tests/positions.rs`
- `crates/rocky-secure/src/tests/tips/frontiers/headers.rs`
- `crates/rocky-secure/src/tests/tips/frontiers.rs`
- `crates/rocky-secure/src/tests.rs`
- `docs/features_10/01-held-positions-100k.txt`
- `docs/features_10/01-held-positions-as-tables.md`
- `docs/features_10/01-held-positions-evidence.md`
- `docs/features_10/README.md`
