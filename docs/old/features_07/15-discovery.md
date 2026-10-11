# 15. Discovery

Status: implemented and qualified on one host on 2026-10-10; see
[acceptance evidence](15-discovery-evidence.md). The hosted CI run and a
physical iOS device were not exercised.
Owner: `rocky_core::net` transport port; `rocky-iroh`; `rocky-sdk` transport
options; `sdks/native`; Kotlin `PeerTransport` and `RockyEndpoint`. Depends on
[phase 14](14-contacts-and-partner-choice.md) and, for the iOS simulator
check, [phase 17](17-kotlin-native-binding.md). Wave 5.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [discovery and partner choice](scope-mesh.md#3-discovery-and-partner-choice),
what was [decided](scope-mesh.md#decided-2026-10-09),
[SDK transports](../runtime/sdk-transports.md#iroh), the iOS
[platform constraints](scope-ios.md#known-platform-constraints) and
`Options::bind` in `crates/rocky-iroh/src/lib.rs`.

The iroh starter already publishes and resolves addresses of a known
endpoint through n0's DNS service. What is missing is finding a peer whose
node key a device does not know. Every qualification so far ran on loopback
with relays and address lookup disabled. Find admitted peers on the local
network and beyond it, and run the first relay check.

## Settled

- Discovery is an optional capability of a transport, next to
  `connectivity`. TCP, the hub's default and custom transports have none and
  work from contacts.
- Wider network: a rendezvous record published under a key derived from the
  topic of phase 14, on the address-lookup service the starter already uses.
  No gossip overlay and no new protocol on an application's router.
- Local network: an mDNS address lookup. iroh 1.3 ships none, so this adds
  one crate; `iroh-mdns-address-lookup` is the candidate and record 0057
  names the one taken.
- The mDNS crate is the only local mechanism on every platform; it compiles
  for the iOS targets. On iOS it is qualified on the simulator alone, which
  needs neither the local-network prompt nor the multicast entitlement. That
  entitlement was requested from Apple on 2026-10-10. Until a build carries
  it, the path on a physical iOS device is off and reported as unavailable
  with that reason, never silently absent; an iPhone finds peers by the
  topic and by contacts.
- Whatever is found is a hint that enters phase 14's contact list with its
  source. The session authenticates whoever answers.
- This phase changes no session message.

## Ordered slices

1. Decision record 0057: the capability, the derivations from the topic, the
   record layout and the crate. First check that the locked `iroh-dns` can
   publish and resolve a record under a key other than the endpoint's, and
   that the mDNS crate matches iroh 1.3; if either fails, name the
   replacement in the record before any other slice.
2. The port: `Transport` gains an optional discovery capability that
   announces under a topic and reports found contacts. The hub gets a
   deterministic one for tests.
3. Options: the starter's `discovery` flag is replaced by three, for address
   lookup, the topic and the local network, in `rocky-sdk`, the native JSON
   and Kotlin `PeerTransport.Iroh` and `RockyEndpoint.bind`. An
   application-owned endpoint has discovery only when its `Attachment` is
   asked for it; Rocky never removes or changes that endpoint's own lookup
   services and never closes it.
4. Local network: announce and find peers of the same topic. The announced
   name derives from the topic; where the crate fixes the service name,
   members filter by a topic-derived tag. A peer found here is marked local
   for the ranking. The Android artifact holds the multicast lock while the
   path is on. On the iOS simulator the path runs while the application is
   in the foreground and stops with `enterBackground`; the device target
   reports it unavailable without opening a multicast socket.
5. Wider network: each member merges its own contact into the rendezvous
   record, encrypted under a topic-derived key, and republishes within a
   stated interval. The record is bounded by the service's size limit and
   the last writer wins, so it holds a stated few contacts, newest first.
   The previous topic is answered for phase 14's window.
6. Status: `peers` and readiness report each path as on, off or unavailable
   with its reason, on every platform.
7. Regenerate both bindings and run the iOS simulator check
   ([phase 17](17-kotlin-native-binding.md)).
8. The first relay run: two endpoints with no direct path converge through a
   relay started locally from iroh's test utilities.

## Completion gate

- [Acceptance 6](scope-mesh.md#acceptance) is a CI run: two processes on one
  host, no trusted node and no custody peer reachable, discovery the only
  source of contacts, once over mDNS and once over the rendezvous record
  against a local lookup server.
- A CI run shows a device outside the domain, and a revoked device once the
  others hold the statement and its value, deriving no current topic and
  finding no peer after the 24-hour window.
- TCP, the hub without its capability and an attached endpoint without
  discovery pass their existing checks; an attached endpoint's lookup
  services are the same before and after a database attaches and detaches.
- No replicated table holds a discovered route; the replica is searched.
- The relay run passes in CI with direct paths disabled.
- In the iOS simulator check a simulator and a host process find each other
  over mDNS; the device target reports the path unavailable with its reason.
  The Android check holds and releases the multicast lock.
- [SDK transports](../runtime/sdk-transports.md) states what a LAN observer,
  the lookup service and a relay each learn from every path, and its
  qualification section no longer says that nothing is qualified.

## Not in this phase

- A gossip or pub-sub overlay, a DHT, or hosting a relay or lookup service.
- Qualifying local discovery on a physical iOS device, and any second local
  mechanism beside the mDNS crate.
- Discovery for TCP or custom transports.
- Changing the session, the contact list bounds or the ranking of phase 14.
- NAT traversal qualification on real networks and phones
  ([phase 22](22-qualification.md)).
- Discovering coordinators, electing anything and multi-hop routing.
- Hiding from the lookup service or a relay which endpoints talk.
