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
| `local` | Standard DNS-SD under `_rocky._udp`, with topic-encrypted claims | Members on the same network; marked local, so a sync tries them first |
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
and leaves it open. The isolated starter, an attachment not asked to discover
and the test hub's default have no discovery and work from contacts.

## Status

`network()` and `DatabaseStatus.network.discovery` report each path:

```json
{"announcing": true,
 "local": {"state": "on"},
           "topic": {"state": "unavailable", "reason": "the topic needs the lookup service, which is disabled"}}
```

`on` means asked for and running, `off` not asked for, detached or stopped in
the background, `unavailable` asked for but impossible, with the reason: a
disabled lookup service, a failed mDNS socket, a lookup service that did not
answer, a transport without discovery or denied local-network access. `announcing` is
false until the device holds the discovery root, and for a device that is
logged out or knows it is revoked. Kotlin: `RockySession.discovery()`,
`NetworkStatus.discovery`, `DatabaseStatus.network.discovery` and `Peer.isLocal`.

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
| Desktop JVM, Linux, macOS | On when asked. Apple platforms use system Bonjour; others use standard DNS-SD. macOS may prompt for local-network access |
| Android | On when asked. `RockyLifecycle.follow` holds the Wi-Fi multicast lock while the path is on and releases it in the background; the SDK's manifest declares `CHANGE_WIFI_MULTICAST_STATE` |
| iOS simulator | On when asked, in the foreground |
| iOS device | System Bonjour in the foreground, after the user grants local-network access. No multicast entitlement |

## Apple application configuration

Applications declare `_rocky._udp` in `NSBonjourServices` and explain discovery
in `NSLocalNetworkUsageDescription`. The SDK cannot supply the host app's plist.
The shopping example declares both. System Bonjour owns the multicast traffic;
Rocky registers the endpoint's existing UDP port without binding another socket.
Raw multicast gateway discovery remains disabled on iOS.

The new local format replaces the topic-derived service types entirely. See
[DNS-SD design and security](discovery-dns-sd.md) for the current format,
expiry, privacy tradeoffs and resource limits.

## Bounds

| Resource | Bound |
| --- | --- |
| Contacts in a rendezvous record | 6 within 560 plaintext bytes; at least 3 of the largest |
| A contact in the record | One relay URL of at most 64 bytes and two socket addresses |
| Republish interval; age at which a contact leaves the record | 5 min plus up to 30 s jitter; 1 h |
| Lookup request | 15 s |
| Found contacts waiting for the SDK | 64; further ones are dropped and reported again |
| Topics announced at once | 2: the current and, for 24 h, the previous |
| Local TXT; sockets per candidate | 384 bytes; 8 |
| Local claim refresh; age; future clock tolerance | 60 s; 120 s; 30 s |
| Bonjour resolutions | 16 concurrent, 3 s per candidate; 64 starts per 30 s |
| Local reports | 64 distinct endpoints per 30 s; one report per endpoint per window |
| Topic re-derivation | After each sync pass and served session, and every 10 min |

## Checks

`cargo test -p rocky-iroh` and `./scripts/check` cover the record, both paths
and the hub scenarios; `./scripts/check --e2e` the two-process runs;
`./sdks/native/qualify-ios-discovery` the simulator and
`sdks/kotlin/gradlew -p sdks/kotlin :qualification:android-discovery:qualify` the multicast lock on an emulator.
[SDK transports](sdk-transports.md#qualification) lists what each shows.

The physical-device probe is built with
`sdks/native/qualification/discovery/build-devices`; see
[device qualification](discovery-devices.md). It tests discovery and direct QUIC
without a relay or public lookup.

## Qualification limits

Loopback DNS-SD and a local lookup server are covered by the Rust gates.
Physical iPhone/Pixel discovery and direct QUIC on one Wi-Fi network are
covered by the [device gate](discovery-devices.md). These do not establish
behavior across arbitrary access points, client isolation, IPv6-only networks,
public lookup services or mobile database convergence under combined faults.
