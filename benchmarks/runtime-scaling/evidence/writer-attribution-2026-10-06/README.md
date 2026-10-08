# Writer attribution, checkpoint overlap and same-file sync interference

Status: 2026-10-06. Measurement-only continuation of the
[writer and ERP evidence](../writer-erp-2026-10-05/README.md). The phase 8 gate
remains **pending**; nothing here claims a pass and no runtime default,
durability, checkpoint threshold or runtime behaviour changed.

This directory records five new things over the previous run:

1. Writer step time split into admission, reduction, encoding and SQLite
   statements, with per-step attribution to local authoring versus incoming
   pages, plus commit, checkpoint and per-write latency samples on the same
   monotonic clock.
2. Latency of writes that overlap a WAL checkpoint versus those that do not,
   including the share of the p99 and p99.9 tails that overlapped one.
3. Per-write instrumentation cost against an otherwise identical
   uninstrumented run.
4. Fixed-rate local authoring while a preloaded peer backlog is applied to the
   same file, with the writer's step time split between authoring and incoming.
5. A short growth/plateau experiment that varies live rows and observer count
   and tracks the subphase counters and SQLite page accounting over time.

All runs are local, on one Apple M5 Pro / APFS machine, and each is at most a
few minutes. Build and source identity are in every `profile.json`.

Two source archives appear in the profiles, both hashed with `.jj`, `target`,
`local` and prior evidence excluded. `65dc1070…` produced the attribution,
overhead, first sync attempt and growth runs; `7e632b51…` produced the
sync-fixed run, which also carries the peer-retention fix and the
during/after-backlog latency split. `source-sha256.txt` and
`source-sha256-sync-fixed.txt` hold the two values. After the runs, the step
timer was additionally guarded behind the switch so the uninstrumented path
does no timing work; that edit changes only the off path and post-dates the
measurements, so the profiles describe the exact source used.

## How to read the counters

`RuntimeOptions::instrument` is off by default. When it is set,
`Replica::writer_stats` gains `admission_micros`, `reduction_micros`,
`encoding_micros`, `sql_micros`, `authoring_steps`, `incoming_steps`,
`authoring_step_micros` and `incoming_step_micros`, and
`Replica::checkpoint_intervals` returns the recent checkpoint windows with
monotonic start/end timestamps, kind, page-copy and database-flush times.

Subphases are charged per mutation step: `reduction` loads the touched rows and
runs the reducer, `admission` checks the candidate, `encoding` encodes the
operation and projects statements, and `sql` executes every statement of the
write. `step_micros` is the whole group's step loop; the four subphases plus
encoding of the incoming path need not sum to it.

**Commit boundary.** At `Durability::Full`, `synchronous=FULL` makes SQLite
flush the database file *inside* `COMMIT`, so `commit_micros` contains both the
SQL commit and the device flush. Separating them would require a different
synchronous mode and changes durability semantics, so this run reports the
combined `commit_micros` and says so. The writer's own checkpoint path *does*
separate its flushes (`checkpoint_wal_flush_micros`, `checkpoint_copy_micros`,
`checkpoint_flush_micros`).

**CPU time.** `process_cpu` is whole-process user+system time; on macOS it is
parsed from `ps -o time=`. Per-thread CPU is unavailable in safe Rust on macOS
(no unsafe code is permitted), so `writer_thread_cpu` reads `n/a` there; on
Linux the probe sums `/proc/self/task/*/stat` for threads whose name starts
with `rocky-writer`.

## Commands

```sh
cargo build --release -p runtime-scaling --bin scaling_probe --locked
target/release/scaling_probe erp-attrib 60 2000 benchmarks/runtime-scaling/evidence/writer-attribution-2026-10-06/attrib/data
target/release/scaling_probe erp-instrument-overhead 30 2000 benchmarks/runtime-scaling/evidence/writer-attribution-2026-10-06/overhead/data
target/release/scaling_probe erp-sync 250 120 2000 300000 benchmarks/runtime-scaling/evidence/writer-attribution-2026-10-06/sync-fixed/data
target/release/scaling_probe soak --directory benchmarks/runtime-scaling/evidence/writer-attribution-2026-10-06/growth/rows-100k-observers-100/data \
  --instrument --tables 2000 --observers 100 --rows 100000 --seconds 480 --operations 300000 --interval 30
```

