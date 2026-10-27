# 2. Bounded holds

Status: in progress (2026-10-29). Checkpoint storage now commits up to 128 rows
and eight immutable metadata records per step; receive groups up to four
one-row frames. The compiled defaults pass the aggregate paired member-pull
gate against phase start. Phone calibration records unresolved storage outliers;
it does not establish uniform latency qualification. This is the foundation for
features_13 phase 2 (interactive first) and phase 4 (intent replay). Remaining
surviving holders and phase qualification are unfinished. The narrow dependent
fan-out exception below remains until features_13 phase 8 deletes the resolver.
See the [evidence](02-bounded-holds-evidence.md) for measurements and the
remaining holder inventory.

Owner: `rocky-secure`: group commit, settle passes, maintenance, pruning,
WAL checkpoints, checkpoint page transfer and metadata scans. Depends on
[phase 1b](01b-see-the-reactor.md). Effort: two weeks.
Follow the [execution contract](README.md#agent-execution-contract).

## Objective

After this phase, no steady-state holder keeps the replica, or the
reactor thread, longer than one bounded step. A local write then waits
for at most the steps queued ahead of it (features_13 phase 2 reduces that
to one).
Sync throughput stays within the [non-regression table](README.md#sync-must-not-regress).

## Inputs

- Phase 1 holder tables and the slice 1 inventory in the evidence.
- Phase 1b poll and attribution tables, which show reactor stalls that
  phase 1 could not see.
- `group.rs`: `GROUP_REQUESTS`, `GROUP_BYTES`, `commit_group`,
  `settle`, `maintain` and `checkpoint_if_due`.
- The 100k fixture and the L1–L4 scenarios.

## Settled

- **Steady state versus transitions.** Steady state is work that runs
  routinely on an open database: receiving frames, settling pending
  frames, maintenance, pruning, page reclamation, WAL checkpoints,
  storing checkpoint pages and the surviving metadata scans that
  serve them. A transition replaces a scope's state wholesale and must
  look atomic to readers: checkpoint installation, rebootstrap replay,
  live schema switch, projection rebuild, compaction and finalization
  rewrites, and tip workspace rebuilding. Transitions are out of this
  phase. They keep their single transaction, and are measured and listed
  in evidence. Work reachable only before `Database::open` returns is
  exempt, as before.
- **Classify by atomicity, not convenience.** A holder is a transition
  only if bounding it needs a multi-step atomic publication. Record the
  reason per site. A holder that only reads, or whose steps are already
  independently valid, is steady state.
- **Work bounds from measurements.** Each bounded holder gets a named
  constant in units of work (frames, rows, pages or bytes), never a
  wall-clock deadline. Size it from the separated step's own measured
  per-unit cost, not phase 1's whole-acquisition cost. Checkpoint storage
  tries 32, 64 and 128 rows against a paired 100k member pull and selects
  the smallest measured step within 110% of the phase-start baseline.
  Confirm that step's phone hold and record it against features_13's
  interactive targets: desktop p95/p99/max ≤ 16/50/100 ms; phone
  ≤ 33/100/250 ms. Receive tries 2–4 one-row frames per transaction,
  including ready application. These are measured constants, not broader
  exceptions. The former phone-cost × bound ≤ 12.5 ms sizing rule no
  longer selects them. Only if every smaller step fails may storage use
  a full 256-row page, gated by features_13 phase 2 so it starts only
  when no interactive write waits. Do not claim that gate exists here.
  Each constant's doc comment links to its measured evidence.
- **Split groups, one flush.** `commit_group` stops adding requests once
  the group's bound is reached, then commits that SQLite transaction. The
  leader then:
  - releases the replica
  - re-acquires it and continues draining
  - collects exposed replies across those transactions
  - answers them after one `flusher.run` covering the last commit

  A single request larger than the bound still forms its own transaction,
  as today. This includes applying an indivisible received bundle (up to
  64 frames / 2,048 rows), as accepted by the owner on 2026-10-27. Count
  all of its members and rows, and report oversized-request holds
  separately; never publish a partial bundle. Each request keeps its own
  savepoint, so its atomicity is unchanged.
- **Resumable steps for steady-state holders.** A steady-state holder
  with unbounded work becomes a loop of bounded steps:
  1. acquire
  2. do one bounded step and commit it
  3. release

  Progress must be durable after each step, or recomputable from durable
  state. Pruning already steps by `STEP_OPERATIONS`; it releases between
  steps.
- **Scans validate instead of holding.** A metadata scan that computes a
  result (blockers, required schemas, publication discovery, tip
  prerequisites, frontiers) reads in bounded pages across acquisitions.
  The step that acts on the result checks it against the replica revision
  or a narrower counter, and rescans or defers if it changed. If a scan
  can't be validated that way, it is a transition.
- **Passive checkpoint off the lock and the reactor.** The replica keeps
  one extra connection, opened at replica open, used only for
  `PRAGMA wal_checkpoint(PASSIVE)`. It runs on the blocking pool and is
  triggered by the same WAL-size rule, without the replica lock. A
  truncating checkpoint (WAL ≥ `WAL_LIMIT_BYTES`) stays under the replica
  lock, as today, with its 50 ms reader wait. Free-page reclamation stays
  bounded by `RECLAIM_STEP`. If measurement shows that step above the
  hold bound, shrink it.
- **Dependent fan-out exception (expires in features_13).** Only the
  constraint resolver's induced reveal/hide of children when a parent is
  received, imported or updated may exceed the normal input-row bound.
  Rows and projections still commit atomically; no partial child reveal.
  Test support counts induced typed-row changes per acquisition, with a
  deterministic assertion against the dependent count. Measure cost per
  induced row and 1k/10k-child cases separately in evidence. Report these
  samples apart from the steady-state targets, as with transition overlaps;
  never silently discard them. Every other holder retains its input-row
  bound. Phase 3 admits an oversized fan-out step only when no interactive
  write is waiting. Features_13 removes this exception when constraints move
  into replay.
- **No persisted format change.** A steady-state holder that can't be
  bounded without one is a transition.
- **No new background threads beyond the checkpoint connection's
  blocking task.** Steps run where they run today.

## Ordered slices

1. **Inventory.** Done; see the evidence. Re-label each row as steady
   state or transition with its reason, adding phase 1b's long polls.
2. Split receive groups and settle passes with one shared flush. Measure
   the separated admission, application and settle costs first. Test:
   an oversized group commits in several transactions, every exposed
   reply follows exactly one flush covering all of them, and a fault
   between transactions leaves each committed request applied and each
   later one retried by handle.
3. Passive checkpoint connection. Test: a passive checkpoint runs while
   another task holds the replica, and a truncating checkpoint still waits
   for the writer.
4. Bound maintenance (excluding transition rewrites), pruning, page
   reclamation, checkpoint page storage and export, and the surviving
   metadata scans: shared `domain_headers` (including its transaction
   callers), required schemas and retention, blockers, publication
   progress, placements, restoration/custody frontier, rebootstrap
   discovery, tip prerequisites, move-following and SQL connection planning.
   Keep completed held work; skip the held generation fingerprint, further
   held-refresh work and resolver bounding because features_13 phase 8
   removes those paths. Test each holder: it processes at most its bound per
   acquisition, and an interruption after any step resumes to the same
   final state as an uninterrupted run (reuse the existing crash and
   fault-injection helpers). Test each validated scan: a write between its
   pages invalidates the result before it is acted on.
5. Rerun L0–L4 on desktop, and on phone where available. Rerun the paired
   sync non-regression rows against this phase's start `ynnvllrn`
   (`4e4e2497`), not the later features_13 baseline. Features_13 phase 9
   rechecks the physical devices.

## Completion gate

- Every surviving steady-state holder in the inventory has a named work
  bound and a deterministic test that the bound holds per acquisition, with only the
  dependent fan-out exception above counted and reported separately.
- Record desktop and available phone steady-state holds and reactor polls
  in L1–L4, including any misses of the original 10/50 ms desktop and
  25/100 ms phone p99/max hold targets. The throughput-selected storage
  and receive bounds are assessed against features_13's interactive
  targets above; no whole-scenario latency claim follows from a step fit.
- Transitions observed in L1–L4 are listed with their site, hold and
  scenario. They don't count against the hold targets.
- Interactive write p95 in L1–L4 is recorded. Meeting its target is
  features_13 phase 2's gate, not this one's.
- The desktop rows of the sync non-regression table hold. Full-rebuild,
  finalization and audit equality tests pass unchanged. The 100k migration
  test passes.
- `./scripts/check` passes.

## Not in scope

Transitions (phase 7), priority between waiters (features_13 phase 2),
statement cuts (phase 4), migration publication (phase 5) and any persisted format
change.
