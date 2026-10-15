# 0075: Generations as sealed forks

Status: accepted design; phase 13 implemented, phases 14–17 and 19 to
follow. Date: 2026-10-15; revised the same day for tips and branches, and
for authority, finality, recovery and the removal of drafts.
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
  on any seal they hold; those take no late frames. A seal changes no row. Every device holds the
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
- **Compaction is local.** Devices finalize a sealed generation whenever
  they choose; the horizon is removed. A seal committed in its tip's log is
  final; elsewhere each `compact` call leaves the latest seals it names
  uncompacted, and a device that compacted at a losing seal rebootstraps.
  Existence logs stay and are compacted at seals.
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
- **Recovery** of a stuck tip is one owner `continue` seal that lists the
  abandoned coordinated entries; ordinary late frames still apply, and cuts
  go.

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
