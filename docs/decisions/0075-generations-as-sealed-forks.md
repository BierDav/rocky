# 0075: Generations as sealed forks

Status: accepted design; phase 13 implemented, phases 14–19 to follow. Date: 2026-10-15.
Covers [phases 13–19](../features_09/README.md#generations-as-sealed-forks)
of features_09. Supersedes, as those phases land, the transitions and
baselines of [0068](0068-adopting-newer-generations.md), the horizon's
guarantees and [0074](0074-owner-signed-recovery-cuts.md)'s cuts.

## Context

Schema changes, compaction, drafts and stuck recovery each grew their own
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
coordinated log, and never forks.

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

### Generations, seals and bookmarks

- **A generation** is one schema and one stretch of history. **A seal** is
  an issuer-signed statement that ends a generation at a per-author frontier
  and names the successor's schema; its digest identifies the successor. It
  changes no row. Every device holds the complete seal graph.
- **Bookmarks** are signed pointers to seals; `main` names the only writable
  generation. Issuers move bookmarks forward; trusted nodes and the owner
  may also move them backwards or sideways. Divergent moves from one seal
  resolve to the lower move digest; the losing seal stays a branch. In a
  coordinated domain every move of `main` is a log entry.
- **Late frames** beyond a frontier apply one generation forward along
  `main`, unless the seal runs a migration or `main` left their line; then
  they return to their author as held changes.
- **Compaction is local.** Devices finalize a sealed generation whenever
  they choose; the horizon is removed. Existence logs stay and are compacted
  at seals.
- **Kept seals and views.** Nodes keep configured seals as history tables;
  read-only views show a kept seal or bookmark under the live tables' names.
  One live workspace per domain holds `main`, behind a workspace boundary.
- **A schema change is a seal.** Transitions, lineage, baselines and both
  [accepted limitations](../features_09/README.md#accepted-limitations) go:
  a split schema or a twice-run migration is a divergent move.
- **Drafts** are unpublished seals in the live workspace; publishing signs
  the seal and moves `main`. A sandbox is a draft never published.
- **Recovery** of a stuck domain is an owner seal at the retained prefix and
  a `main` move; cuts go.

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

## Alternatives rejected

- **Keep pseudonyms with node certificates.** Per-table keys hide the node
  from peers but need an issuer per table and node, which defeats
  self-enrollment. Separate domains give real isolation.
- **Share the publisher key with issuers.** Simpler signing, but no revoked
  issuer is ever excluded and the key has no owner-held backup.
- **Renew credentials per generation.** Keeps manifest-bound grants but
  makes every schema change depend on a reachable issuer.
