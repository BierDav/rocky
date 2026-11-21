# Local Commit & Read Path Benchmark Results

- **Environment**: Apple M5 Pro, macOS (aarch64-apple-darwin), release build (`benchmarks/mesh` `cargo build --release`)
- **Command**: `./benchmarks/mesh/target/release/rocky-mesh-bench local /tmp/rocky-local 1000 100 50 256`
- **Baseline commit**: `c376b565319a275db21d7fa1c04916fc0487bda8`
- **After**: this branch (`t3/perf-local-commit-latency`)

## Before vs After (p50)

| Variant | Metric | Before (`c376b5653`) | After | Change |
| --- | --- | ---: | ---: | ---: |
| `sqlite-normal` | insert p50 (µs) | 16.0 | 9.1 | — |
| `sqlite-normal` | update p50 (µs) | 7.8 | 6.0 | — |
| `sqlite-normal` | read p50 (µs) | 1.2 | 1.0 | — |
| `sqlite-normal` | batch (rows/s) | 402,582 | 600,757 | — |
| `sqlite-normal` | disk (B/row) | 317 | 317 | — |
| `rocky-buffered` | insert p50 (µs) | 5,003.8 | 4,081.4 | -18.4% |
| `rocky-buffered` | update p50 (µs) | 4,870.3 | 4,098.3 | -15.9% |
| `rocky-buffered` | read p50 (µs) | 258.2 | 49.7 | -80.7% (5.2×) |
| `rocky-buffered` | scan p50 (ms) | 0.42 | 0.20 | -51.7% (2.1×) |
| `rocky-buffered` | batch (rows/s) | 7,671 | 8,222 | +7.2% |
| `rocky-buffered` | disk (B/row) | 6,551 | 6,540 | -0.2% |
| `sqlite-full` | insert p50 (µs) | 3,943.4 | 3,037.0 | — |
| `sqlite-full` | update p50 (µs) | 3,023.3 | 3,036.5 | — |
| `sqlite-full` | read p50 (µs) | 3.9 | 3.0 | — |
| `sqlite-full` | batch (rows/s) | 24,745 | 31,825 | — |
| `rocky-flushed` | insert p50 (µs) | 7,857.6 | 6,962.6 | -11.4% |
| `rocky-flushed` | update p50 (µs) | 8,049.4 | 7,779.0 | -3.4% |
| `rocky-flushed` | read p50 (µs) | 256.0 | 59.3 | -76.8% (4.3×) |
| `rocky-flushed` | scan p50 (ms) | 0.41 | 0.17 | -57.9% (2.4×) |
| `rocky-flushed` | batch (rows/s) | 5,855 | 6,378 | +8.9% |
| `rocky-flushed` | disk (B/row) | 6,528 | 6,545 | +0.3% |

## Rocky Phase Breakdown (`rocky-buffered`, p50 µs)

| Phase | Before | After | Change |
| --- | ---: | ---: | ---: |
| `begin_us` | 256.9 | 27.6 | -89.3% (9.3×) |
| `next_id_us` | 377.1 | 133.3 | -64.7% (2.8×) |
| `execute_us` | 86.1 | 146.0 | +69.5% |
| `commit_us` | 4,023.0 | 4,143.2 | +3.0% |

## Summary of Changes

1. **Cached credential-chain verification** (`crates/rocky-secure/src/replica/intents.rs`, `sql.rs`, `author.rs`, `crypto.rs`): Routed `author_log_frame`, `next_id`, and `author_frame_carried` through `Inner::verified_chain` and checked `crypto.chains` before `database_context` / `Extension::read`, avoiding per-commit and per-`next_id` ed25519 verification while preserving revocation and expiry checks.
2. **In-memory `applied_heads` and `latest_replayed_clock` caches** (`crates/rocky-secure/src/replica.rs`, `frontiers.rs`, `intents.rs`, `apply.rs`, `replay.rs`): Cached per-scope applied heads and latest replayed clock on `Inner`, updated in-place on `write_materialized_status` / `record_replayed` and invalidated on `invalidate_context()` (rollbacks, rebuilds, tip switches).
3. **Direct `status='applied'` insert + `opened_payload` reuse + `keep_chain` cache** (`crates/rocky-secure/src/store.rs`, `apply.rs`, `intents.rs`, `chains.rs`): Eliminated the redundant `UPDATE secure_operations SET status='applied'` (which triggered SQLite subjournal temp-file spills via `memjrnlWrite -> unixOpen`), avoided re-verifying/re-decrypting the just-authored frame, and fast-pathed `keep_chain` via `Inner.chains`.
4. **SDK connection pooling** (`crates/rocky-sdk/src/connection.rs`): Pooled idle un-bound `State` instances in `Registry` by `(read_only, durability)`, eliminating per-query and per-transaction `SecureSql` session creation and `SignedManifest` ed25519 verification.
