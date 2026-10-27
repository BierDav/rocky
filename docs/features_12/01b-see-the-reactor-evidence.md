# 1b. See the reactor: prerequisite review

Status: blocked before slice 1 (2026-10-27). Reviewed phase-start commit
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
