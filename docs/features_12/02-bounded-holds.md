# 2. Bounded holds

Status: replanned (2026-10-27) to steady-state holders after the
[inventory](02-bounded-holds-evidence.md) found that whole-scope
transitions need a separate design ([phase 7](07-transitions.md)).
Owner: `rocky-secure`: group commit, settle passes, maintenance, pruning,
WAL checkpoints, checkpoint page transfer and metadata scans. Depends on
[phase 1b](01b-see-the-reactor.md). Effort: two weeks.
Follow the [execution contract](README.md#agent-execution-contract).

## Objective

After this phase, no steady-state holder keeps the replica, or the
reactor thread, longer than one bounded step. A local write then waits
for at most the steps queued ahead of it (phase 3 reduces that to one).
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
  storing checkpoint pages, held refresh and the metadata scans that
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
  per-unit cost, not phase 1's whole-acquisition cost. The phone cost per
  unit times the bound is at most half the phone p99 target (12.5 ms).
  Each constant's doc comment states that cost and links to the evidence.
- **Split groups, one flush.** `commit_group` stops adding requests once
  the group's bound is reached, then commits that SQLite transaction. The
  leader then:
  - releases the replica
  - re-acquires it and continues draining
  - collects exposed replies across those transactions
  - answers them after one `flusher.run` covering the last commit

  A single request larger than the bound still forms its own transaction,
  as today. Each request keeps its own savepoint, so its atomicity is
  unchanged.
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
   reclamation, checkpoint page storage and export, held refresh and the
   metadata scans. Test each holder: it processes at most its bound per
   acquisition, and an interruption after any step resumes to the same
   final state as an uninterrupted run (reuse the existing crash and
   fault-injection helpers). Test each validated scan: a write between its
   pages invalidates the result before it is acted on.
5. Rerun L0–L4 on desktop, and on phone where available. Rerun the paired
   sync non-regression rows that run on desktop.

## Completion gate

- Every steady-state holder in the inventory has a named work bound and a
  deterministic test that the bound holds per acquisition.
- Desktop steady-state holds and reactor polls in L1–L4 meet p99 ≤ 10 ms
  and max ≤ 50 ms. Phone results where measured meet p99 ≤ 25 ms and
  max ≤ 100 ms. Any miss is reported with its site and profile.
- Transitions observed in L1–L4 are listed with their site, hold and
  scenario. They don't count against the hold targets.
- Interactive write p95 in L1–L4 is recorded. Meeting its target is
  phase 3's gate, not this one's.
- The desktop rows of the sync non-regression table hold. Full-rebuild,
  finalization and audit equality tests pass unchanged. The 100k migration
  test passes.
- `./scripts/check` passes.

## Not in scope

Transitions (phase 7), priority between waiters (phase 3), statement cuts
(phase 4), migration publication (phase 5) and any persisted format
change.
