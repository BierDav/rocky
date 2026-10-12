# 0065: iroh as the only transport, with its liveness and path measurements

Status: accepted.
Date: 2026-10-12.

## Decision

iroh is Rocky's only network transport. The starter's options and the native
ABI accept only `{"kind": "iroh", …}`; the TCP transport, the chunk adapter
and the Kotlin `PeerTransport.Tcp` and `PeerTransport.Custom` are removed. A
LAN or loopback deployment without public services uses the isolated starter
with a fixed `bind`. Applications that share a network share an iroh endpoint
through `PeerTransport.Endpoint` or `Network::Borrowed`. The in-process hub
remains, for deterministic tests only.

Live links send no application keep-alive. The link format drops `Ping` and
`Pong` and takes the name `rocky/link/2`; `rocky/link/1` is refused. iroh's
QUIC keep-alive (5 seconds per connection and path) holds an idle link open;
its idle timeout (15 seconds per direct path, 30 for a relay path and the
connection) closes a connection to a vanished peer, which fails the link's
read or write and reconnects it with per-peer back-off. A borrowed endpoint
must keep a keep-alive interval below its idle timeout.

A stream may carry a `PathSource`. iroh's reports each open path of the
stream's connection: selection, kind, addresses, smoothed round trip, MTU,
congestion window and lifetime counters. Link status reads it on each status
request; `round_trip_ms` is the selected path's.

## Rationale

The ping duplicated, more slowly, what QUIC already does below the stream: a
dead peer took 45 to 60 seconds to detect against iroh's 30, and idle links
carried both keep-alives. Its round trip was one sample every 15 seconds of an
idle link, where iroh measures every path continuously. Keeping the ping only
served transports without their own liveness, which a single transport no
longer needs. The application-level check of a peer whose link task stalls
while its QUIC stack still answers is given up; a stall of that kind ends in
the existing ten-second frame and write deadlines once data flows.

A TCP transport inside iroh, as a custom transport, would carry QUIC over TCP
behind an unstable iroh feature. Isolated iroh already gives direct, encrypted
connections with no outside infrastructure, and the relay covers networks that
block UDP.

Measured on 2026-10-12, iroh selected a direct path wherever a direct route
existed, matching NetBird's peer-to-peer path on the same networks.

## Consequences

Kotlin and native consumers pick `PeerTransport.Iroh` or
`PeerTransport.Endpoint`. A build without the `iroh` feature has no network.
Tests that dropped writes to simulate missing pongs use `Hub::sever_links`,
which fails a node's link streams as an idle timeout would.
