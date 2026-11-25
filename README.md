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

## Install

Depend on Rocky by git tag. `rocky-sdk` has no networking of its own; add
`rocky-tcp` for plain TCP or WebSocket, including a browser's WebSocket, or
`rocky-iroh` for peer-to-peer QUIC with relays and discovery.

```toml
[dependencies]
rocky-sdk = { git = "https://github.com/BierDav/rocky", tag = "v0.1.0-beta.2" }
rocky-tcp = { git = "https://github.com/BierDav/rocky", tag = "v0.1.0-beta.2" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Copy Rocky's dev profile into your own `Cargo.toml`. Cargo applies only the
root package's profile, and unoptimized crypto and SQLite make a debug build's
loopback sync take seconds instead of about 100 ms.

```toml
[profile.dev.package."*"]
opt-level = 2

[profile.dev.package.rocky-core]
opt-level = 3

[profile.dev.package.rocky-credentials]
opt-level = 3
```

Rocky needs Rust 1.99 or later and edition 2024; every published crate sets
`rust-version`, so an older toolchain fails with a clear message. On a host
with less than 8 GB of memory, build with `cargo build -j2`.

## One device

Create a database, declare a table, write in a transaction and read it back.
`Transaction::next_id` fills the `id` primary key every replayed table has.

```rust crates/rocky-sdk/examples/one_device.rs
use rocky_sdk::{Affinity, Column, Configuration, Schema, Table, params};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = Schema::builder("notes", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()?;
    let directory = std::env::temp_dir().join(format!("rocky-one-device-{}", std::process::id()));
    let database = Configuration::new(&directory, &schema)?
        .create()
        .await?
        .database;

    let mut transaction = database.transaction().await?;
    let id = transaction.next_id("notes", vec![]).await?;
    let insert = "INSERT INTO notes(id, body) VALUES(?, ?)";
    transaction
        .execute(insert, params![id, "Hello, Rocky"])
        .await?;
    transaction.commit().await?;

    let rows = database.query("SELECT body FROM notes", params![]).await?;
    let note: String = rows
        .collect()
        .await?
        .first()
        .ok_or("no note")?
        .get("body")?;
    println!("{note}");
    database.close().await?;
    Ok(std::fs::remove_dir_all(directory)?)
}
```

`Configuration::open` opens the same directory again. `Database` is one handle:
clones share it, every call is a future the caller may drop, and `close` ends
it for every clone.

`open` on a directory that holds no database fails with `ErrorKind::NoDatabase`,
and `create` or `join` on one that does with `DatabaseExists`, so a host that
boots unattended opens first and joins on `NoDatabase`. A join that fails, for
example with `JoinError::IssuerUnreachable`, keeps its device key so the issuer
can recognize a retry: `open` then fails with `JoinUnfinished`, and `join` with
the same or a newer invitation resumes it.

## Two devices

Alice creates the database and invites Bob over TCP on loopback. Bob joins,
Alice writes, and Bob syncs and reads Alice's note.

```rust crates/rocky-sdk/examples/two_devices.rs
use rocky_sdk::{Access, Affinity, Column, Configuration, Invite, Network, Schema, Table, params};
use std::net::SocketAddr;

