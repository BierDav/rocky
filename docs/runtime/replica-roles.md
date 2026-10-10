# Full and partial replicas

Measurements: [scaling limits](scaling-limits.md#measured-limits).
Format-3 [partitioned scopes](partitioned-scopes.md) derive one scope per tuple.
The calls below subscribe, acknowledge, checkpoint and evict each concrete
partition independently. Custody verifies the signed tuple and grant without its
key. `table_complete(table)` requires an all-partition read grant, the full role,
readable held keys and no incomplete registered partition; a narrowed session
also becomes partial. Eviction keeps permanent identity marks and rebuilds the
shared projection without removing another partition's rows.

A full replica node holds everything so devices can hold less. It runs the same
runtime under the same limits and confirms nothing: strong commands still need
the coordinator. This guide is for an application or transport author who
configures roles, syncs scopes and frees space on a device.

## Encrypted path

### Roles

```rust
device.set_role(RoleConfig {
    role: ReplicaRole::Partial,
    full_nodes: [full_node_key].into(),
    custody: 1,
}).await?;
device.subscribe(scope).await?;
```

`ReplicaRole::Full` is the default and the behavior before roles: every scope
the replica is authorized for and receives is held. A partial replica holds
only subscribed scopes; frames of other scopes are refused at receipt. Roles
are local: naming a full node trusts its key, and its acknowledgments count
only while its credentials grant whole-scope read.

| `Holding` | Meaning |
| --- | --- |
| `Held` | Subscribed, or any scope of a full replica |
| `Left` | `unsubscribe`d: rows stay, reads report `complete: false` |
| `Evicting` | An eviction is in progress; reads are unavailable |
| `Evicted` | Rows and history are gone; `subscribe` holds the scope again |

### Syncing a scope

The SDK exchanges selected group-member frontiers in bounded batches, retaining
one scope per table and partition. Equal applied frontiers and horizons skip
empty row transfers; existence logs and horizon acknowledgments still run.
A scope either side may not hold whole, including a partition the server has not
learned, is refused alone and skipped for that session; the rest of the batch
proceeds. [Decision 0044](../decisions/0044-group-frontier-exchange.md) defines
admission and limits, [decision 0046](../decisions/0046-per-scope-frontier-refusal.md)
refusal and the current wire format. [Phase 9](../features_06/09-group-sync-measurement.md)
provides the reproducible ERP comparison and its budgets.

`NativeSession` (label `rocky.native-session.v11`) pulls by frontier:

| Call | What it does |
| --- | --- |
| `catch_up(replica, scope, quota)` | Pages of at most 32 frames the replica lacks; acknowledges its frontier after each page |
| `sync_scope(replica, scope, quota)` | `catch_up`, rebootstrapping first if the replica is behind the peer's horizon |
| `push(replica, scope, quota)` | Sends what the peer lacks; the peer's frontier comes back as acknowledgment |
| `peer_frontier` / `peer_scopes` | The peer's frontier of a scope; the scopes it holds that this replica may hold |
| `read_remote(replica, scope, after, rows)` | A remote read, below |

The server side calls `serve_once` in a loop. `catch_up` and `push` stop with
`exhausted` when the session's byte budget runs low; continue on a new session,
where the frontiers resume the pass. A device typically pushes its own writes
to a full node and catches up from it, for each held scope. Without a session,
use `frontier`, `range_page` and `acknowledge_frontier`.

A frontier names, per author log, the next sequence not held and the next not
applied. Pages are built from it, so a pass costs the frames that are missing,
not the history. A requester whose prefix ends at or before a pruned part of a
log gets `behind` and no frames.

Any admitted peer may report frontiers, so the replica bounds what peers it
was not told to keep leave behind: at most 256 unconfigured peers hold
frontiers or horizon credentials, and the least recently served lose theirs
first. Full nodes and the peers named with `configure_peers` are never
dropped; a dropped frontier only delays pruning
([decision 0051](../decisions/0051-roster-free-admission.md)).

### Proving a partition

A whole-table profile grant does not reveal every list. In each native session,
a reader proves a named partition with a value derived from one of its retained
key epochs. The session binds the proof to both fresh handshake challenges;
replaying it in another session fails. The client supplies at most four epochs,
newest first; a peer accepts a retained losing sibling or previous generation.
A peer without any matching epoch refuses the member, and renewal or envelope
delivery supplies the missing keys.

A keyless custody peer checks the tuple and the grants instead. A custodian
asking a reader uses its custody grant and tuple. A reader asking a reader still
needs a key proof. Tuples can be guessed and former readers retain them: custody
has this weaker privacy boundary. Unpartitioned scopes keep their grant rule.

Frontier batches carry proofs automatically. Scoped client calls establish them
before their first request; later calls reuse session memory. `peer_scopes`
returns only unpartitioned scopes and partitions already proved; use
`prove_scopes(replica, scopes)` before listing known partitions. An unproved
partition receives `Refused(scope)` in a batch, or `ciphertext custody denied` and
session termination for a raw scoped request, exactly like an unknown scope.
[Decision 0052](../decisions/0052-holding-proofs.md) defines derivation and bounds.

### Custody and pruning

With `custody > 0`, `prune` also waits until that many configured full nodes
have applied each operation, and reports `PruneBlock::Custody` otherwise. A
custody-only node, without the scope key, holds frames but never applies them,
so it never counts.

### Eviction

`evict(scope)` drops a held scope's rows and history. It is refused on a full
replica, for a domain with required recovery custodians, and until
`max(1, custody)` full nodes have applied every operation this node authored in
the scope. It is neither deletion nor pruning: the rows remain at the full
nodes. An interrupted eviction continues on the next call. `subscribe` brings
the scope back; sync it before authoring in it again, which is refused until
this node's own log there is restored.

### Remote reads

`read_remote` reads a scope the device does not hold from a replica that does.
The requester needs whole-scope read and the scope key. A `RemotePage` names its
source, revision, applied frontier and whether the source was complete. It is
never stored or observed locally and a local snapshot of that scope stays
incomplete.

### Operations

- **Status.** `replica_status()` reports per scope: holding, live rows, covered
  and uncovered retained operations, pruned logs, local operations, how many of
  them each full node has applied, and lag against every peer that reported a
  frontier.
- **New full node.** Open a replica with keys and grants for every scope,
  `prove_scopes` for the partitions whose envelopes it received, list
  `peer_scopes` of an existing full node and `sync_scope` each one.
- **Replacing a lost full node.** The same, from another full node, or from the
  lost node's file under its key. Partial replicas hold only their subsets.

## Always-on custody peer

The packaged `rocky-peer` launcher enrolls with only `Custody` grants and no
reader keys. See [deployment, enrollment and replacement](always-on-peer.md).
It stores and serves ciphertext without applying rows and does not satisfy
applied full-node custody requirements.

## Trusted path

A trusted domain is all or nothing; placement selects which domains a process
opens, so there is no subscription, eviction or remote read.

`SyncState` carries `held`, the contiguous prefix per author, and at most 256
operations held above it in `received`. `catch_up_after(peer, state, after)`
returns up to 32 operations the peer lacks with a cursor in `next`; start each
pass without one. Naming a delivery target records its held and applied
frontiers as acknowledgment and clears the outgoing work they cover.

`RuntimeConfig::full_nodes` names delivery targets that are full nodes. With
`RetentionPolicy::custody` set, only their applied frontiers move the horizon,
and the other targets rebootstrap from a full node when they fall behind.
`RetentionStatus::peers` reports what each target last reported.

## Limits

- Every encrypted frame carries the author's credential chain. With 20 scope
  grants a frame is tens of kilobytes, so a page holds few frames; see the
  [measurements](scaling-limits.md#measured-limits).
- The encrypted frontier is recomputed from retained operations per request and
  is limited to 256 logs per scope.
- A gap present at both ends is resent once per pass.
- Ordinary catch-up requests a checkpoint only when history is pruned. SDK
  [join and epoch refresh](sdk-bootstrapping.md#join) request one first. Choosing
  checkpoints dynamically for cheaper ordinary catch-up remains deferred; see [checkpoint instead of history](../optimizations_on_demand/checkpoint-instead-of-history.md).
- Strong command state is not evicted.

## Design rationale

- **Roles are local configuration.** A peer's claim to be a full node grants
  nothing; only this replica's configuration and the peer's verified grants
  count, and only whole-scope read lets a peer's applied frontier count.
- **Absence is never deletion.** Unsubscribing or evicting leaves rows at the
  full nodes; a partial replica reports such scopes incomplete.
- **The own log tip survives eviction**, so a resubscribed scope never reuses a
  sequence number before sync or rebootstrap restores the log.
- **Frontiers, not ID sets.** Exchanging every retained ID or resending every
  retained frame cost history-sized work per pass; a held and an applied prefix
  per log make a pass cost only the missing frames, and the reported frontier
  doubles as acknowledgment for pruning, custody and outbox clearing.
- **Holding everything confers no execution or confirmation right**; strong
  commands still need the coordinator.
