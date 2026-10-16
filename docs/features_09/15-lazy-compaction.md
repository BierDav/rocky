# 15. Lazy compaction

Status: in progress. The [final-seal proposal](#proposal-final-seals) was
accepted and is implemented; slices 1 to 4 are implemented. See the
[evidence](15-lazy-compaction-evidence.md) and the
[guide](../runtime/lazy-compaction.md).
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
  device compacts with one seal left uncompacted, a concurrent seal that
  wins (higher authority, or among equals the lower digest) arrives, and the
  device ends on it without rebootstrapping;
  with none left, it rebootstraps from an issuer's checkpoint listed in the
  graph, and without one by replaying frames.
- Inspection shows each seal's checkpoints with their signers.
- No horizon code, format or SDK operation remains.

## Proposal: final seals

Today members cannot tell a final seal from one that can still lose. The
coordinator commits a terminal entry at once, without acknowledgments, and
signs nothing a device can carry; in-sync nodes' signed requests stay in its
Raft state. Members receive only the `SignedSeal`, identical whether or not
it ended the log, and they verify no coordinated entry: trusted nodes check
`EntryPeers::committed` evidence against the coordinator's `LogHead`.

**Evidence.** After a terminal commits, each trusted node that was in the
log's in-sync set reads `LogHead::terminal` and signs a terminal
acknowledgment: the tip's log `(scope, tip)`, the terminal's position and
running hash, the seal digest and the continuation. A node signs at most one
per log. It submits it to the coordinator, which keeps the acknowledgments
with the terminal as it keeps an owner-signed cut. Any trusted node that
holds enough of them publishes a seal finality record, a new graph statement
naming the seal and carrying the acknowledgments with their signers'
certificate chains. A member verifies, with no coordinator call, that every
signer is a distinct, unrevoked node of an identity holding the domain's
trusted grant, that each signed the same seal, and that their number reaches
the line's signed minimum: the opening's `minimum_in_sync`, or for the
genesis line a minimum the owner signs into the domain's credentials. A seal
with a verified record is final.

**The owner's seal.** Resolution becomes: a final seal, then the owner's,
then a trusted identity's, then an issuer's, then the lower digest. The
coordinator commits one terminal per log and trusted nodes acknowledge only
the committed one, so two final seals of one tip need trusted nodes to sign
falsely. That is a fault the owner answers by revocation, which removes
their acknowledgments and so the finality. An owner seal is admitted as a
terminal like any other. Phase 19's recovery seal stays the one terminal the
coordinator commits without the in-sync set; trusted nodes acknowledge it
lazily as they return, and it becomes final then. An owner seal that arrives
after another seal ended the log was never committed: publishing it fails
`sealed`, received it stays in the graph as a losing seal, and the owner
moves the bookmark off the final seal's continuation instead. Until a
record arrives, the graph resolves by authority as today, so a device
compacts such a tip only within its count of seals left uncompacted. Owner seals
outrank every seal except a final one; phase 19's brief needs that wording.

**Formats.** A new tip statement (finality record) changes the graph's
statement set, so the link (v7) and replica file (v23) versions move. The
coordinator's `LogHead::terminal` carries acknowledgments and its Raft store
(format 8) moves; `rocky-core`'s `LogTerminal` and `DomainLog` gain the
acknowledgment action. The genesis line's minimum is a new field of the
owner-signed domain credentials. `SealStatement`, frames and existence
records do not change.

**Alternative, not proposed.** A coordinator-signed commit would need one
signature, but would make the coordinator an authority members trust, which
the domain model avoids.

## Prerequisites checked

Checked against code on 2026-10-16:

- **Seals and the tip graph** hold: `SealStatement` (format 1) and
  `TipGraph`, persisted in `secure_tip_statements`, now resolved by
  authority. No statement marks a seal final.
- **Frames name their tip** (row frame 10, `Extensions.tip`), checked on
  receipt.
- **Existence logs name tips** for records (existence record 4,
  `Statement.tip`). Existence bases name a tip only through the
  `SignedHorizon` they depend on, which this phase removes; slice 2 must give
  them a seal.
- **Checkpoints name tips** (`CheckpointStatement.tip`, checkpoint 13), but
  not a seal: their frontier is per-author log positions, they still carry
  the horizon and coordinated cut bindings, and no checkpoint record is a
  graph statement. Slice 3's work.
- **Fails: switching to another line's tip needs retained history.**
  `rebuild_tip_workspace` refuses a device with pruned history or a registry
  imported from a checkpoint, which includes every SDK issuer device. Slice 3's
  rebootstrap from a checkpoint at a seal is the remedy; it is recorded as
  open in [phase 14](14-seals-and-bookmarks-evidence.md#what-remains-before-phase-14-is-implemented).
- **Fails: no final seal evidence**, the proposal above.
