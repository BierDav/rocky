# Live links

Phase 2 is implemented and qualified on memory, TCP and local iroh endpoints.
Links carry signals and liveness; rows continue through [native peer sessions](secure-sessions.md#native-peer-sessions).
The execution and qualification gate is in the
[phase 2 brief](../features_08/02-link-session-and-signals.md).

## Authentication and ownership

`rocky/link/1` uses the native handshake's credential admission, revocation
exchange and session-bound partition proofs. It has its own signed handshake
identity; other versions are refused. Scope names in a signal belong only to
holdings proved during this handshake. A new authority requires a new link.

The database's network service owns its links. Detaching, closing the database,
offline mode and disabling the automatic worker close them. Application-owned
transports, endpoints and routers remain open. Links run only with the worker
enabled; explicit sync still uses native sessions during this phase.

There is one registry entry per authenticated node. When both devices dial,
the stream initiated by the lower node key survives. The other stream closes
with the duplicate reason and does not record a reachability failure.

## Signals and scheduling

After the settle window, the worker captures committed scope frontiers and
flushes before publication. It coalesces scopes into a bounded pending signal.
`Changed` carries up to 64 proved scopes and an opaque frontier commitment.
A token already received, or matching the local frontier, needs no pass.
A new token makes only the sending peer pending and resets its back-off.
A peer without a live link keeps ordinary change-driven sync.

A receiving peer opens the native data pass. The publisher sends the signal
on the existing link and does not dial. This also works when the publisher
cannot dial the receiver, using a link the receiver opened.

## Liveness and bounds

| Resource | Bound |
| --- | --- |
| Inbound live links per network service | 32, separate from 32 transient streams |
| Signed link frame | 64 KiB |
| Channels | 2: control and signals |
| Pending signals | One coalesced set, at most 64 scopes |
| Pending control replies | 4 |
| Idle ping interval | 15 seconds |
| Missed pong limit | 3 |
| Started frame read or write | 10 seconds |

Both ends may initiate messages. Frames carry a channel and request direction;
Pong echoes Ping's direction. Control is selected before signals, and a stalled
write ends within ten seconds, before the next ping interval. Idle reads have
no timeout. An unanswered ping expires at the next interval; after pings at
15, 30 and 45 seconds, the third miss closes at 60 seconds. Transport errors
also close the link. Reconnects use the peer's back-off.

The iroh link ALPN has its own connection budget and four inbound streams per
connection, leaving the transient connection and stream budgets available.
See [SDK transports](sdk-transports.md#bounds).

## Verification

The brief records the completed gates and measured evidence. The memory hub can
mute one node's link writes to test missing pongs without sleeps or closing
the transport. Internet traversal, mobile lifecycle and data over the link
belong to later phases.
