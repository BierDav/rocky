# Phase 10 evidence

Status: partial implementation (2026-11-05). Baseline `578840be`; working
change `zxzlwzwt`. Product authorization supersedes the original sketch's
stop-for-design-review rule. The implementation changes replica identity
`rocky.encrypted-replica.v55` → `v56`, rejecting older files. Wire formats
are unchanged. The phase is not complete or latency-qualified.

## Source investigation and decision

Root README/AGENTS, workspace metadata, phase 8/9/10 briefs and current
`rocky-secure` transition sources establish the owners and current behavior.
No nested AGENTS apply to the changed crates. Phase 8 fixed the old gaps:
replayed checkpoints exist, `reproject` skips replayed tables, and both
finalized-record changes run inside compaction's transaction.

Rewind and tip/projection rebuilds execute application statements and admission
on the replica connection, including linked-scope closure and undo. Schema
switches carry history, indexes and undo reshaping. A generic disposable scratch
file does not solve atomic publication of all those effects. Whole-table
selection is viable only after all readers, references and writes select the
same component generation. Snapshot-plus-delta replay also needs a finite remote
cut and a policy for servicing local writes; the existing gate does not supply
one.

Compaction instead has finite authenticated input already captured at a seal.
It can publish a replay base by selecting that immutable generation, without
copying application rows or catching up with concurrent local writes. This is
implemented in `compaction/base_transition.rs` and integrated with production
`compact`, base export, rebuild, audit, eviction/import and maintenance cleanup.
It is not unused infrastructure.

## Implemented work and recovery

- Each seal snapshot gets a monotonic durable generation; older selected
  generations remain immutable when the same seal is reconstructed.
- The journal commits generation, keyset cursor and incremental LtHash together.
  `BASE_ROWS=64` caps hashed rows in one preparation transaction. Indexed ordinal
  lookups are limited to fields referenced by that chunk; historical unrelated
  dictionary entries are not scanned each step.
- Normal reads and writes remain available during preparation. Twelve successful
  writes, more than the gate's eight starvation yields, are deliberately admitted
  at a paused preparation boundary; they remain live after publication and do not
  extend the source cut. A fixed source requires finitely many chunks without an
  assumed arrival-rate bound or an unbounded fallback.
- A generation selector and its signed checkpoint publish in one transaction.
  All scopes prepare first. Replayed base selectors, signed checkpoints and the
  irreversible domain marker commit in one domain transaction. A crash cannot
  expose an incompatible parent/child base pair or conceal a later rebootstrap.
- Retirement has durable ownership separate from readable snapshot headers.
  Before the first destructive chunk, a current snapshot's readable header is
  removed. Its snapshot-registry record survives until every row is deleted.
  Active bases and preparing generations remain pinned. Cleanup deletes at most
  64 data/header records per transaction, plus fixed queue bookkeeping, and
  yields between chunks. Local recovery, maintenance or the next compaction drains
  the queue, including after reopening when the in-memory maintenance queue is empty.

No group is made `waiting` for this mechanism. No application operation is
replayed by preparation, publication or cleanup. Restart therefore cannot
repeat a non-idempotent operation. There is no partially copied live table to
reconstruct from disposable scratch.

The previous overflow counterexample remains relevant to future kinds: live
`(10,20)` replaced one chunk at a time under a `preparing` marker can restart
from `(100,20)` and produce `(1000,200)` instead of `(100,200)`. That proposal
is discarded, not shipped. Similarly, the old `STAGE_TRANSACTIONS > 8`
liveness argument was invalid: nine units of work and ten arrivals per step
is a permitted growing backlog. Fixed immutable C input needs neither premise.

## Explicit remaining unbounded work

This implementation bounds compaction's partition-row hashing/selection/
retirement, not every instruction or all work of kind C. It does not claim the
phase's one-bounded-step interactivity guarantee for arbitrary transitions.

