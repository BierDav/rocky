# Lazy compaction at seals

A [seal](tips-and-bookmarks.md) fixes what a tip means without asking any
device to rewrite storage. Each device finalizes the seals on its followed
path and compacts at one when it chooses; devices that have not compacted
reach the same rows for every late frame. This guide is for application
developers who release storage and for anyone changing finalization. The
plan and remaining slices are in [phase 15](../features_09/15-lazy-compaction.md).

The concrete replay ledger starts from the base's signed `own` marks and folds
only eligible own births. Carries raise the permanent allocation marks, but never
the replay ledger or own-birth receive floor. See [admission evidence](admission-evidence.md).

## Finalizing

A seal of the followed path is finalized once this replica holds every frame
through its frontier in every scope it holds. A node's positions
(`author_sequence`) must be contiguous from the previous seal's mark; a
position below the scope's last held frame of that node is already placed.
An otherwise missing position can be placed by its author's signed header in
a scope this replica has no whole-row Read or Custody grant to hold. Held
scopes still require real payloads; a role, subscription or retained key alone
grants no custody. Graph domains finalize during scope maintenance, in path
order, without a call.

Live links request `Placements { seal, author, from..=to }` in pages of at most
64 headers, with credential chains and no bodies. Serving requires current,
unrevoked membership in the seal's domain, a known seal and positions within
its frontier. Private staged frames stay private. A partition's header is
served only after the requester proves that partition on the link; otherwise
the reply is `CustodyRequired`, without the plaintext partition tuple. A
header whose author chain the server's current set
[revokes](revocations.md#admission) is omitted, as in ordinary range serving;
an omitted position stays unresolved.

The receiver verifies the signature, current row format, domain, author-number
binding, the author's current authority, seal visibility and frontier bound,
then stores accepted headers
atomically in `secure_placements`. Headers of held scopes cannot replace their
payloads or mark anything applied. A later frame replaces its placement; a
different signed hash records author equivocation. Placements are pruned once
finalization and held replay bases no longer need their positions.

Readiness includes scopes authorized by whole-row Read/Custody grants, declared
by the installed manifest and projection, and held by this replica. Partition
templates are excluded. Keys and existing scope-state IDs are not prerequisites.
Inspecting readiness or status creates no state. Retained ciphertext counts
without a key; with a key, pending payloads must apply or become held first.
A keyless scope with covered ciphertext is deferred: finalization records no
applied bound for it. It reports no missing custody, and repeated maintenance
makes no new revision. Once its key arrives, pending payloads block readiness;
after apply, the last seal is checked again and receives its actual bound.
A keyless scope without covered frames still receives a truthful zero bound.

Readiness scans still read every stored header and current status. A replica
may reuse parsed header fields only for identical complete bytes, within both
an entry bound and 32 MiB of retained header bytes. This syntax memo also serves
frontier parsing; it stores no custody, key, authority or readiness decisions.
Changed bytes undergo full canonical decoding, including headers the scan later
excludes. Cache pressure only causes decoding again.

Within one held-frame refresh, a bounded private context can reuse schema,
winning-seal and path facts from the same borrowed graphs and workspace tip.
Each header still checks its own author and sequence against the path bounds;
status, payload, keys and permissions stay live. Recovery uses the full-frame
path, and the context ends with the refresh, including a failed attempt.

Only a verified payload/import or the finalization transaction materializes a
scope. Finalization records a bound for every scope it checked, including zero
for an empty scope. That verified empty state has a signed, transferable base
and ordinary history obligations. History work uses only materialized scopes;
compaction signs nothing for a scope with neither a checked bound nor an earlier
verified base. A newly authorized scope is never assumed empty at an earlier seal.

An unverified scope is checked from position zero. Missing evidence reports
`scope … has no verified state at a seal of the path` and lists a rebootstrap
need, including graph-listed empty checkpoints. A losing seal's verified,
unpruned scope can retain a coverage anchor before replay or settlement deletes
its old accounting. The anchor merges author maxima; it places only already
accounted positions, never a state bound at a winning seal. Each new path seal
still finalizes normally. Anchors survive earlier path seals and reopening;
a signed/imported base, reset replay, eviction or table removal clears them.
Positions beyond an anchor still require payloads or authorized placements.

Kept marks cover only scopes with an applied bound. A missing bound is
unavailable state; it cannot produce an empty checkpoint, view or rewind.
Keyless scopes do not count as readable live state.

Finalization waits while a schema
switch is unfinished, so future tables and partition templates gain no state.

Finalizing a seal changes only scopes whose constraint group changed since
the previous one. For each, in one transaction:

1. Applied operations the seal excludes are renumbered after the included
   ones, so the application order stays causal.
2. Rows are rebuilt at the frontier and projected.
3. Tombstones, rows the typed table does not show and `SET NULL`
   substitutions are recorded as that seal's diff.
4. Rows are rebuilt again with every operation.

A late frame is applied through every finalized seal's diff at its bound. A
row dropped at a seal stays dropped, however many seals the frame walks.

## Compacting

| Kotlin | Rust | JSON `tips` action | Returns |
| --- | --- | --- | --- |
| `compact(keep)` | `Session::compact`, `EncryptedReplica::compact` | `compact`, `keep` | `TipCompaction` |
| `compaction()` | `Session::compaction`, `EncryptedReplica::compaction_status` | `compaction` | `TipCompaction` |

`compact(keep)` finalizes what is ready, then installs a replay base at the
newest finalized seal that is [final](seal-finality.md) or has at least
`keep` seals after it on the path. The base is a checkpoint at that seal
(`CheckpointStatement.seal`), signed by this node. Only the compacted seal
stays recorded with the seals finalized after it. An unchanged scope keeps
its earlier base while its key epoch and revocation policy still match. A
held scope without its key and with no history of its own signs a
[no-frames base](encrypted-checkpoints.md#no-frames-checkpoints);

operations behind the
base are released under the usual [retention](retention-holds.md) rules.
`pruned` and `blocked` report what the call released and what stopped it.
The base names the creations of the rows it stores only, so another device
can [import it](#checkpoints-at-seals).

A base's covered marks fold the previous base and covered frames: `sequence`
includes carries, while `own` excludes them. They never use live maxima, so a retained frame beyond the seal
reapplies on an importer. Every replay (finalization, rebuild, audit and a
kept-seal rewind) keeps its own identity ledger: seeded from exactly the
signed base's `own` it starts from, from nothing without one, or for a rewind from the
retained prefix through the kept bound (unavailable once that prefix is gone).
A birth whose identity its author already used, by the marks the frame
entered with, never materializes again, even after a seal drop removed the
earlier row; every own birth still raises the ledger, while carries never do. The permanent marks, a
persisted ledger and a local journal of admission decisions that let an
admitted frame return are specified in [admission evidence](admission-evidence.md)
(implementation pending).

`keep` trades storage against the chance of a rebootstrap: a seal that is
not final can still lose a race. A final seal is compacted at `keep = 0`
without that risk.

## Existence logs

A replica that holds a logged table's scope compacts its
[existence logs](existence-logs.md) with the rows. Each row frame counts
the existence records its commit signed (`Extensions.existence`), and holders
of the scope key refuse a frame that claims more than its creations and
deletions. A seal waits until, for every node, the records held between the
previous seal's mark and the frontier reach what that node's frames count.

Compacting folds every record whose author position the frontier includes
into a base at the seal: per signer the last folded position and hash,
per author the highest created identity, and the rows evidenced by only a
creation or only a deletion. Rows with both are dead at the seal; their
records and liveness rows are released, and the identity marks keep their
ids retired. Records beyond the frontier stay and continue each log.

A replica's own fold is never offered to peers. After `compact`, an SDK
device that may sign tip statements (the owner, a trusted identity or an
issuer) signs it as a `SignedExistenceBase` naming the seal
(`EncryptedReplica::unsigned_existence_bases`,
`install_signed_existence_base`). Devices that hold only a child, or join
after records were released, fetch that base through the existing base
transfer: it is verified against the graph and must sit at a seal on their
followed path. A base at a seal or later already counts as complete.

## Checkpoints at seals

With a tip graph, a checkpoint exists only at a seal: a replay base that
`compact` installed or a device imported. A peer offers its base only when it
sits at a seal of the followed path, and never signs one at another cut.
After `compact`, an SDK device that may sign tip statements lists each base
this node signed as a checkpoint record (`SignedCheckpointRecord`, a
`TipStatement::Checkpoint`): the seal, the scope, the checkpoint's digest,
the node that signed it and its size in stored rows. Records sync with the
graph to every device and need a seal the graph holds; a revoked lister's
records are left out. The body moves only on request, through the usual
[checkpoint import](checkpoint-import.md).

`EncryptedReplica::unlisted_checkpoints` returns the bases still to list.
Inspection shows each seal's `checkpoints` with their signer, lister
authority and rows (`TipSeal.checkpoints`, Kotlin `TipCheckpoint`).

An SDK join imports a checkpoint at a seal. In a domain with a tip graph, a
scope whose peer offers none, because nobody compacted yet, joins by frames.

## Rebootstrap

After graph admission, a revocation or a bookmark move, finalized seals off
the new path are undone and their scopes rebuilt. A compacted seal that left
the path cannot be undone. The device still follows the bookmark, but its
rows stay as they were, writes are refused and finalizing pauses;
`rebootstrap` names the lost seal. Each held scope whose base left the path,
or was pruned under a checkpoint at another cut before the graph existed,
needs a base at a seal of the path (`EncryptedReplica::seal_rebootstraps`).
In order, the device:

1. imports a checkpoint the graph lists for the scope at a seal of the path,
   the newest first, when the peer offers that one; a logged scope then
   fetches the peer's signed existence base, which replaces a fold at a seal
   off the path whatever it covers;
2. otherwise, if it holds every frame it applied, rebuilds the scope from
   them (`replay_at_seals(scope, false)`);
3. otherwise, if the peer pruned no frame, keeps the frames it holds and
   takes the rest again from the peer (`replay_at_seals(scope, true)`). Its
   own log is guarded as after an [eviction](replica-roles.md#eviction): it
   authors again once the log is whole;
4. otherwise waits, for a listed checkpoint or the owner's
   [recovery](../features_09/19-recovery-as-a-bookmark-move.md).

Once no scope waits, `finish_seal_rebootstrap` forgets the finalized seals,
rebuilds the rows at the followed tip and clears `rebootstrap`. The device
finalizes again from the path's start; a seal at or before a scope's base
needs none of that scope's frames. Frames the new base leaves out, its own
too, apply on top as late frames or stay held, as on any device; Rocky never
re-authors them. The SDK runs these steps at the start of every sync pass
(`NativeSession::rebootstrap_at_seals`).

## Limits

- A replica holding a child only keeps its records until a signed base
  arrives; it never folds a log whose frames it cannot count.
- A creation the frontier excludes whose id is below its author's mark in
  the base is refused as retired: an id reserved before
  a seal and committed after a later one.
- A logged scope whose fold sits at the lost seal cannot replay frames: its
  records were released there. It waits for a listed checkpoint and a signed
  existence base.
- A device without a tip signer lists nothing; its bases still serve peers
  that ask for them, but the graph does not name them.
- A seal before an imported base waits while a frame of another held scope
  may still be missing, as for scopes the replica does not hold.
- Replicas finalize and compact only the scopes they hold.
- Change detection is per table, not per partition.
- While a seal waits, each maintenance step scans the stored frame headers;
  parsing reuse does not remove those repeated SQL visits.

## Formats

| Boundary | Current |
| --- | --- |
| Encrypted checkpoint | Format 16: names the seal it sits at, if any; keyed or no-frames state; covered identity marks |
| Row frame | 11: counts its existence records |
| Existence base | Format 1, signed `rocky.existence-base.v1` |
| Checkpoint record | Format 1, signed `rocky.checkpoint-record.v1` |
| Live link / replica file | `rocky.link.v14` / `rocky.encrypted-replica.v32` |

The declaration protocol and its SDK operation are removed. Checkpoint
statements contain neither admitted nor finalized declarations. Pruned log
prefixes remain: `pruned_page`, `behind_pruned`, `PrunedPage` and
`CheckpointMessage::Pruned`/`PrunedLogs` signal missing history without a
separate finalization protocol. Seals and existence records do not change.


## Evidence

The plateau probes are seal-based and independent of the removed guide:

```sh
cargo test -p rocky-secure compaction_at_seals_plateaus_under_insert_delete_churn -- --nocapture
cargo test -p rocky-secure a_holder_folds_the_existence_log_at_a_seal_and_a_child_holder_imports_its_signed_base
```

After cycles 16 and 64, the row probe has the same counts: one live row,
its creation stub, no tombstones, operations or applied entries, four handles
and one compacted seal. The existence probe folds 131 records after 64 churn
cycles and a parent deletion into one live parent's entry and no records.
No deleted row retains a tombstone or stub. The same signed base gives a
child-only holder the parent's orphan resolution. These counts measure
row-state reclamation; the specified
[admission evidence](admission-evidence.md#growth-and-accounting) grows with
births and is reported separately, never inside them. The
[gate evidence](../features_09/15-lazy-compaction-evidence.md#completion-gate-recheck)
lists the remaining late-frame, incremental, finality and rebootstrap probes.
