# 2. Bounded holds

Status: planned. Owner: `rocky-secure`: group commit, settle passes,
maintenance, WAL checkpoints, compaction, finalization and checkpoint
transfer. Depends on [phase 1](01-see-the-wait.md). Effort: two weeks.
Follow the [execution contract](README.md#agent-execution-contract).

## Objective

After this phase, no steady-state background holder keeps the replica
longer than one bounded step. A local write then waits for at most the
steps queued ahead of it (phase 3 reduces that to one). Sync throughput
stays within the [non-regression table](README.md#sync-must-not-regress).

## Inputs

- Phase 1 evidence: the per-site holder table and the per-unit costs.
- `group.rs`: `GROUP_REQUESTS`, `GROUP_BYTES`, `commit_group`,
  `settle`, `maintain` and `checkpoint_if_due`.
- The 100k fixture and the L1–L4 scenarios.

## Settled

- **Steady state versus open time.** The bounds apply to everything that
  can run while an application holds an open database. That includes
  receiving, settling pending frames, maintenance, pruning, compaction,
  finalization, checkpoint transfer and import, held refresh and WAL
  checkpoints. Work that runs only before `Database::open` returns is
  exempt, but the evidence lists it. Examples are rebuild-at-open, staged
  migration (phase 5) and recovery at open.
- **Work bounds from measurements.** Each bounded holder gets a named
  constant in units of work (frames, rows, pages or bytes), never a
  wall-clock deadline. Size it so the phone's measured cost per unit
  times the bound is at most half the phone p99 target (12.5 ms). Each
  constant's doc comment states the per-unit cost it was derived from and
  links to the evidence.
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
- **Long operations become resumable steps.** A holder that today does
  unbounded work in one acquisition becomes a loop of bounded steps:
  1. acquire
  2. do one bounded step and commit it
  3. release

  Progress must be durable after each step, or recomputable from durable
  state. Where an operation must look atomic to readers, use the pattern
  projection plans already use: stage invisibly in bounded steps, then
  switch in one bounded transaction. If an operation can't be made
  resumable without changing a persisted format, stop and report it
  instead of changing the format.
- **Passive checkpoint off the lock.** The replica keeps one extra
  connection, opened at replica open, used only for
  `PRAGMA wal_checkpoint(PASSIVE)`. It runs on a blocking thread and is
  triggered by the same WAL-size rule, without the replica lock. A
  truncating checkpoint (WAL ≥ `WAL_LIMIT_BYTES`) stays under the replica
  lock, as today, with its 50 ms reader wait. Free-page reclamation stays
  bounded by `RECLAIM_STEP`. If phase 1 shows that step above the hold
  bound, shrink it.
- **No new background threads beyond the checkpoint connection's
  blocking task.** Steps run where they run today.

## Ordered slices

1. **Inventory.** List every background holder from phase 1 whose phone
   p99 or max exceeds the target. Include each one's unit of work, current
   bound if any, whether it is steady-state or open-only, and the proposed
   bound or resumable-step design. Record it in evidence before changing
   code. If a holder needs a persisted format change, mark it blocked and
   leave it out of this phase.
2. Split receive groups and settle passes with one shared flush. Test it:
   an oversized group commits in several transactions, every exposed reply
   follows exactly one flush covering all of them, and a fault between
   transactions leaves each committed request applied and each later one
   retried by handle.
3. Passive checkpoint connection. Test it: a passive checkpoint runs while
   another task holds the replica, and a truncating checkpoint still waits
   for the writer.
4. Bound maintenance, pruning, compaction steps, finalization steps,
   checkpoint transfer and import, and held refresh per the inventory.
   Test each holder: it processes at most its bound per acquisition, and
   an interruption after any step resumes to the same final state as an
   uninterrupted run (reuse the existing crash and fault-injection
   helpers).
5. Rerun L0–L4 on desktop, and on phone where available. Rerun the paired
   sync non-regression rows that run on desktop.

## Completion gate

- Every steady-state holder in the inventory has a named work bound and a
  deterministic test that the bound holds per acquisition.
- Desktop background holds in L1–L4 meet p99 ≤ 10 ms and max ≤ 50 ms.
  Phone holds where measured meet p99 ≤ 25 ms and max ≤ 100 ms. Any miss
  is reported with its site and profile.
- Interactive write p95 in L1–L4 is recorded. Meeting its target is
  phase 3's gate, not this one's.
- The desktop rows of the sync non-regression table hold. Full-rebuild,
  finalization and audit equality tests pass unchanged. The 100k migration
  test passes.
- `./scripts/check` passes.

## Not in scope

Priority between waiters (phase 3), statement cuts (phase 4), migration
publication (phase 5), and any persisted format change.
