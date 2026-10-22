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
publishes nothing. A seal changes no rows; each device
[finalizes and compacts locally](lazy-compaction.md). A
[schema change](generations.md) is a seal whose successor tip has another
schema; every domain has a graph from install.

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
[held changes](held-changes.md). Pruned writes have no known author position:
a `continue` seal leaves them out, so they walk to the continuation like
writes this device never held, and a `close` refuses pruned history. Sealing
refuses a tip this device does not follow.

With a coordinated log, a device outside the tip's in-sync set catches up
and joins before its seal ends the log. When no in-sync node can append any
more, only the owner's `recover()` ends the tip: one `continue` seal that
abandons the committed entries nobody holds; every bookmark on the tip
follows it without a move. See [stuck recovery](stuck-recovery.md).

## Branches

The owner or a trusted identity opens a tip on any seal and points a bookmark
at it. A branch starts from the seal's state and never takes later frames of
the line it came from.

```kotlin
val sealed = tips.seal()                                   // main moves on
val seal = tips.inspect().seals.single { it.id == sealed.tip }.id
val dev = tips.open(seal, minimumInSync = 1u)
tips.createBookmark("dev", dev)
tips.follow("dev")                                         // switches the workspace
```

A branch opens only on a seal this device holds. A switch onto a kept seal's
path costs the rows changed since it; see [kept seals](kept-seals.md), which
also covers pulling a seal's state with `tips.pull`.

`minimumInSync` names the minimum in-sync size of the tip's coordinated log;
a device cannot know the coordinator's domain minimum, so it is required.

Each opening commits its own schema (opening format 2). In a schema migration
an issuer may open on an authenticated `close`, with a different schema and
the parent line's signed minimum. This exception does not authorize ordinary
branch creation. Issuer moves may follow these migration successors as well
as continuations; migration successors remain separate author lines.

