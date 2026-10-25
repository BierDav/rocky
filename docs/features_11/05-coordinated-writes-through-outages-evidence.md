# Coordinated writes through outages: evidence

Date: 2026-10-25. Status: complete; phase gates verified, with the workspace baseline failure below.

## Behavior

Unavailable appends retry with the persisted sequence, hash, epoch, position and
acknowledgment duration. A call waits up to 40 seconds on a monotonic clock;
a deadline or cancellation preserves uncertain staged work for recovery. Proposal
ownership is released on cancellation. Only definite refusals void staged work.
An obsolete append slot returns `Configuration`, which the SDK reports as a
retryable conflict. Admission overload returns `Unavailable` for appends because
it cannot determine whether an earlier attempt was assigned; replicated quota
refusals remain `Backpressure`.

The coordinator computes an assigned deadline from authority time and a nonzero
duration of at most 30 seconds. Retrying an assignment never extends its deadline.
Signed requests outside the ±30-second clock window receive `ClockSkew` with
`coordinator_time`; the SDK JSON error preserves both fields. Signed responses
are bound to the request nonce, so a skewed device can receive that error.

Pending-status reads carry the decision and current members' acknowledged
positions, without the hash window. Only missing members receive frames again.
Volatile acknowledgment knowledge is scoped to the leader's term and discarded
on election. Replies carry a leader hint; the client prioritizes that leader and
the last responsive voter.

Durable store mutations run on blocking workers behind a separate I/O lock.
Readers use the last durable in-memory image while disk I/O proceeds. Mutations
remain serialized through publication even if their async caller is canceled.
Administrative disk accounting also uses the I/O lock, preventing races with
renamed or purged files. The deadline worker subscribes to pending-entry changes and leadership changes,
sleeps until the earliest deadline and retries failed resolution with bounded
backoff. Ordinary Raft metrics updates do not drive it.

Current formats: coordinator wire signature domain v3, Raft store 10, encrypted
replica 36. Older formats are refused; there is no migration or dual reader.

## Measurements and regression gates

- A dropped assignment reply, preceded by unavailability: three append calls
  carry exactly the same arguments; the original frames commit once, with both
  members retained.
- Four status polls while the proposer's acknowledgment is unavailable: the
  already-acknowledged peer receives its frames exactly once.
- A full 1,024-hash head encodes to 33,333 bytes; compact status encodes to 206
  bytes. This saves 33,127 bytes per poll, exceeding the 32,768-byte hash window.
- With log I/O blocked by a per-store synchronization gate, a head read plus
  authenticated preflight completes in 165 µs before releasing disk I/O. The
  append remains incomplete until the gate is released.
- The three-voter process test crashes the leader with `reply.before_send`
  between assignment and the remaining acknowledgments. The new leader reports
  no old-term promises; reacknowledgment commits once. The test also exercises
  deadline resolution, snapshots and voter restarts. Final targeted run: 4.63 seconds.
- The existing storage crash sweep passes five append crash boundaries, two
  snapshot boundaries and an interrupted snapshot transfer in 14.45 seconds.
- Authenticated loopback replies route the client through voters 1, 3, 3: the
  hint skips voter 2 and the next call starts at voter 3. A signed request 60
  seconds ahead receives `ClockSkew`; its corrected retry succeeds.
- Coordinator metadata tests prove authority-owned deadlines, immutable retry
  assignments and compact status sizing. Replica tests also cover correction
  without restart, caller-deadline exhaustion and cancellation after assignment.

## Verification

- `./scripts/lint`: passed, including Kotlin lint after installing the
  repository's checksum-verified `ktlint` pin with `scripts/tools ktlint`.
- `./scripts/check`: Clippy and all 196 unit tests passed. The integration run
  passed 1,333 of 1,334 tests across the workspace and SDK without default
  features. Every phase-5 test passed. Total integration wall time: 248.69 s,
  within its 300 s suite budget.
- The combined format-rejection test passed but exceeded its individual 2 s
  budget at 2.14 s. It now has separate checkpoint, replica and damaged-schema
  tests, retaining every case. The final targeted integration-profile run passes
  at 0.72 s, 1.55 s and 0.38 s respectively, with the budget unchanged.
- Final targeted Clippy, lint and `scripts/commitlint --jj`: passed.
- The three-voter leader-change/deadline test and the full storage crash sweep
  passed as recorded above.

The full workspace check remains red on the unrelated
`tests::tips::frontiers::headers::excluded_and_other_domain_headers_still_decode_after_warming`
assertion. It also fails on unchanged `main` (`3ed4ad8f`): it expects rejection of
an invalid secure frame but gets success. This was reproduced with the exact
test in the original checkout before integrating this phase. No header-validation
code or test was changed here, and no test budget or safety assertion was relaxed.

These tests qualify the SQLite fixture and local three-voter TCP cluster. They
do not establish behavior under an arbitrary-length outage: a pending entry
still expires at its assigned deadline and missing members may then be evicted.
No physical-device run is required by phase 5 or claimed here.
