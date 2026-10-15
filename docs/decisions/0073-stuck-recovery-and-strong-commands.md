# 0073: Stuck recovery and the end of strong commands

Status: accepted. Collection is implemented; cuts are designed in
[0074](0074-owner-signed-recovery-cuts.md). Command removal is implemented,
with acceptance gaps recorded in the
[phase 11 evidence](../features_09/11-removing-strong-commands-evidence.md).
Date: 2026-10-14; command removal 2026-10-15.

## Context

[Coordinated writes](../runtime/coordinated-writes.md) commit after every in-sync
node stored an entry. Permanent loss of that set can strand a domain even when
a member still holds its original encrypted bundles. The coordinator retains
only metadata and must never manufacture missing application state.

Coordinated writes order ordinary encrypted row bundles. Keeping a second
command system would retain immutable handler versions, special tables,
executor fencing and result histories for the same application decisions.

## Decision: stuck recovery

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
from checkpoints, not just retained frames.
[Decision 0074](0074-owner-signed-recovery-cuts.md) designs the cut and rollback.

## Decision: removing strong commands

Phase 11 removes command descriptors, handler generations, command-controlled
and confirmed-read-only tables, preparations, results, tickets, receipt evidence
and forwarding. The coordinator retains its coordinated log and tenant control;
its control certificate lists trusted node keys, with no executor or invoker
roles. Native and Kotlin consumers use coordinated SQL. Shopping requests are
ordinary rows; trusted devices decide them in coordinated transactions, where
the first committed decision stands. Market restocking reads and updates stock
in one coordinated transaction.

## Consequences

The Raft store and snapshot keep the genesis anchor. Every changed format has
one current encoding: removed table classes, command schema fields, old
manifest/transition bytes, coordinator storage and link/file identities are
refused without writes or migration. Bootstrap signatures use
`rocky.genesis.v2` and trust bundles format 2; receipt-retention settings are
removed.

Collection uses bounded scan/reply pages and requires a complete retained bundle
on the serving device. Partial copies must first assemble the missing frames;
there is no multi-device fragment collector. SDK live-link catch-up queries
members as well as trusted nodes and uses the same verifier.

Applications own their business rules and coordination discipline on ordinary
rows. Being a trusted node grants no row access. Network/coordinator calls stay
outside storage transactions, and publication follows durable coordinated
commit. Development logs are process-local fixtures and lose their ledger at
process end; they are not durable services.
