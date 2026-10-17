# 0075: Generations as sealed forks

Status: accepted design; phases 13, 15 and 16 implemented; phases 14, 17 and 19
under repair/requalification, with fresh independent acceptance pending.
Date: 2026-10-15; revised the same day for tips and branches, and
for authority, finality, recovery and the removal of drafts; 2026-10-17 for
the recovery's position range and the in-sync rule for ordinary seals;
2026-10-18 for target-path restoration and scoped frontier placements.
Covers [phases 13–17 and 19](../features_09/README.md#generations-as-sealed-forks)
of features_09. Supersedes, as those phases land, the transitions and
baselines of [0068](0068-adopting-newer-generations.md), the horizon's
guarantees and [0074](0074-owner-signed-recovery-cuts.md)'s cuts.

## Context

Schema changes, compaction, staged publication and stuck recovery each grew their own
mechanism: signed transitions and lineage caps, re-add baselines, the
mergeable horizon with participants and deadlines, and cut records with
fencing ranges. Credentials made it worse: they name a manifest, so every
generation needs an issuer to renew every device, and permissions belong to
devices through per-table pseudonyms. Issuers copy the publisher key, so a
stolen issuer can only be excluded by a new domain, and the publisher key has
no backup.

## Decision

### One domain, identities and nodes

A domain is the owner identity's key and the application. It owns members,
credentials, grants by table and field name, reader key epochs and the
coordinator, and never forks.

- **Identities hold permissions.** An identity is a signing key supplied by
  an `IdentityProvider`. The provider signs node certificates, revocations
  and the statements its authority requires; it never exports the private
  key. Core never sees how an identity is stored; a passkey module is one
  provider.
- **Nodes sign.** Each node generates its own key. Its identity signs a node
  certificate that adds no rights and carries one author number from the
  identity's granted range. A chain runs publisher → issuer identities →
  identity → node. Row identities derive from the node's number, so a node
  needs no issuer to create rows and nodes of one identity never collide.
- **Self-enrollment.** A node of an identity that already holds grants
  enrolls by obtaining a node certificate from its provider. Invitations
  grant to an identity.
- **The publisher key stays with the owner.** Issuers sign manifests, seals
  and bookmark moves through their own chains; no issuer, invitation or pair
  request carries the publisher key. A provider that derives the owner's key
  is its backup.
- **Revocation** of an identity ends every chain through it, so all its
  nodes; revoking a node certificate ends only that node. Coordinator
  control certificates name trusted identities; the in-sync set still counts
  nodes.
- **Pseudonyms are removed.** The node key signs every frame, existence
  record and checkpoint. Frames name their node to every device that sees
  them; an application needing complete isolation uses separate domains.
- **Grants and reader keys outlive generations.** Grants name tables and
  fields ([0067](0067-names-are-identities.md)); reader keys follow key
  epochs only. A device verifies a publisher-rooted generation and adopts it
  without renewal.

### Tips, seals and bookmarks

- **A tip** is an open generation: one schema and the frames written to it.
  **A seal** is a signed statement that ends a tip once, at a per-author
  frontier, and either `continue`s into one successor tip, identified by its
  digest, or `close`s it. Trusted identities and the owner open further tips
  on any seal they hold; an issuer also opens a migration successor on an
  authenticated closing seal, with another schema and its line's signed
  minimum. Openings remain authenticated when their parent seal loses, but
  only canonical, unrevoked ancestry resolves actively. Migration openings
  start another author line and take no late frames. A seal changes no row. Every device holds the
  complete graph of seals, tips and bookmarks.
- **Bookmarks** are named, signed pointers to tips. Rocky defines none; an
  application follows the bookmark it names, by convention `main`. Issuers
  move bookmarks forward; trusted identities and the owner also create,
  delete and move them backwards or sideways.
- **Races resolve by authority.** Concurrent seals of a tip, or moves of
  one bookmark from one tip, resolve to the owner's, then a trusted
  identity's, then an issuer's, and among equals to the lower digest.
  Revoking an identity removes its statements and resolves again.
  Inspection lists losers as `main/1`, `main/2`, as jj does.
- **Peers on a branch.** A device writes only the tip its bookmark points
  to and adopts only what it follows. Frames reach only devices following
  their tip. A node's author log stays on one line of `continue` seals;
  another line uses another node of the same identity.
- **Coordination per tip.** Each tip has its own coordinated log, run by
  the trusted nodes following it; a seal of a coordinated tip is its log's
  terminal entry, and a move off an open tip carries that tip's `close`.
- **Late frames** beyond a frontier walk the `continue` seals to the open
  tip and apply there field by field. A `close`, or a move that leaves
  their line, returns them as held changes, offered on every device that
  holds and may read them.
- **Following and moving differ.** Changing the followed bookmark leaves
  source-line writes on their line, unsent until return. A move of the followed
  bookmark holds only frames visible at its source and excluded by its target.
  This cause persists through schema-switch journals and reopening. A backward
  or sideways switch restores only the target path's state, filtering each
  starting seal/base and frame along its own path to the target. Never-offered
  local bundles are voided for removal only when target-visible and incompatible
  along that path; no common-ancestor intersection crosses branches.
- **Compaction is local.** Devices finalize a sealed generation whenever
  they choose; the horizon is removed. A seal committed in its tip's log is
  final; elsewhere each `compact` call leaves the latest seals it names
  uncompacted, and a device that compacted at a losing seal rebootstraps.
  Existence logs stay and are compacted at seals.
- **Scoped readiness.** Trusted nodes coordinate their authorized scopes.
  Signed header placements settle inherited frontier positions in scopes outside
  whole-row Read/Custody grants, without payloads or fake applied state. Held
  scopes require their real frames; unproved partitions return `custody_required`.
  Entry storage refuses unauthorized scopes before acknowledgment.
  [Lazy compaction](../runtime/lazy-compaction.md#finalizing) defines verification
  and partition privacy; the replica is v32 and the live link v14 only.
- **Checkpoints sit at seals,** signed by issuers and listed in the graph;
  their bodies move on request.
- **Kept seals and views.** Nodes keep configured seals as history tables;
  read-only views show a kept seal or bookmark under the live tables' names.
  One live workspace per domain holds the followed tip, behind a workspace
  boundary.
- **A schema change is a seal.** Transitions, lineage, baselines and both
  [accepted limitations](../features_09/README.md#accepted-limitations) go:
  a split schema or a twice-run migration is a concurrent seal or a
  divergent move. A migrating change `close`s its tip.
- **No drafts.** Every tip is synced to its followers. A change is tried
  on a dev branch; an issuer publishes by sealing `main` and migrating the
  fresh parent, offline if it should be ready at publication.
- **Recovery** of a stuck tip is one owner `continue` seal that abandons
  the committed coordinated entries of the tip's log after a retained
  prefix through the exact commit it ends: the positions `c < p ≤ a`,
  whoever wrote them. The coordinator never learns a committed entry's
  author, so the seal names this position range rather than authors. Each
  frame signs its tip and position; a coordinated write carried across
  `continue` seals keeps its frames and gains its author's signed carry
  record naming the continuation position it committed at. Frames are
  located by that record when present. It is the one terminal entry committed
  without the in-sync set: once a log committed an entry, an ordinary seal
  comes from an in-sync node. Abandoned writes return to their authors as
  held changes, ordinary late frames still apply, bookmarks on the tip
  follow the seal, and cuts go. Every device activates a recovery only
  with the existing finality proof: the line's signed minimum of distinct
  trusted nodes acknowledging the exact replaced head plus its terminal.
  Pending candidates are durably relayed outside the graph; offline members
  keep their admitted state until proof arrives. Proof remains sufficient
  after coordinator retention. Equality of retained and replaced, including
  zero, is allowed to clear stuck membership or pending work.

### Private migration publication

A migration records its exact close, opening, bookmark move and local SQL
program before switching. Its schema catalog record contains no migration
program. The SQL transaction records completion and its immutable publication
frames together. Until release, the candidate exposes no opening, schema,
frames, checkpoints, bootstrap graph or discovery announcement.

A coordinated publisher on the successor relays only the exact signed close
through `rocky.sdk.staged-close.v1`. A source follower qualifies under ordinary
source-log catch-up, applied-state and membership rules using its own trusted
node. It journals the bytes separately from graph admission and recovery
candidates before submitting the terminal. The publisher releases its existing
candidate only with the existing finality proof for that exact close, verified
again under current revocations and the authenticated line minimum. Uncertain
replies preserve the stage and retry the same bytes, including after restart.

The terminal fixes the source log's committed extent. The signed closing
frontier fixes the state inherited by the migration. Source writes committed
meanwhile remain authenticated history and become held changes; they are not
recovery-abandoned entries. Successor readiness verifies the inherited frontier
and durable disposition of later frames, without weakening the applied-state
requirement for joining the source log. Published losing migrations retain their
statements and publication history, and their writes never merge into the winner
or become automatic re-authoring work.

Schema acquisition asks for an exact commitment only while an authenticated tip
requires it. Any eligible routed catalog holder may supply it, irrespective of
its followed tip. The SDK derives and checks the manifest in the local domain
context; the replica rechecks the requirement before insertion. Missing mandatory
path schemas leave the installed workspace read-only. Independent missing
historical schemas affect their views. Attempts are bounded and retried without
schema-count or lineage caps.

Migration history uses one descriptor endorsed by the opening identity, binding
the designated node's exact initial frames through bounded Merkle-sum proofs.
Its durable account applies on publishers, followers and keyless custodians;
ordinary history retains its 64 MiB limit. Endorsement follows SQL commit and
precedes release, independently of exact-close finality. No public bundle-member
list is introduced. [0071](0071-staged-migration-publication.md) specifies the
acquisition, audit, pruning and restart contract.

## Consequences

- One operation replaces four mechanisms; each phase removes the one it
  replaces in the same change, with no legacy reader
  ([0039](0039-single-current-formats.md)).
- Credential, chain, revocation, invitation, frame, existence-record and
  checkpoint formats each move to their next version in phase 13.
- Frames reveal their node within a domain. Rocky never needs a device
  inventory, and a cloned node key stays a
  [deferred optimization](../optimizations_on_demand/cloned-node-detection.md).
- Merging branches and rebasing are out of scope; a losing branch is kept,
  never joined.
- A branch costs devices that do not follow it only its graph records. A
  device switching lines consumes one author number per line and node.

## Alternatives rejected

- **Keep pseudonyms with node certificates.** Per-table keys hide the node
  from peers but need an issuer per table and node, which defeats
  self-enrollment. Separate domains give real isolation.
- **Share the publisher key with issuers.** Simpler signing, but no revoked
  issuer is ever excluded and the key has no owner-held backup.
- **Renew credentials per generation.** Keeps manifest-bound grants but
  makes every schema change depend on a reachable issuer.
- **One implicit tip per seal.** Simpler, but nothing could ever be closed:
  a migration or recovery would still take late writes.
- **Tips created only explicitly.** Late frames would wait, possibly
  forever, for a tip that may never open; the seal decides instead.
- **Late frames only one generation forward.** Every branch would shorten
  how long a device may stay offline.
- **One coordinated log across tips.** A branch's commands would contend
  with `main`'s head and load its trusted nodes.
- **Branches by any identity.** Would need schema authority below issuers
  and scope bases for partial readers; a separate domain is simpler.
- **Lower digest alone.** A malicious issuer could beat any rollback by
  varying its statements; ranking by authority first limits it to beating
  equals until revoked.
- **Drafts as unpublished tips.** Drafting froze or abandoned `main`'s
  writes and needed a second kind of tip; a branch and a fresh seal do
  both jobs.
- **Recovery as a `close` and move.** It would hold every ordinary write
  after the retained prefix, not only the abandoned entries.
- **Per-branch author logs.** A format change to every frame and existence
  record, where another node of the same identity needs none.
