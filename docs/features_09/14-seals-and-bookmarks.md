# 14. Seals, tips and bookmarks

Status: proposed; not started. Revised 2026-10-15 for tips and branches.
Owner: `rocky-credentials` seal, tip and bookmark statements; `rocky-secure`
admission, sync and late frames; `rocky-coordinator` per-tip logs;
`rocky-sdk`, the JSON API and `sdks/kotlin` API. Depends on
[phase 13](13-identities-and-domain-credentials.md). Follow the
[execution contract](README.md#agent-execution-contract). Shares decision
record 0075.

## Inputs and objective

Read [the generation model](README.md#generations-as-sealed-forks),
[coordinated log](../runtime/coordinated-log.md),
[coordinated writes](../runtime/coordinated-writes.md) and
[mergeable horizon](../runtime/mergeable-horizon.md).

After this phase an issuer seals the tip a bookmark points to and moves the
bookmark on; a trusted identity opens a branch tip that a few devices follow
while the rest never see it. Devices follow the bookmark the application
names. Adopting a seal is metadata: it costs every device the same small
amount, whatever the domain holds. Compaction follows in
[phase 15](15-lazy-compaction.md), schema changes in
[phase 17](17-schema-changes-through-seals.md).

## Settled

- **Tips.** A tip is one schema and the frames written to it. Frames name
  their tip; row identities carry over unchanged. A tip's state is its
  seal's sealed state plus its own frames. The domain starts with one
  genesis tip.
- **The seal.** An issuer or trusted identity with whole-scope authority
  signs a statement naming the tip it ends, that tip's frontier as a
  sequence mark per author, and either `continue(schema)` or `close`. A tip
  is sealed once; concurrent seals of one tip resolve to the lower seal
  digest on every device, and the other has no effect. `continue` opens the
  one successor tip, identified by the seal's digest; until phase 17 its
  schema is the parent's.
- **Opening tips.** Trusted identities and the owner open further tips on
  any seal, for branches and drafts. Such a tip starts from the sealed state
  and never takes late frames. Its opening names the minimum in-sync size of
  its coordinated log, by default the domain's. A dev branch from current
  data is one action: seal the followed tip with `continue`, move its
  bookmark, open the branch tip on the same seal and point a bookmark at it.
- **The graph.** Seals, tip openings and bookmark moves are small; every
  device keeps and syncs all of them, so the graph is always complete. Frames
  reach only devices that follow their tip.
- **Bookmarks.** A bookmark is a named, signed pointer to a tip; Rocky
  defines none. The domain's creator makes the first one on the genesis tip,
  and an application names the bookmark it follows, by convention `main`. A
  move names the tip it moves from and the tip it moves to; deleting is a
  move to nothing.
- **Who moves.** Issuers move a bookmark forward along `continue` seals.
  Trusted identities and the owner also create and delete bookmarks and move
  them backwards or sideways. Two moves of one bookmark from the same tip
  resolve to the lower move digest on every device. A device without this
  authority never moves a bookmark.
- **Following.** A device writes only the tip its followed bookmark points
  to; a bookmark on a sealed tip is read-only until it moves. It adopts what
  it follows, never what a peer runs: frames of a tip it does not follow are
  neither offered to it nor adopted, and are refused if they arrive.
  Switching bookmarks rebuilds the live workspace at the new tip. A device
  that stops following a tip may drop its local frames and state.
- **One line per node.** An author log continues only along one line of
  `continue` seals, since peers of another tip never hold its frames. A
  device that follows a bookmark onto another line, by switching or after a
  backward move, writes there under another node of its identity, enrolled
  without an issuer ([phase 13](13-identities-and-domain-credentials.md)),
  and returns to its earlier node when it returns to that line.
- **Late frames.** A frame of tip T beyond T's frontier walks the chain of
  `continue` seals from T to its open tip and applies there field by field,
  ignoring fields a seal on the way removed. A `close` on the way returns it
  as a [held change](04-held-changes.md). A backward or sideways move closes
  the tip it leaves, if still open, and returns that line's writes since the
  common seal as held changes. Held changes are offered on every device that
  holds and may read them; whoever the application chooses re-authors them
  as new writes. A relay never re-signs another author's frame.
- **Coordinated writes per tip.** Each tip has its own coordinated log at
  the coordinator; its in-sync set is the trusted nodes that follow the tip.
  A seal of a coordinated tip and every move of a bookmark off it are
  entries in that tip's log, so neither diverges there. Sealing covers every
  entry committed before it; a pending entry aborts and retries in the
  continuation, or returns as a held change after a `close`. Forwarded
  commands go to a trusted node that follows the tip, so a branch's commands
  never reach other tips' trusted nodes. A tip no trusted node follows
  refuses coordinated writes with an expected error.
- **One format version.** Frames, existence records, checkpoints and the
  encrypted replica file identity name the tip instead of the manifest and
  move to their next version; seal, tip and bookmark statements start at
  their first. Earlier versions are refused.
- **Called by the developer.** Rocky runs no timer. `seal()` seals the
  followed tip with `continue` and moves its bookmark; sealing with `close`,
  opening tips and moving or deleting bookmarks are separate calls. Status
  reports the followed bookmark and tip, operations since the last seal, the
  evidence compaction would release and each device's last seen frontier.
- **Inspection for developers.** An API returns the graph, bookmarks with
  their moves, and which tips and seals each node follows or keeps. It is a
  debugging and development tool, not an application feature.

## Ordered slices

1. Seal, tip and bookmark statements; frames naming their tip; the format
   versions.
2. Admission and sync by tip: following, refusal without adoption, offers
   and frontiers filtered by tip, a node per line; adoption on discovery
   removed.
3. Late frames along `continue` seals; `close` and leaving a line as held
   changes offered on every holder.
4. Per-tip coordinated logs in `rocky-coordinator` and `rocky-secure`; seals
   and moves as log entries.
5. `seal`, `close`, tips, bookmarks, `follow`, local drop, status and
   inspection in `rocky-sdk`, the JSON API and Kotlin.
6. Guides: a seals, tips and bookmarks guide replacing the horizon's place
   in the docs index.

## Completion gate

- Adopting a seal changes no row and takes the same time with 10 and
  10,000 rows.
- A device offline across two `continue` seals returns; its writes apply on
  every device. Offline across a `close`, its writes are offered as held
  changes on every device holding them; one re-authoring converges.
- Two issuers seal one tip at once; every device ends on the lower seal's
  continuation with the same rows.
- A trusted node opens a branch tip on `main`'s latest seal and points `dev`
  at it. Two devices follow `dev` and write; devices on `main` receive none
  of its frames. One switches back to `main`, drops `dev`, and its writes
  on `main` apply everywhere.
- A trusted node moves `main` back to an earlier seal; writes of the
  abandoned line return as held changes, and every device ends with the
  same rows.
- A coordinated write on `dev` commits through `dev`'s log while `main`'s
  in-sync set sees no entry. A coordinated write committed before a seal is
  part of the continuation; a pending one retries there and commits once.
