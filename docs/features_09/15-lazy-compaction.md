# 15. Lazy compaction

Status: proposed; not started.
Owner: `rocky-secure` finalization, existence bases, checkpoints and
pruning; `rocky-sdk` maintenance. Depends on
[phase 14](14-seals-and-bookmarks.md). Follow the
[execution contract](README.md#agent-execution-contract). Shares decision
record 0075.

## Inputs and objective

Read [the generation model](README.md#generations-as-sealed-forks),
[mergeable horizon](../runtime/mergeable-horizon.md),
[existence logs](../runtime/existence-logs.md) and
[encrypted checkpoints](../runtime/encrypted-checkpoints.md).

A seal fixes what the sealed tip means; it does not require anyone to
rewrite storage. After this phase each device compacts on its own schedule,
and the mergeable horizon is gone.

## Settled

- **Compaction is local and optional.** A device finalizes a sealed tip
  at its seal's frontier whenever it chooses: it drops tombstones,
  hidden losers and cascade orphans, keeping register stamps and identity
  marks. A device that has not compacted reaches the same outcome for every
  late frame, however many `continue` seals it walks, from the evidence it
  still holds.
- **Incremental.** A table or partition unchanged since the previous seal
  keeps its base; it is not recomputed, rebuilt or notified.
- **Existence logs stay.** A device holding a log's records through the
  frontier compacts them into an existence base itself. Issuer-signed bases
  and checkpoints serve only devices that fetch a base instead of records:
  joiners and holders of part of the data. Checkpoints and bases name
  their tip; a device imports only those of tips it follows or keeps.
- **Shallow history.** A compacted tip's state is gone unless the node
  keeps its seal ([phase 16](16-kept-seals-and-views.md)). The graph stays
  complete; only the state is missing. A tip the device no longer follows
  is dropped whole ([phase 14](14-seals-and-bookmarks.md)).
- **The horizon is removed:** declarations, participants, acknowledgments,
  deadlines, `horizon_pending`, competing statements, completed
  certificates, the finalized basis on inserts and the `horizon` SDK
  operation. Its finalization and existence-base code moves here.

## Ordered slices

1. Local finalization of a sealed tip, per table and partition.
2. Existence logs compacted locally; issuer bases for joiners and partial
   holders.
3. Removing the horizon, its formats and its tests.
4. Guides: mergeable horizon replaced, existence logs, checkpoints.

## Completion gate

- The plateau probe of the [horizon guide](../runtime/mergeable-horizon.md#evidence)
  holds with seals: 64 insert/delete cycles leave no tombstones, stubs or
  existence records after compaction.
- Two devices, one compacted and one not, apply the same late frames,
  across two `continue` seals, to the same rows.
- A device that holds only a child partition imports the issuer's existence
  base and resolves the same orphans as a device holding the parent.
- Sealing after changes to one table leaves every other table's base,
  observers and sessions untouched.
- No horizon code, format or SDK operation remains.
