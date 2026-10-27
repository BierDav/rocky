# 1b. See the reactor: implementation and qualification

Status: complete with documented attribution limits (2026-10-27).
Changes: `nktmmvzx` and `orswmwyu`.

Qualification builds now record named reactor polls, SQL queue/service timing,
and request-interval coverage. SQL status checks are Interactive. Flushed SQL
commits queue maintenance and return after durability, while the owner driver
retains work on flush failure and checks the current durable watermark before
consuming it. Normal builds have no measurement state or clock reads.

The desktop and physical Pixel L0–L4 matrices are complete in both modes,
with 600 measured writes per block. Original remainder exceptions
are recorded below with request/caller coverage, rather than reported as a
universal sub-millisecond result. The full Rust check, Kotlin check, shopping
check, statement guard and paired 100k sync guard have passed.

## Initial prerequisite review

Initial review: blocked before slice 1 (2026-10-27). Reviewed phase-start commit
`83ecee33` in Jujutsu change `xslnvstn` on a macOS arm64 host.
This is source inspection, not performance evidence. No runtime behavior changed.

### Maintenance was not gated by the durable watermark

The phase brief requires verifying that the maintenance driver orders
maintenance after the durable watermark, and says to stop and report if it
does not. The execution contract also requires stopping when a settled
prerequisite does not match the code.

`settle_commits` in `crates/rocky-sdk/src/database.rs` observes a revision,
discards the result of `replica.flush().await`, and unconditionally calls
`replica.maintain().await`. A device flush failure therefore does not prevent
maintenance from running.

`Flusher::flush` in `crates/rocky-secure/src/replica.rs` advances `durable`
only after the WAL flush succeeds. Opening or syncing the WAL can fail before
that update; its existing test fault injection also returns before the update.
`EncryptedReplica::maintain` takes the entire `deferred_maintenance` set with
`std::mem::take` and runs it without checking the watermark or restoring work
after a failed flush. Thus a failed flush can leave the watermark behind the
commit while the driver drains that commit's maintenance.

There is also a concurrent-commit gap even when the flush succeeds:

1. `EncryptedReplica::flush` snapshots commit N under the replica lock, then
   releases the lock for the blocking flush.
2. A SQL commit N+1 can enqueue maintenance during that flush.
3. The flush guarantees the watermark through N. The driver subsequently
   acquires the replica and drains all queued scopes, including N+1's work.

The queue is a `BTreeSet<ResourceScope>`, with no per-scope commit watermark.
The driver does not recheck durability after acquiring the maintenance lock.
Unlike `flush_guard` in the same replica module, it has no retry loop to cover
commits made while the lock was released. Physical flushing might cover some
newer commits, but the durable watermark does not guarantee that coverage.

### Initial consequence and required decision

The prerequisite for moving Flushed SQL maintenance onto this driver is not
met. Slices 1–5, desktop L0–L4 measurements, pure-work relocation and device
qualification were not attempted. Phase 1b is not complete.

Before resuming, authorize a prerequisite repair or revise the brief: the
driver needs to retain queued work on flush failure and only consume work
covered by the durable watermark, including concurrent commits. Deterministic
flush-failure and paused-flush tests should establish both properties. This
review does not select a scheduling or retry policy.

### Initial validation

- `scripts/commitlint --jj` passed.
- `./scripts/lint` failed because `ktlint` is unavailable; the other modules
  passed.
- `./scripts/check` passed formatting, workspace Clippy and all 204 unit tests.
  All 1,438 integration executions passed their functional assertions, but the
  command exited 1 on the existing
  `rocky-sdk::tips trusted::a_trusted_identity_opens_a_branch_that_devices_on_main_never_see`
  timing gate: best isolated runs were 2.14 s and 2.21 s for the two feature
  configurations, above the 2 s budget. The total integration tier stayed
  within its 300 s budget (258.90 s). No timing limits were changed.
- Kotlin, shopping app, release latency and physical-device checks were not
  run: implementation stopped at the prerequisite review, with documentation
  changes only.

## Authorized prerequisite repair (2026-10-27)

The owner accepted the repair and continuation of phase 1b. Change `nktmmvzx`
adds a replica-owned durability check under the writer lock before deferred
maintenance is removed from the queue. If any current commit is not covered,
maintenance returns without consuming work. The SDK driver retries failed
flushes using its existing bounded exponential backoff, and retries skipped
maintenance on a new revision or a one-second retry deadline. It checks existing
work on startup too. Startup migration maintenance now flushes first.

This preserves the existing scope set and format. Maintenance reads current
state, so checking the whole current commit watermark avoids pretending that
an older scope timestamp provides a snapshot. No lock is held during device
flushing, and no application or network ownership changes.

