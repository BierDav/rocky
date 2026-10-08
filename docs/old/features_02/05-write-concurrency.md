# Phase 5 — Write concurrency and durability classes

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented 2026-10-05 with the latency gate missed; see the
[outcome](#outcome-2026-10-05). The brief text up to it is unchanged.
Proposed after [phase 2](02-sqlite-engine.md); independent of
phases 3 and 4 except where noted. Updated 2026-10-04 for
[decision 0008](../../decisions/0008-buffered-default-and-file-layout.md).
Context: [scaling limits](../../runtime/scaling-limits.md#writes-flushes-are-the-cost),
[strong publication](../../runtime/strong-publication.md).

At full durability each commit costs one device flush, about 3.9 ms on the
measured machine. Throughput comes from sharing flushes, not from faster code.
On Turso, the prototypes behind `RuntimeOptions` reached 9,215 writes/s at 16
domains against 249.

[Phase 2](02-sqlite-engine.md) moved that sharing onto SQLite with one writer per
file ([decision 0007](../../decisions/0007-sqlite-engine.md)): cross-domain group
commit with one savepoint per mutation and a WAL checkpoint policy. By user
decision ([0008](../../decisions/0008-buffered-default-and-file-layout.md)) commits on
both paths are buffered by default and flushed before anything leaves the device,
and a runtime can spread its domains over several files, one by default. This phase verifies
that design under strong and incoming load and builds only what phase 2 left open.

## Slices

1. **Group bounds.** Check phase 2's group limits by count and bytes against the
   gate workload and tune them; keep no added delay under light load.
2. **Grouped incoming replication.** Apply a received page of operations in one
   transaction per bounded batch instead of one per operation. A rejected or
   not-ready operation must not roll back accepted ones in the same page; define
   and test that boundary, using a savepoint per operation.
3. **Writer saturation.** All domains of a file share one writer thread, and
   reads run on it too until [phase 3](03-stored-state-and-reads.md). Measure where it
   saturates on CPU (admission, reduction, encoding) at the gate workload. Move
   work that needs no transaction off the writer only if the gate requires it.
4. **Checkpoints under load.** Measure phase 2's writer-run WAL checkpoints under
   sustained multi-domain load with readers: frequency, duration, WAL size and
   effect on tail latency. Adjust the size policy, or record the stall as a limit.
5. **Fewer flushes per strong command.** A strong command currently takes about six
   flushed transactions on the replica. List each transaction and the recovery
   case that requires it to be separately durable. Merge only those where a crash
   between the merged steps is indistinguishable, after recovery, from a crash
   before both. Every interrupted-request and lost-reply test must still pass.
6. **Durability classes.** Phase 2 implements the watermark and the exposure
   rules of [decision 0008](../../decisions/0008-buffered-default-and-file-layout.md)
   on both paths. Verify them with strong commands and incoming replication
   running, including losing buffered strong steps, and add the missing tests
   for encrypted command, effect and recovery flushes.
7. **Backpressure.** The 32-command queue returns `Backpressure` immediately. Add
   an awaiting submit so grouped workloads do not spin, and expose queue depth.
8. **Encrypted path.** `EncryptedReplica` holds one mutex across authoring,
   incoming application and reads. Phase 2 gives its authoring and incoming
   paths group commit; release the mutex between batches where phase 1 measured
   contention.

## Gate

On the measured machine, at full durability, with strong commands and incoming
replication running alongside local mutations (record the file count; full
durability is fastest in one file):

- 16 domains with 8 clients each sustain at least 5,000 local writes/s with p99
  under 50 ms, with no storage error over a ten-minute run;
- one domain with one client is no slower than today;
- strong commands on two domains exceed 80/s, or the brief records which
  boundaries could not be merged and why;
- crash injection inside a group, inside a grouped incoming page and between
  merged strong steps leaves consistent, reopenable files;
- a buffered-durability receipt is never reported as flushed.

## Out of scope

Coordinator throughput, network transport and multi-file transactions.

## Outcome, 2026-10-05

Guide: [write concurrency](../../runtime/write-concurrency.md). Decision:
[0012](../../decisions/0012-merged-strong-steps.md). Figures:
[scaling limits](../../runtime/scaling-limits.md#after-write-concurrency).

| Slice | Result |
| --- | --- |
| 1 Group bounds | `group_commit` defaults to 128 instead of 64. The 4 MiB byte bound is unchanged. No delay is added |
| 2 Grouped incoming | `Replica::receive_page`: batches of 32 records, one commit and flush each, a savepoint per record, one application pass |
| 3 Writer saturation | Measured with `Replica::writer_stats`. The writer is saturated at the gate workload. Removed from it: redundant strong status reads and whole-ledger scans. Nothing was moved to another thread |
| 4 Checkpoints under load | Measured. `RuntimeOptions::checkpoint_frames` sets the size policy; the default is unchanged. The stall is recorded as a limit |
| 5 Fewer flushes per strong command | Three commits instead of about six at full durability, two flushes when buffered |
| 6 Durability classes | Tests added for strong and incoming work together, lost buffered strong steps, and encrypted command, effect and recovery flushes |
| 7 Backpressure | `Replica::submit`, `submit_with_handle` and `queue_depth` |
| 8 Encrypted path | The group flush runs outside the replica mutex, which is released between groups |

### Gate

| Gate | Result | Evidence |
| --- | --- | --- |
| 16 domains with 8 clients sustain 5,000 writes/s for ten minutes | Met on average, not in every interval | 5,525/s over the run at `checkpoint_frames` 65,536, one file; the last three minutes ran at 4,800 to 5,100/s. 5,271/s at the default |
| p99 under 50 ms | **Missed** | 68 ms; 100 ms at the default. 47 ms in the first two minutes |
| No storage error | Met | Both ten-minute runs |
| One domain with one client no slower | Met within the spread of the flush | 266 to 331/s at `full` against 288; 9,203 to 9,643/s buffered against 6,340 |
| Strong commands on two domains above 80/s | Met narrowly | 81 to 86/s at `full`, 122 to 144/s buffered, against 41 and 45 |
| Crash inside a group, a grouped incoming page and between merged strong steps | Met | `crash_tests`: `death_inside_an_incoming_page_leaves_none_of_its_records`, `death_between_the_merged_steps_of_a_result_leaves_none_of_them` and the existing group cases |
| A buffered receipt is never reported flushed | Met | `watermark_tests`, including `exposure_stays_behind_the_watermark_with_strong_and_incoming_work` |

The gate run used one file. Strong commands ran on two further domains of the
same file, not on two of the sixteen: a package names every operation of its
cut, so a domain that takes 400 writes a second passes the 43,690-operation
bound of a result frame within two minutes.

### Remaining limits

- **The latency gate.** A WAL checkpoint stops the file's writer for 25 to
  160 ms, longer as the file grows. Raising `checkpoint_frames` makes it rarer,
  not shorter. Running the copy on another connection while the writer
  continues would remove most of the stall; that departs from the single write
  connection of [decision 0007](../../decisions/0007-sqlite-engine.md) and was not
  built.
- **Throughput decays over the run**, from about 6,600 to 4,900 writes/s,
  through the longer checkpoints and through `strong_status`, which
  `execute_pending` reads once per pass and which decodes the whole ledger.
- **Strong commands do not scale with a domain's history**: 43,690 operations
  at most, and each command costs in proportion to them. This is the package
  format, a wire format.
- Strong commands under the gate load took 241 ms median: each of their
  requests waits for a group.
- Two domains running one command at a time never share a flush, so `full`
  stays near one command per three flushes.
- Encrypted reads still wait for a group's transaction, and a flush still
  blocks an executor thread. The encrypted path has no kill during a
  checkpoint.
- The trusted desktop transport still delivers one operation per call.

### Follow-up decided, 2026-10-05

The user reviewed the outcome and accepted
[decision 0012](../../decisions/0012-merged-strong-steps.md). The checkpoint stall
is not accepted as a limit yet: throughput is still falling at minute ten, the
stall grows with the file, and [phase 8](08-scale-qualification.md) measures
larger files. The work, in order:

1. **Ledger reads.** Stop `execute_pending` decoding the whole ledger through
   `strong_status` on each pass, then repeat the gate run so the checkpoint is
   the only remaining cause.
2. **Split the stall.** Time the page copy and the flush of the database file
   separately inside a checkpoint.
3. **Checkpoint connection, as a spike.** Run the passive checkpoint on a
   second connection that never commits a transaction, while the writer
   continues. Run the ten-minute gate at the default `checkpoint_frames`.

Two things can defeat step 3. Both flushes are `F_FULLFSYNC`, which covers the
whole device, so the writer may queue behind the checkpoint's flush whichever
connection issued it. And under sustained writes a passive checkpoint never
reaches the end of the log, so the log is reused only after the writer's own
truncating checkpoint, whose remaining stall must be measured.

Stop the spike if p99 stays at or above 50 ms over ten minutes or the WAL is
not bounded. Then record the stall as a limit with the measured figure and
restate the gate. If it holds, amend
[decision 0007](../../decisions/0007-sqlite-engine.md): one connection commits, a
second only checkpoints.

Progress, 2026-10-05
([figures](../../runtime/scaling-limits.md#follow-up-ledger-reads-and-the-checkpoint-split)).
The machine ran slower than on the outcome's runs, about 3,700 writes/s
unchanged, so no run met 5,000/s in every interval.

1. Done. `execute_pending` reads only unresolved requests. Reads on the writer
   fell from 9% to 2% in the last minute, but throughput still decays: each
   step costs more as the file grows. The checkpoint is not the only cause.
2. Done. A passive checkpoint averaged 85 ms: 20.5 ms page copy and 64.5 ms
   in flushes, almost all of it the database file; the WAL's flush is about
   2 ms. The user chose to build step 3 anyway.
3. Built, measured and removed under the stop condition. With the passive
   checkpoints on a second connection the writer stalled only 19 ms on
   average and the WAL stayed under 129 MiB, but every commit flush queued
   behind the checkpoint's flush of the device: p99 186 ms and 2,851
   writes/s, worse than without it.

#### Restated latency gate, accepted 2026-10-05

The user accepted the stall as a limit of the measured hardware and
configuration, not as a general one. At full durability in one file, a
write's p99 is set by the flush of the database file during a WAL checkpoint,
a device-wide `F_FULLFSYNC` on the measured Mac.

- **Recorded figure.** At the gate workload, one file, `Durability::Full`,
  default `checkpoint_frames`, on the measured Mac: p99 about 100 ms (102 to
  103 ms at its usual speed); checkpoints 67 ms on average and about 100 ms
  at most.
- **Regression bound.** A later phase whose gate workload runs clearly above
  that p99 on this machine explains why in its outcome. Larger files are
  the expected cause, so [phase 6](06-wide-schemas.md) measures it at its
  table counts.
- **Recheck.** [Phase 8](08-scale-qualification.md) measures it on the second
  hardware profile, whose flush may be far cheaper, and states the limit
  per profile.
- **Lower tail.** An application that needs one under sustained writes raises
  `checkpoint_frames` and accepts a larger WAL: at 65,536 frames, p99 68 ms
  over ten minutes with the WAL up to about 258 MiB.

The p99 bound of 50 ms is withdrawn only for that configuration. The other
gate items stand, including 5,000 writes/s in every interval.

The three steps were measured while the machine ran slower than for the
outcome. Rerun at its usual speed, the unchanged build gave 5,248 writes/s,
p99 103 ms and 23 of 60 intervals below 5,000/s; with steps 1 and 2, 5,400/s,
p99 102 ms and 6 intervals below, the slowest at 4,810/s. The throughput gate
is therefore missed narrowly, in the middle and late minutes, where the
per-step cost has grown with the file.

Open after the follow-up, none of which blocks [phase 6](06-wide-schemas.md):

- The growth of the per-write cost with the file is moved to the soak of
  [phase 8](08-scale-qualification.md).
- Phase 6 runs the gate workload at its table counts.
- The attempt left `unknown` after a stale fence needs a decision; see
  [decision 0012](../../decisions/0012-merged-strong-steps.md#consequences).

The two checks are traced. No step commits only schema changes
([decision 0008](../../decisions/0008-buffered-default-and-file-layout.md#a-step-that-changes-no-row)).
An attempt refused with a stale fence stays `unknown` and predates decision
0012; it is recorded in that decision's
[consequences](../../decisions/0012-merged-strong-steps.md#consequences), not fixed.