The other two growth arms differ only in `--rows`, `--observers`, `--seconds`
and `--operations` (10,000 rows/100 observers/300 s; 100,000 rows/0
observers/240 s). Each profile's `command` in `profile.json` is exact.

## 1. Open-loop subphase and checkpoint attribution

`erp-attrib 60 2000 ...` ran four 60-second schedules on the local Apple M5
Pro, one file, `Full`, 2,000 tables, group 128, default `checkpoint_frames`
(16,384) and the same 4/s strong stream as the Part 2 `erp-open-loop` baseline.
Every offered write completed; no send was dropped. Raw output is
[`attrib/attrib.txt`](attrib/attrib.txt) and
[`attrib/profile.json`](attrib/profile.json).

Scheduled latency includes time behind schedule. The `samples` columns are the
probe's bounded sampler over the service interval (send to committed); no
sample was dropped at any rate (capacity 2,000,000).

| Offered/s | p50 | p99 | p99.9 | max | admission | reduction | encoding | sql |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 100 | 5.6 ms | 9.5 ms | 14.5 ms | 28.9 ms | 0.06 | 13.62 | 1.07 | 59.38 |
| 250 | 5.5 ms | 11.5 ms | 22.7 ms | 42.7 ms | 0.02 | 8.40 | 0.62 | 47.26 |
| 500 | 6.9 ms | 70.9 ms | 141.0 ms | 176.1 ms | 0.02 | 6.59 | 0.40 | 56.06 |
| 1,000 | 8.0 ms | 64.5 ms | 126.8 ms | 149.3 ms | 0.05 | 7.37 | 0.34 | 62.85 |

