# Mergeable horizon (removed)

Phase 15 removed the declaration protocol, its formats and SDK operation.
Use [lazy compaction at seals](lazy-compaction.md) for finalization,
[existence logs](existence-logs.md#bases-at-seals) for signed bases and
[encrypted checkpoints](encrypted-checkpoints.md) for history transfer.

This pointer preserves links from historical plans and decision records.

## Evidence

The current [seal plateau probe](lazy-compaction.md#evidence) replaces the
old declaration-based probe. See the [phase 15 gate evidence](../features_09/15-lazy-compaction-evidence.md#completion-gate-recheck).

## Dependencies and ordering

See [finalizing seals](lazy-compaction.md#finalizing).

## SDK

Use [tips compaction](lazy-compaction.md#compacting).
