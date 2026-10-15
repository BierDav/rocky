# Coordinated writes

Writes that must not race, such as sequential invoice numbers or a request
decided once, are ordinary bundles that the
[coordinated log](coordinated-log.md) of the tip they are written to orders. A coordinated write becomes
true only once every trusted node of the in-sync set stored it durably;
nothing of it leaves a node before then, and an aborted one is voided. No
split brain is possible: a node outside the set catches up before it writes
again. [Decision 0072](../decisions/0072-coordinated-log-and-writes.md)
records why.

## Roles

| Role | What it does |
| --- | --- |
| Trusted node | A device whose identity the domain's control certificate lists. It composes the logs and, in the log of the tip it follows, proposes, stores others' entries and acknowledges them |
| Member | Any other device. It receives committed entries by ordinary sync and never calls the coordinator |
| Coordinator | Orders entries and decides commit or abort. It holds metadata only: no frame, row or application rule |

Trust is the guard. Nothing prevents a trusted node from writing coordinated
tables directly with an ordinary commit; applications that need every write
of a table coordinated keep that discipline themselves.

## One write

1. **Slot.** `EncryptedReplica::coordinated_slot(followed_log(domain))`
   decides what this node stores, catches up from trusted peers, joins the
   in-sync set of the followed tip's log and returns the commit index `c`
   the write is computed on. Any other tip's log is refused `Unfollowed`.
2. **Stage.** The application runs its SQL transaction on that committed
   state; `SecureSql::commit_coordinated(handle, slot)` commits it as one
   ordinary bundle in one local transaction, flushed. Its frames carry the
   log position `c+1` in their bundle membership and stay unsent: range,
   transfer, push, checkpoint and recovery paths skip or refuse them.
3. **Append.** `propose` appends the entry hash at `c+1`. The hash
   (`rocky.coordinated-entry.v2`) commits to the tip's log, the position and
   the bundle's frame ids, so it is not inside
   the frames it covers; a node holding every member recomputes it.
4. **Store, then acknowledge.** The proposer sends the entry, with the void
   records its frames continue after, to every other in-sync node. Each
   stores it durably and unapplied, then acknowledges it itself; the
   proposer acknowledges last. A node stores only entries of the tip it
   follows, so a branch's writes never reach other tips' trusted nodes. While the entry remains pending, the proposer
   retries storage with bounded backoff, so a replaced live link can recover
   before the existing deadline.
5. **Commit, then publish.** Once every in-sync node acknowledged, the
   coordinator commits it. Each node then applies it in log order and may
   serve its frames to members. Members only ever receive committed entries
   and do not verify the log.

Another node appending at the same head loses with `Conflict`: its staged
bundle is voided and rolled back, and it retries on a new slot after
applying the winner. Lamport stamps order every later write after each
committed entry the node applied.

## Failures

A failed write is never queued as success. Every failure but `Undecided`
has rolled the local effects back; ordinary writes are unaffected.

| `CoordinatedError` | JSON code | Meaning |
| --- | --- | --- |
| `Unavailable` | `coordinator_unavailable` | The coordinator was unreachable before assignment; the bundle was voided |
| `BelowMinimum` | `below_minimum` | The in-sync set is smaller than the domain's minimum |
| `Conflict` | `conflict` | Another entry is pending or took the position |
| `Aborted` | `aborted` | The deadline passed without enough acknowledgments; voided everywhere |
| `Undecided` | `undecided` | The coordinator was lost after assignment; the effects stay unsent until resolution decides |
| `Behind` | `behind` | Committed entries are missing and no trusted peer supplied them yet |
| `Unfollowed` | `unfollowed_tip` | This node does not follow the log's tip; a tip no trusted node follows takes no coordinated write |
| `Sealed` | `sealed` | A seal or bookmark move ended the followed tip's log, or a `close` ended a pending write, now held |
| `Refused` | `rejected` | The coordinator refused the call |

