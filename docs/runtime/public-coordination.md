# Public coordination

A shared coordination service orders the metadata log of each application's
domains. It stores trusted identities, in-sync membership, a pending entry and a
bounded hash window; schemas, rows, business rules and encrypted bundles stay
on application nodes. See [coordinated log](coordinated-log.md) and
[coordinated writes](coordinated-writes.md).

## Trust and ownership

Clients pin a `TrustBundle` containing the certified genesis, root and mesh.
Voters are trusted consensus participants, not a Byzantine quorum. Loss of quorum
makes coordinated progress unavailable; ordinary writes and replication remain
local operations. A trusted-node designation grants no row, reader-key or
enrollment authority.

The owner, whose identity key is the publisher key, signs a coordination
grant naming an application and tenant key. An issuer registers its domain
under that tenant, supplying the signed control certificate listing trusted
identities. Every request of a node presents its node certificate; the
coordinator admits a node whose certificate a trusted identity signed for
its key and that is valid at the request's time. Revocation stays the
domain's: a control revision drops an identity. Tenant keys coordinate
metadata only. Later control revisions update trusted identities while
preserving the log. Closing is permanent and frees the open-domain quota
without reopening its identity.

## Compose

Rust `Open::coordination` accepts `Coordination::Service(Box<TrustBundle>)`.
The starter's JSON options take the same bundle object as `coordinator`.
Kotlin uses `coordinator = ServiceCoordinator(bundleJson)` on database open.
Service clients use the attached network, whose ownership remains independent
of the database. `NetworkCoordinator` implements the metadata-only `DomainLog`;
`certify` sets the node certificate its requests present, which the SDK
replaces at every renewal.

For in-process development, Rust supplies `coordinated::MemoryLog`, and Kotlin
supplies `DevelopmentLog`: configure the domain minimum, then call
`database.coordinate(log)` for each trusted device. This fixture loses its ledger
at process exit and is not a durable service.

## Owner calls

Kotlin `database.service(tenantKey)` returns `ServiceOwner`. Persist each
registration/control/close identity before calling; retries use the same identity.
Only issuers register through the SDK owner lane.

| JSON request | Inputs | Result |
| --- | --- | --- |
| `coordination_register` | `tenant` secret as 64 hex digits, `registration` as 32 bytes, `trusted` identity keys as hex strings, `lifetime`, optional `admission` and owner `grant` | `tenant`, `revision`, `control` |
| `coordination_control` | `tenant`, `id`, `revision` above 1, `trusted`, `lifetime` | accepted control |
| `coordination_grant` | public `tenant` or `tenant_secret`, `lifetime`; the owner only | grant signed by the owner's identity |
| `coordination_close` | `tenant`, `id` | `closed: true` |
| `coordination_usage` | `tenant` | open/closed domains, bytes, registrations today |

Registration/control lifetimes must fit both the request bounds and genesis
policy. The SDK retains bounded signing-time metadata for identical retries;
it stores no receipt cache or command result evidence. The coordinator enforces
per-tenant domains, storage, registration and request budgets, with explicit
quota/backpressure/expiry errors. See [coordinator retention](coordinator-retention.md).

## Formats and verification

Trust bundles use format 2 and bootstrap signatures `rocky.genesis.v2`.
The earlier provider certificate is retired and not composed by default; a
deployment supplies its new bundle explicitly. Domain controls use `rocky.control.v3`, naming trusted identities; requests use `rocky.coordination-request.v5`, carrying the node certificate; Raft stores/snapshots use format 7, whose in-sync members record the identity that certified them.
The removed executor/invoker controls and command operations are not interpreted.
There are no command heads, preparations, receipts or result-forwarding lanes.

[Operations](coordinator-operations.md) covers provisioning and restore fencing.
[Phase 11 evidence](../features_09/11-removing-strong-commands-evidence.md) records
current verification. Owner-signed recovery cuts remain Phase 10 work; this
change does not add them or depend on their unmerged implementation.
