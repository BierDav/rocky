# Coordinated writes

Writes that must not race, such as sequential invoice numbers or a request
decided once, are ordinary bundles that the domain's
[coordinated log](coordinated-log.md) orders. A coordinated write becomes
true only once every trusted node of the in-sync set stored it durably;
nothing of it leaves a node before then, and an aborted one is voided. No
split brain is possible: a node outside the set catches up before it writes
again. [Decision 0072](../decisions/0072-coordinated-log-and-writes.md)
records why.

## Roles

| Role | What it does |
| --- | --- |
| Trusted node | A device whose node key the domain's control certificate lists. It composes the log, proposes, stores others' entries and acknowledges them |
| Member | Any other device. It receives committed entries by ordinary sync and never calls the coordinator |
| Coordinator | Orders entries and decides commit or abort. It holds metadata only: no frame, row or application rule |

Trust is the guard. Nothing prevents a trusted node from writing coordinated
tables directly with an ordinary commit; applications that need every write
of a table coordinated keep that discipline themselves.

## One write

1. **Slot.** `EncryptedReplica::coordinated_slot` decides what this node
   stores, catches up from trusted peers, joins the in-sync set and returns
   the commit index `c` the write is computed on.
2. **Stage.** The application runs its SQL transaction on that committed
   state; `SecureSql::commit_coordinated(handle, slot)` commits it as one
   ordinary bundle in one local transaction, flushed. Its frames carry the
   log position `c+1` in their bundle membership and stay unsent: range,
   transfer, push, checkpoint and recovery paths skip or refuse them.
3. **Append.** `propose` appends the entry hash at `c+1`. The hash commits
   to the log, the position and the bundle's frame ids, so it is not inside
   the frames it covers; a node holding every member recomputes it.
4. **Store, then acknowledge.** The proposer sends the entry, with the void
   records its frames continue after, to every other in-sync node. Each
   stores it durably and unapplied, then acknowledges it itself; the
   proposer acknowledges last. While the entry remains pending, the proposer
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
| `Refused` | `rejected` | The coordinator refused the call |

An abort voids the proposer's never-offered frames with
[void records](held-changes.md#void-records) and rebuilds their scopes from
the checkpoint base and retained frames. Unsent bundles that saw its
effects are held as [held changes](held-changes.md) for the application.

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

A node outside the set reads committed entries from trusted peers: each
position, entry hash and frame ids, verified against the head's hash window
and running hash before any is recorded. The frames themselves are ordinary
committed frames and arrive by range sync; until they are all held the node
is `Behind`. It joins with the committed position and running hash, and only
then proposes. Committed entries are kept for the last 1,024 positions.

## APIs

| Surface | Calls |
| --- | --- |
| Rust | `EncryptedReplica::{coordinated_slot, propose, store_entry, resolve_coordinated, committed_entries, coordinated_status, set_domain_log}`, `SecureSql::commit_coordinated`, the `EntryPeers` transport port and the `rocky_core::DomainLog` metadata port |
| Coordinator | `NetworkCoordinator` implements `DomainLog`; `rocky_coordinator::coordinated::MemoryLog` runs the same log machine in one process |
| Live links | `NativeSession::{store_entry, committed_entries}`; a store is paged and its chains carried as for pushes |
| SDK | `Database::coordinate(log)`; a coordination-service database composes its service's log itself. SQL `BEGIN` and `COMMIT` with `"coordinated": true`; `coordinated` reports `trusted`, `position` and `undecided` |
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
to a member of the set leaves the set at the deadline. Kotlin cannot compose
its own `DomainLog`; it uses a coordination service's.

The encrypted file identity is `rocky.encrypted-replica.v19` and the live
link handshake `rocky.link.v3`; other versions are refused.

## Limits

- The proposer's own reads see its staged effects until the entry is
  decided; remote reads served meanwhile may too, as for held changes.
- An offered frame of another log that depends on an aborted entry's row
  waits for its void and then applies on the rolled-back state.
- One undecided own entry per log; checkpoints of its scopes wait for it.
- Commit is decided against the coordinator's window; older positions need
  phase 10.

## Evidence

[Phase 9 evidence](../features_09/09-coordinated-writes-evidence.md).