fn loopback() -> Network {
    let any_port = SocketAddr::from(([127, 0, 0, 1], 0));
    rocky_tcp::Options::new(any_port).into()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = Schema::builder("notes", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()?;
    let directory = std::env::temp_dir().join(format!("rocky-two-devices-{}", std::process::id()));

    let alice = Configuration::new(directory.join("alice"), &schema)?
        .network(loopback())
        .create()
        .await?;
    println!(
        "Alice's person key, for the app's secure storage: {}",
        alice.owner.phrase()
    );
    let invitation = alice.database.invite(Invite::new(Access::Full)).await?;

    let bob = Configuration::new(directory.join("bob"), &schema)?
        .network(loopback())
        .join(&invitation.code)
        .await?;

    let mut transaction = alice.database.transaction().await?;
    let id = transaction.next_id("notes", vec![]).await?;
    let insert = "INSERT INTO notes(id, body) VALUES(?, ?)";
    transaction
        .execute(insert, params![id, "Hello from Alice"])
        .await?;
    transaction.commit().await?;

    let report = bob.database.sync_now().await?;
    println!("Bob's sync: {report}");
    let rows = bob
        .database
        .query("SELECT body FROM notes", params![])
        .await?;
    let note: String = rows
        .collect()
        .await?
        .first()
        .ok_or("Bob has no note")?
        .get("body")?;
    println!("Bob reads: {note}");

    bob.database.close().await?;
    alice.database.close().await?;
    Ok(std::fs::remove_dir_all(directory)?)
}
```

`join` returns after its first pull from the issuing device, so Bob's first
read already sees Alice's rows. `SyncMode::Live`, the default, pushes Alice's
commit to Bob at once; `sync_now` makes the example deterministic. Run it with
`cargo run -p rocky-sdk --example two_devices`.

## Keys

| Key | What it is | Who holds it | Needed for |
| --- | --- | --- | --- |
| Root key and its 24-word phrase (`Created::root`, `RootKey`) | The database's offline authority | The owner, offline; Rocky never stores it | `make_admin`, `revoke_admin` and `Counters::register` only |
| Person key (`Created::owner`, `Opened::identity`, `IdentityKey`) | One person's identity | The app, in its secure storage; `IdentityKey::phrase` and `IdentityKey::from_phrase` move it as 24 words | Every open, through `Configuration::identity`: it signs the session's invitations, links, unlinks and schema publications |
| Device key | One device's identity | Rocky, under the database directory or in `Configuration::secrets` | Opening and renewing the device's credentials; it never leaves the device |

`create` returns the root and the owner's person key once, and `join` returns
a minted person key in `Opened::identity`; each of those sessions signs with
its key. The owner's person key is derived from the root, so
`RootKey::identity` recovers it from the root phrase. `PersonId::of(&key)`
names the person a key signs for, as `Invite::for_identity` and `remove` take
it. An `IdentityKey` is not `Clone`: share it as an `Arc<IdentityKey>`, which
`Configuration::identity` accepts as an `Arc<dyn IdentityProvider>`. Later opens get the person key from `Configuration::identity`, and a
call that needs it without one is `Denied` with `IdentityRequired`.
`create_with_root(root, owner)` takes signers the application keeps
elsewhere, such as passkey-backed ones, and `FileIdentityProvider` keeps a
person key in a native file.

## People, access and partitions

`Database::invite(Invite::new(access))` issues an `Invitation` whose `code` a
joiner redeems with `Configuration::join`; admin devices invite, and the joiner
needs the issuing device online. An invite can add `partition`s, a `label` for
the device and, on the first invitation, the person, and a validity from one
minute to 30 days (`Invite::valid_for`, ten minutes by default). A default code
admits whoever holds it once, as a person key Rocky mints and returns in
`Opened::identity`. `Database::invitations` lists open and redeemed
invitations, and `cancel_invitation` ends an unused one.

`Access` is `Full`, `Read`, `Store` or a `Role` the schema declares with
`SchemaBuilder::role`. A role reads, writes, appends to or updates named
columns of the tables it lists. A role declared `per_partition` holds no
partition of that group until an admin calls `grant_partition(person,
&partition)`; until then a read of its tables is `Denied` with
`PartitionNotGranted` and `status` lists a matching blocker. Once it holds one,
a read returns the rows of the partitions it holds, also when its filter names
others, and a write into a partition it does not hold is `Denied` with
`PartitionNotGranted`. Grants reach every device of the person,
including devices that join or link later, and `revoke_partition` takes them
back and rotates the key. `Full` and `Read` reach every partition.

A person links another device with `Configuration::link(contact)` on the new
device, configured with their person key, where `contact` is
`Database::contact().uri` of a device they hold. The
person compares `PendingLink::code` with `Database::link_requests` there,
approves with `approve_link` and finishes with `PendingLink::confirm`. A
person who lost their only device cannot link: an admin `unlink`s the lost
device and invites the person again with `Invite::for_identity(person)` and the
same access, and the replacement joins with the person's key and every
partition they were granted.

`Database::me` says who the device belongs to and what it may do per table.
Admins list members with `people`, change access with `set_access`, and end a
member with `remove(person)`. Narrowing access revokes the person's credentials
and rotates the keys of what they could read. The owner makes and ends admins
with `make_admin(person, root)` and `revoke_admin(person, removal, root)`, where
`Removal::Compromised` also removes the people the admin admitted and ends the
credentials they issued; an admin who should stay a member is invited again.
A promotion or access change reaches each of the person's devices at its next
sync with an admin device, including a device still joining when it was made.
Admin devices raise `AdminDeviceLinked` and `AdminDeviceUnlinked` alerts when
an admin links or unlinks a device, and `AdminStale` when no device of an admin
was heard from for 30 days (`set_admin_stale_after`).

A removed or unlinked device learns of it at its next sync. From then on it
refuses every read, commit and sync with `ErrorKind::Removed`, also after
reopening. It drops its keys, so its rows stay in its directory, unreadable,
until `logout(Logout::PurgeOnClose)` deletes the directory at close. A
device that is offline cannot know yet: it keeps reading and committing
locally until it syncs, and those commits never reach anyone else.

## How merging works

A transaction is recorded as SQL intents: its statements and their
parameters. Every device replays every transaction in one global order, so a
device's local commit may be reordered when older transactions arrive, which
`observe` reports as a `Rebuild`. A replayed transaction that violates a
constraint, such as a unique index, a foreign key or a `CHECK`, is dropped
whole, on every device; `observe` reports it in `TableChange::dropped` and
`transaction_outcome(token)` says what happened to it.

Every replayed table has an `id` primary key holding a `RowIdentity`, which
`Transaction::next_id(table, partition_values)` allocates without
coordination; gap-free numbers come from `rocky-coordinator`. `commit` returns
`Commit::Recorded(token)`, or `Commit::NoChanges` when the transaction changed
nothing and nothing syncs. `flush` makes every returned commit durable.

A write must replay identically everywhere, so Rocky refuses some SQL in a
transaction with `ErrorKind::Sql { unsupported, .. }`:

- a clock or random value, such as `datetime('now')` or `random()`, is
  `Unsupported::NonDeterministic(function)`, whose message says to bind the
  value as a parameter instead;
- a `WITH` clause is `Unsupported::With`;
- `IS ?` and `IS NOT` comparisons are accepted;
- an intent over 128 KiB is `ErrorKind::TooLarge` with `Limit::Intent`.

## Sync

`Configuration::sync` picks the mode a database opens in and `set_sync`
changes it:

| `SyncMode` | Serves peers | Reaches out |
| --- | --- | --- |
| `Live` (default) | Yes | On its own, and pushes each commit to connected peers at once |
| `Manual` | Yes | Only on `sync_now` |
| `Off` | No | Never |

`sync_now` runs one pass and fails with `ServiceError::SyncOff` when sync is
off and with `NoPeerReached(report)` when no peer reconciled, which is also
what an owner with no other device yet gets. Its `SyncReport` prints as one
line; `complete()` is true when every known peer reconciled or another peer
covered it, and `pulled()` and `pushed()` count encrypted frames, not rows.
`add_peer`, `remove_peer` and `contact` manage peers, each named by its
`DeviceId`. `remove_peer` forgets a peer however the device learned it and
keeps a handover or discovery from bringing it back, also after a restart,
until `add_peer` adds it again; removing an unknown peer succeeds.

`observe(&["notes"])` streams `TableChange`s for the named tables, or all
tables for an empty list, with their `causes`: `Local`, `Sync`, `Rebuild` or
`Authority`, when the device's access changed what it may read. `status`
returns a `DatabaseStatus`, and `status_changes` yields one each time it
changes. `status().data.blockers` lists data that cannot be read yet, each with
what clears it (`ClearsBy`); a read of such data fails with `ErrorKind::Blocked
{ table, blockers }` naming the same blockers.

Copying a closed database directory is a valid backup. A restored copy that
peers have moved past reads and syncs but refuses to write with
`StoreSuperseded`; invite it again as a new device.

## Errors

Every call returns one `rocky_sdk::Error`. Match on `error.kind()`, an
`ErrorKind`; `retryable()` follows from the kind.

```rust crates/rocky-sdk/examples/refused_write.rs
use rocky_sdk::{Affinity, Column, Configuration, ErrorKind, Schema, Table, Unsupported, params};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = Schema::builder("notes", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()?;
    let directory = std::env::temp_dir().join(format!("rocky-refused-{}", std::process::id()));
    let database = Configuration::new(&directory, &schema)?
        .create()
        .await?
        .database;

    let mut transaction = database.transaction().await?;
    let id = transaction.next_id("notes", vec![]).await?;
    let stamped = "INSERT INTO notes(id, body) VALUES(?, datetime('now'))";
    let Err(error) = transaction.execute(stamped, params![id]).await else {
        return Err("a clock value in a replayed write was accepted".into());
    };
    match error.kind() {
        ErrorKind::Sql {
            unsupported: Some(Unsupported::NonDeterministic(function)),
            ..
        } => {
            println!("{function}() differs on every device; bind its value instead: {error}");
        }
        ErrorKind::Denied { need, reason, .. } => println!("cannot {need}: {reason}"),
        _ if error.retryable() => println!("retry later: {error}"),
        _ => return Err(error.into()),
    }
    transaction.rollback().await?;
    database.close().await?;
    Ok(std::fs::remove_dir_all(directory)?)
}
```

`Denied { table, need, reason }` names the `Right` the call needed (`Read`,
`Write`, `Append`, `Update(column)`, `Allocate`, `Administer` or `Sign`) and
why the device lacks it:

| `DenyReason` | Cause | What clears it |
| --- | --- | --- |
| `NotGranted` | The person's access does not include the right, whatever partitions they hold | An admin's `set_access` |
| `PartitionNotGranted { group }` | The role grants the right, but the person holds no partition with these values | An admin's `grant_partition` |
| `PartitionNotCreated { group }` | No partition with these values exists | `create_partition` on an admin device |
| `StoreOnly` | A store-only device holds no readable data | Nothing; it is a relay |
| `AdminRequired` | Only an admin may do this | An admin device |
| `OwnerRequired` | Only the root key may do this | `make_admin` or `revoke_admin` with the root |
| `IdentityRequired` | The call signs as the person | `Configuration::identity` |

Other kinds an application meets:

- `Removed`: the person was removed or the device unlinked; nothing clears it.
- `NoDatabase`, `DatabaseExists` and `JoinUnfinished`: the directory holds no
  database, already holds one, or holds a join to repeat.
- `Blocked { table, blockers }`: the data has not arrived; retry after the
  named blockers clear. It always names at least one.
- `TooLarge { limit, size, max }`: a write exceeds a size limit.
- `UpgradeRequired { current, declared }`: the database is at a schema version
  that removed something this application declares; upgrade the application.
- `MigrationFailed { version, step }`: a migration step failed and its version
  was withdrawn; a retry does not clear it.
- `Sql { code, message, unsupported }`: SQLite refused the statement, or
  Rocky did for a replayed write when `unsupported` is set.

`Invite`, `Join`, `Link`, `Access` and `Service` carry the typed cause of an
invitation, link, access or network failure, and `Internal` is never
retryable.

## Schema evolution

Raise the version in `Schema::builder(application, version)` with every change
and declare the SQL that brings the data there with
`SchemaBuilder::migration(version, steps)`. A step may read the previous
version's rows, under that version's names, as `parent.<table>`, but not
correlated with the row it writes. When a step fails, `MigrationFailed {
version, step }` counts steps from 1.

Rename a column or table in one version with `Column::renamed_from` or
`Table::renamed_from`, with no copy step and no drop:

```rust crates/rocky-sdk/examples/renamed_column.rs
fn budget(version: u32) -> Result<Schema, rocky_sdk::Error> {
    let memo = match version {
        1 => Column::new("note", Affinity::Text),
        _ => Column::new("memo", Affinity::Text).renamed_from("note"),
    };
    let expenses = Table::replayed("expenses")
        .column(Column::new("cents", Affinity::Integer).required())
        .column(memo);
    Schema::builder("budget", version).table(expenses).build()
}
```

The renamed field keeps its identity, so its rows, indexes, foreign keys,
partition keys and role grants carry over. A write that a device made under an
older version's names, such as `INSERT INTO expenses(note) ...` from a device
that was offline during the rollout, is resolved in the schema it was written
for and lands in `memo` on every device. Renaming again keeps every earlier name,
oldest first: `Column::new("text", ..).renamed_from("note").renamed_from("memo")`.
A version that drops an earlier name fails to open with `ErrorKind::Schema`
and `SchemaError::LostEarlierName`, and an earlier name cannot be used again.
`cargo run -p rocky-sdk --example renamed_column` walks through it with two
devices.

An admin device publishes a new version when it opens with it. A version that
drops tables or fields or narrows a role fails to open with
`ErrorKind::RolloutConfirmation`, whose `RolloutImpact` lists what goes; open
it with `Configuration::open_confirmed(&impact.digest)`. A version that drops a
role someone holds fails with `RoleInUse`.

An application that declares an older version than the database runs
(`DeclaredSchema::Behind` in `status`, and `DatabaseStatus::schema_version`
reads the installed version) keeps working. Only when a newer version removed
or renamed a table or field it declares does open, or a statement using it,
fail with `UpgradeRequired`: an application still on the old name has to
upgrade, though the writes it made before the device learned of the rename
still land under the new name.

A write made under an earlier version that a later one can no longer apply,
such as a late write into a removed field, including one that a later version
replaced with a new field declared without `renamed_from`, is dropped on every device, loudly:
`observe` delivers a `DroppedTransaction` with `DropReason::Schema`, and the
writer's device and every admin device raise a `WriteDropped` alert that
survives restarts until `acknowledge_drop(token)`.

A device whose data predates a version that removed a table or field starts
from the checkpoint taken there. Until it has it, `status` and the `Blocked`
error of a refused read name a `SealRebootstrap` blocker with `ClearsBy::Admin`,
cleared by the next sync with an admin device. Each `Device` in `people`
carries the version it last reported (`Device::schema`), so an admin sees when
every device runs the version that makes a removal safe.

## Relays

A relay holds and forwards encrypted frames and partition-key mailboxes for
devices that are never online together, but never receives a data key. It is
an ordinary person invited with `Access::Store`, usually with
`Invite::for_identity`. `Configuration::store_only(directory)` opens such a
database without the application's schema, so one process can relay for many
applications; see `cargo run -p rocky-sdk --example relay`.

## Transports

A database attaches its network at open (`Configuration::network`) or later
with `attach`; `detach` closes only a transport the database started. Any
transport's options convert into a `Network` that Rocky starts and closes, as
in `.network(rocky_tcp::Options::new(bind).into())`.

A route is a URL: `tcp://10.0.0.2:4000`, `wss://sync.example/rocky` or an
`iroh:` route. Each transport claims URL schemes, and a device dials only the
routes whose scheme its transport claims. A device names at most one route of
its own, its transport's address, and hands it out in invitations, contacts and
syncs.

- **iroh**: `rocky_iroh::Options::default()` uses n0's public relays and
  lookup service, finds the database's other devices by topic and on the local
  network, and runs in the browser through relays. It claims `iroh:` routes. An
  application with its own iroh endpoint registers
  `rocky_iroh::Attachment::new(endpoint)` on its router and passes
  `attachment.transport()` as `Network::Borrowed`. On iOS, also copy the
  `[patch.crates-io]` entries for `netdev` and `netwatch` from Rocky's root
  `Cargo.toml`.
- **TCP**: `rocky_tcp::Options::new(bind)`, for networks that already route
  and encrypt between peers, such as a WireGuard mesh. It claims `tcp://`
  routes; `advertise("tcp://host:port")` names a route other than the bound
  address. It has no discovery or relays.
- **WebSocket**: with the `rocky-tcp` feature `websocket`, a native device also
  dials `ws://` and `wss://` routes, and `Options::websocket` accepts
  WebSocket handshakes on the listener. A `wss://` dial trusts the public web
  roots, plus the DER certificates of a private CA passed to
  `root_certificates`. Behind an HTTPS reverse proxy, or beside an
  application's own HTTP routes, `rocky_tcp::Attachment::external(url)` with
  `admit_websocket` serves connections the application's HTTP server upgraded;
  `cargo run -p rocky-sdk --example websocket_server` shows it with hyper.
- **Browser**: `rocky_tcp::Options::browser()` dials `ws://` and `wss://`
  routes through the browser's own WebSocket and trust store.
- **Dial-only**: `rocky_tcp::Options::dialer()` natively, and every browser
  transport, listen nowhere and name no route. Such a device syncs over the
  connections it opens itself; its peers never dial it and keep no route to it,
  so their syncs stay complete without it. Its `status().network.dial_only` is
  true.
- **Custom**: implement `rocky_sdk::transport::Transport`, claiming its
  schemes, and pass it as `Network::Borrowed`; `Network::Attached` also
  supplies discovery and network-change events. The application keeps and
  closes a borrowed or attached transport.

Rocky's encryption and authorization apply on top of every transport.

### Who can reach whom

- A joiner must reach its issuer. `join` dials only the routes the invitation
  carries, which is the issuer's own route when it invited. An issuer with no
  route, such as a dial-only device, issues invitations nobody can redeem.
- Peers sync with the devices they can dial. With `rocky-tcp`, a device that
  cannot be dialed, because it is dial-only or behind a NAT or firewall, is
  reached by nobody: it syncs only when it dials out, and two such devices meet
  only through a device both can dial.
- So a browser either joins an always-on admin device that listens for
  WebSockets, such as a server behind an HTTPS proxy, or uses `rocky-iroh`:
  both ends dial out to a relay, so a browser reaches an admin on a desktop
  with no open port, and the relay forwards only end-to-end-encrypted
  traffic.

## Coordinator

`rocky-coordinator` adds gap-free counters and admit-once claims to a
`Database`. `Counters::connect(database, &bundle, transport)` reaches the
coordinator a `TrustBundle` names. Voters accept only iroh connections, so pass
a `rocky-iroh` transport, such as
`rocky_iroh::Options::isolated("127.0.0.1:0").client().await?.transport()`.
The owner registers the database once with
`counters.register(&root, &tenant, lifetime)`, where `tenant` is the
coordinator account (`Tenant::generate()`, kept through `Tenant::secret`), and
renews it with `renew(&tenant, &receipt, lifetime)` before it expires.

`counters.claim(&name, &claim)` takes a counter's next number for any person
with write access, and the application writes that number in its own
transaction. `Claim::new(key)` makes one attempt on an application key such as
an order id or a ticket; store the claim and send the same value to retry. The
answer is `Claimed::First(n)` for the attempt that holds the key, on every
retry, and `Claimed::Taken(n)` for any other attempt while the key is in force,
so admitting a ticket once is
`matches!(counters.claim(&door, &Claim::new(ticket)).await?, Claimed::First(_))`.
`Claim::unkeyed()` takes a number no business key names.

A key stays in force for seven days (`CLAIM_WINDOW`), or until
`Claim::expires(at)`, at most 366 days ahead. A counter holds 16,384 keys in
force (`MAX_CLAIMS`); a new key beyond them is refused with
`CoordinationError::Full` and takes no number, and only expired keys are ever
forgotten. A database holds 64 counters, and the tenant's storage limit may
refuse a claim earlier with `Backpressure`.

`CounterError::retryable` says whether sending the same call again may succeed.
`Unavailable` means no voter accepted the request. `OutcomeUnknown` means the
request may have reached the coordinator without an answer: send the same
`Claim` again to learn its number, never a new one.

`scripts/coordinator-local DIR` builds `rocky-voter`, starts three voters on
loopback and prints the trust bundle that `cargo run -p rocky-coordinator
--example counters -- BUNDLE EMPTY_DIR` claims from. Tests in this workspace
start voters in-process with `rocky_voter::LocalCluster::start(dir, 3)` and
reach them through `cluster.bundle()`.

## Browser (wasm)

The same crates build for `wasm32-unknown-unknown`. Rocky must run in a
dedicated Worker, where it keeps each database in the origin's private file
system (OPFS); the directory you pass to `Configuration` names it there. In the
browser, a database dials a WebSocket server with
`rocky_tcp::Options::browser()`, or reaches any peer through `rocky-iroh`'s
relays; see [who can reach whom](#who-can-reach-whom).

Building compiles C (SQLite, ring), so it needs a clang that targets wasm32 and
an `llvm-ar` that indexes wasm objects, which Apple's `ar` does not (`rustup
component add llvm-tools`). Unoptimized builds also poll futures deeper than
wasm's default 1 MiB stack. In your `.cargo/config.toml`, with the path naming
your toolchain and host:

```toml
[target.wasm32-unknown-unknown]
rustflags = ["-C", "link-arg=-zstack-size=16777216"]

[env]
AR_wasm32_unknown_unknown = "<rustc --print sysroot>/lib/rustlib/<host>/bin/llvm-ar"
```

Rocky links its own SQLite build in the browser (`rocky-sqlite`, see
[vendor/README.md](vendor/README.md)). An application that uses rusqlite itself
depends on it with `default-features = false`, since rusqlite's default
`ffi-sqlite-wasm-rs` links another SQLite and the link fails.

### Testing in a browser

Browser tests are `wasm-bindgen-test` tests in a dedicated Worker:

1. Install the `wasm-bindgen-test-runner` of the `wasm-bindgen` version in
   your `Cargo.lock` (`cargo install wasm-bindgen-cli --version <it>`) and set
   it as the runner under `[target.wasm32-unknown-unknown]`.
2. Install headless Firefox with `geckodriver`, or a Chrome with the
   `chromedriver` of its major version, and name the driver in `GECKODRIVER`
   or `CHROMEDRIVER`.
3. Run `WASM_BINDGEN_USE_DEDICATED_WORKER=1 cargo test --target
   wasm32-unknown-unknown`.

Rocky's own `scripts/wasm-runner` does this with pinned drivers:
`ROCKY_BROWSER=firefox` or `ROCKY_BROWSER=chrome ROCKY_CHROME=<a
chrome-headless-shell of chromedriver's major version>`, and
`scripts/browser-sync` starts a native peer (`examples/browser_peer.rs`) with
a relay and a WebSocket listener that rocky-sdk's `browser_sync` tests join.

### Size

A browser app that joins and syncs with `rocky-sdk` and
`rocky_tcp::Options::browser()` measured, before `wasm-bindgen`, with Rust 1.99
on wasm32 (each row adds to the one above):

| Release profile | `.wasm` | gzip -9 |
| --- | --- | --- |
| Cargo's defaults | 55.5 MB | 14.6 MB |
| `strip = true` | 47.8 MB | 13.8 MB |
| `lto = true`, `codegen-units = 1` | 27.0 MB | 8.0 MB |
| `opt-level = "z"`, `panic = "abort"` | 13.6 MB | 4.7 MB |
| then `wasm-opt -Oz` | 12.2 MB | 4.6 MB |

Link-time optimization removes the most, and `opt-level = "z"` halves the
rest; its cost in speed is not measured. Panics already abort in a browser, so
`panic = "abort"` changes no behavior. Serve the file compressed.

```toml
[profile.release]
lto = true
codegen-units = 1
opt-level = "z"
panic = "abort"
strip = true
```

## Developing Rocky

See [AGENTS.md](AGENTS.md) for contribution rules. Crates live in `crates/`,
applications in `examples/` and performance probes in `benchmarks/`.

```sh
./scripts/setup
./scripts/lint
./scripts/check
```

`./scripts/check --package rocky-sdk` checks one package and its workspace
consumers, `--unit` runs just the unit tier, `--wasm` runs clippy for wasm32
and the tests in a headless browser, and `--all` adds e2e. `--changed` scopes to
jj's current change. `./scripts/lint --fix` formats.

More examples: `cargo run -p rocky-sdk --example notes -- ./local/notes
--create` keeps a notes database, and `cargo run -p todo-desktop` runs several
devices of a shared todo app in one window.

## License

Source-available under the [PolyForm Noncommercial License
1.0.0](LICENSE): read, run, modify and share Rocky for any noncommercial
purpose. Commercial use needs a separate paid license from the copyright
holder; ask [d.bieregger@gmail.com](mailto:d.bieregger@gmail.com).
Contributions are accepted under the inbound grant in [AGENTS.md](AGENTS.md).
