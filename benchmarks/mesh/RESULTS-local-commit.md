# Local Commit & Read Path Benchmark Results

- **Environment**: Apple M5 Pro, macOS (aarch64-apple-darwin), release build (`benchmarks/mesh` `cargo build --release`)
- **Command**: `./benchmarks/mesh/target/release/rocky-mesh-bench local /tmp/rocky-local 1000 100 50 256`
- **Baseline commit**: `c376b565319a275db21d7fa1c04916fc0487bda8`
- **After**: this branch (`t3/perf-local-commit-latency`)

## Before vs After (p50)

| Variant | Metric | Before (`c376b5653`) | After | Change |
| --- | --- | ---: | ---: | ---: |
| `sqlite-normal` | insert p50 (µs) | 16.0 | 16.7 | — |
| `sqlite-normal` | update p50 (µs) | 7.8 | 7.7 | — |
| `sqlite-normal` | read p50 (µs) | 1.2 | 1.1 | — |
| `sqlite-normal` | batch (rows/s) | 402,582 | 430,391 | — |
| `sqlite-normal` | disk (B/row) | 317 | 317 | — |
| `rocky-buffered` | insert p50 (µs) | 5,003.8 | 1,269.0 | -74.6% (3.9×) |
| `rocky-buffered` | update p50 (µs) | 4,870.3 | 1,795.4 | -63.1% (2.7×) |
| `rocky-buffered` | read p50 (µs) | 258.2 | 46.4 | -82.0% (5.6×) |
| `rocky-buffered` | scan p50 (ms) | 0.42 | 0.17 | -59.1% (2.4×) |
| `rocky-buffered` | batch (rows/s) | 7,671 | 9,306 | +21.3% |
| `rocky-buffered` | disk (B/row) | 6,551 | 6,528 | -0.4% |
| `sqlite-full` | insert p50 (µs) | 3,943.4 | 3,072.2 | — |
| `sqlite-full` | update p50 (µs) | 3,023.3 | 3,039.0 | — |
| `sqlite-full` | read p50 (µs) | 3.9 | 2.5 | — |
| `sqlite-full` | batch (rows/s) | 24,745 | 23,775 | — |
| `rocky-flushed` | insert p50 (µs) | 7,857.6 | 5,987.0 | -23.8% |
| `rocky-flushed` | update p50 (µs) | 8,049.4 | 6,105.6 | -24.1% |
| `rocky-flushed` | read p50 (µs) | 256.0 | 51.5 | -79.9% (5.0×) |
| `rocky-flushed` | scan p50 (ms) | 0.41 | 0.18 | -55.2% (2.2×) |
| `rocky-flushed` | batch (rows/s) | 5,855 | 6,971 | +19.1% |
| `rocky-flushed` | disk (B/row) | 6,528 | 6,532 | +0.1% |

## Rocky Phase Breakdown (`rocky-buffered`, p50 µs)

| Phase | Before | After | Change |
| --- | ---: | ---: | ---: |
| `begin_us` | 256.9 | 19.8 | -92.3% (13.0×) |
| `next_id_us` | 377.1 | 94.0 | -75.1% (4.0×) |
| `execute_us` | 86.1 | 73.8 | -14.3% |
| `commit_us` | 4,023.0 | 2,430.3 | -39.6% |

## Summary of Changes

1. **Cached credential-chain verification** (`crates/rocky-secure/src/replica/intents.rs`, `sql.rs`, `author.rs`, `crypto.rs`): Routed `author_log_frame`, `next_id`, and `author_frame_carried` through `Inner::verified_chain` and checked `crypto.chains` before `database_context` / `Extension::read`, avoiding per-commit and per-`next_id` ed25519 verification while preserving revocation and expiry checks.
2. **In-memory `applied_heads` and `latest_replayed_clock` caches** (`crates/rocky-secure/src/replica.rs`, `frontiers.rs`, `intents.rs`, `apply.rs`, `replay.rs`): Cached per-scope applied heads and latest replayed clock on `Inner`, updated in-place on `write_materialized_status` / `record_replayed` and invalidated on `invalidate_context()` (rollbacks, rebuilds, tip switches).
3. **Direct `status='applied'` insert + `opened_payload` reuse + `keep_chain` cache** (`crates/rocky-secure/src/store.rs`, `apply.rs`, `intents.rs`, `chains.rs`): Eliminated the redundant `UPDATE secure_operations SET status='applied'` (which triggered SQLite subjournal temp-file spills via `memjrnlWrite -> unixOpen`), avoided re-verifying/re-decrypting the just-authored frame, and fast-pathed `keep_chain` via `Inner.chains`.
4. **SDK connection pooling** (`crates/rocky-sdk/src/connection.rs`): Pooled idle un-bound `State` instances in `Registry` by `(read_only, durability)`, eliminating per-query and per-transaction `SecureSql` session creation and `SignedManifest` ed25519 verification.
5. **Coalesced background `settle_commits` flushing** (`crates/rocky-sdk/src/database.rs`): Coalesced `revisions.changed()` wakeups with a 5 ms quiet / 25 ms max window before calling `replica.flush()`, eliminating back-to-back APFS `F_FULLFSYNC` contention against `Durability::Buffered` WAL writes without affecting `Durability::Flushed`, `Database::flush()`, sync, or shutdown durability.
6. **Single-roundtrip `Command::Query` + `Rows` drop/finish fast-paths** (`crates/rocky-sdk/src/connection.rs`, `database.rs`, `sql.rs`): Added `Command::Query` / `Registry::query_once` so single-page `Database::query` calls execute on a pooled read-only `State` and return it to the pool in one reactor round-trip, and skipped `discard` / `finish` round-trips when `page.done` is already true.
