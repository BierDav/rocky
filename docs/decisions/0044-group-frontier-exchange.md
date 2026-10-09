# 0044: Bounded group frontier exchange

Status: accepted.
Date: 2026-10-09.

## Context

[Phase 9](../features_06/09-group-sync-measurement.md) found an idle
single-table 200-partition round already took 14.184 seconds against the
four-table 10-second review budget. Repeated empty per-scope pull/push exchanges
also repeat credential resolution against all registered partitions.

## Decision

This record introduced native session v5. The current version and its
per-member refusal are defined by [decision 0046](0046-per-scope-frontier-refusal.md).

A `FrontiersQuery` carries an ordered vector of scope frontiers. `Frontiers`
returns the corresponding frontiers, admitted signed horizons and a per-scope
behind flag. The batch contains 1–64 distinct scopes and fits the existing
frame and session byte bounds. The SDK packs selected members of each group
partition together; a group is never split between batches. Independent groups
and ordinary scopes can share a batch. Groups larger than the bound are refused.

The server validates the entire request and verifies current credentials. It
exposes metadata only for members on which the peer holds whole-scope read or
custody, and refuses each other member alone
([decision 0046](0046-per-scope-frontier-refusal.md)).
Acknowledgments retain their existing per-scope meaning: applied marks count
only for whole-scope readers. A batch records them atomically in the existing
peer-frontier table, resolving credentials once for all marks in a batch. The response includes
both peer and serving-replica horizon rebootstrap requirements.

The client verifies scope order/count, frontier structure and horizon scope,
then records the server's acknowledgments. It omits ordinary row sync only for
equal fully applied frontiers, equal admitted horizons, and no local or remote
rebootstrap requirement. A newer unknown peer generation also keeps the
ordinary path, preserving the SDK upgrade signal during idle sync. Changed scopes keep their original input order and
use the existing authenticated range, checkpoint-first repair and push APIs.
Per-scope range and checkpoint authorization resolves selectors only against
that scope; batch acknowledgments resolve only against the requested members.
Credential recording verifies signed selector terms without expanding unrelated
partitions. Signature, generation policy, expiry and revocation checks still
run on every request; there is no session-wide authority cache.

Existence synchronization, horizon acknowledgment/finalization and command
results still run independently, even when all row scopes are idle.

## Consequences

Stored rows, keys, logs, checkpoints and peer marks keep their formats. Only
native-session peers must use the current wire version. The optimization is a
round snapshot: writes concurrent with it may wait for the next round, as with
ordinary per-scope sync. Large author sets remain subject to the existing byte
limits; the fixture qualifies one author per table scope, not maximum-size
frontiers, WAN latency, command results or horizon finalization throughput.
