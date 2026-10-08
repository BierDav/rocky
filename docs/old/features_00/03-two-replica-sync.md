# Phase 3: two-replica synchronization

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented. See [two-replica sync](../../runtime/two-replica-sync.md) for the runtime
port, deterministic harness, recovery strategy and native verification. This phase
completes the first usable MVP; the requirements below remain its acceptance contract.

## Goal and required reading

Show Laptop and Phone side by side. Each has its own database, counters,
projection, outbox and observers. Pausing their connection must not stop local
edits; resuming must reconcile retained logical operations and converge.

Read [MVP scope](README.md), [root instructions](../../../AGENTS.md),
[module boundaries](../../modules.md), [phase 2](02-local-replica-and-ui.md) and the
complete [operation contract](../../protocol/operation-contract.md). Verify two runtime instances
can open/reopen separate files before wiring transport.

Architecture references: [local mutations](../../proposals/architecture-proposal-v3.md#local-mergeable-mutations),
[transport/scopes](../../proposals/architecture-proposal-v3.md#12-p2p-transport-scopes-and-encryption)
and [progress evidence](../../proposals/architecture-proposal-v3.md#13-sdks-sql-reads-and-observation).
The MVP transport transfers unsigned plaintext under the development trust model.

## Ownership and composition

- `rocky-runtime` owns durable receipt, incoming validation, readiness, replay,
  reconciliation and peer acknowledgment state.
- `todo-app` remains the operation meaning/reducer provider; sync does not invent
  a second implementation of its merge rules.
- `todo-desktop` constructs Laptop and Phone runtimes and a controllable in-process
  transport. Keep the transport contract reusable without a Dioxus dependency.
- Place the simulated transport adapter at the composition/test boundary. It can
  implement a runtime-owned transport port; keep that interface narrow.

Use distinct database files and the fixed phase-1 node IDs under `--data-dir`.
Do not share authoritative row state or synchronize by copying SQL files/rows.
Only encoded original logical operations cross the replica boundary.

## Durable incoming processing

1. Validate frame bounds, canonical shape, scope, fixture caller/author, supported
   versions and action policy. Same ID/different bytes is an identity conflict.
2. In a short transaction, store canonical operation bytes and receipt state,
   update maximum observed Lamport time, and identify ready dependencies.
3. Apply ready operations with the app reducer. Persist merge metadata, projection,
   applied frontier, processing status and observer revision atomically.
4. Commit, then publish received/applied acknowledgments and observer wakeups.

Persist dependency-pending operations without making incomplete rows visible.
Receipt and later application may use separate transactions when dependencies
are missing; each acknowledgment must describe the state that actually committed.
Resume ready-pending processing on startup and when dependencies arrive.

Ready work can be processed in bounded batches. A batch rollback cannot leave
some rows applied without their processing/revision state. Invalid operations
remain rejected; do not advance an applied prefix over a missing/rejected item.
Remote processing must never allocate a local operation ID or echo it as a newly
authored operation. Forwarding original retained bytes is a different action.

## Reconnection and acknowledgments

Reconcile durable known operation IDs on both sides after connection/restart.
For this small retained-history demo, exact ID sets and bounded transfer pages
are acceptable. Include received-but-pending IDs, and separately report applied
frontiers. This choice is intentionally not a production large-history protocol.

Send missing canonical records, including dependencies from either author. Retry
unacknowledged outgoing work after interruption. Do not mark work delivered when
it is merely enqueued into the channel. A missing/dropped acknowledgment permits
duplicate transmission and must not produce duplicate application.

Retain operation history after acknowledgments; do not implement compaction.
Persist named-peer acknowledgment state when using it to clear outgoing work.
Reconciliation must recover missed work even if an outbox entry is already cleared
or the transport's entire in-memory queue is lost.

Do not use a single maximum operation/change ID as a completeness cursor.
One author's receipt does not prove another author's progress. Report the applied
frontier from the operation contract and only the named peer's acknowledgment.

## Controllable transport and UI

Support pause/resume without closing the runtimes. The harness must also allow
manual delivery, delay, duplicates, reordering and dropped messages/acknowledgments.
Use bounded queues; queue saturation causes backpressure/retry, never data loss
from the durable operation log. Clear error reporting must distinguish transport
failure from rejected data.

The UI now shows both editable lists with the same CRUD controls. Add a connection
toggle and per-peer pending/received/applied indicators. Test-only controls may
remain in the deterministic harness; the main demo needs only pause/resume and
an understandable pending indicator, not every fault-injection detail.

The launch command remains:

```sh
cargo run -p todo-desktop --locked -- --data-dir local/todo-demo
```

## Automated acceptance checks

- Edits on either node become committed observer snapshots on the other.
- Concurrent different-field edits preserve both; same-field conflicts select
  the specified winner; delete/edit races retain the tombstone.
- Duplicate, delayed and reordered operation/ack deliveries converge, including
  internal merge state and applied frontiers once all accepted history arrives.
- A setter or author successor arriving first survives restart as pending, then
  applies after the missing dependency arrives.
- A receiver restart after durable receipt but before acknowledgment/application
  preserves bytes and produces correct eventual acknowledgment/projection.
- A sender restart with outgoing work and a lost transport queue loses no edit.
- Clearing acknowledged outbox work does not prevent history reconciliation.
- Malformed frames, foreign scope, denied writes and ID conflicts have no accepted
  projection effect and cannot advance applied acknowledgment state.
- A remote apply adds no newly authored operation; no unbounded echo loop occurs.
- Bounded queues and slow UI observers eventually drain under resumed delivery.

Use explicit delivery steps/barriers and assertions rather than arbitrary sleeps.
Run relevant package tests and `./scripts/check`.

## Manual completion scenario

1. Start a fresh demo directory and create a todo on Laptop; see it on Phone.
2. Pause connection. Rename it on Laptop and complete it on Phone; both respond
   locally, with outgoing work pending.
3. Resume; both lists show the new title and completed state.
4. Pause again, make competing title edits and an independent create/delete.
5. Exit and relaunch the same directory; the two files preserve their edits.
6. Reconnect; both views converge, and deleted identities stay deleted.

Document the observed run and limitations: two instances in one process are not
native networking, encrypted replication, phone background execution or recovery
from lost storage. Fixed in-process identities are not authentication.

## Handoff

Record the transport port, receipt/apply distinction and reconciliation strategy.
Phase 4 needs exact applied frontiers, canonical replay at a requested cut and
isolated app-node lifecycles. Add cut replay in phase 4 if not needed earlier.
Keep ordinary todo mutations independent of coordinator reachability.

Next: [strong snapshot publication](04-strong-snapshot-publication.md).
