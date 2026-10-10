# Storage engine, durability and write concurrency

How both runtime paths store, commit and flush, and what a caller uses to feed
the writer. Measurements are in [scaling limits](scaling-limits.md).

## Engine

`rocky-storage`, `rocky-secure` and `rocky-sdk` depend on `rusqlite =0.40.2`
with `bundled`; the lockfile's `libsqlite3-sys` 0.38.2 compiles SQLite 3.53.2.
No system SQLite is used. Changing either pin means rerunning the
[scale evidence](scaling-limits.md).

The private adapter `crates/rocky-storage/src/sqlite.rs` is synchronous behind
`async` signatures. It reads each result fully and resets cached statements, so
no statement or cursor outlives a call or holds a read snapshot open. Before
admitting writes it verifies `journal_mode=wal`, `synchronous` (1 for
`Buffered`, 2 for `Full`), `fullfsync=1`, `checkpoint_fullfsync=1`,
`wal_autocheckpoint=0` and a 4 MiB `journal_size_limit`.

Initialization creates the schema in one `BEGIN IMMEDIATE` transaction. Only
`SQLITE_BUSY`/`SQLITE_LOCKED` are retried: ten attempts with delays doubling
from 2 to 64 ms, 318 ms in total, then `RetryExhausted`. A single in-process
writer never meets them; the bound covers a file briefly held by another
process. SQLite ends a transaction itself after some errors, such as a full
disk, so the adapter checks `is_autocommit()` before rolling back. A persisted file must carry the current Rocky application-id tag
(`0x524b5901`); a read-only check refuses any other tag before lock creation
or initialization writes.

A second runtime opening the same file fails with `Incompatible` after about
100 ms of lock retries. The lock is the path with
`.lock`.

## File layout

