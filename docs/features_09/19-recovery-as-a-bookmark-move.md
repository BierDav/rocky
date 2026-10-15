# 19. Recovery as a bookmark move

Status: proposed; not started.
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
[Phase 14](14-seals-and-bookmarks.md) already moves `main` backwards and
returns abandoned writes to their authors. After this phase a stuck domain
recovers the same way and the cut mechanism is gone.

## Settled

- **Collect first** stays as in phase 10.
- **The owner seals at the retained prefix.** When no in-sync node can
  append, the domain owner signs a seal whose frontier is the retained
  prefix `c` and moves `main` to it; the coordinator commits the move and
  the in-sync set empties. Entries after `c` are beyond the frontier.
- **Abandoned entries are not late frames.** A frame of an abandoned log
  position never applies in the new generation; its author's device
  receives it back as a held change.
- **Rollback stays.** A device that applied abandoned entries rebuilds at
  the new generation's base, through [phase 15](15-lazy-compaction.md)'s
  finalization.
- **Removed:** `SignedCut`, the cut chain on devices, the cut entry at
  `a+1`, cut fencing ranges and checkpoint taint.

## Ordered slices

1. The owner's seal and `main` move at the coordinator and on every device.
2. Abandoned frames as held changes; rollback through the new base.
3. Removing cuts, their formats and their tests.
4. Guides: stuck recovery, coordinated log; decision 0074 superseded.

## Completion gate

- [Phase 10](10-stuck-recovery.md)'s gate passes with an owner seal and
  `main` move in place of a cut.
- No cut record, chain or taint remains in code or formats.
