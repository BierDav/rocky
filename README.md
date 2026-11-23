# Rocky

Rocky provides local-first SQLite storage, encrypted peer replication,
credentials and optional iroh networking.

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

`Configuration` opens a database and `Database` is its one handle: clones
share it, every call is a future the caller may drop, and `close` ends the
database for every clone. `observe(&["notes"])` streams committed changes to
the named tables (an empty list means all), `flush` makes every returned commit
durable, and `status` with `status_changes` reports progress, blockers and the
network.

Every call returns a typed `rocky_sdk::Error`: an `ErrorKind` (which keeps
typed causes such as `ServiceError`, `AwaitingLink` or
`RolloutConfirmationRequired`), whether a retry can succeed, a detail and the
blockers in the way. SQL calls return a `SqlFailure` whose `SqlReason` says why
a table could not be read: `Unavailable { table, reason }`, where `Incomplete
{ blockers }` names the scope that still waits and what clears it. `status`
lists the same blockers until the missing data arrives.

A database's authority is an offline root key: `Configuration::create`
returns it once as a 24-word phrase (`Created::root`), or
`create_with_root` takes a passkey-backed signer. Rocky never stores it.
The owner's identity is derived from that root. Identity signers are also
never stored: a device joining with `Database::provision`'s URI joins as a
single-use identity the issuer minted, or `Configuration::join_linkable`
returns its identity phrase once. To keep an identity the person already
has, an issuer calls `Database::invite` with its public key instead; the
URI carries no secret, and `Configuration::accept_invitation` joins with
the configured identity provider over the network, asking the issuing node
for an enrollment that node signs. Such an invitation is single use and expires (ten
minutes by default, `InviteTerms::valid_for`); the issuer withdraws an
unused one with `Database::withdraw_invitation`. Supply the identity for
later linking; opening and renewal use the node key alone.
`Database::provision` and `Database::invite` take an `Access`: `Full`,
`Read`, `Store` or a `Role` the schema declares with `SchemaBuilder::role`.
A role reads, writes, appends to or updates named columns of the tables it
lists; column lists restrict writes only. A role declared `per_partition`
holds no partition of that group until a trusted identity calls
`Database::grant_partition`; the person's next renewal, which the delivered
group secret starts, carries the grant. `Full` and `Read` reach every
partition. Only trusted identities grant partitions or share group secrets
with members; a member hands the secret of a partition it created only to
trusted nodes, and no access delegates further. A trusted identity becomes an
administrator by provisioning it, granting `Database::trust` with the root key
and calling `Database::install_trust` on its node.
A relay is an ordinary identity invited with store-only access
(`Access::Store`): it holds and forwards encrypted frames and
partition-key mailboxes for devices that are never online together, but never
receives a data key. `Configuration::store_only` opens such a database without
the application's schema, so one process can relay for many tenants
(`cargo run -p rocky-sdk --example relay`).
The root grants and revokes trust per identity; those calls fail with
`publisher_required` without it. Every linked node of a trusted identity
can invite, renew and revoke ordinary identities, and seals: Rocky seals
routinely and when the schema changes, never through an application call.
Linking another node needs the identity signer and matching codes on both
devices, with no root ceremony. Every node certificate carries the node's
own signature naming its identity and database, so no identity can list
another's node; a node key that two identities hold live certificates for
belongs to neither. A node that once signed for two identities stays ended
for both until one of them unlinks it; revoking either identity does not
release it, and if neither unlinks it, the remedy is a new node key.
Trusted observers retain `TrustedNodeLinked` and `TrustedNodeUnlinked`
alerts, including for their own identity. Trust never expires; an identity
raises `TrustedIdentityStale` on its trusted peers when none of its linked
nodes has been heard from within the configured threshold. Root revocation
keeps each linked node's seals through the latest heads the revoking node
holds; unlink cuts that node's lane independently. Later statements raise
`TipStatementsPastCut`.

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
`Attachment::transport` and `Network::Borrowed`. A configured network attaches
at open; `Database::attach` and `detach` change it later, and `detach` closes
only a transport the database started. `set_sync` picks `SyncMode::Off`,
`Manual`, `Foreground` or `Background`; `sync_now` runs one pass and returns
its `SyncReport`, and `add_peer`, `remove_peer` and `contact` manage peers. `Attachment::connected` also
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
Guarantees that need a global authority, gap-free numbers and at-most-once
admission, come from the optional `rocky-coordinator` crate, an add-on to a
`Database`: `Counters::new(database, client)` claims the next number of a
named counter for any person with write access, and a claim key such as an
order or ticket id keeps every retry on its first number. The application
writes that number in its own transaction; an unreachable coordinator is a
typed error that takes no number. The coordinator itself is `rocky-voter`, a
three-voter Raft service: `scripts/coordinator-local DIR` builds it, starts
three voters on loopback and prints the trust bundle clients pin.

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
