# Phase 8 — Scale qualification

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: in progress, 2026-10-05. Trusted storage soak and hardware evidence capture
are implemented. All six 500,000-row full-durability soaks passed the three-hour
and five-million-operation thresholds. All six phase 5 rechecks missed
throughput and p99 budgets. Oracle 1/2/3 reopen exceeded 2 seconds, Oracle 2
reached 327 MiB RSS during reopen, and Oracle 1's point-read p50 bound was
1.023 ms. The full qualification gate remains pending. See the
[qualification guide](../../runtime/scale-qualification.md) for workload, reproduction
and outstanding slices, after phases 1–7 of the [plan](README.md).

Each earlier phase proves its own gate in isolation. This phase proves the whole
system at the target size and records what is and is not supported.

## Workload

One application with 2,000 tables. A hot set of about 50 tables holds most rows;
the rest are small. One domain reaches 500,000 live rows through creates, updates
and deletes totalling at least 5,000,000 operations, so checkpointing and pruning
run many times. A second and third domain run concurrently at smaller size.
Readers issue point reads, paged range reads and at least 100 observers. Strong
commands and effect jobs run throughout.

## Slices

1. **Soak.** Run the workload for at least several hours on one full node. Record
   throughput, tail latency, memory, file size and open time at intervals. Any
   figure that trends upward without levelling off is a failure to explain.
   [Phase 5](05-write-concurrency.md#follow-up-decided-2026-10-05) left one:
   at full durability in one file the writer's cost per write rose by about
   60% over ten minutes, to about three million operations, and throughput
   fell with it. Profile where the step time goes as the file grows.
2. **Topology.** Repeat with one full node, three coordinator voters and two
   partial replicas over the native transport, including a replica that goes
   offline past the horizon and returns.
3. **Faults.** During the run: kill each process at random points, fill the queue,
   disconnect and reconnect peers, and interrupt a checkpoint, a prune and a
   rebootstrap. Afterwards every file opens, passes the audit call and converges.
4. **Second hardware profile.** Repeat on a machine with different flush behaviour,
   such as Linux on a datacentre SSD, and on the slowest supported target that can
   run it. Flush cost dominated every earlier result and was measured on one Mac.
   Include the phase 5 gate workload: its p99 limit of about 100 ms is the
   [flush of the database file](05-write-concurrency.md#restated-latency-gate-accepted-2026-10-05)
   during a checkpoint and is stated per profile.
5. **Encrypted path.** Run the same workload through `EncryptedReplica` with real
   grants and keys. Report it separately; do not infer it from the trusted run.
6. **Durability check.** For each durability class, cut power or use a fault
   device where available and verify the stated guarantee. Where no such test is
   possible, say so rather than claim it.
7. **Report.** Publish a supported-limits table: tables, rows, operations, domains
   per file, observers, replicas, with the measured figure and the hardware beside
   each. Update the [scaling limits](../../runtime/scaling-limits.md) guide and the
   root README. List every budget that was missed.

## Gate

- Every [target budget](README.md#target-budgets) is measured on both hardware
  profiles and either met or reported as missed with the measured value.
- No correctness failure in the fault slice: no lost acknowledged write within its
  durability class, no duplicate execution, no resurrection, no divergence.
- The published limits are what was measured, not what the design allows.
