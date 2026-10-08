# Scale implementation plan

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: proposed 2026-10-03, after the [scaling limits](../../runtime/scaling-limits.md)
measurements. Phases 1 and 2 are implemented and the target budgets were
confirmed on 2026-10-04; phase 2 missed two gates, which phase 3 took over and met.
Phase 3 is implemented with [stated limits](03-stored-state-and-reads.md#outcome-2026-10-04).
Phase 4 is implemented with [stated limits](04-checkpoints-and-pruning.md#outcome-2026-10-04).
Phase 5 is implemented. It [missed its latency gate](05-write-concurrency.md#outcome-2026-10-05);
the [follow-up](05-write-concurrency.md#follow-up-decided-2026-10-05) of 2026-10-05
recorded the WAL checkpoint stall as a limit of the measured hardware and
restated that gate. What
remains open is listed there.
Phase 6 is implemented with [stated limits](06-wide-schemas.md#outcome-2026-10-05).
Phase 7 is implemented with [stated limits](07-replica-roles.md#outcome-2026-10-05).
Phase 8 has a trusted storage soak and hardware evidence capture; its combined
qualification gate remains pending.
Nothing else in this folder is implemented unless a brief's status line
says so. [Features 1](../features_01/README.md) records the earlier functional plan; its
unfinished phase 6 gates for checkpoints and pruning are taken over by phase 4 here.
Future generation upgrades and qualification follow [features_04](../../features_04/README.md);
mobile support remains separately deferred.

## Goal

One application spanning about 2,000 tables and hundreds of thousands of rows per
domain, on devices that hold a subset and on full replica nodes that hold all of it.
Today a domain is limited to 4,096 operations and 4,096 readable rows, keeps all
state and history in memory, and copies that state on every write and read.

## Target budgets

The user confirmed these targets on 2026-10-04 with the phase 1 measurements
beside them. Later phases treat them as gates; a brief may not relax one silently.
The measured column is the [scale baseline](../../runtime/scaling-baseline.md) on the
reference machine, at 20 tables unless stated.

| Property | Target | Measured 2026-10-04 |
| --- | --- | --- |
| Tables per application | 2,000 | Installs in 4.8 s and holds 120 MiB empty; writes 8× slower and reads 0.7 s slower than at 20 tables. Engine alone: 25× per-statement cost and a 1.6 s blocking checkpoint |
| Live rows per domain | 5,000,000 | 3,796, stopped by the operation quota. Engine alone: 5,000,000 with flat latency |
| Lifetime operations per domain | unbounded; retained history bounded by policy | 4,096 |
| Write cost, empty versus full domain | within 2× | 1.3× inside the quota; 15× at 100,000 operations with the quota raised |
| Indexed point read on a full domain | under 1 ms median | 12.9 ms at 3,500 rows; 734 ms at 2,000 tables |
| Observer cost per write | proportional to changed rows, not to state or observer count | One full copy per observer: 102 ms per write with 8 observers at 3,500 rows |
| Open a full domain | under 2 s, without replaying full history | 32 ms at 3,796 rows by full replay; 736 ms at 2,000 tables |
| Resident memory, full domain | under 256 MiB, independent of history length | 55 MiB at 3,500 rows; 126 MiB at 2,000 tables; 233 MiB at 100,000 operations |
| Catch-up of a replica 10,000 operations behind | bounded pages; no full ID exchange | Trusted path exchanges its whole receipt set; encrypted path pages 32 frames, 984 ms per 4,000 |

The measured column predates the [switch to SQLite](../../decisions/0007-sqlite-engine.md),
so its engine figures are Turso's; the same rows on SQLite are in the
[baseline comparison](../../runtime/scaling-baseline.md#after-the-sqlite-switch).
"Open" means reopening an installed file. The first install of a 2,000-table schema took 4.8 s and has a user-confirmed 10-second budget for one 2,000-table domain in
[phase 6](06-wide-schemas.md). Larger placements report total install time separately. The memory budget includes the engine's schema
overhead, 57 MiB after a 2,000-table install and 30 MiB after reopen on Turso.

The engine was measured without Rocky on 2026-10-04
([engine at target size](../../runtime/engine-scale.md)). Row count is not a limit.
Table count is: one linear lookup in the engine pinned then, Turso 0.8.1, makes
every statement and checkpoint proportional to the tables and indexes in a file. A
[comparison of engines](../../../benchmarks/engine-scale/RESULTS.md) on the same
workload found SQLite in WAL mode flat to 16,000 tables within the memory budget,
with one writer per file. [Phase 2](02-sqlite-engine.md) moved the runtime to it
([decision 0007](../../decisions/0007-sqlite-engine.md)). By user decision a runtime
keeps its domains in one file by default, can spread them over up to 16, and commits buffered, flushing
before anything leaves the device
([decision 0008](../../decisions/0008-buffered-default-and-file-layout.md)).

## Sequence and gates

| Phase | Brief | Completion gate |
| --- | --- | --- |
| 1 | [Baseline, harness and hardening](01-baseline-and-hardening.md) | Large-data probe exists; encrypted path measured; prototyped options have crash coverage; every transaction retries |
| 2 | [SQLite storage engine](02-sqlite-engine.md) | Both paths run on SQLite; cross-domain group commit matches the old engine's durable throughput; no table-count cost. Implemented; 2,000-table read and 318 ms wait gates missed |
| 3 | [Stored state and indexed reads](03-stored-state-and-reads.md) | No full in-memory state; reads and observers meet the read budgets at 500,000 rows. Implemented; reads outside the keyset shape page by offset, and the encrypted path stayed inside its 4,096-record quota until phase 4 |
| 4 | [Checkpoints and history pruning](04-checkpoints-and-pruning.md) | Operation quota removed; open and memory independent of history length; old replicas rebootstrap safely. Implemented on both paths; see the [outcome](04-checkpoints-and-pruning.md#outcome-2026-10-04) |
| 5 | [Write concurrency and durability classes](05-write-concurrency.md) | Grouped writes and durability classes pass strong and incoming workloads at the write-throughput gate. Implemented; 5,525 writes/s over ten minutes, p99 68 ms against 50. The p99 bound is [restated](05-write-concurrency.md#restated-latency-gate-accepted-2026-10-05) as a limit of the measured Mac at full durability in one file: about 100 ms, a checkpoint's flush of the file |
| 6 | [Wide schemas and domain placement](06-wide-schemas.md) | Implemented: single-domain install and reopen meet budget; warm mean statement cost meets 20%, read p50 narrowly misses. Explicit placement routes 256 domains; additive indexes evolve in place. See stated limits |
| 7 | [Partial replicas and full replica nodes](07-replica-roles.md) | A device holding a subset converges with a full node; pruned history is served from checkpoints. Implemented; the full node's write p99 during a 10,000-operation catch-up was 120 ms, above the restated limit |
| 8 | [Scale qualification](08-scale-qualification.md) | Every target budget measured end to end on two hardware profiles, encrypted path included |

Phases are ordered by dependency. Phase 2 comes first because every later phase
changes storage code and should not be written twice. Phase 4 needs stored state from phase 3 because
a checkpoint is the stored projection plus its evidence. Phase 7 needs checkpoints
from phase 4 because a full node serves them. Phase 5 can run in parallel with
phase 4 once phase 2 is complete.

## Two runtime paths

The trusted `Runtime`/`Replica` path and the `EncryptedReplica` path have separate
storage code and separate quotas. Both are measured; the encrypted figures are in
the [encrypted baseline](../../runtime/encrypted-baseline.md). Each
brief lists a trusted slice and an encrypted slice; the encrypted slice is not
optional and has the same gate. Do not unify the two paths as a side effect; if a
phase shows they should share code, record that as a decision first.

## Implementation workflow

Read [AGENTS.md](../../../AGENTS.md), [module boundaries](../../modules.md) and
[development](../../development.md). Use Jujutsu only and leave edits in the current
change unless asked to publish.

For each slice:

1. Verify the previous gate against code and tests, not against a status line.
2. Run the relevant `scaling_probe` sections in a release build and keep the output
   as the before measurement.
3. Implement the slice with fault-injection and invariant tests. Storage format
   changes need a decision record under [decisions](../../decisions/README.md) and an
   explicit answer for existing files: migrate, or refuse to open with a clear error.
4. Rerun the probe, `cargo test -p <package>` and `./scripts/check`.
5. Update the owning guide under `docs/runtime/`, append the before/after numbers to
   that phase's evidence, set the brief's status line and report remaining limits.

Stop and ask when a slice would change a wire format, weaken a durability or
convergence guarantee, or miss its gate by more than a small margin. Do not raise a
quota to pass a gate; the [retention sweep](../../runtime/scaling-limits.md#raising-the-retention-quota)
shows why that fails.

## Invariants every phase preserves

- Operation, projection, outbox and revision changes commit atomically.
- Observers and outgoing data see only committed state.
- Retries keep stable identities and canonical bytes; a retried handle never
  produces a second operation.
- Replicas converge over all dependency-ready delivery orders.
- Deletion evidence, unresolved strong requests, pending jobs and sole recovery
  copies survive any pruning.
- The todo demo and the Kotlin consumer keep working throughout.
