# See the wait: evidence

Status: complete (2026-10-27), with the application and device limits below.
Phase start: `txzxmumx`; implementation: `uuzywvqz` in workspace `see-the-wait`,
integrated into `ynnvllrn`.

## Implementation

`ReplicaLock` owns a private Tokio mutex. Borrowed and owned acquisitions
share one non-async `#[track_caller]` acquisition function. Every call supplies
`Interactive` or `Background`; raw mutex storage and uninstrumented `inner`
acquisitions are rejected by `scripts/lint.d/replica-lock` and the type boundary.
The transaction macro receives an acquired guard; it never acquires a mutex.

SQL BEGIN, writing autocommit and the requesting author/bundle group leader
are interactive. Receive leaders and post-flush maintenance are background.
Existing FIFO ordering, transactions, durability defaults and statements remain
unchanged. Holds above 1 ms emit debug events on `replica` with `site`, `class`,
`wait_us` and `hold_us`. Without a subscriber or qualification, acquisition and
release are the only clock reads. Histograms are replica-local, opt-in and
compiled only with the existing `qualification` feature; no dependency was added.

## Baseline coverage and method

The [measurement tables](01-see-the-wait-measurements-evidence.md) contain
percentiles, top holders by p99 and maximum, work units and per-unit costs.
Desktop L0–L4 and physical Pixel L0–L4 passed in both durability modes. Each accepted pass has
600 writes after 50 warm-up writes, paced at 100 ms with skipped missed ticks.
A complete durability pass remains usable if a later fixture preparation fails;
failed or incomplete measurement passes are excluded.

The observer is registered once before warm-up and load. Only notifications
for `write_probe` with the `local` cause satisfy the probe. Registration cannot
hide a background wait outside the timed write. Flushed calls the unmodified
`Database::transaction()` for every sample; Buffered opens a connection before
warm-up. Their aggregate difference includes connection setup and is not an
isolated fsync cost. Connection identities attribute lock and flush breakdowns
to the probe rather than concurrent writers.

Long stalls extend the run beyond 60 seconds. L1/L3 are finite loads and report
actual load duration; L2/L4 continue until the probe finishes. L4 pins a reader
snapshot and requires both passive and truncating checkpoints in full runs.
The host was shared with compilation/check work. The phone tests run the native
SDK with in-memory peer transport, measuring phone CPU/storage and contention.
They do not verify LAN performance, the shopping UI or iPhone latency.

`write_flush_wait` covers only the explicit asynchronous flusher wait. Zero
means no additional wait there, not an absence of durability or I/O work.
L4 produces nonzero flush waits. Per-unit costs charge the whole acquisition
to each observed frame, byte or row category, including other work in that hold;
do not add categories together. Checkpoint page costs divide by SQLite's returned
page count, not newly copied pages. Zero-page truncation has no per-page cost.

Reproduce all scenarios (the unset durability variable runs both modes):

```sh
cargo test --release -p rocky-sdk --no-default-features --features qualification \
  --test migrations write_probe:: -- --ignored --nocapture --test-threads=1
```

`ROCKY_PROBE_SMOKE=1` selects 100 rows, one warm-up and three samples.
`ROCKY_PROBE_DURABILITY=Flushed` or `Buffered` selects one durability pass.
All five two-mode release smoke scenarios pass; the final L1 corrections also
pass their two-mode smoke run. Android commands and device details are beside
the measurement tables. Wireless L1 writes its log on the phone so a USB/ADB
disconnect cannot discard output or terminate the test.

## L1 adjustment and fixture corrections

The owner approved an already writable member receiving a queued update of
all 100k migration-fixture rows on 2026-10-27. The peer authors disconnected
32-row transactions and flushes the final backlog transaction; reconnection
after warm-up resets peer retries. Every row must arrive during the load.

The first smoke failure, `no such table: write_probe`, was a fixture grant
error: `Device::invite()` grants only `items`. The fixture now grants both
tables. The earlier attribution to bootstrap projection availability was wrong;
that failure did not establish that the original L1 was unworkable.

Readiness retries the entire preparation transaction (including connection,
BEGIN and COMMIT) while the publication peer remains available. Row count alone
does not establish write readiness. After a full-size preparation exhausted
64 retries, readiness was corrected to reset peer backoff before each sync
through the existing networking-enable operation. The final desktop Buffered
run passes. The corrected phone bootstrap hit one seal-rebootstrap refusal,
then converged after reset and sync. Timed probe writes never suppress or retry
errors. USB disconnected during the first phone L1 attempt; its incomplete
output was discarded and the native test was restarted with a device-local log.

## Statement counts

Apple M5 Pro, arm64 macOS, test profile, targeted current-thread test:

```sh
ROCKY_TRACE_AUTHOR=1 cargo test -p rocky-secure --lib --features qualification \
  an_ordinary_update_applies_in_few_statements_per_row -- --nocapture
```

