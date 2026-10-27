# 1b. See the reactor

Status: blocked at prerequisite review (2026-10-27); [maintenance ordering evidence](01b-see-the-reactor-evidence.md).
Owner: `rocky-sdk` (database reactor, connection
serving, write probe), `rocky-secure` (SQL session and commit path).
Depends on [phase 1](01-see-the-wait.md). Effort: three to five days.
Follow the [execution contract](README.md#agent-execution-contract).

## Objective

Phase 1 attributed a write's time to replica lock wait and hold. In most
scenarios that accounts for little of it:

| Scenario | write p95 | lock wait p95 | lock hold p95 |
| --- | ---: | ---: | ---: |
| Pixel L0 Buffered (idle) | 359.850 ms | 0.009 ms | 25.841 ms |
| Pixel L1 Buffered | 509.377 ms | 0.009 ms | 31.028 ms |
| Desktop L4 Buffered | 317.582 ms | 7.026 ms | 19.991 ms |
| Desktop L3 Buffered, max | 132.867 s | 0.032 ms | 81.663 ms |

Desktop L1 Buffered hold p50 rises from 3.2 ms (L0) to 64.1 ms with no
lock wait. Source: [phase 1 measurements](01-see-the-wait-measurements-evidence.md).

The likely cause is in the code's shape. `Database` serves every
connection, link, receive and maintenance task with `spawn_local` on one
`LocalSet` thread. Holders run synchronous SQLite inline on that thread.
While one runs, an interactive call isn't polled at all. It can't even
start waiting for the lock, so its measured lock wait stays near zero.
While the write holds the lock and awaits (crypto on `spawn_blocking`),
other local tasks take the thread before the write resumes, which inflates
its own hold.

This phase measures that directly. It also removes the interactive
path's own detours. After it, every millisecond of a probe write is
attributed to a named site.

## Settled

- **Reactor poll timing.** Under the existing `qualification` feature,
  each task the database reactor spawns is wrapped with a name, chosen at
  the spawn site. The wrapper records each poll that takes longer than
  1 ms, with the task name and duration. Each SQL call records two more
  values. Its queue delay runs from the command being sent until `serve`
  first polls. Its in-call time runs from that first poll until the reply
  is sent. No new dependency, and nothing in builds without the feature.
- **Unattributed time.** The probe reports `write_reactor_delay`, which
  sums the queue delays of the write's calls. It also reports
  `write_unattributed`, the per-sample remainder of `write` minus lock
  wait, lock hold, flush wait and reactor delay. It adds a table of the
  longest polls by task, in the same format as the holder tables.
- **Status checks are interactive.** `SecureSession::status()` runs per
  statement on the SQL path and currently acquires as Background. Called
  on behalf of an SQL connection, it acquires as Interactive. Its checks
  are unchanged.
- **Flushed commits return after the flush.** `SecureSql::finish` stops
  re-acquiring the replica for `maintain` and `checkpoint_if_due` before
  returning. It queues the scopes into `deferred_maintenance`, as
  Buffered already does, and the maintenance driver runs them after the
  flush. Verify first that the driver orders maintenance after the
  durable watermark. If it doesn't, stop and report.
- **Pure work leaves the reactor.** CPU-bound work that touches no SQLite
  connection runs on the existing blocking pool, as
  `replica/crypto.rs` already does. Examples are frame decoding,
  decryption and verification on the receive path. Move only sites that
  the poll table shows above 1 ms. SQLite work stays where it is; phase 2
  bounds it.
- **No change** to the `LocalSet` structure, admission, durability,
  ordering, formats or SQL statements.

## Ordered slices

1. Poll timing, queue delay and the probe's new columns. Test: a
   deliberately blocking local task shows up in the poll table under its
   name, and a probe call queued behind it reports that delay.
2. Rerun desktop L0–L4 in both durability modes, and Pixel where
   available. Record the attribution table per scenario. If the reactor
   hypothesis is refuted (unattributed time stays dominant with no long
   polls), stop and report the evidence before slices 3–5.
3. Interactive `status()`. Test: its acquisitions on the SQL path record
   the Interactive class.
4. Flushed commit without inline maintenance. Tests: maintenance for a
   Flushed commit still runs after its flush, and a fault before it runs
   leaves the work queued.
5. Move pure work above 1 ms to the blocking pool, then rerun slice 2's
   scenarios.

## Completion gate

- In every measured L0–L4 run, `write_unattributed` p95 is at most 1 ms,
  or the remaining gap is explained in evidence.
- Each poll above the steady-state hold target (desktop 10 ms, phone
  25 ms) is attributed to a named task and listed. Phase 2 bounds the
  SQLite ones. Transition ones go to [phase 7](07-transitions.md).
- `./scripts/check`, the Kotlin check and the shopping app check pass.

## Not in scope

Bounding SQLite holders (phase 2), priority (phase 3), multithreading the
reactor and a second writer connection.
