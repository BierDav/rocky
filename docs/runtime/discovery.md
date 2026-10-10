# Discovery

Implemented by phase 15. How a device finds domain members it has no contact
for, on iroh. The formats and derivations are in
[decision 0057](../decisions/0057-discovery.md); what each outside party
learns is in [SDK transports](sdk-transports.md#discovery); the topic itself
is described under [peers and contacts](sdk-enrollment.md#peers-and-contacts).

## What it does

Every unrevoked member derives the same topic once an admitted session handed
it the domain's discovery root. On a transport that can, the SDK announces the
device under that topic and keeps whatever it finds as a contact:

| Path | Mechanism | Finds |
| --- | --- | --- |
| `local` | mDNS under a service name derived from the topic | Members on the same network; marked local, so a sync tries them first |
| `topic` | A sealed rendezvous record on the lookup service, under a key derived from the topic | Up to 6 members anywhere, newest first, refreshed every 5 minutes |

A found contact is a hint. The session it leads to authenticates the peer and
checks the revocation set, exactly as for a contact the application added. It
is kept in `identity.json` with source `discovery`, ages and is evicted like
other learned contacts, and is never written to a replicated table. A known
contact gains the found route; one the application added keeps its routes and
gains only the local mark.

Discovery finds a first peer. Further contacts arrive by
[handover](sdk-enrollment.md#peers-and-contacts) in the sessions that follow.

## Turning it on

The starter asks for both paths by default:

```kotlin
PeerTransport.Iroh()                                  // lookup "default", topic and local on
PeerTransport.Iroh(local = false)                     // the rendezvous record only
PeerTransport.Iroh(lookup = "https://lookup.example/pkarr")
PeerTransport.Iroh.isolated()                         // no relay, lookup service or discovery
```

| Option | Values | Effect |
| --- | --- | --- |
| `lookup` | `default`, `disabled` or a pkarr relay URL | Where endpoint addresses are published and resolved, and where the rendezvous record lives |
| `topic` | boolean | The rendezvous record. Unavailable while `lookup` is `disabled` |
| `local` | boolean | mDNS |

An endpoint the application owns discovers only when asked:
`RockyEndpoint.bind(secret, topic = true, local = true)`, or in Rust
`Attachment::discovering(endpoint, options.discover())`. Rocky announces
beside the endpoint: it adds no lookup service to it, sets no user data on it
and leaves it open. TCP, custom transports and the hub's default have no
discovery and work from contacts.

## Status

`peers()`/`network()` and readiness report each path:

```json
{"announcing": true,
 "paths": {"local": {"state": "on"},
           "topic": {"state": "unavailable", "reason": "the topic needs the lookup service, which is disabled"}}}
```

`on` means asked for and running, `off` not asked for, detached or stopped in
the background, `unavailable` asked for but impossible, with the reason: a
disabled lookup service, a failed mDNS socket, a lookup service that did not
answer, a transport without discovery or an iOS device build. `announcing` is
false until the device holds the discovery root, and for a device that is
logged out or knows it is revoked. Kotlin: `RockySession.discovery()`,
`NetworkStatus.discovery`, `Readiness.discovery` and `Peer.isLocal`.

## Revocation

A revocation changes the topic for everyone who receives its statement and
discovery value, which a revoked device never does. Members announce under the
previous topic for 24 hours more, so a device that has not caught up is still
found; after that a revoked device and its former peers share no topic. A
revoked device that is reached within the window is told so by the session
and stops announcing. A device of another domain derives another topic and
finds nobody.

## Lifecycle and platforms

`enterBackground()` (native `network_background`) stops the local path;
`enterForeground()` resumes it. The rendezvous record keeps its 5-minute
cadence while the process runs.

| Platform | Local path |
| --- | --- |
| Desktop JVM, Linux, macOS | On when asked. macOS may prompt for local-network access |
| Android | On when asked. `RockyLifecycle.follow` holds the Wi-Fi multicast lock while the path is on and releases it in the background; the SDK's manifest declares `CHANGE_WIFI_MULTICAST_STATE` |
| iOS simulator | On when asked, in the foreground |
| iOS device | Unavailable: no build carries the multicast entitlement yet. The status names that reason; peers are found by topic and contacts |

## Bounds

| Resource | Bound |
| --- | --- |
| Contacts in a rendezvous record | 6 within 560 plaintext bytes; at least 3 of the largest |
| A contact in the record | One relay URL of at most 64 bytes and two socket addresses |
| Republish interval; age at which a contact leaves the record | 5 min plus up to 30 s jitter; 1 h |
| Lookup request | 15 s |
| Found contacts waiting for the SDK | 64; further ones are dropped and reported again |
| Topics announced at once | 2: the current and, for 24 h, the previous |
| Topic re-derivation | After each sync pass and served session, and every 10 min |

## Checks

`cargo test -p rocky-iroh` and `./scripts/check` cover the record, both paths
and the hub scenarios; `./scripts/check --e2e` the two-process runs;
`./sdks/native/qualify-ios-discovery` the simulator and
`./sdks/kotlin/qualify-android-discovery` the multicast lock on an emulator.
[SDK transports](sdk-transports.md#qualification) lists what each shows.

## Not covered

Qualified on one host: loopback mDNS and a local lookup server. Multicast
across real access points, client isolation, the public lookup service, the
Android multicast lock on real Wi-Fi and a physical iPhone belong to
[phase 22](../features_07/22-qualification.md).
