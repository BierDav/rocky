# 0077: Compact row representation

Status: proposed with [features_10](../features_10/README.md); not yet accepted.
Date: 2026-10-22.
Covers [phase 4](../features_10/04-compact-row-representation.md). Keeps
[0067](0067-names-are-identities.md): names stay identities; only their
encoding on the wire and at rest changes.

## Context

A stored secure row record of seven 50-byte fields measures 1.35 KB and
its frame share 3.1 KB: field maps keyed by 32-byte commitments, per-field
stamps carrying 32-byte node keys, and a 230-byte signed creation proof on
every birth. Bytes decrypted, decoded and written are the member stage's
dominant cost ([measurements](../features_10/00-sync-performance-review.md#measurements)).

## Decision

- Frames name fields by their ordinal in the source tip's canonical
  schema; files key stored fields by an interned id per name, allocated
  once and never reused.
- Secure row stamps carry the domain author number instead of the node
  key; merge order is `(lamport, author, sequence)`.
- An ordinary birth's creation statement is derived from its frame and
  bound by the frame signature; its provenance commitment is computed over
  the same canonical bytes as the signed statement, so admission evidence
  is unchanged. Carried rows keep the original author's signed proof.
- Checkpoint pages carry a field legend once per page.
- Row frame, checkpoint, replica and link formats move one version each;
  no earlier version is read.

## Consequences

- A seven-field row costs at most 500 B in a frame and 300 B at rest; the
  100k fixture's member file shrinks from 1.0 GB to at most 200 MB.
- Imported checkpoint rows no longer carry an author signature per row;
  they were already trusted on the page signer's authority and never
  became admission claims.
- Field ordinals are per schema; every mapping goes through the source
  schema that admission already requires to be installed.