Changing the followed bookmark preserves each line's writes there. Unsent
`main` writes wait while the device follows `dev`, and dev test rows never
become held re-authoring work merely on following `main` again. Across a schema
change the target's state comes from its own path, including fields a different
branch removed; see [the path filter](removal.md#the-path-filter).

Retained statements verify structurally, including losing and revoked
openings. Active tips require canonical parent seals and unrevoked openings
throughout their ancestry. An issuer migration also inherits the currently
resolved line minimum. Learning a larger owner-signed genesis minimum makes
an older, lower-minimum migration ineligible without invalidating its history.
Races between closing seals resolve before their openings' bookmark moves;
openings on one winning close resolve by move authority, then digest. Losing
migration openings never redirect to a rival opening.

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
followed tip and its ancestors, and its own writes until a peer that reads
them reported holding them, by acknowledgment or frontier, as pruning counts.
It also refuses frames compacted history may hold: when their author's log
lost a pruned prefix, or a checkpoint of their scope sits at a seal whose
state includes the tip. A base at a seal the tip opened on does not.

A device that bootstrapped a table from a checkpoint holds no author
positions for it: it neither seals nor switches to a branch whose base it
lacks, until [phase 15](../features_09/15-lazy-compaction.md) places
checkpoints at seals. Following another bookmark retries peers at once that
refused links while their tips differed.

## Trusted identities

The owner grants an identity trust for the domain; its devices then open tips
and move bookmarks backward or sideways like the owner. The grant is an
owner-direct link with no table grants, handed to a device of that identity,
which installs it; revoking it by its capability id ends the authority and
leaves the identity's other grants.

```kotlin
val grant = ownerTips.trust(identity)       // identity: its `session` principal
deviceTips.installTrust(grant.grant)        // on a device of that identity
ownerTips.revokeTrust(grant.capability)     // later, to remove it
```

The owner issues it for the domain's credential lifetime; granting again
extends it. Only an identity's own device installs it, and only while it is
current and unrevoked.

## Move or delete a bookmark

Issuers move a bookmark forward along `continue` seals and restricted
migration-successor openings. The owner and trusted
identities also create and delete bookmarks and move them backwards or
sideways. Such a move off an open tip closes it in the same batch, so it is
made on a device following that tip. Frames visible at the old target and
excluded by the new target return as held changes on every readable holder.
Common-ancestor late frames that remain target-visible still apply. A call whose statements
the graph would not resolve to, such as a move losing a race, is refused
instead of published.

## Races and revocation

Concurrent seals of one tip, and moves of one bookmark from one tip, resolve
to the owner's statement, then a trusted identity's, then an issuer's; only
among equals does the lower digest win. A [final seal](seal-finality.md),
one its tip's coordinated log committed and enough trusted nodes
acknowledged, beats every other seal of its tip, the owner's included. An issuer cannot outbid an owner's
rollback by varying its statement. Revoking an identity leaves its seals, openings and
moves in the graph but out of resolution; every device resolves again in the
commit that learns the revocation, and writes on a tip that loses this way
return as held changes. `inspect` reports each seal's and move's `authority`
(`owner`, `trusted` or `issuer`) and `revoked`, each seal's `final` and the
genesis line's `genesis_minimum`, and lists a bookmark's
losing moves and their tips as `losers` named `main/1`, `main/2`, in path
order; their tips stay branches.

## Status and inspection

| Kotlin | Rust `Session` | JSON `tips` action | Returns |
| --- | --- | --- | --- |
| `status()` | `tip_status()` | `status` | `TipStatus` |
| `inspect()` | `inspect_tips()` | `inspect` | `TipInspection` |
| `seal(close)` | `seal(close)` | `seal`, `close` | `TipStatus` |
| `open(seal, minimumInSync)` | `open_tip` | `open`, `seal`, `minimum_in_sync` | `{tip}` |
| `setGenesisMinimum(minimumInSync)` | `set_genesis_minimum` | `genesis_minimum`, `minimum_in_sync` | `TipStatus` |
| `createBookmark(name, tip?)` | `create_bookmark` | `create`, `name`, `tip` | `TipStatus` |
| `moveBookmark(name, tip)` | `move_bookmark` | `move`, `name`, `tip` | `TipStatus` |
| `deleteBookmark(name)` | `delete_bookmark` | `delete`, `name` | `TipStatus` |
| `follow(name)` | `follow` | `follow`, `name` | `TipStatus` |
| `drop(tip)` | `drop_tip` | `drop`, `tip` | `TipStatus` |
| `compact(keep)` | `compact(keep)` | `compact`, `keep` | `TipCompaction` |
| `compaction()` | `compaction()` | `compaction` | `TipCompaction` |
| `trust(identity)` | `trust` | `trust`, `identity` | `TrustGrant` |
| `installTrust(grant)` | `install_trust` | `install_trust`, `grant` | `TipStatus` |
| `revokeTrust(capability)` | — | `{"type": "revoke", "trust": capability}` | `{}` |

The JSON request is `{"type": "tips", "action": ...}` in the
[SDK contract](sdk-contract.md) envelope; Kotlin's `RockyClient.tips` and
`RockySession.tips` use it. Digests and tips are lowercase hex; counters are
unsigned.

Status reports the followed `bookmark` and `tip`, `operations_since_seal`
(writes held locally beyond the last seal) and, per peer, the tip its open
link claims and its last reported per-scope frontiers. `evidence_releasable`
stays null; `compaction` reports what compacting would keep instead.

`compact(keep)` moves the replay base to the newest finalized seal that is
final or has `keep` seals after it and releases operations behind it; see
[lazy compaction](lazy-compaction.md). `TipCompaction` reports `finalized`,
`compacted`, `uncompacted` seals, why the next seal is `waiting`, a seal to
`rebootstrap` at, and, for `compact`, `pruned` operations and `blocked`
scopes.

Inspection is for developers: the genesis tip and schema, every seal with its
frontier, action, whether it won (`canonical`), its signer's `authority` and
whether a revocation ended it (`revoked`), openings with their minimum in-sync
size, bookmarks with their tip, moves (with `authority` and `revoked`) and
`losers` (`name` such as `main/1`, losing `move`, its `tip`), and nodes. A `continue`
seal's id is its successor tip; an opening's id is its tip. For this device a
node lists the tips it holds frames of and its
[kept seals](kept-seals.md); for peers both are null, since peers do not
report them. Each seal's `held` says how this device holds its state.

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
The SDK's `trust` call issues it; an identity with an installed grant signs
through that single link, else through its issuer links.

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
