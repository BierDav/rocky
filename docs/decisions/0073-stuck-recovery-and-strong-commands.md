# 0073: Stuck recovery and the end of strong commands

Status: partially implemented; collection only. Recovery cuts and phase 11 remain pending.
Date: 2026-10-14.

## Context

[Coordinated writes](../runtime/coordinated-writes.md) commit after every in-sync
node stored an entry. Permanent loss of that set can strand a domain even when
a member still holds its original encrypted bundles. The coordinator retains
only metadata and must never manufacture missing application state.

## Decision

Collect complete retained bundles from reachable devices before considering
acknowledged loss. A member derives candidate entry hashes from original frame
identities and their signed positions; it does not decide canonical history.
Every bundle member must agree on the transaction, member openings and position.
The requesting trusted node checks every candidate against the coordinator's
hash window and chains from the coordinator's genesis anchor. No signature is
needed for complete reconstruction and rejoin. A gap outside the window fails
closed even when the final running hash would match.

An owner-signed recovery record must bind an exact old head and the retained
prefix's position/hash, fence the abandoned history, and remain enforceable on
members that never call the coordinator. It cannot reuse a position without an
authenticated history identity. Abandoned contributions must also be removed
from checkpoints, not just retained frames. These cut requirements are pending;
this record does not claim an implemented recovery operation.

## Consequences

Raft store and snapshot format 4 stores the genesis anchor. Earlier and later
formats are refused without writes or migration. Client receipt evidence stays
format 3; encrypted replica and link identities stay v18 and v3.

Collection uses bounded scan/reply pages and requires a complete retained bundle
on the serving device. Partial copies must first assemble the missing frames;
this slice adds no multi-device fragment collector. SDK live-link catch-up
already queries members as well as trusted nodes and uses the same verifier.

The existing checkpoint has no coordinated prefix or history binding. Its replay
base may contain abandoned effects after the original frames were pruned. A cut
requires a verified canonical replacement base, preservation/reconciliation of
ordinary writes, and propagation of signed fencing evidence. Merely marking
retained suffix frames rejected and rewinding the log would not converge.
No cut is admitted or acknowledged as success in this slice.

[Phase 11](../features_09/11-removing-strong-commands.md) owns removal of strong
commands. This change removes none of them and does not depend on phase 7.
