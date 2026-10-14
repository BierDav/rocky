# 8. Coordinated log

Status: proposed; not started.
Owner: `rocky-coordinator` machine, retention, service and client. Follow the
[execution contract](README.md#agent-execution-contract). Takes decision record 0072.

## Inputs and objective

Read [shared coordination](../runtime/shared-coordination.md),
[public coordination](../runtime/public-coordination.md) and
[coordinator retention](../runtime/coordinator-retention.md).

Today the coordinator confirms strong commands: `Acquire`, `Takeover`, `Head`,
`Confirm` and `Lookup` per scope, with a receipt per request until it is
retired. After this phase it orders one log per domain for the domain's
trusted nodes, decides when an entry is committed and which history is
canonical. It never holds entry data.

## Settled

- **Trusted nodes.** The keys the domain's control certificate lists as
  trusted. Only they call the coordinator; members never do.
- **State per domain.** The in-sync set with its epoch and each member's last
  acknowledged position; the log head; the commit index with a running hash;
  at most one pending entry (proposer, entry hash, position, deadline); the
  last request id per node, so a retried call returns its first outcome.
- **Append.** A node in the set proposes position `c+1` with an entry hash,
  on condition that the head is `c`. Any other head refuses it. One order per
  domain; per-key conditions may follow under
  [optimizations on demand](../optimizations_on_demand/README.md).
- **Commit.** An entry is committed once every node of the in-sync set has
  acknowledged it, and the set has at least the domain's minimum size
  (default 1). Below the minimum, appends are refused.
- **Abort.** An entry not acknowledged by every node before its deadline
  either commits without the nodes that missed it, which leave the set, or,
  if that would leave fewer than the minimum, is aborted.
- **Lazy membership.** A node joins when it connects with an acknowledged
  position equal to the commit index. It leaves only by missing an
  acknowledgment during a write. There are no heartbeats; an idle domain
  costs nothing.
- **Durability.** Appends, commits, aborts and membership changes are
  Raft-replicated. Acknowledgments are leader memory; after a leader change
  nodes acknowledge again.
- **Hash window.** Besides the running hash, the coordinator keeps the hashes
  of the last 1,024 committed positions, for [recovery](10-stuck-recovery.md).
  Older hashes are dropped.
- **Limits.** Per tenant and domain: trusted nodes, appends per minute and a
  capped acknowledgment deadline, besides the existing `Usage` budgets. A
  domain holds about 1–2 KiB plus at most 40 KiB of window.

## Ordered slices

1. Decision record 0072 (with [coordinated writes](09-coordinated-writes.md)):
   log, in-sync set, commit and abort.
2. Machine operations, Raft persistence, retries and the hash window.
3. Lazy membership, minimum set size, deadlines and limits.
4. Client API; guides: shared coordination, public coordination, retention.

## Completion gate

- Two nodes appending at the same head: one is assigned, the other refused.
- An entry commits only after every in-sync node acknowledged it; a node that
  misses the deadline leaves the set and the entry commits without it.
- With a minimum of 2 and one node left, appends are refused.
- A leader change during a pending entry loses no commit and commits nothing
  twice.
- A measured per-domain footprint and per-write message count are recorded.
