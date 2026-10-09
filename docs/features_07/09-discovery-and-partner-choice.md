# 9. Discovery and partner choice

Status: not started.
Owner: `rocky_core::net` transport port; `rocky-iroh`; `rocky-sdk` contacts
and sync targets; Kotlin `PeerTransport`. Depends on
[phase 2](02-change-driven-sync.md) and
[phase 7](07-roster-free-admission.md); lands after
[phase 10](10-holdings-after-authentication.md) on the session. Wave 3.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [discovery and partner choice](scope-mesh.md#3-discovery-and-partner-choice),
the [proposed defaults](scope-mesh.md#proposed-defaults),
[SDK transports](../runtime/sdk-transports.md), the iOS
[platform constraints](scope-ios.md#known-platform-constraints) and
`targets()` in `crates/rocky-sdk/src/enrollment.rs`.

A device dials every configured peer, issuers first. Nothing is discovered,
and nothing about relays, NAT or address lookup is qualified. Find admitted
peers without a roster and sync with the best ones.

## Ordered slices

1. Decision record: discovery as an optional capability of a transport, next
   to `connectivity`. A transport without it works from contacts. An
   application-owned endpoint enables it explicitly; Rocky never changes that
   endpoint's own discovery or closes it.
2. The discovery secret: delivered with enrollment and renewal, replaced by
   every revocation. The topic and the local service name derive from it and
   from nothing an outsider knows.
3. Local network: announce and find peers of the same topic on the LAN.
   State what a LAN observer learns. Android needs a multicast lock; iOS
   needs the local-network prompt and, for raw multicast, an entitlement. If
   that entitlement is unavailable, local discovery on iOS uses the system's
   service browser, or is reported as unavailable, never silently absent.
4. Wider network: members of the topic exchange signed contact records
   through it. A record is a hint: the session authenticates whoever answers.
5. Contacts learned are kept in the bounded local list of
   [phase 7](07-roster-free-admission.md), aged and evicted. They are never
   written to a replicated table.
6. Ranking: local routes first, then peers that proved holdings this device
   needs ([phase 10](10-holdings-after-authentication.md)), then trusted
   nodes; unreachable peers demoted by the back-off of phase 2. A pass syncs
   with a bounded number and reports why each was chosen.
7. A session may hand over contacts of peers it reached, within bounds, as
   renewal answers hand over issuers today.

## Completion gate

- [Acceptance 6](scope-mesh.md#acceptance) passes with two processes on one
  host network and discovery as the only source of contacts.
- A device outside the domain, and a revoked one after the others renewed,
  derives no topic and finds no peer.
- With a local peer and a trusted node both reachable and both sufficient,
  the pass syncs with the local peer alone.
- A device needing a partition only the custody peer holds reaches it.
- TCP, the hub and an endpoint without discovery pass their existing checks.
- No replicated table holds a learned route; the replica is searched for one.
- Two networks joined only by a relay converge: the first qualified relay run.
- [SDK transports](../runtime/sdk-transports.md) states what each discovery
  path reveals and to whom.

## Not in this phase

Discovering coordinators, electing anything and multi-hop routing.