`RuntimeOptions::files`, 1 by default, is fixed at first install. An automatic
layout uses `min(files, domains)` files, at most 16: file 0 is the path given to
`Runtime::open`, file `k` adds `-k` to the stem (`laptop.db`, `laptop-1.db`),
and domain `i` in sorted scope order lives in file `i % used`. Each file records
the binding `rocky.layout.v1`, the requested and used counts and its index.
Install writes file 0 last, so its record marks the set installed and an
interrupted install resumes. Another count, or a missing, extra or foreign file,
is refused with `Incompatible`. [Explicit placement](wide-schemas.md#explicit-placement)
records assignments in a catalog instead.

Each file has its own writer thread, write connection, read-only connections,
WAL, lock and flush. No transaction spans files and there is no `ATTACH`.

## Durability and exposure

**Invariant: nothing leaves the device above its durable watermark.** A request
whose answer can reach a peer, the coordinator, an effect provider or a
recovery export is answered only after a device flush covers everything its
domain (trusted path) or replica (encrypted path) has committed.

`Durability::Buffered`, the default, commits groups at `synchronous=NORMAL`.
`Durability::Full` flushes every group. A flush is a commit at `FULL` with
`fullfsync`; with no group to carry it, the writer commits a transaction that
only increments `PRAGMA user_version`, which Rocky reserves for this. The
writer flushes once at open.

Per domain the writer tracks unflushed commits and the last author sequence a
flush covered, comparing `total_changes()` around each step: a step that
changed no row leaves its domain clean, and an unreadable counter counts as a
change. Schema statements do not move the counter; no step commits only DDL
because runtime DDL runs at open, before the writer starts. An exposing
request of a dirty domain waits, and one flush serves every waiting request; a
group holding an exposing write commits at `FULL` itself.

| Trusted request | Class |
| --- | --- |
| `mutate`, process pending, record a peer acknowledgment, strong bind/store/verify/reject/apply/acknowledge | Buffered write |
| `receive`, `receive_page`, strong publish and prepare | Exposing write |
| `flush`; sync state, operation pages, missing operations; command lookup, status, attempts, results, publications and pages | Exposing read |
| `lookup_mutation`, `lookup_commit`, `query`, `read`, progress, reconciliation's own reads | Local read |

`MutationReceipt::receipt` returns a `CommittedMutation` with its
`CommitClass`; `flushed()` waits for a flush and forces one. `Replica::flush`
waits until a flush covers the domain. Forwarding the envelope from
`committed()` to a peer bypasses the watermark; use the replication APIs.

`EncryptedReplica` verifies WAL, `synchronous=NORMAL`, `fullfsync` and
`checkpoint_fullfsync` at open (`Error::Durability` otherwise) and counts commits against a
`durable` watermark; a flush is one `sync_all` of the `-wal` file. `author`,
`receive` and `receive_all` flush before answering; `author_buffered` returns a
`Buffered<Hash>` without the frame, and `receive_buffered` must not be
acknowledged before `flush` or `durable_commit` covers it. Calls that hand data
out flush first: `transfer_page`, `acknowledge`, `receipt_status`, the
`*_encrypted` command calls, result acknowledgments, effect begin/finish/dispatch
and recovery export and retention. A reopened replica treats existing commits
as unflushed.

After power loss:

- Local work never exposed may disappear; counters, projection and history
  return to the last flush together, and retrying a handle commits it once,
  reusing the sequence number no peer saw.
- A received but unacknowledged operation, a recorded peer acknowledgment and
  buffered strong steps may disappear; their senders resend them.
- Anything exposed survives. `watermark_tests` simulate this by restoring files
  copied at the last flush.

## Groups

Each file's writer commits the writing steps already queued across its domains
as one transaction, one savepoint per step, at most one step per domain per
pass in rotating order. It answers after `COMMIT`; if that fails, every domain
in the group rebuilds memory from storage and every waiter gets the error. A
group holds at most `RuntimeOptions::group_commit` steps (128) and 4 MiB and
never waits for more work, so one client pays one commit per write. 128 lets
16 domains with 8 waiting writers share one commit.

A [SQL session](sql-sessions.md) transaction leases the writer: no group commits
in its file until it ends, at most its timeout later. `WriterStats::leases`,
`lease_micros` and `lease_max_micros` count the time held.

## Submit and queue depth

A domain's queue holds 32 commands and 8 MiB. When it is full, `mutate`,
`mutate_with_handle` and `mutate_with_precondition` return `Backpressure`;
`submit` and `submit_with_handle` wait for room, and dropping the future first
queues nothing. Closed-loop writers should call `submit`.

`Replica::queue_depth()` reports what waits and the bounds.
`Replica::writer_stats()` counts groups, steps, flushes, time in steps, in
`COMMIT`, in requests outside groups and in WAL checkpoints (passive split into
WAL flush, copy and file flush; truncating counted apart) and the WAL size.
Uncounted time was idle. `RuntimeOptions::instrument` adds per-step subphase
timing; it is off by default because it costs measurably at high rates.

## Incoming pages

`Replica::receive(bytes)` is one exposing write. `Replica::receive_page(records)`
cuts a page into batches of at most 32 records and one frame of bytes; each
batch is one step, one commit and one flush. Each record has its own savepoint,
so a refused record (undecodable, foreign, conflicting, unconfigured author)
fails alone. One application pass follows in the same commit: a record with a
missing dependency stays pending and is acknowledged `applied: false`, one
rejected on application reports `Rejected`. Results are per record in input
order; an outer error means the named batch did not commit and resending is
safe. `process_pending` applies at most 32 operations per pass. The flush
precedes the acknowledgments because a sender drops its outbox entry on them.

## Strong commands

A command takes three commits on its executor: the published request, the
prepared package, and the result stored, verified and applied together
([design rationale](strong-publication.md#design-rationale)). With buffered
durability the result is flushed only when exposed. A package names the
executor's checkpoint base and covers the operations after it, so a request is
refused with `Quota` once more than 32,768 operations follow that base. A newer
checkpoint lowers the count
([Decision 0042](../decisions/0042-bounded-strong-cut.md)). `execute_pending`
reads only unreleased requests without an applied result.

## WAL checkpoints

The writer runs a passive checkpoint between groups once the WAL holds
`checkpoint_frames` frames (16,384, 64 MiB) and a truncating one at twice that,
waiting at most 50 ms for readers; closing the file truncates the WAL. Nothing
else runs on the file meanwhile. The writer issues the passive checkpoint's
two flushes itself so their time is measured apart from the copy. The stall
grows with the file, not the log; a smaller bound makes it more frequent, not
shorter. A file under sustained load that needs a lower tail should raise the
bound; the default stays at 64 MiB so a lightly used file does not keep a large
log for hours.

## Encrypted path

`EncryptedReplica` keeps one mutex around its connection. Callers enqueue
requests and one leads a group of up to 64
([permissions and sync](permissions-and-sync.md#transactions-and-recovery)). The
leader releases the mutex before the device flush, answers the exposed results
and retakes it only for maintenance; it also releases it between groups. A
caller whose requests another leader took waits for that answer. A flush covers
the commit count read under the mutex, so it never claims an uncommitted write.

A group applies at most 64 dependency-ready frames in its own transaction. When
more are ready, as when a catch-up delivers what earlier frames waited for, the
leader applies them in further transactions of at most 64 and releases the
mutex between them, so a writer that asked meanwhile goes first; the receivers
are answered once the frames settled. Key installation, learned revocations,
checkpoint import and open apply what they unblocked the same way. Only a log's
first pending frame can be ready, since each later frame names it as its
backlink, so a pass examines one frame per waiting log rather than every
waiting frame. Interrupted between two transactions, the work resumes at the
next receipt (including a duplicate retry) or open, and no frame applies twice: its status changes in the
transaction that applies it.

## Design rationale

- **SQLite, not Turso MVCC.** Turso 0.8.1 allowed several durable writers per
  file but made every statement and checkpoint linear in the file's table count
  (a 1.6 s blocking checkpoint at 2,000 tables) and held 260 to 800 MiB. SQLite
  is flat in table count; its single writer is recovered by cross-domain group
  commit ([engine comparison](../../benchmarks/engine-scale/RESULTS.md)).
- **Flush before exposure, not re-adoption.** An author that loses an
  acknowledged, already-sent operation reuses its sequence with other bytes;
  peers keep the old bytes and the log forks irreparably. Re-adopting one's own
  history would need a protocol; flushing before exposure needs none.
- **No per-domain durability classes.** Classes by content (custody,
  published, local, received) were superseded: one buffered default plus the
  watermark gives the same safety with one setting.
- **One file by default.** Four files raised buffered writes but cut flushed
  writes by almost two thirds, and every exposure is a flush. The count is fixed
  at install, so the default must be safe to keep.
- **No separate checkpoint connection.** Measured and removed: the writer's
  commit flushes queued behind the checkpoint's device flushes and p99 rose.