`cargo test -p rocky-secure tests::sql::maintenance --lib -- --nocapture`
passed both deterministic regressions (0.18 s total): a failed flush retains
work until retry, and a paused flush followed by a second commit cannot drain
maintenance until another flush covers the second commit. These are behavioral
checks, not latency measurements.

## Slice 1: instrumentation

Qualification builds name every SDK reactor task at its spawn site and record
polls longer than 1 ms. SQL calls record send-to-first-poll queue time and
first-poll-to-reply service time, keyed by connection. Connection opening and
closing are included because Rust transactions await both. Normal builds have
no timing state or clock reads from this instrumentation.

The probe adds `write_reactor_delay`, `write_sql_in_call`, `write_unattributed`
and a longest-poll table. Lock holds span multiple SQL calls and can overlap
queue delays within a transaction. Consequently the requested subtraction is
not an exclusive decomposition: `write_unattributed` records the nonnegative
remainder, and `write_attribution_overlap` explicitly reports any excess in the
sum instead of hiding negative remainders. The real SDK connection regression
`database::tests::blocking_reactor_task_and_queued_sql_call_are_attributed`
passed with `--no-default-features --features qualification` (0.13 s). It queues
a connection call while a named local task is deliberately blocked, then
checks both the named poll and the queue delay. The five release smoke
scenarios also passed in both durability modes (24.10 s); these use 100 rows
and three samples, so they are validation, not qualification measurements.

Full results are being recorded in the
[attribution tables](01b-see-the-reactor-measurements-evidence.md). Initial
600-sample desktop L0 remainder p95 is 786 us Flushed and 648 us Buffered.
Long polls are present (SQL max 84.775 ms Flushed; maintenance max 106.771 ms
Buffered), so the reactor hypothesis is not refuted by the idle runs.

## Interactive path changes

`SecureSql::status` performs the same authority and grant checks as the general
session status, using an Interactive acquisition. Driver connections call that
boundary between statements; transactions and pinned read snapshots retain
their existing authority rules. The SQL handle already owns its session, so the
driver no longer retains a duplicate session field.

Every SQL commit queues its scopes before publishing its revision. Flushed and
coordinated commits still wait for the same durability flush, but return without
reacquiring the replica for maintenance or WAL checkpointing. The driver uses
the repaired watermark gate before consuming these scopes. Additional regression
tests exercise a successful Flushed commit with maintenance still queued, a
failed Flushed acknowledgement with committed rows and queued work intact until
retry, and the acquisition classes of SQL versus general session status.

The probe also reports signed `write_server_remainder` (SQL service minus lock
wait, hold and flush) and `write_client_remainder` (write time outside the union of SQL request/reply
intervals, clipped to that write). `write_sql_covered` reports that union.
Overlapping cleanup calls are counted once, so the client remainder measures
application scheduling and reply handling without subtracting overlapping
calls twice. Negative server remainders expose overlapping lock intervals;
they are not clamped into apparently complete attribution. The original remainder and overlap
metrics remain available for comparison.

`cargo test -p rocky-secure --features qualification --lib tests::sql::maintenance
-- --nocapture` passed all five regressions in 0.23 s
on the resumed change `orswmwyu`. This includes the paused-flush concurrent
commit regression from the prerequisite repair.

## Transition evidence from the initial full runs

The desktop L3 Flushed run captured a `call` poll of 196.834277 s while the
probe queued for 196.801230 s. The same interval contains compaction holders
up to 113.596783 s (`replica/compaction.rs`) and tip finalization holders up to
27.490251 s (`replica/tips.rs`). These are the existing wholesale transitions
assigned to phase 7, not evidence for moving SQLite to a blocking pool. The
remaining steady-state SQL and maintenance polls belong to phase 2. Initial
L0–L2 desktop remainder p95 values were all below 1 ms; L3 Flushed was 7.329 ms.
The final probe records request-interval coverage to distinguish that remaining
gap without double-counting overlapping statement cleanup calls.

The release regression
`write_probe::overlapping_cleanup_calls_cover_each_write_instant_once` passed
on `orswmwyu` (0.00 s). It checks the union against overlapping, unordered,
outside-window and empty intervals without wall-clock waits. A diagnostic
run before this correction produced negative client remainders; those blocks
are excluded from the final after-change tables.

The phone baseline was deliberately stopped after completed L0 Flushed,
L0 Buffered and L1 Flushed blocks. Preparing each 100k-row phone fixture took
roughly half an hour. Remaining device time is assigned to the final-code
matrix instead of duplicating the remaining baseline fixtures. This is partial
before-change coverage; no paired phone speedup claim is made.

