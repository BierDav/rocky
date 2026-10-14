# 9. Coordinated writes

Status: implemented; slices 1–5 and every completion gate verified (2026-10-14).
See [evidence](09-coordinated-writes-evidence.md) and the
[coordinated writes guide](../runtime/coordinated-writes.md).
Owner: `rocky-secure` outbox, bundles and admission; `rocky-sdk` coordinated
write API, sync worker and status; `sdks/kotlin`. Depends on
[phases 4](04-held-changes.md) and [8](08-coordinated-log.md). Follow the
[execution contract](README.md#agent-execution-contract). Shares decision record 0072.

## Inputs and objective

Read [secure sessions](../runtime/secure-sessions.md),
[SQL sessions](../runtime/sql-sessions.md) and
[shared coordination](../runtime/shared-coordination.md).

Applications need writes that stay consistent across concurrent writers:
sequential invoice numbers, a request decided once, a row updated in order.
After this phase a trusted node makes such a write as an ordinary bundle,
ordered by the [coordinated log](08-coordinated-log.md) and stored by every
in-sync trusted node before it becomes true. No split brain is possible.

## Settled

- **Requests are rows.** Members insert ordinary request rows under an
  insert-only grant and receive results by sync. Any trusted node may handle
  a request; the first committed entry wins and the others see it handled.
- **Propose on committed state.** A trusted node in the set, caught up to
  the commit index, runs the application's code on its local state in one
  local transaction. The resulting bundle, within the bundle limits, stays
  unsent; its hash is appended at `c+1`.
- **Store, then acknowledge.** The proposer sends the bundle peer to peer to
  every in-sync trusted node. Each stores it durably, unapplied, and
  acknowledges it to the coordinator.
- **Commit, then publish.** On commit every in-sync node applies the entry
  and may publish it to members. A bundle carries its log position and entry
  hash. Members only ever receive committed entries and do not verify the log.
- **Abort.** An aborted entry is discarded everywhere; the proposer voids its
  never-offered frames with a [void record](04-held-changes.md#settled) and
  rolls back its local effects.
- **Catch up first.** A node outside the set fetches the committed entries
  from trusted peers before it rejoins or proposes. Lamport stamps then order
  its writes after every committed one.
- **Explicit failure.** Without a reachable coordinator, or below the minimum
  set size, a coordinated write fails with a typed result; it is never queued
  as success. Ordinary writes are unaffected.
- **Trust is the guard.** Nothing prevents a trusted node from writing
  coordinated tables directly; the guide says so.

## Ordered slices

1. Proposing, peer-to-peer storing, acknowledging and applying on commit.
2. Abort with void records; restart recovery for every step.
3. Catch-up before rejoining; publication with position and hash.
4. SDK and Kotlin API, typed failures and status.
5. Guides: a coordinated-writes guide, secure sessions, SQL sessions.

## Completion gate

- Two trusted nodes issue invoices concurrently; numbers are sequential
  without gaps or duplicates on every device.
- A proposer crashing before or after storing, and a node crashing before
  acknowledging, never leave an entry true on one node and missing on another.
- A member never receives an aborted entry.
- A node that rejoins after missing entries proposes only after catching up.
