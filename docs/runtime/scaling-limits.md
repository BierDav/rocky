# Scaling limits and evidence

Phase 11 removes the command streams from the trusted probes. Earlier measured
command mixes remain historical evidence; current local/incoming/paced-write
probes do not reproduce those mixes. No new performance result is claimed by
[command removal](../features_09/11-removing-strong-commands-evidence.md).

Enforced bounds and measured limits of both runtime paths on SQLite. Figures are
one release run on the reference machine (Apple M5 Pro, internal SSD/APFS)
unless a range or another profile is named; repeated runs varied by about ±10%.
They are measurements, not portable service levels. Raw output and per-run
reports live under
[benchmarks/runtime-scaling/evidence](../../benchmarks/runtime-scaling/evidence/).
The mechanisms are in [write concurrency](write-concurrency.md); encrypted
budgets are in [scaling and rebuilds](scaling-and-rebuilds.md#budgets).

## Reproduce

```sh
cargo run --release -p rocky-runtime-bench --bin scaling_probe --locked -- <section>
```

| Section | Measures |
| --- | --- |
| `all` | `domains`, `files`, `clients`, `history`, `observers`, `strong`, `strong-cut`, `retention`, `wide`, `encrypted`, `hardening`; about fifteen minutes |
| `layouts` | Sixteen domains over one, four and sixteen files |
| `stored ROWS TABLES DIR` | Writes, point reads and pages at e.g. 500,000 rows; rerun on `DIR` for a fresh process |
| `checkpoints OPS ROWS TABLES` | Sustained edits with checkpoints and pruning |
| `gate SECONDS FILES FRAMES GROUP DIR [TABLES]` | The [gate workload](#gate-workload) |
| `wide-matrix DIR TABLES DOMAINS`, `wide-encrypted` | Wide schema install, reopen and statement cost |
| `roles ROWS BEHIND` | Full node and two partial encrypted replicas over loopback TCP |
| `soak`, `soak-audit` | The [scale qualification](#scale-qualification) |

Writers are closed-loop through `submit`. Resident memory is process-wide, so run
one table count per process. Engine-only comparisons, including the earlier
Turso engine, are in [engine-scale](../../benchmarks/engine-scale/RESULTS.md).

## Enforced bounds

| Bound | Value | Refusal |
| --- | --- | --- |
| Files per `Runtime` | 1 by default; at most 16 automatic, 256 with `open_placed` | `Quota` |
| Domains per runtime or manifest | 4,096 | `Incompatible` / manifest error |
| Manifest | 32,768 items per group, 4 MiB body | Manifest error |
| Group commit | 128 steps by default, at most 256; 4 MiB of queued bytes | `Quota` for the option |
| Domain queue | 32 commands and 8 MiB | `mutate`: `Backpressure`; `submit` waits |
| `checkpoint_frames` | 16,384 (64 MiB) by default; 256 to 1,048,576 | `Quota` for the option |
| Bound read page | 1 to 1,024 rows, at most 1 MiB, SQL at most 8 KiB | `Quota` |
| Read transaction | 500 ms | Retryable error |
| Busy file | 10 attempts, 318 ms of backoff | `RetryExhausted` |
| File lock at open | About 100 ms of retries | `Incompatible` |
| Strong cut | 32,768 operations after the executor's checkpoint base; lifetime operations do not count | `Quota` (`strong cut`) |
| Trusted `SyncState` | 256 held operations above the prefix; 32 operations per range page | Truncated list / cursor |
| Schema bootstrap | 64 statements per transaction; 4 files installed concurrently | — |

## Measured limits

Trusted path writes, writes/s:

| Workload | `full` | `buffered` |
| --- | ---: | ---: |
| One domain, one client | 266 to 331 (one 3 to 4 ms flush each) | 9,203 to 9,643 |
| One domain, 8 / 32 clients | 1,276 / 3,533 | 13,011 / 15,324 |
| 16 domains × 8 clients, one file | 11,212 | 26,884 |
| 16 domains, one client each: 1 file / 4 files | 1,846 / 680 | 10,453 / 13,852 |
| 16 domains × 8 clients: 1 file / 4 files | 9,519 / 4,172 | 20,016 / 41,901 |
| Strong commands, two domains | 81 to 86/s, 23 ms median | 122 to 144/s, 13 to 17 ms |

Without group commit `full` stays at about 300 writes/s whatever the load, so
sharing flushes is the whole gain. More files help buffered writes and cost
flushed ones; every exposure is a flush.

Strong commands on one long-lived domain, one row per write, default retention
(`strong-cut`, [evidence](../../benchmarks/runtime-scaling/evidence/strong-cut-2026-10-08/)):

| Lifetime operations | Most operations after the base | Command median / slowest |
| ---: | ---: | --- |
| 50,000 | 830 | 84 / 92 ms |
| 100,000 | 1,660 | 153 / 162 ms |
| 200,000 | 3,305 | 300 / 309 ms |

Every lifetime here was refused before [Decision 0042](../decisions/0042-bounded-strong-cut.md).
The cost grows with the rows of the checkpoint copy loaded into memory,
not with the cut.

Reads and storage, 500,000 live rows in one domain:

| Measure | 20 tables | 2,000 tables |
| --- | --- | --- |
| Median write, empty against full | 129 against 117 µs | 143 against 146 µs |
| Point read by key / index | 32 / 31 µs | 40 / 35 µs |
| Page of 10 / 100 / 1,000, 12,500 deep, keyset | 30 / 54 / 285 µs | — |
| Same by offset cursor | 579 / 589 / 668 µs | — |
| Writes/s with 120 observers of another table / the edited row | 6,630 / 4,520 | 6,326 / 4,391 |
| Resident; reopen | 14 to 20 MiB; 46 ms | 62 to 68 MiB; about 100 ms |

With checkpoints and pruning, 500,000 live rows edited to 2,000,000 operations:
median write 100 against 99 µs (20 tables) and 146 against 170 µs (2,000
tables); the file stays at 148.6 / 211 MB because freed pages are reused; the
WAL reaches 56 to 67 MB between engine checkpoints; reopen 1 / 117 ms; audit
0.9 / 1.3 s.

Wide schemas, three fresh runs per size, one domain:

| | 10 tables | 2,000 tables |
| --- | --- | --- |
| First install (budget 10 s) | 36 to 42 ms | 7.9 to 8.2 s |
| Reopen (budget 2 s); resident | 2 ms; 13 MiB | 97 to 103 ms; 50 to 51 MiB |
| Cold reader startup + eight reads | 0.7 ms | 23 ms |
| Warm write / read mean | 107 / 25 µs | 73 / 28 µs |

Dense placements: 8 domains × 2,000 tables in one file (about 32,000 physical
tables) hit the read deadline; in four files they installed in 128 s. 64 × 2,000
in 32 files installed in about 17 minutes, reopened in 4.6 s and held 1.1 GiB.
No hard table limit is claimed; DDL and connection startup grow steeply with
physical tables per file.

Encrypted path, one scope, 4,000 rows: flushed `author` and `receive` 5 to 8 ms
per call; one submitter 160 to 200 writes/s, eight 660 to 680/s; a full paged
pass about 4.5 µs per row; a page of 10 or a point read about 0.2 / 0.08 ms at
any scope size; rebuild 0.19 ms per operation; reopen 433 ms after a checkpoint.
With a retention policy, retained history and file size plateau (2,000 rows
edited 14,000 times: 256 retained operations, 11.2 MB). A 2,000-entity signed
manifest verifies in 19 ms at open; per-operation cost is flat in unused
entities.

Replica roles (`roles 500000 10000`): a partial replica holding 5% converged in
8.4 s; 10,000 operations caught up at 205 operations/s while the full node's
writes reached a 120 ms p99; rebootstrap 5.8 s; eviction of 25,000 rows 0.5 s.
These frames carried the author's credential chain: about 30 KB per 32 rows
with one grant, 67 KB with 20 grants. A header now names the chain by
[digest](replica-roles.md#chains-by-reference).

## Gate workload

`gate 600 1 16384 128 DIR`: one file at `Full`, 16 domains with 8 closed-loop
clients each, 4 of them also receiving a page of 32 operations every 100 ms
through `receive_page`, and 2 more domains running one strong command at a time
against an in-memory coordinator. Strong commands use separate domains because
of the strong-cut bound. Ten minutes on the reference machine:

| Measure | Value |
| --- | --- |
| Local writes | 5,250 to 5,500/s; about 7,000/s at the start, 4,700 to 5,300/s at the end |
| Write latency median / p99 / maximum | 20 / about 100 / 174 to 208 ms |
| WAL checkpoint, mean | 67 ms: 2.6 WAL flush, 14 copy, 50.6 file flush |
| `checkpoint_frames` 4,096 / 16,384 / 65,536 / 131,072, one minute | WAL 11 / 64 / 233 / 515 MiB, write p99 58 / 55 to 60 / 46 / 41 ms |
| With 500 or 2,000 extra tables in one domain | 6,587 / 6,649 writes/s, p99 100 ms, slowest ten seconds 5,800 / 4,007 |

## Scale qualification

Not qualified. `scaling_probe soak` runs three domains of 2,000 logical tables
in one file at `Full`, 100 observers, 500,000 live rows in the main domain,
until both three hours and 5,000,000 main-domain operations have passed. It
requires a fresh `--directory`, then checkpoints, prunes, reopens and audits;
any task error fails the run. `benchmarks/runtime-scaling/qualification.py run`
captures a hardware profile and raw output, and its `report` always leaves the
gate pending.

All six soaks passed duration, operation count and audit:

| Profile | Operations | Reopen ms | Peak RSS MiB | Gate mean / slowest 10 s writes/s | Gate p99 ms |
| --- | ---: | ---: | ---: | --- | ---: |
| x86-64 KVM i5-1235U / ext4 | 6,448,914 | 1,567 | 210 | 3,291 / 2,100 | 158.7 |
| Neoverse-N1 cloud block / ext4 (3 hosts) | 8.7 to 9.4 M | 2,370 to 2,499 | 207 to 327 | 2,165 to 2,233 / 1,375 to 1,498 | 329 to 351 |
| Apple M4 / APFS | 8,682,443 | 1,033 | 201 | 3,085 / 1,778 | 310.0 |
| Apple M5 Pro / APFS | 11,402,819 | 626 | 253 | 5,076 / 4,368 | 128.9 |

Missed or pending:

- The gate's 5,000 writes/s in every interval and its p99 near 100 ms, on all
  six profiles.
- Reopen under 2 s on the cloud hosts; 256 MiB RSS on one of them; point-read
  p50 under 1 ms on one of them.
- 5,000,000 live rows, a demonstrated plateau of per-step cost, combined
  coordinator/full node/partial replica topology with faults, strong commands and
  effects in the soak, the same workload on the encrypted path, and power loss
  on a real device.

## Known costs

- **The flush is the cost.** `COMMIT` takes 39 to 94% of writer wall time at
  `Full`; SQL statement execution is 43 to 58% of step time, admission and
  encoding under 1%.
- **WAL checkpoints stall the writer** for a time that grows with the file.
  Writes overlapping one are 3 to 5 times slower, but they explain only part of
  the p99 tail; queueing behind the single writer sets the rest. Database-file
  flushes averaged 191 to 228 ms on the cloud hosts against 66 to 77 ms locally.
- **Per-step cost rises over a soak** without attribution.
- **Strong commands** replay their sealed cut in memory from the replica's
  checkpoint base; a verifier with an older base replays more than the
  executor did, bounded by its retained bytes. `strong_status`, `strong_results` and their pages decode
  every retained result on the writer thread.
- **`query()` and `observe()`** return the whole projection on the writer thread.
- **Cold read connections** parse the whole file schema before their read
  deadline starts; the cost remains in first-read latency.
- **Encrypted path:** reads share the replica mutex; open replays each keyed
  scope after its checkpoint; out-of-log-order delivery is quadratic in pending
  frames; command state is one record per scope with whole before and after row
  maps; credential chains dominate frame size.
- **Power loss** is simulated by restoring files copied at the last flush, not
  tested on a device.
