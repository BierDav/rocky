# Rocky

Rocky is an embedded SQLite database for Rust apps that keeps a local copy on
each device and syncs it, end-to-end encrypted, between the people its owner
authorizes and their devices. You declare a schema and write SQL in
transactions; Rocky merges concurrent changes, lets the owner and admins invite
and remove people, links each person's devices, and enforces what each person
may read and write, down to tables, columns and partitions. It runs natively
and in the browser. Everything else is yours: UI, servers, the transport you
plug in (iroh, TCP or WebSocket), business rules and backups. Guarantees that
need a global authority, such as gap-free numbers or admitting a ticket once,
come from the optional `rocky-coordinator` crate.

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
typed causes such as `ServiceError`, `JoinError` or
`RolloutConfirmationRequired`), whether a retry can succeed, a detail and the
blockers in the way. SQL calls return a `SqlFailure` whose `SqlReason` says why
a table could not be read: `Unavailable { table, reason }`, where `Incomplete
{ blockers }` names the scope that still waits and what clears it. `status`
lists the same blockers until the missing data arrives.

A database's authority is an offline root key: `Configuration::create`
returns it once as a 24-word phrase (`Created::root`), or
`create_with_root` takes a passkey-backed signer. Rocky never stores it.
The owner's identity is derived from that root. Identity signers are also
never stored. An admin invites with `Database::invite(Invite::new(access))`,
optionally naming `partition`s, a `label`, a validity (`Invite::valid_for`,
ten minutes by default) or the one person it is for (`for_identity`). The
label names the device the invitation admits (`Device::label`) and, on the
person's first invitation, the person. The
returned `Invitation` holds an `InvitationCode`; `Configuration::join` redeems
it over the network with the issuing device and returns `Opened`. A default
code carries a single-use secret, so whoever holds it joins once, as an
identity Rocky mints and returns once in `Opened::identity`; a `for_identity`
code carries no secret and admits only that person's configured identity. A
refused join says why with a `JoinError` (`Expired`, `Used`, `Cancelled`,
`OtherIdentity`, `AlreadyMember`, `Removed`, `IssuerUnreachable`, ...).
`Database::invitations` lists open and redeemed invitations and
`cancel_invitation` ends an unused one. A person links another device with
`Configuration::link`, which returns a `PendingLink` whose `code` the person
compares with `Database::link_requests` on a device they already hold,
approves with `approve_link`, and finishes with `PendingLink::confirm`.
`Database::unlink` ends one device: the person's own, or as an admin a
member's lost device, while the member keeps the others. Linking needs a
device the person still holds, so a person who lost their only device gets a
replacement through an admin: `unlink` the lost device and invite the person
again with `for_identity` and the same access; the replacement joins with the
person's identity and every partition they were granted. Opening and renewal
use the device key alone.
`Database::invite` takes an `Access`: `Full`,
`Read`, `Store` or a `Role` the schema declares with `SchemaBuilder::role`.
A role reads, writes, appends to or updates named columns of the tables it
lists; column lists restrict writes only. A role declared `per_partition`
holds no partition of that group until an admin calls
`Database::grant_partition(person, &partition)`, which delivers the group
secret to every device of that person, including devices that join or link
later (at an admin's next sync once it learns of them); granting a partition
the person already holds delivers it to their devices again.
`revoke_partition` takes it back. Before any grant, a table of that group
reads as `Unavailable` with an `Incomplete` reason whose `PartitionNotGranted`
blocker names the group, and `status` lists the same blocker.
`Full` and `Read` reach every partition. A new partition's secret reaches
every device of every person whose access covers it through the delivery
mailboxes, with no application call; store-only devices hold its frames but
never its secret, and a device's replica follows its access.
`DataStatus::partitions` reports whether each partition is held and complete.
`Database::me` says who the device
belongs to, its access and what it may do per table. Admins list people with
`Database::people` (a `Person` per member with access, partitions and
devices), change access with `set_access` and end a person with
`remove(person, Removal::Departed | Removal::Compromised)`. Narrowing access
or taking a partition back revokes the person's current credentials and
rotates the keys of everything they could read before; the issuer records
that revocation before it changes the access, so a crash in between still
revokes once on restart. Widening and `make_admin` leave a rights notice in
the mailbox of each of the person's devices, so each renews into the wider
rights at its next sync with an admin device. A
refused call says why with an `AccessError` (`AdminRequired`,
`OwnerRequired`, `UnknownPerson`, `TargetIsAdmin`, `PartialGroup`,
`TooManyPartitions`, `Removed`, ...). The owner makes a member an admin with
`make_admin(person, root)` and ends one with `revoke_admin(person, removal,
root)`; both need the root key. An admin who should stay a member is invited
again. `logout(Logout::KeepData | Logout::PurgeOnClose)` ends this device's
session and keeps or deletes its directory at close.
A schema version that drops tables or fields or narrows a role opens on an
issuer only through `Configuration::open_confirmed` with the digest of the
`RolloutImpact` its refusal carries; `narrowed_roles` names the roles that
lose rights. The issuer records who holds a narrowed role before it stages
the version; once the version is published it revokes their capabilities in
one revocation and rotates the keys of what they could read, exactly once even
across a crash and restart. A version that drops a role someone still holds
fails with `RoleInUse`; widened roles reach their holders at their next sync
with an admin device.
Copying a closed database folder is a valid backup; a restored copy that
peers have moved past reads and syncs but refuses to write
(`StoreSuperseded`), and rejoins as a new device.
A relay is an ordinary identity invited with store-only access
(`Access::Store`): it holds and forwards encrypted frames and
partition-key mailboxes for devices that are never online together, but never
receives a data key. `Configuration::store_only` opens such a database without
the application's schema, so one process can relay for many tenants
(`cargo run -p rocky-sdk --example relay`).
Every linked node of an admin can invite, renew and remove ordinary
members, and seals: Rocky seals
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
