# SDK transports and iroh

The port (`Transport`, `Connector`, `Protocol`, stream type) is in
`rocky_core::net`; iroh is in `rocky-iroh`; TCP, the in-process hub and the
chunk adapter are in `rocky-sdk`. See [enrollment](sdk-enrollment.md) for what
travels over a transport and [composition](sdk-composition.md#attach-a-network)
for attaching one. Qualified on loopback only.

## What a transport does

A transport connects two devices with a reliable ordered byte stream and
carries the protocol name: pairing, sync, renewal or commands. It adds no
security the SDK relies on. Pairing answers are sealed under the invitation
token, renewals are signed, and sync runs
[mutually authenticated native sessions](permissions-and-sync.md) carrying
encrypted rows. A relay or network observer still sees who talks to whom,
credential chains and sizes.

Protocols are versioned strings: `rocky/sdk-pair/1`, `rocky/native-session/11`,
`rocky/sdk-renew/2`, `rocky/command/1`, and `rocky/coordinator/1`, which only
voters answer. A changed wire format takes a new name; an endpoint that does
not register a name refuses the stream.

A domain is bound to no transport. Each transport has a kind naming its
address format. A device knows [routes](sdk-enrollment.md#peers-and-contacts)
of its peers and dials those of the attached network's kind. To move to
another kind, attach it and exchange fresh contacts. After an exchange over a
route fails, a caller may `forget` that address; iroh then drops a cached
connection a restarted peer would otherwise leave looking open.

## Selecting one

```kotlin
RockyDatabase.create(directory, schema)                                   // starter iroh
RockyDatabase.create(directory, schema, PeerTransport.Iroh(relay = "https://relay.example"))
RockyDatabase.create(directory, schema, PeerTransport.Tcp("0.0.0.0:47470", advertised = "192.168.0.20:47470"))
RockyDatabase.create(directory, schema, PeerTransport.Endpoint(endpoint))  // your RockyEndpoint
RockyDatabase.create(directory, schema, PeerTransport.Custom(transport))   // your transport
```

Native options carry the starter's choice as `transport`; an application's
endpoint or transport goes to its own constructor instead.

```json
{"kind": "iroh", "relay": "default", "discovery": true, "bind": null}
{"kind": "tcp", "listen": "127.0.0.1:47470", "advertised": null}
```

| Transport | Kind | Use |
| --- | --- | --- |
| iroh, starter or your endpoint | `iroh` | The Internet transport |
| TCP | `tcp` | Loopback or a trusted network: plaintext with a five-byte protocol preamble |
| In-process hub | `memory` | Tests and deterministic faults |
| Yours | Your name | Anything that carries a byte stream |

TCP listens on `listen`; without it an issuer listens on an ephemeral loopback
port (`127.0.0.1:0`) and a member only dials, so it cannot be another member's
peer. Multiple issuing apps can use their defaults concurrently. Invitations
and contacts carry `advertised`, or the bound address. Configure `listen` and `advertised` for
stable or remotely reachable TCP routes.

`rocky_sdk::transport::memory::Hub` joins named nodes of one process with
bounded pipes, counts starts, dials and closes, and injects faults: `stalled`
holds every dial, `stall_peer(address, true)` holds just one peer,
`dials_to(address)` counts its attempts, `cut_after(bytes)` breaks streams,
`hold_cut` pauses at the cut for cancellation tests, `capacity` sets the pipe
buffer and `network_changed()` signals connectivity.

## Sync wakeups

The optional `Transport::connectivity` watch reports reachability changes;
iroh supplies it from endpoint address updates. Without a transport signal,
the application calls `setOnline` / the native `connection` request. See
[change-driven sync](always-on-peer.md#change-driven-sync). Connectivity and
`setOnline(true)` reset per-peer back-off; committed local writes do not.
Pausing the worker cancels outstanding dials and sessions without closing the
attached transport.

## Supplying a transport

In Rust, implement `Transport` and attach it as `Network::Borrowed`. A
`Connector` is only needed for a transport Rocky should start and close.

In another language, implement the chunk contract (strings and byte arrays
only, no native build):

```kotlin
class MyTransport : StreamTransport {
    override val kind = "mine"
    override suspend fun address(): String = …
    override suspend fun connect(address: String, protocol: String): ByteStream = …
    override suspend fun accept(): InboundStream? = …   // null once closed
}
```

| `ByteStream` call | Contract |
| --- | --- |
| `read(max)` | The next bytes, at most `max`; empty once the peer finished |
| `write(bytes)` | Delivered in order; returns once accepted |
| `finish()` | No more writes; the peer's `read` ends after what was sent |
| `close()` | Abandons the stream; a pending `read` ends |

Carry `protocol` to the accepting side unchanged and return it from `accept`.
Both ends send their handshake before reading, so a stream must buffer some
bytes without a reader; the adapter buffers 64 KiB per direction in chunks of
at most 16 KiB. The application starts and closes the transport; Rocky never
does. Native names: `ForeignTransport`, `ByteStream`,
`NativeSession.open_with_transport`; Rust: `transport::adapter`.

## iroh

iroh is the default `iroh` feature of `rocky-sdk` and the native ABI. Without
it both build with no iroh dependency, the starter's `iroh` option reports
that the build has no iroh transport, and `NativeEndpoint`/`RockyEndpoint`
are absent.

| | Endpoint and router | Closed by |
| --- | --- | --- |
| Attach to your endpoint | Yours, with your protocols next to Rocky's | You |
| Starter | Bound by Rocky, Rocky's protocols only | Rocky, completely |

### Attach to your endpoint

Rocky provides handlers, not a router:

```rust
use rocky_sdk::{Database, Network, iroh::{Attachment, iroh::protocol::Router}};

let attachment = Attachment::new(endpoint.clone());
let router = attachment
    .register(Router::builder(endpoint.clone()).accept(b"example/echo/1", Echo))
    .spawn();
let service = database.attach(Network::Borrowed(attachment.transport())).await?;
```

Use the iroh that `rocky_sdk::iroh::iroh` re-exports. iroh fixes a router's
protocols at `spawn`, so register first; afterwards a database attaches and
detaches on the same attachment as often as needed, also across reopen.
Opening a database blocks its caller: call it from a thread that does not
drive the endpoint (`spawn_blocking`), or the endpoint cannot answer the
enrollment it waits for. Your protocols' traffic never passes through Rocky.

From Kotlin, an endpoint made by another library cannot share a router.
`RockyEndpoint.bind(secret, protocols = mapOf("example/echo/1" to { remote, stream -> … }))`
hosts Kotlin protocols and Rocky's; pass it as `PeerTransport.Endpoint` and
dial with `endpoint.connect(address, protocol)`. `secret` is 32 bytes you keep:
the endpoint identity, so addresses in invitations survive restarts; it is
not device authority. Native: `NativeEndpoint.bind`, `connect`, `address`,
`shutdown`, `NativeSession.open_on_endpoint`.

| Call | Ends | Leaves running |
| --- | --- | --- |
| `service.close()`, `database.close()` | Rocky's serving and syncing | Endpoint, router, your protocols |
| `attachment.transport().close()` | Rocky's streams and cached connections for good | Endpoint, router, your protocols |
| `router.shutdown()`, `RockyEndpoint.shutdown()` | Everything on the endpoint | Open databases, without a network |

While no service is attached, Rocky's ALPNs are refused at once; yours are
unaffected. Detaching drops the connectivity watch without closing the
endpoint. Closing the starter's service or database shuts its router and
endpoint down.

### The starter

`PeerTransport.Iroh(...)` / `{"kind": "iroh"}` starts an endpoint keyed by the
device's stored transport key, so its address is stable.

- `relay`: `default` and `staging` use n0's public relays, `disabled` none,
  anything else is a relay URL.
- `discovery`: publishes and resolves addresses through n0's DNS service.
- `bind`: a local socket address. Without relay and discovery, peers know only
  addresses in invitations and contacts: use a fixed port.

With the defaults, n0's DNS and relays and any path observer learn which
endpoints talk, when and how much. Rows and commands stay encrypted and
sessions authenticated; credential chains cross a session in the clear to its
peer. Disable both options or name your own relay to depend on no public service.

## Bounds

| Resource | Bound |
| --- | --- |
| Inbound streams served at once per network service | 32; further ones are dropped and counted (`network.inbound.refused`) |
| Inbound streams queued, only while a service is attached | iroh 32, TCP 32, hub 8 |
| Inbound iroh connections with Rocky ALPNs; streams in flight each | 64; 4 |
| Cached outbound iroh connections | 64 |
| `RockyEndpoint` inbound streams per application protocol | 64 |
| Dial and stream open | iroh and SDK dial 15 s each; TCP connect and preamble 10 s |
| Session frame read or write | 10 s |
| Pairing/renewal exchange | 30 s after dialing; each route has its own dial deadline |
| Sessions per sync and peer | 16 |
| Contacts kept; contacts dialed per sync pass | 64; 16, issuers first, then the least recently attempted |
| Peers met without a contact that `peers` lists | 64, the least recent dropped |
| Unconfigured peers with horizon credentials or frontiers per replica | 256, the least recently served since open losing theirs; contacts, issuers and full nodes are kept |
| Closing a network service | 5 s drain, then cancellation |

A peer attempt tries routes in order until one connects, each with its own
deadline. A pass reconciles at most four peers at once, so a stalled route
does not hold up the other three. Explicit `sync` attempts every peer;
[change-driven sync](always-on-peer.md#change-driven-sync) skips peers still
in back-off. Disabling that worker cancels its attempts within one second.

## Qualification

- `cargo test -p rocky-sdk`: convergence over TCP, iroh and the hub (`engine`),
  a chunk-contract transport with small queues (`adapter`), and `faults`:
  streams cut during handshake, push and pull lose no local row and leave no
  partial one; a 4 KiB pipe converges; inbound streams beyond 32 are dropped
  and served again once free.
- The SDK's `node::sync_tests::change_driven` tests use the hub and paused Tokio
  clock for independent peer deadlines, four concurrent peers, lifecycle
  cancellation, bootstrap progress, two-worker idle behavior and an unreachable
  idle hour. Representative byte cuts run in the baseline. The exhaustive gate
  runs with `cargo test --locked --release -p rocky-sdk --lib cancellation_at_every_session_byte -- --ignored --nocapture`;
  every offset audits both replicas before a healed pass converges.
- `cargo test -p rocky-iroh` and `-p rocky-sdk --test iroh`: Rocky's handlers
  and an echo protocol share an application router; Rocky's ALPNs refuse before
  a service listens and after it closes while echo keeps working; the same
  attachment serves a reopened database; the starter closes its endpoint.
- `cargo test -p rocky-sdk --no-default-features` and
  `cargo check --manifest-path sdks/native/Cargo.toml --no-default-features`.
- `./examples/shoppinglist-app/check`: a Kotlin transport and a Kotlin protocol
  on a `RockyEndpoint`.

All loopback with relays and discovery disabled: nothing about NAT traversal,
relays or address lookup is qualified.

## Design rationale

- The port lives in `rocky-core` (with `tokio` only for `AsyncRead`/`AsyncWrite`)
  so `rocky-iroh` depends on no SDK that composes it.
- Rocky registers handlers on the application's router instead of owning one,
  so an application keeps its endpoint and protocols; closing Rocky never
  closes application infrastructure.
- Contacts and routes are unsigned hints: the session handshake already
  proves the node key, and a signature would suggest routes are trusted.
