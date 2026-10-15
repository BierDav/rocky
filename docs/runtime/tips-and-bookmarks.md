# Seals, tips and bookmarks

A tip is one schema and the frames written to it. A seal ends a tip at a
frontier and either `continue`s into one successor tip or `close`s it.
Bookmarks are named, signed pointers to tips; a device writes only the tip its
followed bookmark points to. Every device holds the whole graph; frames reach
only devices following their tip. The design is
[phase 14](../features_09/14-seals-and-bookmarks.md) of
[generations as sealed forks](../features_09/README.md#generations-as-sealed-forks).

With a [coordinated log](coordinated-log.md) configured, a seal or a move
off an open tip first ends that tip's log
([seals and moves](coordinated-writes.md#seals-and-moves)). A call whose seal
lost to one this device already holds fails `sealed` (not retryable); one the
coordinator did not confirm fails `coordinator_unavailable` (retryable) and
publishes nothing. Until
[phase 15](../features_09/15-lazy-compaction.md) a seal compacts nothing and
the [mergeable horizon](mergeable-horizon.md) still releases evidence; until
[phase 17](../features_09/17-schema-changes-through-seals.md) schema changes are
explicit [generations](generations.md), refused once a domain has a graph.

## Start with a bookmark

Rocky creates no bookmark. The domain's owner creates the first one on the
genesis tip and follows it; `main` is a convention. Other devices follow it
once sync has brought them the graph.

```kotlin
val tips = session.tips
tips.createBookmark("main")          // on the followed tip: genesis
tips.follow("main")
// another device, after sync:
tips.follow("main")
```

Until a device follows a bookmark, its status has a null `bookmark` and
sealing is refused. A device follows one bookmark at a time.

## Seal

`seal()` seals the followed tip with `continue` and moves its bookmark to the
successor in one signed batch. Writes continue on the successor; no row
changes, and adopting a seal costs every device the same small amount
whatever the domain holds. `seal(close = true)` ends the tip instead; its
bookmark stays on it and is read-only until moved. Rocky runs no timer.

The seal's frontier holds the last global author position of every write this
device holds on the followed tip. Writes beyond it that arrive later walk
`continue` seals to the open tip; behind a `close` they become
[held changes](held-changes.md). Sealing refuses pruned history, whose author
positions are gone, and a tip this device does not follow.

## Branches

The owner or a trusted identity opens a tip on any seal and points a bookmark
at it. A branch starts from the seal's state and never takes later frames of
the line it came from.

```kotlin
val sealed = tips.seal()                                   // main moves on
val seal = tips.inspect().seals.single { it.id == sealed.tip }.id
val dev = tips.open(seal, minimumInSync = 1u)
tips.createBookmark("dev", dev)
tips.follow("dev")                                         // rebuilds the workspace
```

`minimumInSync` names the minimum in-sync size of the tip's coordinated log;
a device cannot know the coordinator's domain minimum, so it is required.

An author log continues along one line of `continue` seals. A device that
writes on another line does so under another node of its identity, certified
without an issuer and kept in the secret store (role `tip-nodes`). Rocky
enrolls it after `follow`, at open and after each sync pass; returning to a
line, or reopening on it, reuses that line's node. A stored identity's node
takes the next free number of its [author range](sdk-enrollment.md#node-author-numbers);
a supplied identity's provider reserves one. Until a node exists the
workspace refuses writes, and a failed enrollment is recorded as
`line_node_failed` [activity](activity.md).

`drop(tip)` releases a tip the device no longer follows. It refuses the
followed tip and its ancestors, and frames other devices do not hold yet.

## Move or delete a bookmark

Issuers move a bookmark forward along `continue` seals. The owner and trusted
identities also create and delete bookmarks and move them backwards or
sideways. Such a move off an open tip closes it in the same batch, so it is
made on a device following that tip. The line's writes since the common seal
return as held changes on every device holding them. Two moves of one bookmark
from the same tip resolve to the lower digest; slice 7 of
[phase 14](../features_09/14-seals-and-bookmarks.md) ranks the owner, trusted
identities and issuers before the digest. A call whose statements the
graph would not resolve to, such as a move losing to a lower digest, is
refused instead of published.

## Status and inspection

| Kotlin | Rust `Session` | JSON `tips` action | Returns |
| --- | --- | --- | --- |
| `status()` | `tip_status()` | `status` | `TipStatus` |
| `inspect()` | `inspect_tips()` | `inspect` | `TipInspection` |
| `seal(close)` | `seal(close)` | `seal`, `close` | `TipStatus` |
| `open(seal, minimumInSync)` | `open_tip` | `open`, `seal`, `minimum_in_sync` | `{tip}` |
| `createBookmark(name, tip?)` | `create_bookmark` | `create`, `name`, `tip` | `TipStatus` |
| `moveBookmark(name, tip)` | `move_bookmark` | `move`, `name`, `tip` | `TipStatus` |
| `deleteBookmark(name)` | `delete_bookmark` | `delete`, `name` | `TipStatus` |
| `follow(name)` | `follow` | `follow`, `name` | `TipStatus` |
| `drop(tip)` | `drop_tip` | `drop`, `tip` | `TipStatus` |

The JSON request is `{"type": "tips", "action": ...}` in the
[SDK contract](sdk-contract.md) envelope; Kotlin's `RockyClient.tips` and
`RockySession.tips` use it. Digests and tips are lowercase hex; counters are
unsigned.

Status reports the followed `bookmark` and `tip`, `operations_since_seal`
(writes held locally beyond the last seal) and, per peer, the tip its open
link claims and its last reported per-scope frontiers. `evidence_releasable`
stays null until lazy compaction exists.

Inspection is for developers: the genesis tip and schema, every seal with its
frontier, action and whether it won (`canonical`), openings with their minimum
in-sync size, bookmarks with their tip and moves, and nodes. A `continue`
seal's id is its successor tip; an opening's id is its tip. For this device a
node lists the tips it holds frames of and no kept seals, which arrive with
[phase 16](../features_09/16-kept-seals-and-views.md); for peers both are null,
since peers do not report them.

## Replica boundary

`TipGraph` holds a domain's signed seals, openings and bookmark moves.
Admitted peers exchange the complete graph before rows, on new and open links;
row subscriptions do not filter it. Statements are verified before the union,
and the union and any resulting workspace switch commit together; invalid
statements or a failed switch leave the graph and rows intact. Genesis is
`commitment("rocky.genesis-tip.v1", manifest_digest)`, qualified by the
graph's domain scope.

`EncryptedReplica` provides `follow_bookmark`, `followed_bookmark`,
`followed_tip`, `tip_graph`, `publish_tip_statements`,
`receive_tip_statements`, `seal_frontier`, `tip_observation`, `tip_nodes`,
`enroll_line_node`, `register_line_node`, `line_node_needed` and `drop_tip`.
The caller keeps line-node keys and supplies them again after reopening.

Statements are signed by the identity, never a node. The owner signs with no
links; trust is an owner-direct grant, `Extended.trusted: Option<Scope>`,
issued by `Authority::issue_trusted`; issuers sign through their issuer links.
The SDK offers no call that grants trust yet.

Peers declare their followed tip in the authenticated hello. Row offers,
frontiers, range transfers and remote reads need the same tip; frames of
unrelated tips are refused, and discovering another schema never adopts it.
A link attempt while two peers follow different tips fails and leaves a retry
backoff. Checkpoints name the selected tip; a switch that needs history
compacted away fails with an expected error.

## Late and held writes

Late frames keep their original signatures and apply field by field under the
installed schema's removal rules. A relay serves held frames with
`held_frame_page`; `NativeSession` supplies `pull_held_page`, `push_held_page`
and bounded `sync_held_changes`, independent of followed tips. Re-authoring
writes new frames on the followed tip with the held-change transaction API.
Adopting a seal or forward move changes metadata only; the regression compares
stored rows and SQLite write counts with 10 and 10,000 rows.
