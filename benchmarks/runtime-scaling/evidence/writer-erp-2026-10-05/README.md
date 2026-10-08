# Writer attribution and ERP latency evidence

Status: measurement in progress. The phase 8 gate remains pending.

## Local phase 5 gate

The fresh Apple Silicon run uses one file, `Full`, 2,000 tables, 16 domains ×
8 clients, group 128, 16,384 checkpoint frames and 600 seconds. It runs the
existing closed-loop `scaling_probe gate`, which includes its existing strong
command and incoming streams. Raw output is in [gate.txt](local-phase5/gate.txt).
The binary was built with `cargo build -p runtime-scaling --bin scaling_probe
--release --locked`.

This is the current working tree, including unrelated pre-existing edits. The
source archive hash (excluding `.jj`, `target`, `local` and prior evidence) is
`677953ef7755afa4596c31014906120c0a1c53e4534ed6ab311bab1ec273a6f9`; its binary
hash, Apple M5 Pro profile and exact command are in `local-phase5/profile.json`.
Raw output shows 4,945 writes/s average, 2,076/s in the slowest 10-second
interval, and 130.6 ms local p99 (292.8 ms max). Writer totals were step 50%,
commit 37%, reads 1% of one thread's wall time; passive checkpoints averaged
89.3 ms (20.7 ms copy, 66.0 ms database-file flush, 2.6 ms WAL flush). Strong
command p99 was 599.8 ms. The prior local M5 run was 5,076/s average with
4,368/s slowest interval and 128.9 ms p99, so both rechecks miss the interval
floor and have p99 above 100 ms.

These existing counters still do not split admission/reduction/encoding/SQL
stages or retain checkpoint intervals for per-write overlap analysis. Thus
they cannot identify the dominant step subphase or tell what fraction of tail
samples overlapped a checkpoint. The broad time-share and mean checkpoint
durations are correlation only; checkpoint causality for p99 remains unproven.

## Open-loop ERP rates

Command: `target/release/scaling_probe erp-open-loop 60 2000 <fresh-dir>`.
The probe ran four 60-second schedules on the local Apple M5 Pro, one file,
`Full`, 2,000 tables and group 128. Each offered write was an independent task
in a capped set of 1,024; latency starts at its scheduled send time. No scheduled
events were dropped. `open-loop.txt` is raw output and `profile.json` records
the source/binary hashes and command.

| Offered writes/s | p50 | p99 | p99.9 | Max | Behind 10 s intervals | Strong p99 (nominal 4/s) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 100 | 5.8 ms | 8.9 ms | 13.9 ms | 29.0 ms | 0/6 | 26.6 ms |
| 250 | 4.7 ms | 14.6 ms | 19.9 ms | 25.7 ms | 1/6 | 111.3 ms |
| 500 | 6.7 ms | 13.1 ms | 24.0 ms | 39.2 ms | 2/6 | 110.2 ms |
| 1,000 | 9.0 ms | 73.1 ms | 116.1 ms | 140.5 ms | 6/6 | 357.8 ms |

The strong stream achieved about 3–4 commands/s; each row's p99 is reported
separately. At 1,000/s, 38,957 of 60,000 scheduled starts were more than one
period late even though all 60,000 completed. This is queueing evidence; it
does not identify a checkpoint cause. The probe prints interval counts and
writer/checkpoint aggregates but cannot join individual write samples to
checkpoint intervals.

### Histogram collection overhead

`erp-overhead 30 2000 <fresh-dir>` runs one 30-second uninstrumented-histogram
control and one measured run at each rate, on fresh files. Both runs completed
every scheduled offer at the target rate (100, 250, 500 and 1,000/s), with zero
dropped sends. The measured stream therefore showed no throughput loss at this
resolution. One pair per rate is too small to prove a p99 overhead bound; the
control intentionally does not collect latency histograms, so tail percentiles
cannot be compared. Raw paired output and profile are under `local-overhead/`.

## Same-file bulk burst

Command: `target/release/scaling_probe erp-bulk 180 2000 <fresh-dir>`. The
probe authored 50,000 writes on domain 2 while domain 1 offered 250 writes/s,
both in the same full-durability 2,000-table file, with the 4/s strong stream
also active. The burst took 267.9 seconds. The 180-second background schedule
offered and completed 45,000 writes with zero dropped sends; 43,749 scheduled
samples fell during the burst. During-burst background p99 was 11.6 ms,
p99.9 28.8 ms and max 44.8 ms. The separate 250/s no-burst run measured 14.6 ms
p99, 19.9 ms p99.9 and 25.6 ms max. This single unpaired comparison shows no
p99 increase, but a larger p99.9 and max; run-to-run variation prevents a causal
claim. Strong p99 was 94.8 ms during bulk versus 111.3 ms in the separate
250/s run. Raw output and profile are under `local-bulk/`.

