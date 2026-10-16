# Kept seals and read-only views

A node can keep the state of earlier [seals](tips-and-bookmarks.md) so that
migrations, audits and debugging read them with the same queries and DAOs as
the live tables. This guide is for application developers who configure what
a node keeps and open connections on a sealed state. The plan and its gate
are in [phase 16](../features_09/16-kept-seals-and-views.md).

## One live workspace

A domain's live tables hold the tip the followed bookmark points to, under
their plain names, in the same file and transaction as history. Following
another bookmark rebuilds them. Every other state is read-only and reached
through the views below. A second writable workspace is an
[idea](../ideas/additional_live_workspaces.md), not part of the runtime.

## Choosing the seals to keep

| Kotlin | Rust | JSON `tips` action | Returns |
| --- | --- | --- | --- |
| `tips.keep(KeptSeals)` | `EncryptedReplica::keep_seals` | `keep` with `kept` | the configuration |
| `tips.kept()` | `EncryptedReplica::kept_seal_config` | `kept` | the configuration |
| `tips.inspect()` | `EncryptedReplica::held_seal` | `inspect`, each seal's `held` | how a seal is held |

`KeptSeals` names, among the seals of the tips the node follows:

- `bookmarked`: the seal each bookmarked tip starts from, or a closed tip's
  own;
- `latest`: the latest this many seals;
- `every`: every seal.

The configuration is per node. A seal is kept as it is
[finalized](lazy-compaction.md#finalizing) here, that is, once the node holds
every frame through its frontier; an earlier seal newly named is kept only if
the live tables still hold its state. Until then the seal is
*shallow* on this node. Removing a seal from the configuration releases it at
once: the history rows only it needed are deleted and the seal stays in the
graph, shallow.

Keeping the latest seal is optional. Nodes that open branches should keep
it, since a branch opens only on a held seal.

## Held seals

A node *holds* a seal whose state it has (`held` in `inspect`):

- `kept`: history tables keep it;
- `checkpoint`: every held scope's replay base sits there, or its state was
  pulled from a node that keeps it;
- `live`: the live tables hold it now, as right after sealing.

Opening a branch on a seal, or following a bookmark onto a tip that starts
from one, is refused when the node does not hold it and no replay base lies
on that tip's path. The node pulls the seal's state from a peer
(`EncryptedReplica::begin_seal_pull`, `stage_checkpoint_page`,
`finish_seal_pull`), or seals now. A pulled state is kept aside without
changing any row; following a tip whose path passes the seal installs it as
the replay base. Today a peer offers its replay base checkpoint, so it serves
the pull when its base sits at that seal.

## History tables

Each application table `t` has one history table, `secure_history_t`. A
change to a live row writes the version it replaces there, in the same
transaction, while a kept seal still needs it. A kept seal's state is the
live rows unchanged since the seal plus the history rows valid at it, so
storage grows with the rows that changed, not with the number of seals:
keeping five seals of a table where 1% of rows change between seals adds
about 5%.

Schema changes keep sealed states readable:

- after an additive change, a kept seal selects only its own columns;
- before a removal or migration that drops or narrows a table, each kept seal
  freezes a copy of only that table's state.

## Read-only connections

A connection bound to a kept seal, or to the seal a bookmark shows, creates
temporary views with the live tables' names and shape. The bookmark resolves
when the connection opens: its tip's own seal once sealed, else the seal it
starts from.

| Kotlin | Rust |
| --- | --- |
| `driverConnection(binding = RockyBinding.Seal(id))` | `ConnectionOptions { binding: Some(Binding::Seal(id)), .. }` |
| `RockyBinding.Bookmark("main")` | `Binding::Bookmark("main".into())` |
| Room: `setRockySeal(database, binding)` | |

Bound connections are read-only whatever `read_only` says: every write,
including `PRAGMA query_only = 0`, fails with SQLite's read-only code 8.
Any number may be open at once. They need no change notifications, since a
sealed state never changes, so a bound Room database needs no `followRocky`.
Binding a seal the node does not keep fails when the connection opens.

An unbound connection names history explicitly, in a read statement of its
own: `"items@<seal hex>"` or `"items@main"`.

## Limits

- Following a bookmark rebuilds the live tables from the replay base, so its
  cost grows with the table, not with the rows changed since the seal.
- Seal pulls are replica calls; the network session and SDK do not drive them
  yet.