Author: **87** statements; one-row receive: **83**; 32-row receive:
**173 / 32 = 5.40625** per row. The paired phase-start receive test reports the
same 83 and 173 statements. The roadmap's earlier 6.40625 is not reproduced by
this corpus. Authoring is a new baseline, not a claimed reduction.
The [grouped trace](01-see-the-wait-statements-evidence.md) assigns each statement
to its first referenced table; transaction/control statements share a group.

## Paired sync and checkpoint import

Apple M5 Pro, release, the same full 100k two-step migration fixture, one run
per revision on the shared host. `ynnvllrn` has no secure, SDK or storage changes
from phase-start `txzxmumx` (verified with `jj diff`). Candidate: `uuzywvqz`.
Compilation/check load varied between these sequential runs; no controlled
speedup is claimed.

```sh
cargo test --release -p rocky-sdk --no-default-features --features qualification \
  --test migrations a_migration_of_100000_rows_converges_on_every_member \
  -- --exact --nocapture --test-threads=1
```

| Measurement | Phase start | Instrumented |
| --- | ---: | ---: |
| Member pull, seconds | 31.260787 | 20.797623 |
| Issuer file, bytes | 125673472 | 115785728 |
| Member file, bytes | 69959680 | 69324800 |

Both full row/fingerprint and file checks pass. The observed member pull is
below 110% of the paired start (34.386866 s). Phone LAN sync non-regression is
not verified by the in-memory transport probe.

Full-acquisition import costs, nanoseconds per row. Sites are relative to
`crates/rocky-secure/src/`; all are Background. Phone setup includes 100k item
rows and the probe row. These bootstrap measurements precede warm-up and are
separate from the L1 write-probe tables.

| Host / site | Unit | Holds | Rows | p50 | p95 | p99 | Max |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Desktop `replica/checkpoint/transfer.rs:1661:36` | checkpoint_row | 847 | 100000 | 93511 | 441300 | 859353 | 1517821 |
| Desktop `replica/checkpoint/transfer.rs:1715:36` | base_row | 1 | 100000 | 23562 | 23562 | 23562 | 23562 |
| Pixel `replica/checkpoint/transfer.rs:1661:36` | checkpoint_row | 769 | 100001 | 476080 | 973891 | 1445538 | 13041000 |
| Pixel `replica/checkpoint/transfer.rs:1715:36` | base_row | 2 | 100001 | 26353 | 149181000 | 149181000 | 149181000 |

Desktop base installation holds for 2.356266 s. The two Pixel installations
hold for 149.181 ms (one probe row) and 2.635374 s (100k item rows). The second Pixel preparation also records 100k base rows in one 39.5204-second
acquisition (395204 ns per row). Both preparations' full holder and per-unit
tables are appended to the measurement evidence. The one-row scope shows
fixed work in the acquisition: row bounds alone do not establish
the phase 2 hold target for this path. This phase records that constraint
without changing the phase split or implementation.

## Findings for later phases

Desktop Flushed L1 receives all 100k updates in 39.44 s, with write p95
325.879 ms and maximum 682.156 ms. Pixel Flushed L1 receives them in 54.30 s,
with p95 340.441 ms and maximum 920.541 ms. Refreshed L3 Flushed maxima are
88.54 s on desktop and 185.22 s on Pixel, dominated by compaction. These exceed
the later interactive-wait goals. One finite maintenance stall can sit above
p99 in a 600-sample run; retain maximum and load overlap in future comparisons.
No bound, priority, durability default, format or SQL statement changed here.

## Verification and limits

- `./scripts/lint` and `./scripts/commitlint --jj` pass.
- `./scripts/check` passes: 204 unit tests (143.5 ms wall), 1,438 integration
  tests (256.10 s against 300 s), including SDK without default features.
- Five explicit qualification tests pass: caller event, owned histogram,
  SQL classes, author classes and statement counts. Qualification-only
  migration Clippy passes with `-D warnings`.
- Kotlin `check` passes: 202 tasks, 5m 49s, including JVM, Android compilation,
  iOS simulator, schema qualification and binding checks. An earlier run hit
  the service-owner command timeout; the complete rerun passes.
- Shopping desktop and Android builds pass. `./examples/shoppinglist-app/check`
  fails three JVM tests: schema upgrade, revoked-device reconciliation and
  household revocation observation. A serial diagnostic rerun was terminated
  after more than ten minutes stalled in trusted bootstrap. Thread dumps were
  captured in `/tmp`. Earlier phase-start rechecks also showed shopping
  failures; the cause of every failure here is not established. This is not
  a passing application qualification claim.

The new event test uses an isolated nextest process because concurrent callsite
registration can invalidate its temporary capture subscriber. Its threshold is
injected by moving the recorded acquisition instant back 2 ms, without a sleep.
The histogram test stays in unit; no existing test or tier budget moves.
An earlier full run passed its integration tests but exceeded the suite budget
amid overlapping builds; the final run above passes the unchanged budgets.
