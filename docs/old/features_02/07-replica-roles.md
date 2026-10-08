# Phase 7 — Partial replicas and full replica nodes

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented 2026-10-05, with the limits in the
[outcome](#outcome-2026-10-05). The brief text up to it is unchanged.
Proposed after [phase 4](04-checkpoints-and-pruning.md).
Context: [two-replica sync](../../runtime/two-replica-sync.md),
[permissions and sync](../../runtime/permissions-and-sync.md),
[shared coordination](../../runtime/shared-coordination.md),
[recovery and effects](../../runtime/recovery-and-effects.md).

A full replica node does not add capacity: it runs the same runtime and is bound
by the same limits. After phases 3 and 4 it becomes useful as the place that holds
everything, so that devices can hold less.

## Roles

- **Full replica node:** holds every scope of its domains, retains history to the
  declared horizon, produces and serves checkpoints, holds recovery custody.
  It is not a coordinator voter and not automatically an executor.
- **Partial replica:** a device holding the scopes it is authorized for and has
  subscribed to, with a short retained window. It rebootstraps from a full node
  when it falls behind the horizon.

Write a decision record covering how a role is declared and authenticated, what a
partial replica may evict, and what "converged" means for a replica that holds a
subset.

## Slices

1. **Range reconciliation.** Trusted sync currently exchanges lists of known
   operation IDs, which cannot scale with history. Replace it with per-author
   frontiers plus bounded range requests, tolerating gaps left by pruning.
   The encrypted path already pages by hash cursor; give both the same guarantees.
2. **Checkpoint serving.** A full node serves verified checkpoints in bounded,
   resumable pages. The receiver verifies before switching and keeps its unsent
   local operations, as defined in phase 4.
3. **Scope subscription.** A partial replica declares the scopes it holds. The
   sender filters by authorization and subscription. Changing a subscription
   restarts a pass and never treats absence as deletion.
4. **Scope eviction.** A partial replica may drop a scope's rows and history only
   after a full node has acknowledged custody of every local operation in it.
   Eviction is a separate, explicit operation from deletion and from pruning.
5. **Custody-aware pruning.** Connect phase 4 pruning to full-node acknowledgments:
   a device prunes once the configured number of full nodes holds the data.
   Receipt alone is not custody; the acknowledgment must cover application.
6. **Read offload.** Optionally serve authorized reads from a full node for data a
   device does not hold. The result states its completeness and revision; it is
   never presented as local state.
7. **Catch-up and backpressure.** A replica far behind catches up in bounded pages
   without starving local writes on either side, and a slow peer cannot grow the
   sender's memory.
8. **Operations.** Starting a new full node from an existing one, replacing a lost
   one, and reporting lag, retained window and custody coverage.

## Gate

With one full node and two partial replicas over the native transport:

- a partial replica holding 5% of a 500,000-row domain converges on its subset and
  never stores rows outside it;
- a replica 10,000 operations behind catches up in bounded pages while both sides
  keep accepting local writes within the phase 5 latency budget;
- a replica behind the horizon rebootstraps from the full node and its offline
  edits are applied or explicitly rejected;
- eviction without custody acknowledgment is refused, and a scope evicted and
  later resubscribed converges to the same rows;
- killing the full node mid-transfer and mid-checkpoint-serve leaves every replica
  consistent, and a replacement full node reaches the same state;
- strong commands still require the coordinator; a full node alone confirms nothing.

## Out of scope

More coordinator groups, BFT, and relays that hold plaintext.

## Outcome, 2026-10-05

Design: [decision 0014](../../decisions/0014-replica-roles.md). Guide:
[full and partial replicas](../../runtime/replica-roles.md). Measurements:
[evidence](../../runtime/replica-roles-evidence.md).

| Slice | Encrypted path | Trusted path |
| --- | --- | --- |
| 1 Range reconciliation | Per-log frontiers, `range_page`, `NativeSession::catch_up`/`push`; session v3 | `SyncState::held`, at most 256 exceptions, `catch_up_after` with a cursor |
| 2 Checkpoint serving | Existing pages; `sync_scope` rebootstraps and resumes across sessions | Existing pages |
| 3 Scope subscription | `RoleConfig`, `subscribe`/`unsubscribe`; receipt refuses other scopes | Not applicable: a domain is whole; placement selects domains |
| 4 Scope eviction | `evict`, bounded and resumable, needs applied custody; own log guarded | Not applicable |
| 5 Custody-aware pruning | `RoleConfig::custody`, `PruneBlock::Custody` | `RuntimeConfig::full_nodes`, `RetentionPolicy::custody` |
| 6 Read offload | `read_remote`, `RemotePage` | Not applicable |
| 7 Catch-up and backpressure | Pull-based pages; the page flush runs outside the replica lock; sessions stop before their budget | Bounded pages with a cursor |
| 8 Operations | `replica_status`, `peer_scopes`, replacement from a full node | `RetentionStatus::peers` |

### Gate

Every gate item was met at 500,000 rows over native TCP except one bound. While
a replica caught up 10,000 operations, the full node's own buffered writes had
p99 120 ms. That is above the restated phase 5 limit of about 100 ms. The
partial replica's writes had p99 89 ms. See the
[evidence](../../runtime/replica-roles-evidence.md#gate).

### Limits

- The full node's write p99 during catch-up, above.
- Each encrypted frame carries its author's credential chain. With wide grants,
  pages hold few frames and catch-up slows to about 32 operations/s.
- The kill of the full node was simulated inside the process.
- The trusted path has no subscription, eviction or remote read.
- The encrypted frontier is recomputed per request; 256 logs per scope at most.
- The interactive desktop demo was not rerun.
