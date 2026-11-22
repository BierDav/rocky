# Rocky

Rocky provides local-first SQLite storage, encrypted peer replication,
credentials, seal coordination and optional iroh networking.

See [AGENTS.md](AGENTS.md) for contribution rules. Use Cargo manifests, module
declarations and tests to navigate the implementation.

```sh
./scripts/setup
./scripts/lint
./scripts/check
```

For an edit, `./scripts/check --changed` validates the affected Rust packages
and their workspace consumers; shared or unknown inputs run the full workspace.
Use `./scripts/check --package rocky-sdk` to select a package explicitly, or
add `--unit` for just its unit tier. `./scripts/check --all` also runs e2e. These commands keep the workspace build and
filter test execution to avoid additional dependency variants. Cranelift's
separate build is opt-in with `ROCKY_CRANELIFT=1`.

The Rust application SDK is the `rocky-sdk` crate in `crates/rocky-sdk`. We only
provide the Rust-native API; developers who want to use it from Kotlin or other
languages need to write their own Rust wrapper/bindings.

Try it with:

```sh
cargo run -p rocky-sdk --example notes -- ./local/notes --create
cargo run -p rocky-sdk --example notes -- ./local/notes
```

A database's authority is an offline root key: `Configuration::create`
returns it once as a 24-word phrase (`Opened::root`), or
`create_with_root` takes a passkey-backed signer. Rocky never stores it.
The owner's identity is derived from that root. Identity signers are also
never stored: a new device joins as a single-use identity, or
`Configuration::join_linkable` returns its identity phrase once. Supply the
identity for later linking; opening and renewal use the node key alone.
The root grants and revokes trust per identity; those calls fail with
`publisher_required` without it. Every linked node of a trusted identity
can invite, renew, revoke ordinary identities and seal. Linking another node
needs the identity signer and matching codes on both devices, with no root
ceremony. Every node certificate carries the node's own signature naming its
identity and database, so no identity can list another's node; a node key
that two identities hold live certificates for belongs to neither. A node
that once signed for two identities stays ended for both until one of them
unlinks it; revoking either identity does not release it, and if neither
unlinks it, the remedy is a new node key. Trusted observers retain
`TrustedNodeLinked` and `TrustedNodeUnlinked` alerts, including for their
own identity. Trust never expires; an identity raises `TrustedIdentityStale`
on its trusted peers when none of its linked nodes has been heard from
within the configured threshold. Root revocation keeps each linked node's
seals and bookmark moves through the latest heads the revoking node holds;
unlink cuts that node's lane independently. Later statements raise
`TipStatementsPastCut`.
Every data key epoch is also sealed to the root's public key in records
that sync with its frames, so with every device lost the root and any
node's backup, a keyless custodian's too, restore the data that store
holds (`Configuration::recover`); the recovery revokes every trusted
grant older than its own, including ones its backup never knew. Of two
recoveries, the later one ends the earlier.

The todo app shares ordered lists between trusted, full and partial
devices. Run several installations in one window, or one per device over
iroh; start, join and reset them in the window:

```sh
cargo run -p todo-desktop
cargo run -p todo-desktop -- --device --data-dir ./local/todo --lan
```

wasm32 (a browser, in a dedicated Worker) is a supported target of the same
crates; `./scripts/check --wasm` runs clippy for it and the tests in headless
Firefox.

`rocky-sdk` starts without networking on native targets and in the browser.
Applications select a connector explicitly: add `rocky-iroh` and supply its
`Options` through `Network::Owned`, or attach an application's endpoint with
`Attachment::transport` and `Network::Borrowed`. `Attachment::connected` also
supplies optional discovery and network-change events through `Network::Attached`.
One attachment routes RPC and replication streams to independent database scopes.
Endpoint diagnostics and lifecycle controls belong to the iroh attachment.
On a network that already routes and encrypts between peers, such as a
WireGuard mesh, `rocky-tcp` is the plain-TCP connector instead (native only):
`rocky_tcp::Options::new(bind).advertise("host:port")` through `Network::Owned`,
or `Options::start` for an attachment the application keeps. Routes are
`host:port`; it has no discovery or relays, and Rocky's own encryption and
authorization apply on top as with any transport. Its `websocket` feature also
dials `ws://` and `wss://` routes (TLS with webpki roots), accepts WebSocket
handshakes with `Options::websocket`, and admits connections an application's
HTTP server already upgraded through `Attachment::external(url)` and
`Attachment::admit_websocket`, so Rocky can sit behind an HTTPS reverse proxy.
A dialer pins the node it expects, so a route must reach one node: behind a
load balancer give each node its own path.
Coordination is an application-owned slot: supply `Arc<dyn rocky_sdk::Coordinator>`
with `Configuration::coordination` (or the engine/custody equivalents).
For voters, construct `rocky_coordinator::Client::connect(&bundle, transport)`
and pass the returned client. The SDK binds its database node credentials;
it never starts a coordinator or closes the application's coordinator transport.
Without one, counter claims and reads return `CoordinatorNotConfigured`.
Local storage, enrollment and peer replication work independently.
The database's owner-signed settings name its coordinator. Creating with
a coordinator signs them at once; for an existing database the owner calls
`Session::declare_coordinated` (signing a coordination grant also does it).
Every device learns them by sync. With `CoordinatorSlot::Connector` (for
voters, `rocky_coordinator::Connector`) a device reaches the named coordinator
itself; an explicit client must be that coordinator, or opening, sealing and
counters fail with `coordinator_mismatch`. Without one, a trusted device
refuses to seal or stage a schema (`coordinator_required`), and its routine
and member seals are skipped with a `SealSkipped` alert. A coordinated database
is never made uncoordinated and its coordinator never changes: the owner's
devices refuse to sign such a revision, and every device resolves the
revisions it holds alike, the earliest coordinated one naming the coordinator.
Only finality acknowledged at that coordinator counts; finality compacts
only once the settings name it, and the owner may name only the coordinator
its finality records name. When that coordinator's log decided a tip, a
seal that lost it never ended the tip: rows written after it are late
writes of that tip and reach the winning line, and nothing ends the log of
a tip past it. A loss graphs decided before the log confirmed it, or one
through a revoked seal, keeps its rows held for re-authoring. A trusted
node whose finality pass cannot end or confirm a log raises
`FinalityStalled`.

Building for `wasm32-unknown-unknown` compiles C (SQLite, ring), so
it needs a clang that targets wasm32 and an `llvm-ar` that indexes wasm
objects, which Apple's `ar` does not. In the application's
`.cargo/config.toml` (`rustup component add llvm-tools`; the path names your
host):

```toml
[env]
AR_wasm32_unknown_unknown = "<rustc --print sysroot>/lib/rustlib/<host>/bin/llvm-ar"
```

Rocky links its own SQLite build in the browser (`rocky-sqlite`, see
[vendor/README.md](vendor/README.md)); an application that uses rusqlite
itself depends on it with `default-features = false`, since rusqlite's default
`ffi-sqlite-wasm-rs` links another SQLite and the wasm32 link fails.

Rust crates live in `crates/`, applications in `examples/` and performance
probes in `benchmarks/`.

Source-available under the [PolyForm Noncommercial License
1.0.0](LICENSE): read, run, modify and share Rocky for any noncommercial
purpose. Commercial use needs a separate paid license from the copyright
holder; ask [d.bieregger@gmail.com](mailto:d.bieregger@gmail.com).
Contributions are accepted under the inbound grant in [AGENTS.md](AGENTS.md).
