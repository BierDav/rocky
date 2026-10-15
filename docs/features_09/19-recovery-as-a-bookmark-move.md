# 19. Recovery as a seal

Status: proposed; not started. Revised 2026-10-15: one owner `continue`
seal replaces the `close`, opening and bookmark move.
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
  author, what collection retrieved. It also lists the abandoned
  coordinated entries, the log positions after the retained prefix `c`, by
  author and position. Only the owner may sign such a list.
- **Ordinary writes are untouched.** Frames beyond the frontier walk to the
  continuation as late frames, as after any `continue` seal. Bookmarks on
  the tip follow the seal; no move is needed.
- **Abandoned entries are not late frames.** A listed frame never applies
  in the continuation; its author's device receives it back as a held
  change.
- **The coordinator.** An owner seal is the one terminal entry the
  coordinator commits without the in-sync set. It ends the stuck log; the
  continuation's log starts with an empty in-sync set that trusted nodes
  refill lazily as they return. Owner seals outrank every other seal of the
  tip ([phase 14](14-seals-and-bookmarks.md)).
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
