# 19. Recovery as a seal

Status: in repair; carry records implemented, final qualification remains; see the [evidence](19-recovery-as-a-bookmark-move-evidence.md)
and the [guide](../runtime/stuck-recovery.md). Revised 2026-10-15: one owner
`continue` seal replaces the `close`, opening and bookmark move; 2026-10-17:
the abandoned entries are a position range, and ordinary seals need the
in-sync set.
Owner: `rocky-coordinator` log entries; `rocky-secure` coordinated recovery;
`rocky-sdk` recovery flow. Depends on [phase 10](10-stuck-recovery.md) and
[phase 14](14-seals-and-bookmarks.md). Follow the
[execution contract](README.md#agent-execution-contract). Shares decision
record 0075.

## Inputs and objective

Read [the generation model](README.md#generations-as-sealed-forks),
[stuck recovery](../runtime/stuck-recovery.md) and
[decision 0074](../decisions/0074-owner-signed-recovery-cuts.md).

[Phase 10](10-stuck-recovery.md) abandons what no device holds with an
owner-signed cut that every device keeps in a chain beside the log.
[Phase 14](14-seals-and-bookmarks.md) already moves bookmarks backwards
and returns abandoned writes as held changes. After this phase a stuck tip
recovers with one owner seal and the cut mechanism is gone. Each tip has its own
coordinated log, so only the stuck tip's log is involved.

## Settled

- **Collect first** stays as in phase 10.
- **One owner `continue` seal.** When no in-sync node of a tip can append,
  the domain owner seals that tip with `continue`. Its frontier is, per
  author, what collection retrieved. It also names the abandoned
  coordinated entries: the positions of the tip's log after the retained
  prefix `c` through the commit `a` it ends, whoever wrote them. Refined
  from "by author and position": the coordinator never learns a committed
  entry's author. Each frame signs its tip and position; a coordinated
  write carried across `continue` seals keeps its frames and gains its
  author's signed carry record naming the continuation position it
  committed at. Frames are located by that record when present, so the
  range identifies every abandoned frame on every device. Only the owner may sign
  such a seal.
- **Ordinary writes are untouched.** Frames beyond the frontier walk to the
  continuation as late frames, as after any `continue` seal. Bookmarks on
  the tip follow the seal; no move is needed.
- **Abandoned entries are not late frames.** A listed frame never applies
  in the continuation; its author's device receives it back as a held
  change.
- **The coordinator.** An owner seal is the one terminal entry the
  coordinator commits without the in-sync set. Once a log committed an
  entry, an ordinary seal comes only from an in-sync node, so nothing else
  ends a stuck log; a trusted node outside the set catches up and joins
  first. It ends the stuck log; the
  continuation's log starts with an empty in-sync set that trusted nodes
  refill lazily as they return. Owner seals outrank every other seal of the
  tip except an already-final seal ([phase 14](14-seals-and-bookmarks.md)).
- **Proof-gated admission.** The existing finality record, at the configured
  owner-signed minimum of distinct trusted signers, binds the exact replaced
  head and terminal. The signed candidate and dependent statements stay durable
  outside the graph until proof verifies. This includes offline members;
  coordinator composition is optional for verification. A proof is accepted
  after the coordinator retires the log. No lower quorum is substituted.
- **Fully collected recovery.** Retained may equal replaced, including zero:
  unreachable full membership and pending work can still require owner recovery.
- **Rollback stays.** A device that applied abandoned entries rebuilds at
  the new generation's base, through [phase 15](15-lazy-compaction.md)'s
  finalization.
- **Removed:** `SignedCut`, the cut chain on devices, the cut entry at
  `a+1`, cut fencing ranges and checkpoint taint.

## Ordered slices

1. The owner's `continue` seal with abandoned entries, at the coordinator
   and on every device.
2. Abandoned frames as held changes; rollback through the new base.
3. Removing cuts, their formats and their tests.
4. Guides: stuck recovery, coordinated log; decision 0074 superseded.

## Completion gate

- [Phase 10](10-stuck-recovery.md)'s gate passes with an owner seal in
  place of a cut.
- Ordinary writes made after the retained prefix on the stuck tip apply on
  every device after recovery; only the abandoned coordinated entries
  return, as held changes, to their authors.
- Recovering a stuck branch tip leaves `main`'s log untouched.
- No cut record, chain or taint remains in code or formats.
