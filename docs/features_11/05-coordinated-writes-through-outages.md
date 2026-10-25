# 5. Coordinated writes through outages

Status: complete (2026-10-25); see evidence for the workspace baseline failure.
Owner: `rocky-secure` coordinated writes; `rocky-coordinator`
service, client and Raft store; `rocky-core` coordination types. Depends on
nothing. Effort: one week. Follow the
[execution contract](README.md#agent-execution-contract). Evidence is recorded in
[the phase report](05-coordinated-writes-through-outages-evidence.md).

## Inputs and objective

Read the [review](00-reliability-review.md), finding 12. Today a
coordinator timeout aborts a staged append the coordinator may already have
assigned, which blocks the log until the deadline and evicts the in-sync
set; device clock skew surfaces as `Conflict`; the proposer re-sends every
frame and the full head while it waits; and the Raft store syncs files
under its state lock on the async runtime.

After this phase a coordinated write survives coordinator unavailability
and leader changes, a skewed clock gets a clear error, and waiting costs
little.

## Settled

- **Retry the same append.** On `Unavailable` or a transport error, the
  staged entry stays staged and the same append is sent again with the
  same sequence, with backoff, until the caller's deadline. The
  coordinator's deduplication returns the original assignment. Only a
  definite refusal (`Sealed`, `Configuration`, `Denied`, `Backpressure`)
  aborts.
- **The coordinator sets deadlines.** An append carries a duration bounded
  by the coordinator's cap; the coordinator computes the deadline from its
  own clock. A request outside the voters' ±30 s window is refused with
  `ClockSkew { coordinator_time }`, which the SDK reports as such, not as
  `Conflict`.
- **Cheap waiting.** The proposer polls a pending-status read that returns
  the acknowledged set without the hash window, and re-sends frames only to
  members that have not acknowledged.
- **Store I/O off the state lock.** Raft log file writes and `sync_all`
  run on blocking threads with a lock for the log file separate from the
  in-memory state; head reads and pre-checks do not wait on disk.
- **Explicit deadline timer.** The leader resolves pending entries from a
  timer keyed to the earliest deadline, with retry backoff on failure, and
  watches metrics only for leadership changes.
- **Leader hint.** Replies name the current leader; the client tries the
  last voter that answered and the hinted leader first.
- **Format.** The coordinator wire format moves one version for the
  duration, `ClockSkew`, pending status and leader hint.

## Ordered slices

1. Retry on unavailability, with the abort rule for definite refusals.
2. Durations, `ClockSkew` and the SDK error.
3. Pending status and targeted re-sends.
4. Raft store I/O and locking.
5. Deadline timer and leader hint.
6. Evidence.

## Completion gate

- With the coordinator reply dropped after assignment, the write commits
  on retry with the same operation identity and no member is evicted.
- Killing the leader between append and acknowledgment, with the existing
  crash hooks, still commits the write once a new leader is elected.
- A device clock 60 s ahead receives `ClockSkew`; the same write succeeds
  once the clock is corrected, with no restart.
- During a pending entry the proposer's bytes per poll drop by at least the
  hash window's size; members that acknowledged receive no re-sent frames.
- Head reads during a slow disk sync complete without waiting for it
  (fault-injected delay in the store).