History preparation is now bounded too (`compaction/history_transition.rs`):
when a captured snapshot exists, the covered frontier, operation count, identity
coverage and custody summary are folded in 64-record steps, with the lock
released between steps. A durable token (`secure_base_history`) records the
captured prefix `last`; triggers on applied frames, operations, births, acks,
checkpoints and replay snapshot generations invalidate it, so
successful writes beyond the cut create no catch-up work. Publication re-checks
the token and seal bound under the publishing guard and returns `Conflict`
otherwise; the identity ledger is folded from the prepared marks instead of
re-seeded. Identity coverage scans positions up to `last` only: it relies on an
author's frame with sequence at or below the frontier never being applied after
the captured prefix (per-author chain order).

Publication still processes checkpoint pages and stubs and fans out across
held scopes. Without a captured snapshot, history falls back to the previous
unbounded path. Counters expose `transition_history_record`,
`transition_identity_record`, `transition_ledger_record`, referenced dictionary
fields and decoded outcome records alongside hashed/deleted rows and selector
publications. Outcome lists have the existing 512-entry cap; decoding is counted
separately. Dictionary work depends on fields in a chunk, and row cost depends
on image size. Those are not phone-calibrated constant-time guarantees.

Finalization and no-captured-snapshot compaction retain their existing unbounded
paths. R/F/S/T/M/I/P retain atomic in-place behavior. Retaining publication under
one guard preserves linked-base safety but leaves scope/metadata fan-out to bound.
Next work is C's metadata state machine, then staged full-component P/T with a
finite remote cut and explicit local service policy. No successful-write latency,
phone hold target, 100k fixture, four-times-routine tip or full phase completion
claim follows from the row bound alone.

## Deterministic verification

Seven production-path tests in `tests/compaction/bounded.rs` passed natively:

1. Reads and twelve successful writes plus an update during paused preparation;
   the signed base contains the fixed seal cut while the live suffix survives.
   Per-acquisition hash/cleanup counters are at most 64, total hashed rows match
   the fixture and interactive acquisitions cross at most one background step.
2. Failure before/after preparation commit, during publication, after publication,
   and before/after cleanup; reopen, resume, compare rows, checkpoint coverage and
   run the real audit. Cursor and digest cannot double-count after rollback.
3. Unchanged S0 base, unchanged S1 snapshot, interruption after the first destructive
   S1 cleanup chunk, reopen and compact again. The partial retired snapshot has no
   readable header and cannot be signed as a one-row replacement base.
4. Same-seal snapshot reconstruction preserves the selected older generation;
   another reconstruction during paused preparation invalidates its digest.
5. Linked parent/child scopes with a cascading deletion: a failure after one
   scope's publication work rolls back every selector and checkpoint. A crash
   after the domain transaction leaves every base at the same new seal. Reopen,
   audit and receive a winning rival; the durable marker triggers rebootstrap.
6. A cleanup queue from a deeper obsolete path cannot retire current uncompacted
   snapshots on the replacement path, even when its recorded depth is greater.
7. Base metadata preparation over `2×64+3` rows: every per-acquisition
   history, identity, ledger and outcome counter is at most 64 and history is
   observed in steps.

`cargo test -p rocky-secure compaction::bounded --lib`: 7 passed. The full
`cargo test -p rocky-secure --lib` matrix passed 548 tests, with 5 ignored.
`cargo check -p rocky-secure --lib`, `cargo fmt --all -- --check` and
`./scripts/lint` also passed.

Full matrix and paired release measurements are running; results are recorded
below when complete. The ignored release probe is separate from correctness
tests and uses the same source on baseline and implementation.

## Earlier check context

Before runtime edits, the previous full check passed native Clippy, 245 unit and
1,109 integration tests, regular wasm Clippy/tests, then failed browser relay
sync with `Refused("invitation expired or locked")`. Code inspection identifies
two possible causes of that exact message: invitation expiration or ten invalid
proof attempts. `scripts/browser-sync` creates its 600-second invitation before
compiling the browser-specific test with the invitation embedded at compile time.
A long rebuild can consume its validity. The previous log does not establish
which predicate fired; this is a diagnosis of the harness's expiration exposure,
not proof of the earlier failure's cause. No authorization checks are relaxed.
