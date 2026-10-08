# Phase 2 — SQLite storage engine

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented 2026-10-04, with two gates missed and the file layout and
default durability changed by user decision; see the [outcome](#outcome-2026-10-04)
and [decision 0008](../../decisions/0008-buffered-default-and-file-layout.md).
Proposed 2026-10-04, after [phase 1](01-baseline-and-hardening.md); the brief
text below is unchanged.
Context: [engine comparison at target size](../../../benchmarks/engine-scale/RESULTS.md),
[engine at target size](../../runtime/engine-scale.md),
[scaling limits](../../runtime/scaling-limits.md#writes-flushes-are-the-cost),
[decision 0006](../../decisions/0006-durability-classes.md) and the
[earlier engine decision](../../../benchmarks/engine-comparison/DECISION.md).

The pinned engine (Turso 0.8.1, MVCC) makes every statement and checkpoint
proportional to the tables in a file and holds 260 to 800 MiB at 2,000 tables.
SQLite in WAL mode is flat to 16,000 tables and stays at 47 to 57 MiB, but allows
one writer per file: 295 durable commits/s against Turso's 1,187 with eight
writers. This phase moves both runtime paths to SQLite and recovers that
throughput in the runtime instead of the engine. Later phases build on it, so it
comes before stored state.

No Rocky code has run on SQLite. Every figure above is engine-only evidence.

## One file

Superseded 2026-10-04 by [decision 0008](../../decisions/0008-buffered-default-and-file-layout.md):
the file count is configurable. The default was four files and went back to
one the same day, so this section describes the default again.

Rocky is a single-file database: one process keeps all of its domains in one
file. A single file performs better device-wide. The flush is a device-wide
operation, so separate files cannot share it: 16 files reached 633 durable
writes/s against 2,323 for 16 threads in one file. One file gives one writer,
one WAL and one flush for every domain's waiting work.

The cost is the table limit. SQLite is practical to about 16,000 physical tables
in a file, and each domain has its own copy of every table, so 2,000 tables fit
eight domains. More needs shared physical tables or tables created on first use,
which [phase 6](06-wide-schemas.md) decides. A second file is the last resort
for capacity, never a way to gain throughput.

## Three optimizations

1. **One writer per file with cross-domain group commit.** One thread owns the
   file's only write connection and commits the mutations waiting across all of
   its domains in one transaction, so they share one flush. This replaces
   Turso's engine-level group commit and `StorageThreads::PerDomain`.
2. **Buffered commits behind a durable watermark.** Commits in the `Local` and
   `Received` classes of decision 0006 use `synchronous=NORMAL`: no flush per
   commit, no corruption after power loss, but the newest commits can be lost.
   One flushed commit advances the watermark that gates publication.
3. **A single file.** All domains share the one writer and its flush, as stated
   under [one file](#one-file). The runtime does not split domains across files
   to gain speed.

## Slices

1. **Decision record.** Supersede the earlier engine decision. Fix the SQLite
   binding and pinned bundled version, how the synchronous binding meets the
   async runtime API, and the answer for existing Turso files: convert, or refuse
   to open with a clear error. List every Turso-only construct in use, at least
   `BEGIN CONCURRENT` and `journal_mode=mvcc`, with its replacement.
2. **Trusted path on SQLite.** Replace the engine behind `Runtime`/`Replica`
   without changing the supported API, SQL shapes or stored schema. Writes use
   `BEGIN IMMEDIATE`. The phase 1 whole-transaction retry stays as the handler
   for a busy file held by another process; inside one process the single writer
   must never see it. Reset cached statements after use.
3. **File writer and cross-domain groups.** Implement optimization 1. Each
   mutation runs in its own savepoint so a failing one rolls back alone and the
   rest of the group commits; this replaces rerunning a failed group one by one.
   Bound a group by count and bytes, add no delay under light load, and take
   mutations round-robin so one busy domain cannot starve another. Strong steps
   and incoming pages join the same writer. Observers and outgoing data are
   released only after the group's commit returns.
4. **Reads beside the writer.** Serve reads from separate read-only connections;
   WAL readers do not block the writer. A reported revision must be the one the
   snapshot was taken at. Bound read transaction time, because an open reader
   stops the WAL from being reset.
5. **WAL checkpoints.** Run checkpoints on the writer between groups with an
   explicit size policy instead of the automatic one, which raised write p99 to
   0.8 ms. Verify the WAL file stays bounded under sustained writes with readers.
6. **Buffered commits and watermark.** Implement optimization 2 for the trusted
   path. Verify first that `synchronous` can change between transactions on one
   connection and that a `FULL` commit makes all earlier `NORMAL` commits
   durable; decision 0006 left this open. Receipts state the class reached.
   `Full` stays the default, custody records never use the weaker class, and
   rule 4 of the decision is enforced at open.
7. **Confirm the single file.** Measure durable and buffered throughput on
   SQLite for 16 domains in one file against the same domains in 4 and 16 files.
   The 633 against 2,323 figure was measured on Turso; record whether one file
   still wins. If it does not, stop and ask before changing the layout. No
   `ATTACH` and no transaction across files in either case.
8. **Encrypted path.** Move `EncryptedReplica` to the same adapter. Its single
   mutex already matches one writer; give it group commit in authoring and
   incoming application. It keeps full durability only.
9. **Remove Turso and record.** Drop the dependency from `rocky-runtime`. Rerun
   every `scaling_probe` section and add a batched-writer case to
   `benchmarks/engine-scale`. Update the baseline, scaling limits and engine
   guides with before and after figures.

## Gate

- Every existing test passes on SQLite for both paths, including convergence,
  crash, interrupted-request and observation-race tests. The todo demo and the
  Kotlin consumer work.
- At 2,000 tables the wide probe's write and point read are within 2× of the
  20-table figures, no thread waits longer than the 318 ms retry bound, and
  resident memory is under 256 MiB.
- Eight clients on one file at full durability reach at least 1,200 writes/s, the
  patched Turso figure. Missing this by more than a small margin removes the
  reason for the switch: stop and ask.
- One client on one domain is no slower than on Turso.
- Sixteen domains in one file reach higher durable throughput than the same
  domains spread over several files.
- Killing the process inside a group, between a savepoint and the commit, and
  during a checkpoint leaves a file that opens with every handle either fully
  committed or absent.
- A buffered commit is never reported as flushed and is never offered to a peer,
  sealed or exported before the watermark covers it.
- A Turso file from before the switch converts or is refused; it never opens as
  empty.

## Out of scope

The state model, quotas, shared physical tables, encryption at rest, and
power-loss testing, which [phase 8](08-scale-qualification.md) covers where a
fault device is available.

## Outcome, 2026-10-04

Both runtime paths run on SQLite and Turso is removed. Figures are one release
run on the machine in [scaling limits](../../runtime/scaling-limits.md#after-the-sqlite-switch).

### User decisions

The engine measurement for slice 7 found that buffered commits in one file
reach only 64 to 78% of several files: 11,459 writes/s in one file, 17,817 in
four and 14,763 in sixteen; durable 2,813, 921 and 297
([results](../../../benchmarks/engine-scale/RESULTS.md#one-file-against-several)).
The user then decided on a multi-file layout, configurable and four files by
default, and on `synchronous=NORMAL` for every commit on both paths, provided
nothing that has left the device can be lost. The runtime flushes before any
exposure to meet that condition. Later the same day the user set the default
back to one file, because four files cost the flushed path about two thirds of
its throughput; several files stay available through `RuntimeOptions::files`.
[Decision 0008](../../decisions/0008-buffered-default-and-file-layout.md) records
both, the exposure rules and what they supersede here: the "One file" section,
optimization 3, slice 7's expectation, slice 6's `Full` default, slice 8's full
durability only and the one-file gate.

### Gate

| Gate | Result | Evidence |
| --- | --- | --- |
| Every existing test passes on SQLite, both paths | Met | `./scripts/check` passes: format, clippy and all workspace tests |
| The todo demo works | Starts; not exercised interactively | A fresh data directory starts with WAL files; a copy of a pre-switch directory is refused and left byte-identical |
| The Kotlin consumer works | Met | `./sdks/kotlin/check` passes |
| 2,000 tables: write within 2× of 20 tables | Met | 7,299 against 7,033 writes/s |
| 2,000 tables: point read within 2× | **Missed** | 82 ms against 0.79 ms median. Each new revision rebuilds every authorized read table in a fresh in-memory database; with `read_cache` the median is 29 µs but the first read after a write still takes about 90 ms |
| No thread waits longer than 318 ms | **Missed** | No busy or retry wait occurred, but with 8 observers at 2,000 tables writes waited 653 to 703 ms behind reads, which run on the writer thread |
| Resident memory under 256 MiB at 2,000 tables | Met | 92 MiB at most |
| 8 clients, one file, `Full`: at least 1,200 writes/s | Met, narrowly | 1,276 writes/s |
| One client, one domain, no slower than Turso | Met | 315 writes/s at `Full` against 257; 7,423 buffered |
| 16 domains in one file beat several files at durable throughput | True; changed by decision | 1,983 writes/s in one file, 680 in four, 373 in sixteen. No longer decides the layout |
| Kill inside a group, between savepoint and commit, during a checkpoint | Met for the trusted path | `crash_tests`; the encrypted path has group-kill tests but no kill during a checkpoint |
| A buffered commit is never reported flushed or exposed before the watermark | Met | `watermark_tests` and the encrypted `secure::tests::group` tests, with power loss simulated by restoring files copied at the last flush |
| A Turso file converts or is refused | Met: refused | `files_from_the_earlier_engine_are_refused_and_never_opened_as_empty` and the demo check above |

### Remaining limits

- Reads use a read-only connection but still run on the file's writer thread,
  between groups, because applied state lives in worker memory. A slow read
  delays that file's writes. [Phase 3](03-stored-state-and-reads.md) takes over
  both missed gates.
- With four files, `Full` throughput falls: 680 writes/s for 16 domains against
  1,846 in one file. Applications that need `Full` should use one file.
- The flushed encrypted path is several times slower per call than before the
  switch, and its reopen and snapshot reads are slower too
  ([encrypted baseline](../../runtime/encrypted-baseline.md#after-the-sqlite-switch)).
- An application that sends the envelope from `committed()` to a peer itself
  bypasses the watermark; this is documented, not enforced. The watermark is per
  domain and coarse: one exposure flushes the whole file.
- Encrypted flush placement on command, effect and recovery methods has no
  dedicated tests. A cancelled encrypted group leader leaves its followers with
  `RetryExhausted` and an unknown outcome; retrying by handle is safe. Flushes
  block an executor thread. Out-of-order delivery stays quadratic.
- Files from an intermediate single-file development build are refused with the
  "another file count" message. Rocky uses `PRAGMA user_version` as a flush
  counter.
- Power loss is simulated, not tested on a fault device
  ([phase 8](08-scale-qualification.md)).
