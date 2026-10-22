# Kept seals and read-only views

A node can keep the state of earlier [seals](tips-and-bookmarks.md) so that
migrations, audits and debugging read them with the same queries and DAOs as
the live tables. This guide is for application developers who configure what
a node keeps and open connections on a sealed state. The plan and its gate
are in [phase 16](../features_09/16-kept-seals-and-views.md).

## One live workspace

A domain's live tables hold the tip the followed bookmark points to, under
their plain names, in the same file and transaction as history. Following
another bookmark switches them, as [below](#switching-tips). Every other
state is read-only and reached through the views below. A second writable
workspace is an [idea](../ideas/additional_live_workspaces.md), not part of
the runtime.

## Choosing the seals to keep

| Kotlin | Rust | JSON `tips` action | Returns |
| --- | --- | --- | --- |
| `tips.keep(KeptSeals)` | `EncryptedReplica::keep_seals` | `keep` with `kept` | the configuration |
| `tips.kept()` | `EncryptedReplica::kept_seal_config` | `kept` | the configuration |
| `tips.inspect()` | `EncryptedReplica::held_seal` | `inspect`, each seal's `held` | how a seal is held |
| `tips.pull(seal)` | `EncryptedReplica::want_seal` | `pull` with `seal` | the tip status |

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
on that tip's path. The node then pulls the seal's state from a peer, or
seals now:

1. `pull` names the seal; the node wants it until it holds it.
2. Each sync session asks its peer for every held scope's checkpoint at a
   wanted seal (`NativeSession::pull_seal`). The peer serves its replay base
   when that sits at the seal, else signs a checkpoint of the seal it keeps,
   from its history tables. A peer serves only scopes it reads in whole;
   another peer may serve the rest.
3. The pulled state is kept aside without changing any row. `inspect` shows
   the seal `checkpoint` once every held scope has it, and the refused call
   succeeds. Following a tip whose path passes the seal installs the state
   as the replay base.

Keeping the latest seal on a device that reads every scope, such as an
issuer, lets the others pull from it.

### Pulled states

A pulled state is a candidate base, not trusted state:

- open and full audit check only its integrity: it decodes, names its row's
  scope and seal, is signed, and a no-frames state stores nothing;
- installing it repeats every import check inside the installing
  transaction; a state that fails them is dropped and the install is
  `Refused`, a committed outcome that leaves the scope waiting for a
  rebootstrap, never an error that fails the follow or revocation;
- storing evidence of the scope that a pulled
  [no-frames state](encrypted-checkpoints.md#no-frames-checkpoints) excludes,
  or whose coverage cannot be decided, stores the evidence and drops the
  state in the same commit; a fetched state never refuses live data;
- learning a revocation drops every pulled state that no longer passes the
  import checks, in the revocation's commit.

A pulled state may make a seal count as held, and stop a pull from asking
for it, only while it passes the import checks now, judged without writing.
It never stands in for live coverage: readiness and finalization count only an
installed base. A scope that needs a pulled state at a seal of the path, with
or without a pruned prefix, installs it in the transaction before readiness is
judged; after a refusal the scope stays listed.

Tampering with a pulled state is corruption everywhere and is never dropped
silently. A state whose seal leaves the followed path stays cached and is
not installed, and a state at a seal a revocation excluded is not usable.

## History tables

Each application table `t` has one history table, `secure_history_t`. A
change to a live row writes the version it replaces there, in the same
transaction, while a kept seal still needs it. A kept seal's state is the
live rows unchanged since the seal plus the history rows valid at it, so
storage grows with the rows that changed, not with the number of seals:
keeping five seals of a table where 1% of rows change between seals adds
about 5%. The encrypted stored rows behind the live tables keep their
replaced versions the same way, in `secure_stored_history`; a switch and a
pulled checkpoint start from them.

Schema changes keep sealed states readable:

- after an additive change, a kept seal selects only its own columns;
- before a removal or migration that drops or narrows a table, each kept seal
  freezes a copy of only that table's state;
- a switch that recreates constrained or partitioned tables records its own
  difference into history; the constraint rebuild inside it records nothing,
  since the new tables' history is installed only at the end of the switch.

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

## Switching tips

Following a bookmark onto a tip whose path passes a kept seal restores only
the stored rows that changed since that seal, from history, and replays the
tip's own operations after it. Only rows that differ from the live ones are
written and projected, and an existence log indexes again only the ids those
rows or records past the seal name. Switching from `main` to a branch two
seals back and returning so costs time proportional to the rows changed since
the seal, not to the table. The newest kept seal on the new path that this
node finalized and kept under the installed schema is used.

Without such a seal, or for a scope in a constraint group or whose replay
base lies off the path, the switch rebuilds that scope from its replay base.

## Limits

- Returning to a path finalizes again, at the next maintenance step, the
  seals the other path did not pass; finalizing scans the scopes a seal
  changes.
- A seal's state is held only once every scope this node holds has it; a
  peer that reads fewer scopes serves part of it. A seal kept before a schema
  change that adds a table therefore stays kept but is no longer reported
  held, and no branch opens on it.