An abort voids the proposer's never-offered frames with
[void records](held-changes.md#void-records) and rebuilds their scopes from
the checkpoint base and retained frames. Unsent bundles that saw its
effects are held as [held changes](held-changes.md) for the application.

## Seals and moves

On a trusted node with a composed log, a seal of a tip enters the graph only
after its digest ended that tip's log as its terminal entry, so no entry
commits on a tip the graph sealed. `publish_tip_statements` and
`receive_tip_statements` verify each new seal as admission does, then end its
tip's log with `LogTerminal { digest, continuation }` unless it ended already:

- A log ended by this seal, or by a terminal entry whose statement this node
  does not hold, admits the seal; concurrent seals resolve in the graph by
  the lower digest, as on devices without a log.
- When a seal this node already holds ended the log, its own publication
  fails `sealed` and publishes nothing. A received seal is admitted anyway.
- When the coordinator does not confirm, publication fails
  `coordinator_unavailable`; a received seal and the statements depending on
  it wait outside the graph until a later exchange.

The SDK's `tips` calls report both with these JSON codes: `sealed` is not
retryable, `coordinator_unavailable` is.

Moves need no entry of their own: a move off an open tip carries the close
seal that ends its log, and a forward move leaves an already sealed tip.
Deleting a bookmark leaves its tip and the tip's log open. An opening's
minimum in-sync size holds for every tip of its line: before a node's join of
a log nobody joined yet, it relays that minimum with `DomainLog::configure`,
and it never joins a log with a smaller one.

`EncryptedReplica::seal_log(log, terminal, coordinator, peers)` ends a log
directly and decides this node's entries. Entries committed before the
terminal entry are the sealed state; their frames beyond the seal's frontier
walk on as late frames. A continuation's log starts at zero, so before a
node takes anything of it, at its first slot or resolution there, it decides
its entries of the sealed tip's log and catches up and applies every one
committed before the terminal entry; otherwise it is `Behind`. A pending
entry the terminal entry aborted is decided by its proposer:

- With a continuation, the write moves unsent into the log of the
  continuation of the tip's winning seal in the proposer's graph, else the
  terminal entry's. Once the node follows that tip, it is appended there as
  the first entry, which is the state it was computed on, and commits once.
  Until then `propose` returns `Undecided`; a continuation whose seal lost
  hands it on to the winner's. Another node that stored it discards it.
- After a `close`, or when the continuation already took a write, it is
  voided, rolled back and returned as a [held change](held-changes.md):
  `Sealed`.

Slots on an ended log fail `Sealed`.

## Recovery

`resolve_coordinated` decides every undecided entry against the head and
runs at slot time, after storing, and in the SDK whenever an entry is
stored. It repeats lost acknowledgments, which are coordinator leader
memory.

| Crash | Outcome |
| --- | --- |
| Proposer before its append | The staged entry appends with its recorded sequence after restart and proceeds |
| Proposer after storing on peers, before acknowledging | The deadline decides: committed without it if the rest meet the minimum, else aborted and voided |
| Node before acknowledging | It leaves the set at the deadline; after restart it applies the entry if it committed, discards it otherwise, then rejoins |

Join and append sequences are allocated in a flushed transaction before the
coordinator sees them. A position committed beyond the coordinator's
1,024-hash window cannot be decided by a node that slept through it; stuck
recovery is [phase 10](../features_09/10-stuck-recovery.md).

## Catch-up

A node outside the set reads entry evidence from any reachable device: each
position, entry hash and frame ids, verified against the head's hash window
and running hash before any is recorded. The coordinator supplies the genesis
anchor even after the first commit, so a fresh identity can start at zero. The frames themselves are ordinary
committed frames and arrive by range sync; until they are all held the node
is `Behind`. It joins with the committed position and running hash, and only
then proposes. Trusted nodes keep the committed entry index for the last 1,024 positions.
A member without that index reconstructs it from complete retained bundles:
every member must be applied, agree on its signed position and bundle membership,
and be authorized for the requesting peer. It serves the original encrypted
frames and void evidence. It neither calls the coordinator nor claims that its
candidate is canonical; the receiving trusted node requires an exact hash in
the coordinator's window. The scan and reply use bounded pages.

A missing copy, incomplete bundle or entry outside the hash window leaves the
node `Behind`; even a matching final running hash cannot bypass a missing hash
in that window. Only an owner-signed cut continues past entries no device
holds, and only a held cut's retained chain verifies positions before the
window; see [stuck recovery](stuck-recovery.md).

## APIs

| Surface | Calls |
| --- | --- |
| Rust | `EncryptedReplica::{followed_tip, followed_log, coordinated_slot, propose, store_entry, resolve_coordinated, seal_log, committed_entries, coordinated_status, set_domain_log}`, `SecureSql::commit_coordinated`, the `EntryPeers` transport port and the `rocky_core::DomainLog` metadata port |
| Coordinator | `NetworkCoordinator` implements `DomainLog`; `rocky_coordinator::coordinated::MemoryLog` runs the same log machine in one process |
| Live links | `NativeSession::{store_entry, committed_entries}`; a store is paged and its chains carried as for pushes |
| SDK | `Open::coordination` accepts `Coordination::Composed(Arc<dyn DomainLog>)` or `Coordination::Service(bundle)`; `Database::coordinate(log)` changes the composed log after open. A service database composes its log itself. SQL `BEGIN` and `COMMIT` with `"coordinated": true`, through the followed tip's log; `coordinated` reports `trusted`, `tip` (hex), `position` and `undecided` |
| Kotlin | `RockySession.coordinated { query(...); execute(...) }` returns `CoordinatedCommit(position, hash, value)`; `coordinatedStatus()`. Failures are `RockyException` with the codes above |

```kotlin
val invoice = session.coordinated {
    val count = query("SELECT COUNT(*) FROM invoices")[0].jsonArray[0]
        .jsonObject.getValue("value").jsonPrimitive.long
    execute("INSERT INTO invoices(id,number) VALUES(?,?)", nextId("invoices"), count + 1)
    count + 1
}
```

The SDK sends entries only over open live links: a trusted node without one
to a member of the set leaves the set at the deadline. Kotlin uses a coordination service, or attaches `DevelopmentLog` for
in-process development. A development log loses its ledger at process exit.

The encrypted file identity is `rocky.encrypted-replica.v23` and the live
link handshake `rocky.link.v7`; entries, catch-up requests and cut counts name
their tip's log. Other versions are refused.

## Limits

- The proposer's own reads see its staged effects until the entry is
  decided; remote reads served meanwhile may too, as for held changes.
- An offered frame of another log that depends on an aborted entry's row
  waits for its void and then applies on the rolled-back state.
- One undecided own entry per log; checkpoints of its scopes wait for it.
- A write carried into a continuation keeps its frames' tip and log
  position, the sealed tip's; members cannot rebuild its entry evidence from
  bundles, so catch-up needs a trusted node's committed index for it.
- Members fence and rebuild entry evidence by the tip each frame names.
- The coordinator keeps 8 ended logs per domain; an older one reads as
  unopened. A node holding committed entries of it refuses its slots
  `Sealed`; one that held none could join it again until its seal arrives.
- Only a continuation's own sealed parent log is caught up, while that log
  is retained, and a branch tip's log checks nothing of the log its opening
  seal ended: a trusted node must hold that sealed state before it writes.
- Only the followed tip's log, and the sealed log it continues, are
  resolved: an own write a crash left undecided on a tip the node left for
  another line stays unsent, with its effects.
- Commit is decided against the coordinator's window; older positions need
  an owner-signed [recovery cut](stuck-recovery.md).

## Evidence

[Phase 9 evidence](../features_09/09-coordinated-writes-evidence.md).
