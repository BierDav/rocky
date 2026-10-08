# Should Rocky change its storage engine?

Status: measured 2026-10-04 on an Apple M5 Pro (18 cores, 48 GiB, macOS, APFS).
Every figure is one release run of the [shared workload](README.md#workload),
one process at a time on an otherwise idle machine. The full matrix is in
[results/summary.md](results/summary.md) and the raw lines in `results/raw/`.

## Answer

- **DuckDB: no.** At 2,000 tables it needs 10 GiB of memory and a 3.2 GiB file
  for 500,000 rows, and every operation is 10 to 50 times slower than the others.
- **A key-value store (redb, fjall): no.** redb has no per-table cost at all,
  but neither engine is faster than SQLite where it matters, both use more
  memory and disk, and Rocky would have to build SQL reads itself.
- **Turso as pinned: not at this size.** The table-count cost is confirmed.
- **Turso with a 96-line engine patch and a larger page cache: works**, and it
  is the only engine here that commits durably from several threads at once.
- **SQLite in WAL mode: meets the target as shipped**, with the lowest memory,
  smallest file and no stalls, but one writer per file.

The scale target does not force a change, because the patch removes the
limit. The measurements still favour SQLite unless concurrent durable writers
inside one file are required; see [the choice](#the-choice).

## At the target: 2,000 tables

Medians in µs unless stated. "Patched" is Turso 0.8.1 with
`turso-patched/root-page-lookup.patch` and a 256 MiB page cache.

500,000 rows:

| | Turso 0.8.1 | Patched | SQLite 3.53 | redb 4.3 | fjall 3.1 | DuckDB 1.5 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Point read | 146 | 14 | 6 | 2 | 2 | 147 |
| Range read, 100 rows | 182 | 50 | 17 | 19 | 87 | 145 |
| Rocky-shaped write, unflushed | 672 | 40 | 30 | 23 | 6 | 1,539 |
| Load, rows/s | 1,651 | 101,481 | 1,014,198 | 998,003 | 453,309 | 140,845 |
| Schema install | 30.8 s | 31.1 s | 0.6 s | 0.04 s | 1.2 s | 0.6 s |
| Open to first read | 98 ms | 104 ms | 13 ms | 4 ms | 16 ms | 203 ms |
| Checkpoint | 3,041 ms | 52 ms | under 1 ms | 164 ms | 575 ms | 1,400 ms |
| Files | 119 MiB | 95 MiB | 95 MiB | 257 MiB | 151 MiB | 3,241 MiB |
| Peak memory, unflushed writers | 242 MiB | 595 MiB | 57 MiB | 596 MiB | 708 MiB | 10,476 MiB |
| Peak memory, durable writers | 260 MiB | 263 MiB | 47 MiB | 127 MiB | 157 MiB | 9,988 MiB |

5,000,000 rows (unpatched Turso and DuckDB were not run at this size):

| | Patched | Patched, default cache | SQLite | redb | fjall |
| --- | ---: | ---: | ---: | ---: | ---: |
| Point read | 14 | 13 | 6 | 3 | 3 |
| Range read, 100 rows | 53 | 51 | 17 | 24 | 124 |
| Rocky-shaped write, unflushed | 44 | 46 | 32 | 25 | 8 |
| Files | 713 MiB | 713 MiB | 707 MiB | 2,056 MiB | 916 MiB |
| 8 writers, unflushed, tx/s | 30,738 | 1,341 | 27,611 | 23,344 | 56,432 |
| 8 writers, worst commit | 148 ms | 4.9 s | 164 ms | 1.9 s | 13 ms |
| Peak memory, unflushed writers | 791 MiB | 514 MiB | 57 MiB | 1,104 MiB | 1,095 MiB |
| Peak memory, durable writers | 321 MiB | 302 MiB | 47 MiB | 242 MiB | 359 MiB |

Row count changes nothing for any engine that passed at 500,000 rows.

## Durable commits

With every commit forced to stable storage (`F_FULLFSYNC`), one commit costs 3
to 5 ms on every engine. Transactions per second at 2,000 tables, 5,000,000 rows:

| Writer threads | Patched Turso | SQLite | redb | fjall |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 308 | 295 | 210 | 327 |
| 4 | 657 | 171 | 208 | 322 |
| 8 | 1,187 | 167 | 205 | 319 |

Turso is the only engine whose durable throughput grows with writers in one
file: four times SQLite at eight threads. This is the advantage the
[earlier decision](../engine-comparison/DECISION.md) was based on, and it holds
at target size. SQLite falling below its one-writer figure is partly this
adapter's sleep-and-retry on a busy lock; a queueing writer would stay near 300.
Batching several mutations into one transaction raises any engine's figure and
was not measured.
It was measured later the same day: see
[SQLite batched writer](#sqlite-batched-writer-2026-10-04).

## Per engine

**Turso 0.8.1, unpatched.** Confirms [engine at target size](RESULTS.md):
statements are 20 times slower at 2,000 tables than at 20, a checkpoint takes
3 s, and a second thread waited 2.1 s during the load.

**Turso 0.8.1, patched.** The patch adds a keyed index beside the root-page
map: 96 lines added and 31 removed in two files of `turso_core`. The engine's
523 MVCC unit tests pass with it. It is not upstream; the scan is still in
0.8.2-pre.2 and on the main branch. Three limits remain after it:

- With the default 8 MiB page cache, write throughput falls as the file grows
  (1,341 tx/s and a 4.9 s commit at 5,000,000 rows). A 256 MiB cache fixes
  this; Rocky's runtime has no cache setting today.
- Schema install takes 31 s for 2,000 tables in this adapter, one statement per
  commit. Rocky's own install measured 4.8 s, so this is partly the adapter.
- Memory under sustained unflushed writes reaches 500 to 800 MiB, above the
  256 MiB budget. Under durable writes it stays at 260 to 320 MiB.

Upstream's manual calls MVCC a supported journal mode and, in its journal-mode
table, not production ready.

**SQLite 3.53.2, WAL.** Default page size and cache. Latency is flat from 20
to 16,000 tables. What grows with table count is per-connection schema parsing:
13 ms and 7 MiB at 2,000 tables, 353 ms and 26 MiB at 16,000. Creating tables is
quadratic: 0.6 s at 2,000, 45 s at 16,000. One writer per file; a waiting writer
saw at most 33 ms during the load. Its write p99 reaches 0.8 ms when a commit
triggers the automatic checkpoint.

**redb 4.3.0.** No per-table cost: 48,000 tables install in 0.1 s and open in
4 ms. One writer, no group commit, files two to three times larger, and
occasional commits of 1 to 3 s under eight writers. Memory figures include a
512 MiB cache the adapter configured. No SQL.

**fjall 3.1.12.** Only the layout with one keyspace for all rows was run. With
one keyspace per table, the agent's preliminary run could not reopen a
2,000-table store (recovery hangs) and install took over six minutes. No SQL.

**DuckDB 1.5.** Memory and file size follow the number of tables and indexes,
not the data. With a 256 MB memory limit the 2,000-table load fails after 56
tables. Primary-key and range reads do not use an index; they are scans that
only look acceptable because each table is small. Every commit is flushed with
no way to turn that off. A checkpoint stalled another writer for 1.4 s. It
would suit analytical queries over exported data, which this workload does not
contain.

## The choice

| | Stay on Turso | Move to SQLite |
| --- | --- | --- |
| Work needed for the target | Carry or upstream the patch; add a cache setting; reset cached statements | Replace the storage adapter; Rocky's SQL and schema carry over |
| Durable writers in one file | Scales to about 1,200 tx/s at 8 threads | About 300 tx/s whatever the thread count |
| Memory at 2,000 tables | 260 to 800 MiB | 47 to 57 MiB |
| Stalls seen by another thread | 0.1 to 0.15 s with the larger cache | Up to 33 ms |
| Maturity | MVCC documented as not production ready | Decades of production use, including phones |

More than about 16,000 physical tables in one file is impractical on either
engine, so phase 6 needs domains
spread across files or shared physical tables regardless. Once each file has
one writer, the earlier comparison found no Turso advantage.

## Not covered

- Rocky itself. This is engine evidence; no Rocky code ran on SQLite.
- Repeated runs. Each cell is one run; treat differences under 1.5 times as noise.
- More than one process, cold caches, phone hardware and power loss.
- Encryption at rest, long-running readers, deletes and rows over about 100 bytes.
- Turso's experimental passive checkpoint.

## SQLite batched writer, 2026-10-04

Measured 2026-10-04 on an Apple M5 Pro (18 cores, 48 GiB, macOS, APFS) with
SQLite 3.53.2 bundled in rusqlite 0.40.2, WAL mode, for
phase 2. This is engine-only
evidence from `sqlite-writer` (see [README](README.md#batched-writer-probes));
no Rocky code ran. Each cell is the median of three 3 s runs (range in
brackets) unless marked. Raw lines are in `results/raw/sqlite-writer.jsonl`.

The writer thread owns the file's only connection. Clients send the
`mutation` of the [workload](README.md#workload) over a channel and wait for
an acknowledgement. The writer takes whatever is waiting (at most 64 or
256 KiB, no added delay), runs `BEGIN IMMEDIATE`, each mutation in its own
`SAVEPOINT`, `COMMIT`, then acknowledges. "Durable" is `synchronous=FULL` with
`fullfsync` and `checkpoint_fullfsync`; "buffered" is `synchronous=NORMAL`.
`wal_autocheckpoint=0`; the writer runs `wal_checkpoint(PASSIVE)` between
groups once the WAL exceeds the threshold, with `journal_size_limit=0` so the
file length measures content.

### Group commit on one file

2,000 tables and 500,000 rows in one file, split over one domain per client.

| Clients | Checkpoint at | Durable writes/s | Mean group | Ack p50 / p99, ms | Buffered writes/s | Ack p50 / p99, ms |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 4 MiB | 256 (245–283) | 1.0 | 4.0 / 8.0 | 4,422 (4,421–4,641) | 0.07 / 0.28 |
| 8 | 4 MiB | **1,295** (1,189–1,345) | 7.3 | 4.9 / 22.7 | 4,456 (4,455–4,489) | 0.53 / 19.1 |
| 16 | 4 MiB | 2,276 (2,245–2,315) | 15.4 | 5.0 / 23.2 | 4,572 (4,469–4,572) | 1.2 / 20.3 |
| 1 | 64 MiB | 259 (254–270) | 1.0 | 4.0 / 5.3 | 8,896 (8,196–8,984) | 0.07 / 0.18 |
| 8 | 64 MiB | **1,478** (1,350–1,486) | 7.2 | 5.0 / 10.9 | 9,623 (8,835–9,806) | 0.49 / 1.7 |
| 16 | 64 MiB | 2,725 (2,698–2,733) | 15.4 | 5.0 / 14.4 | 9,173 (9,165–9,448) | 1.0 / 5.8 |

- **Gate met**: eight durable clients reach 1,295 to 1,478 writes/s against
  patched Turso's 1,187 to 1,314 (256 MiB cache). One of three runs at 4 MiB
  gave 1,189.
- One durable client: 245 to 286 writes/s across twelve runs at 20 and 2,000
  tables (including `batched-1client-wal64`), against 284 to 297 for the existing SQLite
  writer and 296 to 321 for patched Turso. Commits take one `F_FULLFSYNC`,
  which takes about 3 or about 4 ms on this device, so 10 to 15% under Turso is
  within this device's run-to-run spread. The figures are not shown to be equal.
- The WAL stayed at the threshold plus one group: 4.1 to 4.7 MiB, or 64 to
  66 MiB. A passive checkpoint takes 11 to 16 ms at 4 MiB and 55 to 62 ms at
  64 MiB, and is the p99 above.
- Each mutation adds about 30 KiB of WAL (whole 4 KiB pages: row, two
  indexes, operation, outbox, replica). Buffered throughput is checkpoint-bound
  at 4 MiB: about 115 checkpoints in 3 s take half the writer's time.

### One file against several

16 domains of 8 tables with 1,000 rows each, one client per domain; one batched
writer per file. Checkpoints at 64 MiB per file (three runs). The 4 MiB and
no-checkpoint rows are two runs each and show the range.

| Layout | Durable writes/s | Durable ack p50 | Buffered writes/s |
| --- | ---: | ---: | ---: |
| 1 file, 16 domains | **2,813** (2,708–2,822) | 5.0 ms | 11,459 (10,526–12,061) |
| 4 files, 4 domains each | 921 (888–922) | 16 ms | **17,817** (16,916–18,645) |
| 16 files, 1 domain each | 297 (293–329) | 47–52 ms | 14,763 (13,464–19,099) |
| 1 / 4 / 16 files, 4 MiB | 2,188–2,342 / 840–877 / 294–329 | | 4,587–4,652 / 6,740–7,161 / 6,752–7,320 |
| 1 / 4 / 16 files, none in window | 2,953–2,998 / 937–941 / 328–337 | | 11,337–11,700 / 16,186–16,846 / 16,013–19,214 |

- **Durable: one file wins**, by 3 times over 4 files and 9.5 times over 16,
  as on Turso (2,323 against 633). Flushes on separate files queue
  device-wide: a group commit takes 14 ms with 4 files and about 50 ms with 16.
- **Buffered: one file does not win.** It reaches 64 to 78% of 4 or 16 files
  at every checkpoint setting, including none. Without flushes, the limit is one
  writer thread doing all SQL and WAL page writes, about 1 ms for a group of 14.
  Several files spread that work over several threads.

### Synchronous switching on one connection

One connection, one-row commits of 256 bytes, `fullfsync=1`; three runs agree.

| Measured | Result |
| --- | --- |
| `synchronous` set between transactions, NORMAL ↔ FULL | Reads back 1, 2, 1, 2, 1 as set |
| Set inside `BEGIN IMMEDIATE` or a `SAVEPOINT` | Error "Safety level may not be changed inside a transaction"; value unchanged |
| `fullfsync=0` inside a transaction | Accepted and reads back 0; the pager is only updated outside a transaction |
| Commits alternating NORMAL / FULL, p50 | 43–68 µs / 2.95–3.03 ms |
| Blocks of NORMAL / FULL / FULL with plain `fsync`, p50 | 8–17 µs / 3.0 ms / 35–56 µs |
| FULL commit after 0 / 1 / 10 / 100 / 1,000 NORMAL commits, p50 | 6.0 / 2.9–3.8 / 2.7–3.5 / 3.0–3.2 / 4.1–4.2 ms |
| Unflushed WAL before that FULL commit | 4 / 12 / 40 / 458 / 4,562 KiB |
| First NORMAL commit after a WAL reset, p50 | 3.1–3.2 ms; later NORMAL commits 7–31 µs |

What the measurement shows: the level switches per transaction on one
connection. NORMAL commits issue no device flush (faster than even a plain
`fsync`), and each FULL commit issues one `F_FULLFSYNC`, whose cost rises
slightly with the unflushed WAL before it. The first commit after a WAL reset
flushes once even at NORMAL (6 ms is two flushes at FULL). No device flushes
were counted: `fs_usage` and DTrace need root, and a counting VFS needs unsafe
code.

Taken from SQLite 3.53.2's source and documented WAL behaviour, not measured:
`sqlite3WalFrames` syncs the WAL only on a commit when `synchronous>=FULL`
(`sqlite3PagerSetFlags`), and the sync covers the whole file, so every earlier
NORMAL frame in it. Recovery replays frames while their cumulative checksums
hold and stops at the last valid commit frame, so a durable FULL commit makes
all earlier commits in the WAL recoverable. Checkpoints sync the WAL first, and
a reset syncs the new header. Power loss was not tested.

Implications for slice 6: switch the level only between transactions, set
`fullfsync` once at open and never inside a transaction, and treat a FULL commit
or a completed checkpoint as advancing the durable watermark.