## Reducer order analysis

Trusted mergeable reducers reviewed:

- Todo title and completion fields retain the greatest `(Lamport, node,
  sequence)` stamp independently. Deletes become a permanent tombstone and
  discard hidden registers. This is a deterministic max-register merge; valid
  operations converge across dependency-ready delivery orders. See
  [`reducer.rs`](../../../../examples/todo-app/src/reducer.rs) and the
  [operation semantics](../../../../docs/applications/todo/operation-semantics.md).
- Inventory draft notes likewise retain the greatest `Stamp` per entity; see
  [`inventory-app/src/lib.rs`](../../../../examples/inventory-app/src/lib.rs).
- Inventory stock reservations are not mergeable operations. They execute as
  strong commands against the confirmed predecessor, so their result depends
  on the coordinator's total predecessor order. This is the appropriate
  boundary for gapless numbering or other stateful business transitions.
  See [app runtime evidence](../../../../docs/runtime/app-runtime.md#examples-and-qualification)
  and [strong publication](../../../../docs/runtime/strong-publication.md).

The trusted merge path therefore supports a separate working copy for
commutative draft fields, while strong command state still requires ordered
application and reconciliation. This code review does not establish a complete
working-copy/sync-store architecture or SQL-session merge safety for every
application.

## Remaining measurements

The open-loop, matched histogram controls, bulk burst and 10-minute local growth
pilot are complete. Same-file incoming-backlog attribution has not been run.
No phase 8 pass is claimed.

### Growth/plateau experiment design

The fresh local pilot used 2,000 tables, 100,000 target live rows, 100 observers,
full durability and default retention/checkpoints for 600 seconds. At 120
seconds it reached 100,000 live rows; it stayed there through the end. The first
full-row interval was 233.33 µs/write. Across the last four intervals step cost
averaged 250.16 µs/write (244–258), while throughput stayed between 1,128 and
1,153 writes/s. File size rose from 262 MiB at 180 seconds to 280 MiB at 600;
retained operations varied from 3,162 to 6,033 as retention ran. This pilot
shows a roughly 7% rise from the first full-row interval, then a noisy plateau
over its final four intervals. It does not reproduce or explain the earlier
1.3–1.9× three-hour growth at 500,000 rows. See
[`local-growth/soak.txt`](local-growth/soak.txt) and its `profile.json`.

The soak changes live rows and retained history together, so the longer trend
still needs a separating experiment. Use fresh files and matched 10-minute
intervals at fixed write shape, group size and durability, changing one
dimension at a time:

| Candidate | Hold fixed | Vary and record |
| --- | --- | --- |
| Live rows | schema, write mix, observer count and age of the seeded file | 10k / 100k / 500k rows; record retained count/bytes and file/WAL sizes |
| Operation age | live rows, schema and observer count | add only updates to existing entities; record cumulative and retained operation counts under default pruning |
| Index depth | live rows, schema and operation age | vary key cardinality/distribution; inspect read-only `dbstat` paths and `EXPLAIN QUERY PLAN` |
| DB/WAL size | live rows, keys and write mix | compare matched fresh and churn-aged files at the same default checkpoint policy; record page counts, frames and bytes |
| Free-page fragmentation | live rows, keys and observer count | churn-heavy delete/reinsert vs fresh load; read-only `freelist_count` and page-count snapshots |
| Observer count | all storage dimensions | 0 / 100 / 120 observers (the current soak limit); notification count and step/commit costs |

Sample counters and storage facts every 60 seconds, run at most 30 minutes
locally for a first pass, and call a trend flat only when the final three
intervals' slope is within the run's measured noise. Each arm uses a fresh
seeded file and changes one workload dimension; exact equality of retained
history cannot be forced without changing retention behavior, so record the
observed retained count and reject mismatched intervals. Per-stage µs/write is
not available yet, so the pilot cannot distinguish reducer work from row reads
or SQL execution. A 30-minute pilot at 500,000 live rows is the next useful
local check; a multi-hour or remote extension is proposed only after review.

### Sync interference

The existing `gate` accepts incoming pages while authoring, but its peer source
is a separate database and it continually produces only bounded 320-operation
pages. The `roles` probe exercises native session catch-up in the replica-role
topology, not a trusted same-file authoring queue. A useful comparison needs a
new trusted probe mode that preloads a peer backlog, then feeds pages to one
domain in the authoring runtime while open-loop local writes target another
domain in that same file. Existing `receive_page`/`process_pending` APIs are
enough; no protocol change is indicated. It has not been implemented or run.
