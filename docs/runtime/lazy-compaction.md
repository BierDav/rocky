# Lazy compaction at seals

A [seal](tips-and-bookmarks.md) fixes what a tip means without asking any
device to rewrite storage. Each device finalizes the seals on its followed
path and compacts at one when it chooses; devices that have not compacted
reach the same rows for every late frame. This guide is for application
developers who release storage and for anyone changing finalization. The
plan and remaining slices are in [phase 15](../features_09/15-lazy-compaction.md).

## Finalizing

A seal of the followed path is finalized once this replica holds every frame
through its frontier in every scope it holds. A node's positions
(`author_sequence`) must be contiguous from the previous seal's mark; a
position may be missing only below the scope's last held frame of that node,
because it then belongs to a scope the replica does not hold. Graph domains
finalize during scope maintenance, in path order, without a call.

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
stays recorded with the seals finalized after it; operations behind the
base are released under the usual [retention](retention-holds.md) rules.
`pruned` and `blocked` report what the call released and what stopped it.

`keep` trades storage against the chance of a rebootstrap: a seal that is
not final can still lose a race. A final seal is compacted at `keep = 0`
without that risk.

## Races

After graph admission, a revocation or a bookmark move, finalized seals off
the new path are undone and their scopes rebuilt. A compacted seal that left
the path cannot be undone: the domain records it, `rebootstrap` names it,
and switching tips is refused with "needs a checkpoint at the winning seal"
until the base is rebuilt from a checkpoint at the winning seal or replayed
frames. That rebootstrap is phase 15's slice 3.

## Limits

- Domains with [existence logs](existence-logs.md) wait; the next slice
  compacts their records at seals.
- Scopes pruned under a checkpoint at an arbitrary cut before the graph
  existed wait for a checkpoint at a seal.
- Replicas that hold part of the domain never finalize locally; issuer
  checkpoints at seals will serve them.
- Change detection is per table, not per partition.
- A [mergeable horizon](mergeable-horizon.md) is refused in a domain with a
  tip graph.
- While a seal waits, each maintenance step scans the stored frame headers.

## Formats

| Boundary | Current |
| --- | --- |
| Encrypted checkpoint | Format 14: names the seal it sits at, if any |

Row frames, seals and existence records do not change.
