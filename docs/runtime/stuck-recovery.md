# Stuck recovery

Phase 19's final qualification is recorded in its
[evidence](../features_09/19-recovery-as-a-bookmark-move-evidence.md).

A tip's [coordinated log](coordinated-log.md) is stuck when no node of its
in-sync set can append any more: the coordinator knows the commit, but no
reachable trusted node holds it. Recovery first collects what any device
still holds. What nobody holds is abandoned only by one owner `continue`
seal of the tip, the recovery seal ([phase 19](../features_09/19-recovery-as-a-bookmark-move.md),
[decision 0075](../decisions/0075-generations-as-sealed-forks.md#tips-seals-and-bookmarks)).
Only the stuck tip's log is involved; other tips' logs never see it.

## Collect first

[Coordinated writes](coordinated-writes.md#catch-up) rebuild missing
committed entries from complete bundles any reachable device retains,
members included, and from those this node holds itself. Entries are
checked against the coordinator's hash window. A node that catches up
rejoins without any signature.

## The recovery seal

A recovery seal is a `continue` seal (`SealStatement`) whose `recovery`
names two points of the tip's log:

| Field | Meaning |
| --- | --- |
| `retained` | The end of the prefix that stays: position `c` and the running hash after it |
| `replaced` | The exact commit the seal ends: position `a` and its running hash |

It abandons every committed entry of that tip's log at a position `p` with
`c < p ≤ a`, whoever wrote it. The coordinator never learns a committed
entry's author, so the seal names the positions. Each frame signs its tip
and position; a coordinated write carried across `continue` seals keeps its
frames and gains its author's signed
[carry record](coordinated-writes.md#carry-records) naming the continuation
position it committed at. Frames are located by that record when present,
which identifies them on every device. Only the owner signs a
recovery seal; one endorsed through any chain is refused. Owner seals
outrank every other seal of the tip, except one already
[final](seal-finality.md).

Its frontier is the proposer's, per author, after collection: frames
beyond it walk to the continuation as late frames, as after any `continue`
seal. Frames of abandoned entries never apply, wherever they sit.

## At the coordinator

`DomainLog::recover` (`Action::Recover`) carries the encoded seal. The
coordinator commits it as the log's terminal entry only when:

- the domain's control authority signed it directly;
- it names this log's tip, and `replaced` equals the commit and running hash;
- `retained` chains through the hash window to the commit; a prefix ending
  before the window is endorsed by the owner's signature alone.

It is the one terminal entry committed without the in-sync set. Once a log
committed any entry, an ordinary seal or a move's `close` is committed only
from a node of the in-sync set, which holds every committed entry; a
trusted node outside it catches up and joins first. So nothing but the
owner's recovery ends a stuck log. A retry of the committed recovery
answers the same commit; another terminal is `Sealed`; a head that moved
is `StaleHead`, and the owner collects and signs again.

## Recovering

| Kotlin | Rust | JSON `tips` action | Returns |
| --- | --- | --- | --- |
| `tips.recover()` | `Session::recover` | `recover` | the tip status |

The owner's device follows the stuck tip with its coordinated log
composed. It collects (`EncryptedReplica::recovery_proposal`), signs the
seal at its frontier and publishes it alone; every bookmark on the tip
without an explicit move follows the recovery's continuation. The call is
refused (`rejected`) on any other device or on a sealed tip.
`retained == replaced`, including zero, is allowed: a fully collected log
may still have full, unreachable membership or pending work.
`stale_recovery` means the log moved meanwhile. `recovery_pending` is
retryable: the exact signed action is durable and waits for commitment proof;
the application does not need to sign a second action.

A replica without the SDK builds the seal from `recovery_proposal` and
publishes it with `publish_tip_statements_through`.

## On every device

- **Admission.** Every device waits for a verified finality record at the
  line's owner-signed minimum of distinct trusted nodes. Acknowledgments
  bind position `a + 1` and `coordinated_chain(replaced.running, a + 1,
  seal_digest)`, as well as scope, tip and continuation. Trusted nodes inspect
  and acknowledge pending candidates without activating them. Candidates
  travel separately from the admitted graph. Offline members keep their last
  admitted state until proof arrives. A verified proof needs no coordinator
  call, including after the ended log leaves retention.
- **Genesis authority.** Provision the genesis line once with the owner's
  configured minimum (`Session::set_genesis_minimum`, JSON `genesis_minimum`)
  before recovery. An absent signed minimum refuses submission; the runtime
  never derives a weaker quorum from a coordinator response. Existing signed
  minima are never lowered. A composed client alone is not
  acknowledgment authority: a node needs an owner-direct trusted grant and
  node certificate. The SDK certifies installed trust grants for this purpose;
  a grant with no row permissions gives no row access.
- **Rollback.** In the transaction that admits the seal, every frame of an
  abandoned entry this device applied or staged becomes `abandoned`: held
  for re-authoring and offered as a [held change](held-changes.md) on every
  device that may read it, so it returns to its author. The scopes they
  changed are all rebuilt, including existence and constraints, and the
  log's entries there are removed. A frame
  arriving later is held on receipt. A later write to a row only an
  abandoned write created has no effect.
- **Handles.** A mutation handle of an abandoned write reports `abandoned`.
- **Pending writes.** An own write the recovery aborted, or one carried
  across it, was computed on state that included abandoned entries: it is
  held, never carried into the continuation. These calculations are settled
  in admission's transaction, including staged and already carried work;
  resolution of the old log is not required.
- **The continuation.** Before a trusted node takes anything of the
  continuation's log, it holds the sealed log through `retained`, verified
  against the window or, before it, against the retained hash. A branch
  opened on a seal that ended its log needs the entries that seal covered
  the same way.

## Durability

Publication journals the signed batch, flushed, before the coordinator
sees it, and admits it with its rollback in one transaction after the
terminal entry committed. A crash or a lost reply leaves the journal;
`settle_tip_statements` sends the same bytes again and admits them. A
received seal that this node cannot confirm, because it is outside the
in-sync set and cannot catch up yet, waits in the journal until a later
settlement through peers; it is never part of the graph meanwhile. The SDK
settles at the start of every sync pass.

## Checkpoints

With a tip graph a replay base sits only at a seal: no checkpoint is cut or
imported beside one, so no base can hold an abandoned write. A base
compacted at the recovery seal excludes them. A device whose compacted
seal lost rebootstraps at the winning seal as
[lazy compaction](lazy-compaction.md#rebootstrap) describes.

## Limits

- A member admits the owner's recovery without asking the coordinator. If
  another seal ended the log first and is not final yet, the member resolves
  to the recovery until that seal's finality record arrives.
- A node keeps at most 32 received batches waiting; a peer resends others.
- Collection rebuilds only complete retained bundles.
- An undecided own write is classified by the position it was staged at.

## Formats

Seal statements carry `recovery`; the coordinator's `Action::Recover`
replaced `Action::Cut`. Replicas journal pending batches in
`secure_pending_tips` and record entry positions in
`secure_entry_frames`, moved by carry records in the graph. Cut records, the cut chain, cut guards and the
checkpoint's coordinated binding are gone.

## Durable submission and unavailable bases

The exact signed seal and dependent batch are validated, journaled and flushed
before an external commitment call. Every retry keeps the same bytes, including
a lost reply followed by an unavailable head or a partially submitted batch.
Admission replaces its unresolved remainder atomically, preserving ownership.
An invalid batch is skipped without losing its submission bytes or blocking later
batches. Pending candidates are absent from graph inspection and graph pages.

A pruned base without a seal, or at a seal outside the followed path, cannot
serve reads or authoring. Graph metadata can still be admitted and queues a
pull of its target seal; the workspace waits for a verified replacement or
complete frame replay. This survives reopening and does not expose the old
checkpoint's abandoned effects. Checkpoint build/install and import completion
recheck the graph, so an ordinary checkpoint started earlier cannot cross this
boundary unnoticed.

Branch writes require the complete inherited seal frontier, including ordinary
frames, or a verified held seal/checkpoint. Ancestor serving is bounded by the
branch's inherited frontier and never splits a bundle.