In the corrected desktop L0 Flushed run, unexplained p95 was 3.412 ms. The
server remainder p95 was 0.191 ms; time outside the request-interval union
(`write_client_remainder`) had p95 13.897 ms. That boundary covers the Rust
transaction wrapper constructing calls and consuming replies, including
scheduling between a reply and the next request, rather than time inside a
reactor SQL call. These are separate per-sample distributions, not additive
percentiles. Buffered L0 remainder p95 was 0.377 ms, with server remainder
0.209 ms and client remainder 0.312 ms. The heavily shared host is a plausible
source of the caller scheduling gap; this attribution does not establish a
controlled speedup or isolate OS scheduling from caller CPU work.

The initial desktop matrix completed successfully: five ignored release tests,
both durability modes, 600 measured samples per block (3,017.00 s including
fixture preparation). The original release smoke matrix also passed all five
scenarios after the interactive changes (13.74 s). These validate the fixtures;
no test budgets or admission behavior were changed.

The first complete workspace check exposed two compaction fixtures that relied
on Flushed SQL performing inline maintenance. The kept-checkpoint fixture now
flushes and runs the owner maintenance step after the post-seal write. The
restoration fixture does the same before capturing its retained frames and
evicting them. All signed identity-mark and rejected-frame snapshot assertions
are preserved. These fixtures use a bare replica, so they must drive the owner
step that the SDK normally runs in the background.

Both compaction regressions passed after explicitly driving maintenance
(0.238 s and 0.279 s). The final desktop matrix also completed: five release
probe scenarios, both durability modes, 600 samples per block, 1,993.96 s
including fixture preparation. The phase-start sync guard is being checked
separately from these deliberately shared-host attribution runs.

## Final desktop attribution

All ten desktop scenario/mode blocks completed. Seven have original
`write_unattributed` p95 below 1 ms. The three exceptions are:

| Scenario | Original remainder p95 | Server diagnostic p95 | Outside timed requests p95 |
| --- | ---: | ---: | ---: |
| L0 Flushed | 3.412 ms | 0.191 ms | 13.897 ms |
| L1 Buffered | 2.279 ms | 0.291 ms | 3.594 ms |
| L2 Buffered | 3.065 ms | 0.700 ms | 3.531 ms |

The request-interval union and its caller complement partition each measured
write exactly, up to microsecond rounding. The complement identifies time outside those timed intervals, including the
Rust transaction/connection wrapper between submitting calls and consuming
replies. The reply bound is stamped before recording the timing and handing
the reply to the oneshot channel, so the complement also includes recording
and reply-handoff overhead. The union covers named SQL/connect/disconnect
service and queue waits; this is a coverage partition, not a causal CPU-time
partition.
The original lock-plus-queue remainder omits that caller boundary and remains
overlap-sensitive. Server diagnostics sum service intervals and subtract
transaction-spanning holds, so positive as well as negative values can reflect
overlap; they are not an exclusive CPU-time decomposition. Separate p95 values
must not be added or subtracted as if they were one sample.

These results explain the original remainder exceptions at the request/caller
boundary. They do not isolate OS scheduling from caller CPU work or claim that
interactive latency meets the later steady-state targets. All long polls are
listed in the measurement tables. SQLite SQL/maintenance polls remain for
phase 2, and wholesale compaction/finalization remains for phase 7. No newly
isolated pure computation site exceeded 1 ms: receive signature verification,
decryption and verification already use `replica/crypto.rs`'s blocking pool,
so this phase makes no speculative pure-work relocation.

## Final verification

- `./scripts/lint` passed.
- `./scripts/check` passed formatting, workspace Clippy, 204 unit tests and
  1,443 integration executions. Integration wall time was 235.36 s against
  its unchanged 300 s budget; all individual timing gates passed.
- The receive statement guard
  `tests::scaling::an_ordinary_update_applies_in_few_statements_per_row` passed:
  32-row receive 5.40625 statements/row (uncached 5.78125), one-row receive 83,
  author 87. This preserves the phase-1 corpus and stays below 6.5 per row.
- Kotlin is rerun after prebuilding `rocky-voter`, followed by the shopping
  app check. Its earlier service-owner failure was the test's 180-second
  `cargo build` subprocess waiting on the workspace build lock, before any
  service-owner assertions executed. No timeout or test budget was changed.

- `sdks/kotlin/gradlew -p sdks/kotlin check publishToMavenLocal` passed
  (397 tasks, 6 m 11 s), including the service-owner test and iOS simulator
  checks. Its published artifacts were then used by the shopping app.