Subphases are µs per mutation step. SQLite statement execution (`sql`) is the
dominant step subphase at every rate (43–58% of the group's step time);
`reduction` (touched-row reads plus the reducer) is 7–10%; `admission` and
`encoding` are each under 1.1 µs/write and at most 1% of step time. Step
attribution to the request kind was authoring 75–101 µs/step and "other"
(strong commands) 15–30 µs/step; `incoming_steps` is zero here.

| Offered/s | commit share of wall | checkpoints (count, mean, max) | checkpoint copy / file flush | p99 tail overlapping | p99.9 tail overlapping |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 100 | 39.5% | 3, 12.4 ms, 18.4 ms | 4.84 / 5.45 ms | 4 / 60 = 6.7% | 2 / 6 = 33.3% |
| 250 | 87.6% | 9, 9.0 ms, 11.9 ms | 3.54 / 4.37 ms | 24 / 150 = 16.0% | 2 / 15 = 13.3% |
| 500 | 94.2% | 11, 16.8 ms, 35.0 ms | 9.14 / 5.87 ms | 0 / 300 = 0.0% | 0 / 30 = 0.0% |
| 1,000 | 87.3% | 14, 25.1 ms, 55.8 ms | 11.15 / 11.21 ms | 111 / 600 = 18.5% | 0 / 60 = 0.0% |

| Offered/s | no-checkpoint p50 / p99 / p99.9 | overlapping p50 / p99 / p99.9 | overlapping samples |
| ---: | ---: | ---: | ---: |
| 100 | 3.6 / 7.2 / 12.0 ms | 10.8 / 21.7 / 21.7 ms | 7 (0.1%) |
| 250 | 3.9 / 9.4 / 18.9 ms | 13.2 / 26.8 / 26.8 ms | 32 (0.2%) |
| 500 | 5.9 / 69.8 / 141.1 ms | 21.7 / 55.8 / 58.1 ms | 152 (0.5%) |
| 1,000 | 7.0 / 55.6 / 125.2 ms | 30.3 / 107.7 / 111.2 ms | 485 (0.8%) |

What this establishes and what it does not:

- **SQLite statements dominate the step, and commit dominates the writer.**
  Reducing row reads or projection encoding would not move the step cost much;
  the statement execution and the `COMMIT` (whose device flush is inseparable at
  `Full`) are the costs. Both are stated limits, not tuning here.
- **Checkpoint overlap is real for part of the tail, not all of it.** At 100,
  250 and 1,000 writes/s, 6.7–18.5% of the p99 tail overlapped a checkpoint and
  those writes ran at about 22–108 ms p99. At 500 writes/s no p99 or p99.9
  sample overlapped a checkpoint while the p99 was still 71 ms, so queueing
  behind the single writer (not the checkpoint) set that tail. The instrument
  therefore rules out a pure checkpoint explanation; it does not attribute the
  remaining tail to one subphase.
- **CPU vs wall.** Whole-process CPU was 5.6%, 10.2%, 14.3% and 20.6% of the
  window at 100–1,000 writes/s; the rest is idle or blocked on the flush/device.
  The writer thread's own CPU is reported as `n/a` on macOS.

## 2. Instrumentation overhead

`erp-instrument-overhead 30 2000 ...` ran a control (`instrument=false`) and a
measured (`instrument=true`) 30-second schedule at each rate on fresh files,
both collecting the probe's histogram and samples, with no strong stream. Every
offered write completed at the target rate with zero dropped sends, so the
measured stream showed no throughput loss. Raw output is
[`overhead/overhead.txt`](overhead/overhead.txt).

| Offered/s | control p50 / p99 / p99.9 | measured p50 / p99 / p99.9 | control CPU | measured CPU |
| ---: | ---: | ---: | ---: | ---: |
| 100 | 4.9 / 9.6 / 31.2 ms | 5.2 / 8.1 / 11.2 ms | 1.43 s | 1.64 s |
| 250 | 5.1 / 11.1 / 19.3 ms | 4.6 / 10.7 / 22.6 ms | 3.13 s | 3.32 s |
| 500 | 6.8 / 11.1 / 19.2 ms | 7.0 / 11.4 / 22.4 ms | 4.56 s | 4.87 s |
| 1,000 | 7.7 / 16.2 / 31.5 ms | 7.1 / 12.6 / 27.7 ms | 6.29 s | 5.47 s |

Across one control/measured pair per rate, the medians differ by at most
0.4 ms and the measured arm is sometimes lower on the tail; whole-process CPU
differs by at most 0.8 s in 30 s (about 2.7% of the window). This is the same
weak bound as the earlier histogram control: one pair cannot establish a p99
overhead, and the control necessarily reports zero subphases (the switch is
off), so its "step attribution" line is not a measurement. The switch is off by
default, so production is unaffected; the measured per-write cost is within
run-to-run variation at this resolution.

## 3. Same-file sync interference

`erp-sync 250 120 2000 300000 ...` authors at a fixed 250 writes/s on domain 1
while a preloaded 300,000-operation peer backlog is paged into domain 2 of the
same full-durability file (`operation_page` → `receive_page` →
`process_pending`, no new protocol). The peer preloaded in 40.1 s and then
applied 300,000 operations over 80.8 s (9,375 pages of 32 records). Every one of
the 30,000 scheduled local writes completed with no dropped sends. Raw output
is [`sync-fixed/sync.txt`](sync-fixed/sync.txt); the first attempt, which hit the
peer's default retention window, is kept in [`sync/`](sync/first-attempt.txt).

| Local writes | p50 | p99 | p99.9 | max |
| --- | ---: | ---: | ---: | ---: |
| Part 2 baseline, 250 writes/s, no incoming | 4.7 ms | 14.6 ms | 19.9 ms | 25.7 ms |
| This run, whole window | 5.6 ms | 13.0 ms | 40.8 ms | 57.6 ms |
| During the backlog drain (n=20,190) | 6.6 ms | 15.4 ms | 43.4 ms | 57.6 ms |
| After the drain (n=9,810) | 3.9 ms | 9.4 ms | 29.8 ms | 40.8 ms |

The p99 is essentially unchanged; the p99.9 about doubles and the maximum more
than doubles while the backlog is applied. 75.7% of the p99 tail and 100% of
the p99.9 tail overlapped a checkpoint (31 intervals), so under this load the
tail is checkpoint-correlated as well as incoming-correlated.

Writer step time split, from the new attribution counters:

| Kind | Steps | µs/step | Total step time |
| --- | ---: | ---: | ---: |
| Local authoring | 30,000 | 106 | 3.18 s |
| Incoming pages | 9,375 | 756 | 7.09 s |

Incoming pages are about a quarter of the steps but more than twice the step
time of local authoring, because each page is one receive commit (and flush).
`COMMIT` (with the `Full` flush) still dominates: 99.96 s of the 120 s window
(83%). No protocol or topology change was needed. This is one rate and one
backlog size on one machine; it does not bound the interference at other rates
or with a slower, networked peer.

## 4. Growth and plateau

Three fresh 2,000-table, full-durability soak arms used the new per-interval
subphase counters and SQLite storage facts. Each ran creates, updates and
periodic delete/replace churn, with 100 observers subscribing to hot-table
ranges where noted. Raw output and profiles are under [`growth/`](growth/).

| Arm | Live rows | Observers | Seconds | t=30 step µs/write | t=210 step µs/write | Last interval step µs/write | Throughput first → last |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| A | 100,000 | 100 | 480 | 174 | 229 | 331 | 1,280 → 916/s (min 586) |
| B | 10,000 | 100 | 300 | 208 | 211 | 226 | 1,239 → 1,185/s |
| C | 100,000 | 0 | 240 | 136 | 164 | 155 | 1,295 → 1,262/s |

Selected arm A intervals show the trend past the point where live rows had
plateaued at 100,000 (reached by t=90):

| t (s) | writes/s | step µs/write | reduction µs | sql µs | page_count | freelist % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 90 | 1,115 | 230 | 36 | 99 | 62,781 | 0.81 |
| 210 | 1,171 | 229 | 53 | 94 | 64,373 | 0.09 |
| 240 | 924 | 341 | 78 | 136 | 64,742 | 0.23 |
| 300 | 718 | 513 | 88 | 182 | 65,445 | 0.97 |
| 390 | 677 | 563 | 94 | 196 | 66,252 | 0.78 |
| 450 | 586 | 635 | 117 | 213 | 66,494 | 0.79 |
| 480 | 916 | 331 | 61 | 124 | 66,905 | 0.41 |

**Conclusion: in this configuration the writer cost is growing, not
flattening.** In arm A (100,000 rows, 100 observers) the per-write step time
rose 2–3× and throughput fell to roughly half after live rows had stopped
growing, entirely in `reduction` (touched-row reads plus reducer: 36 → 94–117
µs) and `sql` (99 → 196–213 µs). The same 100,000-row file with **zero
observers** (arm C) stayed near flat, and 10,000 rows with 100 observers (arm B)
also stayed flat. Free-page fragmentation was under 1.3% throughout and showed
no upward trend in arm A, and the database grew only ~18% in pages while cost
grew 2–3×, so neither free-page fragmentation nor file size alone explains it.
The pattern is consistent with observer read contention on the shared read path
(`reduction` includes touched-row reads) once the live-row set is large, but
this experiment does not isolate the mechanism.

What the design does and does not control:

- **Live rows** were varied (A vs B) and **observer count** (A vs C); each arm
  used a fresh file. Retained history moves with the checkpoint/prune cycle
  (2,200–6,100 retained operations) and cannot be held equal without changing
  retention, so it is observed, not fixed. **Index depth** is fixed by the
  schema and was not varied. **DB/WAL size** and **free-page fragmentation**
  were recorded, and neither tracks the arm A growth.
- No phase 8 pass is claimed. Phase 8's soak slice says a figure that trends
  upward without levelling off is a failure to explain; this 8-minute arm
  reproduces an upward trend at 100,000 rows with observers and points at the
  subphases, but it is far shorter than the three-hour soak and far below the
  5,000,000-row target.

## Limitations

- Everything is local, on one Apple M5 Pro / APFS machine, over seconds to
  eight minutes. It does not qualify a second hardware profile, a remote or
  networked peer, or power loss.
- Per-thread CPU is `n/a` on macOS: no safe-Rust API exposes it without a
  system dependency, so only whole-process CPU is reported.
- At `Durability::Full` the device flush is inside `COMMIT`; the combined
  figure is reported and the boundary is not separable without changing
  semantics.
- The sync run uses one rate (250 writes/s) and one backlog (300,000). Its
  first attempt showed that a peer with default retention exposes only the
  window behind its last checkpoint (about 4,416 operations), so a preloaded
  backlog needs the peer's automatic retention disabled; the file under test
  kept runtime defaults throughout.
- The sampler keeps the most recent 2,000,000 samples and reports drops; none
  were dropped in any run here.
- Growth arms vary live rows and observers only; retained history, index depth
  and an explicit fragmentation workload are not independently controlled.

## Next recommended run

1. A matched multi-hour (or longer, with approval) pair at 100,000 and 500,000
   live rows, 100 versus 0 observers, sampling subphases every 30–60 s and
   adding observer-read latency/count. This separates observer read contention
   from retained-history growth and shows whether arm A's rise levels off.
2. Re-run the phase 5 gate with `instrument` set to correlate the gate's p99
   with checkpoint overlap at the workload that missed its budget.
3. The 5,000,000-live-row target and the second hardware profile remain phase 8
   slices; neither is started here.


