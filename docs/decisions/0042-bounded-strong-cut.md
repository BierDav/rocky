# 0042. A strong cut is named by its checkpoint base

Status: accepted. Date: 2026-10-08. Phase: [features_06 phase 11](../features_06/11-bounded-strong-cut.md).

## Context

A strong request seals its cut as an applied frontier and a state digest. The
result package listed every operation identity of that cut from sequence 1, 24
bytes each, and the executor refused a cut whose frontier sum would not fit a
1 MiB frame. Pruning did not lower that sum, so once a domain's authors had written about
43,690 operations in total, every strong command was refused for good.

The executor never needed those identities. It rebuilds the cut from its
checkpoint copy plus the retained operations after it
([checkpoints](../runtime/checkpoints.md#design-rationale)).

## Decision

The cut is named by the **checkpoint base plus the operations after it**. The
alternative, the frontier plus a digest of the covered operations, was
rejected: a verifier would need every covered operation, or a digest of
history that the trusted path does not keep, to check it.

- The request is unchanged. It names the cut by frontier and state digest.
- The package carries `cut_base`, the frontier of the checkpoint copy the
  executor rebuilt the cut from. It no longer lists identities. The covered
  operations are derived: per author, those after `cut_base` up to the
  request's frontier (`rocky_core::cut_operations`, exposed as
  `StrongPackage::required`). A base author outside the frontier, or a base
  ahead of it, is invalid.
- At most `MAX_CUT_OPERATIONS` (32,768) operations follow the base. The
  executor refuses more with `Quota { resource: "strong cut", limit }` before
  loading any operation. Encoding and decoding refuse them as well.
- The executor's base is its own checkpoint copy. A request that the base is
  past for some author, or that a replica without a base receives (for
  example after an aborted import), is `StrongPending(CutBase)`. This is
  decided before any operation is loaded. The executor never falls back to
  history before its base.
- An operation of the cut that the replica holds as `rejected` is counted
  as held. The request is refused (`Rejected`) instead of waiting for input
  that sync will never supply.

**Format.** The package starts with the 16-byte magic `rocky.package.v2`,
then the length-prefixed request, the base head fields, the publication
sequence, the repeated request frontier, `cut_base` as a frontier, and the
length-prefixed result rows. Bytes without this magic, including the earlier
package with an identity list, are refused. There is one format and no reader
for any other. The result commitment hashes these bytes, so `cut_base` is
bound by the receipt. Vectors: [strong encoding](../protocol/strong-encoding.md).

**What a verifier checks.** Decoding enforces the canonical form and the
derivation. Every replica that applies a result, and the audit, rebuilds the
cut from *its own* base and the operations after it. The derivation is the
same one, so the verifier's base may differ from the executor's. The rebuilt
rows must equal the package's rows. A verifier whose base is past the cut
leaves the result pending until it imports a newer checkpoint. One that has
pruned up to any point at or before the cut verifies normally. The encrypted
command path runs on confirmed rows, never on a mergeable cut. It refuses any
package with a `cut_base` or a covered operation, using the same derivation.

## Consequences

- A strong command costs the operations written since the executor's base,
  not the domain's lifetime. Pruning restores headroom.
- A reader asks only for the operations after the executor's base
  (`read_strong_from`). A reader with an older base needs the operations
  between the two as well, through ordinary sync.
- `cut_base` is not authenticated beyond the receipt, as on the rest of the
  trusted path. A wrong base can only make readers fetch fewer identities.
  The rows are still verified by replay.
- Existing installations reinstall. Earlier packages are refused, not migrated.
