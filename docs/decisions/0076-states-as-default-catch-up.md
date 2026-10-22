# 0076: States as the default catch-up and automatic compaction at final seals

Status: proposed with [features_10](../features_10/README.md); not yet accepted.
Date: 2026-10-22.
Covers [phase 2](../features_10/02-states-as-default-catch-up.md). Revises
the "called by the developer" rule of [0075](0075-generations-as-sealed-forks.md)
for compaction at final seals and the migration's closing seal; seals,
branches and bookmarks stay application calls.

## Context

A device that is far behind, or joining, receives every frame since its
frontier, verified, decrypted, admitted and projected one row at a time, at
about 430 rows/s on the reference machine. Checkpoint import moves signed
pages of 256 rows at more than ten times that rate, but a checkpoint exists
only at a seal, after an application called `compact`, and in the
migration workload no seal exists at all. The
[review](../features_10/00-sync-performance-review.md) measured a 100,000-row
migration moving 570 MB of frames for 5 MB of rows.

## Decision

- A range page may advise a requester that a base at a seal of the
  sender's path is cheaper than the frames it lacks. The rule is fixed:
  every log of the requester is at or below the base and at least 64
  covered frames are lacking. The receiver imports through the existing
  checkpoint import and pulls the tail. `behind` is unchanged.
- The issuer that finished publishing a migration seals the successor tip
  with `continue` and the same schema, compacts at `keep = 0` and lists
  the checkpoint, in the same sync pass. Bundles remain the migration's
  truth for devices with concurrent writes.
- Every device compacts at a final seal of its path without an application
  call, in the sync pass that sees the seal final. `compact(keep)` stays
  for seals that are not final.

## Consequences

- A migration ends with one more seal than before; every follower
  finalizes it. Until [phase 3](../features_10/03-incremental-finalization.md)
  that costs a rebuild of changed scopes; after it, the excluded
  operations only.
- Compacted devices hold surviving rows twice until retention releases
  frames; the default policy still never releases a sole copy.
- An issuer on an uncoordinated tip that compacts at a losing seal rebuilds
  from its own retained frames; it loses no data.
- The link format moves one version for the advice field.
