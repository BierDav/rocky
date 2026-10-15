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
  at its seal's frontier whenever it chooses, once it holds every frame
  through that frontier: it drops tombstones,
  hidden losers and cascade orphans, keeping register stamps and identity
  marks. A device that has not compacted reaches the same outcome for every
  late frame, however many `continue` seals it walks, from the evidence it
  still holds.
- **Final seals.** A seal committed in its tip's coordinated log cannot
  lose a race and may be compacted at once. A seal of a tip without a log
  can still lose to a concurrent seal that arrives late, so each `compact`
  call names how many of the latest seals it leaves uncompacted; the
  developer trades storage against the chance of a rebootstrap.
- **Rebootstrap.** A device that compacted at a seal that later lost
  rebuilds at the winning seal: it imports an issuer-signed checkpoint of
  that seal if the graph lists one, else replays the frames through its
  frontier from peers, else waits for the owner's
  [recovery](19-recovery-as-a-bookmark-move.md). Its own writes follow the
  usual late-frame and held-change rules; Rocky never re-authors them.
- **Checkpoints sit at seals.** A checkpoint is the state of one seal,
  signed by an issuer; checkpoints at any other frontier are removed. Its
  record (seal, signer, digest and size) is a graph statement, synced to
  every device like seals, and inspection shows it beside its seal. The
  body moves only on request.
- **Incremental.** A table or partition unchanged since the previous seal
  keeps its base; it is not recomputed, rebuilt or notified.
- **Existence logs stay.** A device holding a log's records through the
  frontier compacts them into an existence base itself. Issuer-signed bases
  and checkpoints serve only devices that fetch a base instead of records:
  joiners and holders of part of the data. Checkpoints and bases name
  their seal; a device imports only those of tips it follows or keeps.
- **Shallow history.** A compacted tip's state is gone unless the node
  keeps its seal ([phase 16](16-kept-seals-and-views.md)). The graph stays
  complete; only the state is missing. A tip the device no longer follows
  is dropped whole ([phase 14](14-seals-and-bookmarks.md)).
- **The horizon is removed:** declarations, participants, acknowledgments,
  deadlines, `horizon_pending`, competing statements, completed
  certificates, the finalized basis on inserts and the `horizon` SDK
  operation. Its finalization and existence-base code moves here.

## Ordered slices

1. Local finalization of a sealed tip, per table and partition; final
   seals and the per-call count of seals left uncompacted.
2. Existence logs compacted locally; issuer bases for joiners and partial
   holders.
3. Checkpoints at seals as graph records; rebootstrap at a winning seal.
4. Removing the horizon, its formats and its tests.
5. Guides: mergeable horizon replaced, existence logs, checkpoints.

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
- A seal committed in its tip's log is compacted at once. Without a log, a
  device compacts with one seal left uncompacted, a concurrent seal with a
  lower digest arrives, and the device ends on it without rebootstrapping;
  with none left, it rebootstraps from an issuer's checkpoint listed in the
  graph, and without one by replaying frames.
- Inspection shows each seal's checkpoints with their signers.
- No horizon code, format or SDK operation remains.
