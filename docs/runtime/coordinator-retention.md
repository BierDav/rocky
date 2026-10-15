# Bounded coordinator metadata

The coordinator retains generic tenant controls and the
[coordinated domain log](coordinated-log.md). It holds no application rows,
command results, receipt evidence or external-effect state. Applications keep
their decisions as ordinary rows, ordered by [coordinated writes](coordinated-writes.md).

## Budgets

The certified genesis fixes tenant domain/storage quotas, the total storage
budget and bounded metadata request retries. Defaults are 16 open domains and
4 MiB per tenant, 24 MiB overall, and 64 retained retry records per signer for
one hour. A public service additionally fixes tenant count, registration rate
and concurrent client lanes. A service's genesis must reserve disjoint tenant
budgets. Saturation returns an explicit quota/backpressure failure; it never
evicts another tenant's live state to report success.

Each domain log keeps trusted in-sync membership, one pending entry, the committed
head/running hash and the last 1,024 committed entry hashes. Node request identities
are monotone, with one retained first outcome per node. An older identity is
retired; changing inputs at the last identity conflicts. The coordinator reserves
the bounded log footprint at admission, so committing a staged entry cannot fail
because the hash window grows.

## Retries and closure

Persist a metadata mutation identity before sending it and retry identical input
with that identity. The service keeps its first retained answer. Expired generic
retry records do not constitute historical application evidence. The owner's
SDK lane keeps bounded signing times so an identical retry signs the same input.
No client receipt cache, response-acknowledgment floor or command session remains.

Closing a domain is permanent. Its marker remains, and the identity cannot be
registered again. Closure frees open-domain quota under the service's accounted
storage policy. Control revisions preserve committed log history and cannot remove
an in-sync node or change membership while an entry is pending.

## Uncertain coordinated writes

The application runtime durably stores bundles and reconciles an uncertain append
against the coordinator's committed head. It repeats storage acknowledgments after
leader changes and publishes only after commitment. An aborted entry is voided;
`Undecided` retains unsent local effects until resolution. A retry is never
synthetic success.

An entry older than the hash window cannot be resolved by inventing receipt
history. Lost in-sync sets and owner-signed cuts belong to
[Phase 10](../features_09/10-stuck-recovery.md), which this removal does not
implement. Ordinary row history follows its own
[checkpoint](encrypted-checkpoints.md) and [horizon](mergeable-horizon.md) policies.

## Durability and formats

Controls, log transitions and resolved outcomes are Raft-replicated. Partial
storage acknowledgments are leader memory and are collected again after election;
resolution entries carry the proofs voters verify. Files/snapshots use format 4,
controls `rocky.control.v2`, bootstrap signatures `rocky.genesis.v2`, and service
bundles format 2. Other formats are refused, without migrations or alternate readers.

[Public coordination](public-coordination.md) describes composition and owner
calls; [Phase 11 evidence](../features_09/11-removing-strong-commands-evidence.md)
records verification. Historical measurements and the pre-removal design remain
in their dated evidence and decision records.