- `./examples/shoppinglist-app/check` passed its full rerun, including JVM
  tests and desktop/Android compilation. One earlier parallel run failed the
  schema-upgrade test's immediate post-resume reconciliation assertion; the
  unchanged test passed alone in 3.353 s and in the complete rerun. This is an
  observed transient live-connection assertion, not a test-timeout increase or
  a production-code workaround.

## Paired sync guard

Apple M5 Pro, 18 cores, arm64 macOS; release,
`--no-default-features --features qualification`. One full 100k two-step
migration run per revision, sequentially after host builds/checks completed.
Baseline `rzszssxw` / `0b097aa0` has the same crate sources as the features_12
start `txzxmumx` / `41195190`; their intervening diff contains CI/scripts only.
Candidate: `orswmwyu`. Both complete row/fingerprint, wire-size and file checks
passed. This is one pair, not a statistically established throughput claim.

```sh
cargo test --release -p rocky-sdk --no-default-features --features qualification \
  --test migrations a_migration_of_100000_rows_converges_on_every_member \
  -- --exact --nocapture --test-threads=1
```

| Measure | Phase start | Candidate |
| --- | ---: | ---: |
| Samples | 1 | 1 |
| Member pull | 10.213180666 s | 11.035976541 s |
| Member main file before close | 69,255,168 B | 68,820,992 B |
| Member WAL before close | 33,554,432 B | 33,554,432 B |
| Full fixture elapsed | 310.23 s | 287.64 s |

Candidate/baseline member pull is 1.080562, below the 1.10 guard
(11.234498733 s). With one sample, p50/p95/p99/max all equal the observed
member-pull value. One-minute host load was 9.66 before the baseline, 5.62
between runs and 5.87 after the candidate, on 18 cores. No other agent builds
or checks ran during this pair; ordinary host activity was not controlled.
The physical phone probe uses in-memory transport and does not qualify LAN
sync latency or throughput; those device network guards remain for phase 6.

## Phone attribution

All ten final physical Pixel blocks completed, with 600 measured writes each.

| Scenario | Original remainder p95 | Server diagnostic p95 | Outside timed requests p95 |
| --- | ---: | ---: | ---: |
| L0 Flushed | 3.685 ms | 0.736 ms | 4.218 ms |
| L0 Buffered | 4.793 ms | 1.780 ms | 4.698 ms |
| L1 Flushed | 4.805 ms | 0.812 ms | 6.217 ms |
| L1 Buffered | 8.333 ms | 1.205 ms | 7.480 ms |
| L2 Flushed | 3.461 ms | 0.890 ms | 5.824 ms |
| L2 Buffered | 5.507 ms | 0.944 ms | 5.386 ms |
| L3 Flushed | 20.090 ms | 0.933 ms | 12.427 ms |
| L3 Buffered | 6.909 ms | 3.128 ms | 6.539 ms |
| L4 Flushed | 2.823 ms | 48.102 ms | 4.415 ms |
| L4 Buffered | 6.309 ms | 0.502 ms | 9.004 ms |

These are separate distributions. Request coverage and its caller complement
identify the remaining wall-time boundaries; the summed server diagnostic is
overlap-sensitive and is not an exclusive CPU-time decomposition. The caller
boundary includes connection/transaction wrapper work, scheduling between
replies and subsequent requests, and timing-record/reply-handoff overhead.
This is an explanation at those boundaries,
not a universal sub-millisecond result or a measured isolation of OS scheduling.

The final headless Pixel release run passed all five probe scenarios in both
durability modes, with ten 600-write blocks after 50 warm-up writes per block.
The harness reported 5,119.35 s including fixture preparation and cleanup.
Its interval-union regression also passed on the device. The durations use
process monotonic clocks; device power state and ordinary system activity were
not controlled. This qualifies the measured in-memory SDK path, not a phone
UI, iPhone, or LAN transport performance claim.

The implementation was rebased onto the concurrent release-publishing fix
`fca4962a`. Its Rust sources, Cargo manifests, lockfile and Cargo configuration
remain identical to tested `5eac0bb4`; the rebase added only the release workflow
and scripts. Lint and commitlint passed after the rebase, including the new
11-test release-publishing suite.

## Integration

Integrated into the original workspace in Jujutsu change `qyxtuntm`, preserving
the concurrent release-publishing fix `fca4962a`. The integration tree matched
the qualified implementation before adding this evidence note; it introduced
no merge conflicts or additional runtime changes.

Both temporary Jujutsu workspaces were forgotten and their directories removed.
The task's before/after phone binaries and fixture directories were removed
from `/data/local/tmp`; the measured tables remain in this roadmap.
