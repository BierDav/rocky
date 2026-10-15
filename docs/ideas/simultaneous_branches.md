# Simultaneous branches on one node

Status: idea; not planned. Depends on [features_09
phases 14–17](../features_09/README.md#generations-as-sealed-forks), extends
[additional live workspaces](additional_live_workspaces.md) and shares
[decision 0075](../decisions/0075-generations-as-sealed-forks.md).

## Context

After phases 14–17 a node has one live workspace, on the tip its followed
bookmark points to; every other seal is reachable read-only
([kept seals](../features_09/16-kept-seals-and-views.md)).

Two different wants are easy to confuse:

- **Peers on a branch (B).** A subset of devices follows a dev bookmark and
  writes another schema, while the rest keep writing `main`. Each device is
  still on exactly one generation; only the domain spans two.
- **One node on two branches (A).** The same node writes `main` and a dev
  branch at once — the "sandbox or draft on the same device that keeps
  working on `main`" case of
  [additional live workspaces](additional_live_workspaces.md).

B needs no second workspace; [phase 14](../features_09/14-seals-and-bookmarks.md)
provides it with tips, following, per-tip sync and coordination, and a
node per line. A needs all of that plus everything
[additional live workspaces](additional_live_workspaces.md) costs. This
record covers A.

## What already points this way

- Stored rows are schema-independent: `secure_rows(scope,row,fields,stamp)`
  holds rows by identity, and the live tables are a projection rebuilt from
  them (`rebuild_projection`, `reproject`).
- Two physical table sets already coexist: a schema change projects
  `secure_shadow_*` tables beside the live ones before switching, so
  "two live table sets" is a proven, if transient, mechanism.
- Grants name tables and fields and reader keys follow epochs, so a second
  schema needs no new key material and no issuer round trip.
- Frames name their tip, and every device holds the complete graph of
  seals, tips and bookmarks.

## The one-author-log invariant

`secure_operations` has `UNIQUE(author,scope,sequence)`, and a second
operation at a held position is refused as an author-log fork. One author
therefore has one log per scope, which is why **one node cannot author two
branches of the same scope** without per-branch logs.

This is the whole difficulty of A, and the reason B is cheap: a device on
one branch has one author log per scope. Phase 14 lets a device that changes
line write under another node of its identity. A device writing `main` and a
dev branch at once could do the same with two nodes, one per workspace,
instead of keying the author log by `(author, scope, tip)`.

## What a node must hold twice

- The installed typed descriptor: one row today (`secure_typed`,
  `id INTEGER PRIMARY KEY CHECK(id=1)`), one schema, domain and reader set.
- The stored rows for scopes both branches write.
- Each scope's checkpoint chain, which is per scope today.
- The projection cursor per workspace, and the reservation state of ids.
- Observers and the published revision, which are one stream today.

`Inner` mirrors this assumption: one `definition`, `manifest`, `names` and
`typed`, with grant caches invalidated on a definition swap.

`additional live workspaces` covers the workspace mechanics (history-first
writes, replay on open, WAL atomicity per file). A adds the generation
dimension to each of the above and the physical-name problem: live tables
carry their plain SQL names, so the second workspace's tables must be
renamed or live in another file or an attached database.

## Format changes

One current version per format, so each of these is a replacement, not an
addition:

- Frame and seal frontier: an author's sequence within a branch, not only
  within a scope.
- Row and checkpoint keys: include the generation for scopes two branches
  write.
- Checkpoint `frontier` and `identities`, and existence records'
  `sequence`/`previous`, follow the same per-branch position.
- The encrypted replica file identity.

Branch-aware admission and bookmark-scoped move resolution are part of
[phase 14](../features_09/14-seals-and-bookmarks.md).

## Costs

- Creating a workspace builds its live tables from history, proportional to
  the data, unless a file-system clone is available.
- A forgotten workspace pins history until it is deleted; SQLite shares one
  writer across workspaces.
- Observers, notifications and status become per workspace.

## Open questions

- How a connection binds to a writable non-default workspace on SQLite,
  where tables cannot share plain names in one file.
- Whether a branch is a workspace of its own or a branch dimension inside
  one workspace.
- Whether two nodes per device, one per workspace, avoid every per-branch
  format change listed above.
