# 14. Seals and bookmarks

Status: proposed; not started.
Owner: `rocky-credentials` seal and bookmark statements; `rocky-secure`
admission and sync; `rocky-coordinator` log entries; `rocky-sdk` and
`sdks/kotlin` API. Depends on [phase 13](13-identities-and-domain-credentials.md). Follow
the [execution contract](README.md#agent-execution-contract). Shares
decision record 0075.

## Inputs and objective

Read [the generation model](README.md#generations-as-sealed-forks),
[coordinated log](../runtime/coordinated-log.md) and
[mergeable horizon](../runtime/mergeable-horizon.md).

After this phase an issuer seals the generation `main` points to, opening a
successor with the same schema, and moves `main` to it. Devices that decide
nothing follow a bookmark. Adopting a seal is metadata: it costs every
device the same small amount, whatever the domain holds. Compaction follows
in [phase 15](15-lazy-compaction.md), schema changes in
[phase 17](17-schema-changes-through-seals.md).

## Settled

- **The seal.** An issuer with whole-scope authority signs a statement
  naming its parent seal, the parent generation's frontier as a sequence
  mark per author, and the successor's schema. Its digest identifies the
  successor generation. Seal records are small and every device keeps and
  syncs all of them: the seal graph is always complete.
- **Frames name their generation.** Row identities carry over unchanged.
  A generation's state is its parent's state at the frontier plus its own
  frames.
- **Bookmarks.** A bookmark is a named, signed pointer to a seal. A move
  names the seal it moves from and the seal it moves to. `main` always
  exists; the generation it points to is the only writable one.
- **Who moves.** Issuers move a bookmark forward, to a descendant. Trusted
  nodes of the coordinated log and the domain owner may also move it
  backwards or sideways. In a coordinated domain every move of `main` is a
  log entry, so `main` never diverges there.
- **Divergent moves.** Two moves from the same seal resolve to the lower move
  digest on every device. The losing seal stays in the graph as a branch.
- **Following.** A device follows one bookmark for writing, `main` by
  default, and may follow others read-only. A device without issuer or
  trusted-node authority never moves a bookmark.
- **Late frames.** A frame of generation N beyond N's frontier applies in
  N's successor on `main`, admitted by any device that holds it. Once that
  successor is sealed too, or when `main` moves to a seal that does not
  descend from N, it is consumed without effect; its author's device
  receives it back as a [held change](04-held-changes.md). A relay never
  re-signs another author's frame.
- **Coordinated writes.** The domain keeps one coordinated log. Moving
  `main` covers every entry committed before the move; an entry pending at
  it aborts and retries in the new generation.
- **Called by the developer.** Rocky exposes `seal()` and runs no timer.
  Status reports operations since the last seal, the evidence compaction
  would release and each device's last seen frontier.
- **Inspection for developers.** An API returns the seal graph, bookmarks
  with their moves, and which seals each node keeps. It is a debugging and
  development tool, not an application feature.

## Ordered slices

1. Seal statements, generation identity in frames and the late-frame rule.
2. Bookmarks: moves, authority, divergence and following.
3. Bookmark moves as coordinated log entries.
4. `seal()`, bookmark following, status and inspection in `rocky-sdk`, the
   JSON API and Kotlin.
5. Guides: a seals and bookmarks guide replacing the horizon's place in the
   docs index.

## Completion gate

- Adopting a seal changes no row and takes the same time with 10 and
  10,000 rows.
- A device offline across one seal returns; its writes apply on every
  device. Offline across two, its writes come back to it as held changes.
- Two issuers move `main` at once; every device ends on the lower move's
  seal with the same rows, and the other seal is listed as a branch.
- A trusted node moves `main` back to an earlier seal; writes of the
  abandoned generations return to their authors as held changes, and every
  device ends with the same rows.
- A coordinated write committed before a move is part of the new
  generation; a pending one retries there and commits once.
