# Standard local DNS-SD

The local path uses one fixed service type, `_rocky._udp`, in `local.`.
This replaces decision 0057's topic-derived service types and local claims;
there is one current local format, with no reader for the old announcements.
The [rendezvous record](discovery.md) is unchanged.

## Platforms and ownership

Apple builds use `async-dnssd` to call the OS's DNS-SD registration, browsing
and resolution APIs. The application declares the fixed type in its plist and
requests local-network access. No raw multicast socket or multicast entitlement
is needed. iOS's raw multicast gateway discovery remains disabled.

Other builds use `mdns-sd`, which implements standard PTR/SRV/TXT/A/AAAA DNS-SD.
Android retains the existing Wi-Fi multicast-lock lifecycle. Both backends run
beside iroh, advertise its existing UDP port and stop in the background. Dropping
local discovery withdraws its registration without closing the endpoint.
A failed local path retries after 30 seconds or an endpoint replacement.

The native boundary and Kotlin API are unchanged. Applications still choose
`local`; a consumer does not need to compile Rust or implement an adapter.

## Current local format

The service-instance name is `rk` followed by 16 random bytes as lowercase hex.
It changes when the local worker restarts, independently of topic or node keys.
TXT has exactly these fields, with a total maximum of 384 bytes:

- `v=2`
- `ep`: the 64-character hexadecimal iroh endpoint ID
- `c0`: the current topic's claim
- `c1`: optional previous-topic claim

Each claim is base64url without padding of `nonce(12) || ciphertext(56)`.
The plaintext is `node(32) || issued(u64 LE seconds since Unix epoch)`.
ChaCha20-Poly1305 uses a fresh random nonce and the key
`commitment("rocky.discovery-local-key.v2", topic)`. Associated data is
`endpoint(32) || port(u16 LE)` from the advertised SRV record.
This binds the node claim to the expected endpoint and UDP port.

Claims refresh every 60 seconds. Receivers refuse claims older than 120 seconds,
issued more than 30 seconds in the future, in another version, under another
topic, or bound to another endpoint/port. Large clock errors can make local
peers undiscoverable; topic discovery and explicit contacts remain available.

## Security boundary

DNS-SD is not enrollment, identity proof or authorization. A valid topic claim
only filters candidates: any topic holder can manufacture one. iroh verifies
the expected endpoint at connection establishment, then Rocky's authenticated
session checks the node, credentials and revocations before application data.
Do not grant rights, install keys or accept a new member from DNS-SD alone.

Captured claims can be replayed within their freshness window. IP addresses
remain unsigned routing hints, so an attacker replaying a claim may redirect
bounded connection attempts; they cannot change its UDP port or impersonate
the expected endpoint without that endpoint's key. Discovery does not guarantee
availability against an attacker controlling or flooding the LAN.

Rocky limits Apple resolutions to 16 concurrent requests, each lasting at most
3 seconds, and starts at most 64 per 30-second window. Host resolution collects
a bounded 250 ms batch of A/AAAA results instead of trusting the first address.
It validates TXT and the claim before hostname resolution. Both paths report at most 64 distinct
endpoints per window, once each, retain at most 8 socket addresses per candidate,
and feed the existing 64-entry nonblocking discovery queue. Invalid TXT cannot
trigger an application connection. The raw DNS-SD library has bounded event
channels, but its DNS cache and the OS responder are outside these Rocky limits;
these bounds are not a total process-memory guarantee under a packet flood.

The fixed type exposes Rocky's presence. Endpoint IDs, addresses, ports and
claim counts remain visible. Random instance names and encrypted claims avoid
publishing user, node or domain names or a shared public topic tag. System
Bonjour can expose the operating system's hostname through SRV/A records;
randomizing the instance name does not anonymize the device or its stable
endpoint ID. See [what observers learn](sdk-transports.md#discovery).

## Design rationale

A fixed type can be declared before installation, unlike types derived from
runtime secrets. Moving the topic filter into encrypted TXT claims permits
ordinary Apple Bonjour without the multicast entitlement. Standard DNS-SD on
other platforms replaces `swarm-discovery`'s modified record exchange so both
mobile platforms speak the same protocol.

Using safe Rust wrappers keeps ownership, cancellation and claims in
`rocky-iroh`, avoiding a second Kotlin worker or foreign callback contract.
The qualification probe is separate and never ships in SDK artifacts. Its
known test topic proves transport interoperability, not application admission;
[device qualification](discovery-devices.md) states the exact coverage.

Platform requirements: [Apple TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy).
Protocol risks: [RFC 8882](https://www.rfc-editor.org/info/rfc8882/).
