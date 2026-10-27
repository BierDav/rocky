# 1b. See the reactor: prerequisite review

Initial review: blocked before slice 1 (2026-10-27). Reviewed phase-start commit
`83ecee33` in Jujutsu change `xslnvstn` on a macOS arm64 host.
This is source inspection, not performance evidence. No runtime behavior changed.

## Maintenance is not gated by the durable watermark

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

## Consequence and required decision

The prerequisite for moving Flushed SQL maintenance onto this driver is not
met. Slices 1–5, desktop L0–L4 measurements, pure-work relocation and device
qualification were not attempted. Phase 1b is not complete.

Before resuming, authorize a prerequisite repair or revise the brief: the
driver needs to retain queued work on flush failure and only consume work
covered by the durable watermark, including concurrent commits. Deterministic
flush-failure and paused-flush tests should establish both properties. This
review does not select a scheduling or retry policy.

## Validation

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
