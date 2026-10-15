# 18. Drafts and sandboxes

Status: skipped (2026-10-15). Drafts are not needed: a developer tries a
change on a dev branch, and an issuer publishes by sealing `main` and
migrating, offline if it should be ready at publication
([phase 17](17-schema-changes-through-seals.md)). Every tip is synced to
its followers; none is unpublished. The brief is kept for reference only.
Owner: `rocky-secure` unpublished seals and tips; `rocky-sdk` and `sdks/kotlin` API.
Depends on [phase 17](17-schema-changes-through-seals.md). Follow the
[execution contract](README.md#agent-execution-contract). Shares decision
record 0075.

## Inputs and objective

Read [the generation model](README.md#generations-as-sealed-forks),
[kept seals and views](16-kept-seals-and-views.md) and
[staged migration publication](../decisions/0071-staged-migration-publication.md).

An issuer should migrate and test a new schema on real data before anyone
else sees it, and a developer should experiment on the domain without risk.
After this phase both are an unpublished tip in the node's one live
workspace. A draft is private to one node; a shared branch is a published
tip ([phase 14](14-seals-and-bookmarks.md)).

## Settled

- **A draft is an unpublished tip.** `draft(schema)` seals the tip `main`
  points to, with the kind [phase 17](17-schema-changes-through-seals.md)
  requires, and opens a tip on that seal, neither signed nor offered, and
  moves the node's live workspace there. The migration reads the parent through
  a read-only view and writes the successor; nothing leaves the node.
- **While drafting, the node writes the draft.** Its own writes go there,
  not to `main`; other devices keep writing `main` and see nothing.
- **Publishing moves `main`.** `publish()` signs the seal, the tip and the
  bookmark move together and offers the draft's frames with them, as staged
  migration publication does. Writes to `main` after the draft's frontier
  follow [phase 17](17-schema-changes-through-seals.md)'s late-frame rules.
- **A sandbox is a draft never published.** `discard()` deletes the draft's
  frames and rebuilds the live workspace at `main`, which costs a rebuild
  proportional to the data.
- **Refusals.** Every device refuses a frame naming an unpublished tip. A
  draft whose parent tip another issuer sealed refuses `publish()` and can
  only be discarded.

## Ordered slices

1. Unpublished seals and tips; the live workspace on a draft.
2. Publishing as a seal and `main` move.
3. Discarding and rebuilding at `main`.
4. `draft`, `publish` and `discard` in `rocky-sdk`, the JSON API and Kotlin.
5. Guides: drafts and sandboxes in generations.

## Completion gate

- An issuer drafts schema 9, runs its migration and queries it while other
  devices keep writing schema 8 and see nothing of the draft.
- Publishing moves every device following `main` to schema 9 with the
  migration's rows.
- A frame from a discarded draft, replayed to another device, is refused.
- A draft whose parent another issuer sealed refuses `publish()`.
