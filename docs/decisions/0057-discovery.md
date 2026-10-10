# 0057. Discovery capability and topic

Status: accepted; implemented by phase 15.
Date: 2026-10-10.

## Context

[Decision 0056](0056-contact-exchange-and-discovery-secrets.md) gives every
unrevoked member one 32-byte topic and a previous one for 24 hours. Nothing
used it: a device learned contacts only from the application, an issuer or
another session. Finding a peer whose node key a device does not know needs a
network mechanism, and that mechanism belongs to the transport: TCP and custom
transports have none.

Two checks preceded the design. The locked `iroh-dns` 1.3 signs a pkarr packet
under any Ed25519 key (`SignedPacket::from_txt_strings`), iroh's
`PkarrRelayClient` publishes and resolves it by that key, and n0's
`iroh-dns-server` 1.3 stores any packet whose signature matches the key in its
path, so a record under a key other than the endpoint's works on the service
the starter already uses. `iroh-mdns-address-lookup` 0.6 depends on iroh 1.x
and accepts a service name, so no replacement was needed.

## Decision

**The port.** `rocky_core::net::Transport::discovery` returns an optional
`Discovery`, next to `connectivity`. It announces a node key under a list of
topics, the current one first, reports each `Found` contact (node key, address
of the transport's kind, path, whether it is local) and names each path as on,
off or unavailable with a reason. `foreground(false)` stops paths that may run
only in front. The hub has a deterministic implementation behind
`Hub::discovery`: every node announcing under a shared topic is found at once.

**Derivations.** Each is `commitment(label, topic)`, SHA-256 over the
length-prefixed label and the topic:

| Label | Use |
| --- | --- |
| `rocky.discovery-mdns-service.v1` | mDNS service name: `rk` and the first 13 base32 characters, lower case |
| `rocky.discovery-local-key.v1` | ChaCha20-Poly1305 key sealing the announced node key |
| `rocky.discovery-rendezvous-key.v1` | Ed25519 secret key; its public key names the rendezvous record |
| `rocky.discovery-rendezvous-seal.v1` | ChaCha20-Poly1305 key sealing the record |

**Local network.** One `iroh-mdns-address-lookup` instance per topic, built
beside the endpoint and never added to its lookup services. It announces the
endpoint ID and its IP addresses, without relay URLs, under the topic's
service name. The crate's `user-data` attribute carries the node key sealed
under the local key with a random nonce and the endpoint ID as associated
data, base64url. A member that cannot open the attribute ignores the
announcement. A found peer is marked local.

**Wider network.** One pkarr packet per topic on the lookup service's relay,
signed by the topic's rendezvous key. Record layout version **1**: TXT strings
under `_rocky`, each `index=chunk` of at most 200 characters, together the
base64url of `nonce(12) ‖ ciphertext`, sealed with the public rendezvous key as
associated data. The plaintext is the version byte and up to 6 contacts within
560 bytes, the writer's first and the others newest first:

`node(32) ‖ endpoint(32) ‖ seen(u64 LE seconds) ‖ relay length(u8 ≤ 64) ‖ relay URL ‖ sockets(u8 ≤ 2) ‖ (4 ‖ IPv4 | 6 ‖ IPv6) ‖ port(u16 LE)`

A member reads the record, reports its contacts, merges its own and
republishes every 5 minutes plus up to 30 seconds of jitter. A contact not
republished for an hour is dropped. The service keeps the newest packet, so
the last writer wins and a dropped contact returns with its owner's next
merge. A record that could not be read is not overwritten. The previous topic
is announced and merged the same way while its window lasts.

**Options.** The starter's `discovery` flag is replaced by `lookup`
(`default`, `disabled` or a pkarr relay URL), `topic` and `local`, in
`rocky_iroh::Options`, `TransportOptions::Iroh`, the native JSON,
`PeerTransport.Iroh` and `RockyEndpoint.bind`. An application-owned endpoint
discovers only through `Attachment::discovering`; Rocky adds no lookup service
to it, sets no user data on it and never closes it.

**The SDK.** One worker per link announces under the topics this device
derives, again after each sync pass and served session and every 10 minutes.
A device without the root, logged out or knowing itself revoked announces
nothing. A found contact enters the contact list with source `discovery`; a
known contact gains the route and the local mark, an application contact only
the mark. Local marks are cleared when a link attaches. Nothing found is
written to the replica. No session message, identity or secret-store format
changes: `discovery` was already a contact source of identity 13.

**iOS.** The mDNS crate is the only local mechanism. A build for a physical
iOS device (`target_os = "ios"` without the simulator ABI) opens no multicast
socket and reports the path unavailable with the missing entitlement as its
reason.

## Consequences

The topic path depends on the lookup service: with `lookup` disabled it is
reported unavailable, never silently absent. The record holds few contacts by
design; it bootstraps a first session, after which
[contact handover](0056-contact-exchange-and-discovery-secrets.md) spreads the
rest. A member that knows the topic can overwrite the record with nothing:
that delays discovery for one interval and reveals nothing new, since such a
member is admitted anyway.

The mDNS crate puts the endpoint ID in the announced name. A LAN observer
therefore sees a stable endpoint key and a service tag that changes only when
a revocation changes the topic. What each party learns from each path is in
[SDK transports](../runtime/sdk-transports.md#discovery).

Adds `iroh-mdns-address-lookup` and its `swarm-discovery` dependency. Existing
installations keep working; their options name the new flags.

See [discovery](../runtime/discovery.md) and the
[phase 15 evidence](../features_07/15-discovery-evidence.md).
