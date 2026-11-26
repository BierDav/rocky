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
rocky-sdk = { git = "https://github.com/BierDav/rocky", tag = "v0.1.0-beta.3" }
rocky-tcp = { git = "https://github.com/BierDav/rocky", tag = "v0.1.0-beta.3" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Copy Rocky's dev profile into your own `Cargo.toml`. Cargo applies only the
root package's profile, and unoptimized crypto and SQLite make a debug build's
loopback sync take seconds instead of about 100 ms. Line tables keep file and
line numbers in a dependency's backtrace for less than full debug info costs.

```toml
[profile.dev.package."*"]
opt-level = 2
debug = "line-tables-only"

[profile.dev.package.rocky-core]
opt-level = 3

[profile.dev.package.rocky-credentials]
opt-level = 3
```

Rocky needs Rust 1.99 or later and edition 2024; every published crate sets
`rust-version`, so an older toolchain fails with a clear message. Pin the
toolchain with a `rust-toolchain.toml` next to your `Cargo.toml`:

```toml
[toolchain]
channel = "1.99.0"
```

The first build compiles 121 crates, among them C code (SQLite and ring), so
the host needs a C compiler. With this profile, a cold debug build of an app
that depends on `rocky-sdk` and `rocky-tcp` took 96 s on an 18-core Apple M5
Pro and left 1.7 GB in `target/`; with Cargo's full debug info for
dependencies it took 141 s and 2.4 GB. On a host with less than 8 GB of
memory, build with `cargo build -j2`.

## One device

Create a database, declare a table, write in a transaction and read it back.
The table declares no primary key, so Rocky keys it by a generated `id` and
fills it when an insert leaves it out.
The application name in `Schema::builder(application, version)`, like every
table, column and role name, is 1 to 64 ASCII letters, digits or underscores,
not starting with a digit; `build()` checks it and fails with
`SchemaError::InvalidName`.

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

    let insert = "INSERT INTO notes(body) VALUES(?)";
    database
        .write(async |tx| tx.execute(insert, params!["Hello, Rocky"]).await)
        .await?;

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

`Database::write(async |tx| ..)` runs its body as one transaction. It commits
when the body returns `Ok` and rolls back when the body returns `Err`, as a
refused statement does when `?` passes it on, or when the future is dropped.
The body's error is any type that converts from `rocky_sdk::Error`; a body that
ends in its last statement's result needs no annotation, and one that ends in
`Ok(value)` names it, as in `Ok::<_, rocky_sdk::Error>(id)`. One write runs at a
time: a write waits up to `TRANSACTION_TIMEOUT` (5 s) for another and then
fails with `ErrorKind::Busy`, and a write that holds the database longer than
that from its first statement is rolled back and fails with
`ErrorKind::TimedOut`, so keep slow work such as network calls outside the
body. Reads never wait for a write: `query` and `observe` beside an open write
see the last commit, and `status` returns the status last observed before it.

`Transaction::execute` and `query` refuse a statement the device's access can
never allow before it runs, with `Denied` naming the `Right`: an `INSERT` needs
`Append`, a `DELETE`, `REPLACE` or `INSERT OR REPLACE` needs `Write`, and an
`UPDATE` needs `Update(column)` for each column it sets, as does each `DO
UPDATE SET` column of an upsert. The check is the same whether the statement
would match rows or none, so a refused statement records nothing; partition
rights are checked as soon as the statement knows its rows. A statement refused
before it runs leaves the transaction open for the body to go on.

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

    let insert = "INSERT INTO notes(body) VALUES(?)";
    alice
        .database
        .write(async |tx| tx.execute(insert, params!["Hello from Alice"]).await)
        .await?;

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

`open` on a directory that holds no database fails with `ErrorKind::NoDatabase`,
and `create` or `join` on one that does with `DatabaseExists`, so a host that
boots unattended opens first and joins on `NoDatabase`. A join that cannot reach
the issuing device fails with `ErrorKind::Service(ServiceError::Unreachable)` or
`TimedOut`, which are retryable and whose `source()` is the transport's error,
such as a refused connection or an untrusted TLS certificate; a code whose
routes no attached transport dials, as a `wss://` route without rocky-tcp's
`websocket` feature, fails with `NoRoute`, which is not. A join that fails keeps
its device key so the issuer can recognize a retry: `open` then fails with
`JoinUnfinished`, and `join` with the same or a newer invitation resumes it.
`Configuration::link` and `PendingLink::confirm` fail the same way when the
other device cannot be reached.

## API map

`Configuration` opens a database and `Database` is everything after. Run
`cargo doc -p rocky-sdk --open` for every type and method; this table names the
ones an application calls, and the sections below explain each.

| Group | Method | For |
| --- | --- | --- |
| Lifecycle | `Configuration::create` | A new database; returns the root and owner keys once |
| | `Configuration::open` | The database already in the directory |
| | `Configuration::join` | Redeems an invitation code on a new device |
| | `Configuration::link` | Adds a device to a person who holds another |
| | `close` | Ends the database for every clone |
| | `flush` | Makes every returned write durable |
| | `logout` | Ends this device's session, optionally deleting its data |
| | `attach`, `detach` | Changes the network after open |
| SQL | `write` | One transaction: `Transaction::execute`, `query`, `next_id` and `token` |
| | `query` | A read outside a transaction |
| | `observe` | A `Stream` of changes to named tables |
| | `transaction_outcome` | Whether a commit applied or was dropped on replay |
| | `acknowledge_drop` | Clears a `WriteDropped` alert |
| Sync | `sync_now` | One sync pass, returning a `SyncReport` |
| | `set_sync` | Switches `SyncMode` |
| | `contact` | How other devices reach this one |
| | `add_peer`, `remove_peer` | Adds or forgets a peer by contact or `DeviceId` |
| People | `me` | Who this device belongs to and what it may do per table |
| | `people` | Every member, their access, partitions and devices; admins only |
| | `invite`, `invitations`, `cancel_invitation` | Issues, lists and cancels invitations |
| | `set_access`, `remove` | Changes or ends a member's access |
| | `make_admin`, `revoke_admin` | Promotes or ends an admin, with the root key |
| | `create_partition`, `grant_partition`, `revoke_partition` | Opens a partition and gives or takes it |
| | `link_requests`, `approve_link`, `unlink` | Adds or ends one of a person's devices |
| Status | `status`, `status_changes` | Blockers, alerts, network and schema, now or as they change |
| | `schema_version` | The schema version installed on this device |
| | `renew` | Renews this device's credentials with an admin device now |

## Keys

| Key | What it is | Who holds it | Needed for |
| --- | --- | --- | --- |
| Root key and its 24-word phrase (`Created::root`, `RootKey`) | The database's offline authority | The owner, offline; Rocky never stores it | `make_admin`, `revoke_admin` and `Counters::register` only |
| Person key (`Created::owner`, `Opened::identity`, `IdentityKey`) | One person's identity | The app, in its secure storage; `IdentityKey::phrase` and `IdentityKey::from_phrase` move it as 24 words | Every open, through `Configuration::identity`: it signs the session's invitations, links, unlinks and schema publications |
| Device key | One device's identity | Rocky, under the database directory or in `Configuration::secrets` | Opening and renewing the device's credentials; it never leaves the device |

`create` returns the root and the owner's person key once, and `join` returns a
minted person key in `Opened::identity`; each of those sessions signs with its
key. The owner's person key is derived from the root, so `RootKey::identity`
recovers it from the root phrase. `PersonId::of(&key)?` names the person any
`IdentityProvider` signs for, as `Invite::for_identity` and `remove` take it.
An `IdentityKey` is not `Clone`: share it as an `Arc<IdentityKey>`, which
`Configuration::identity` accepts as an `Arc<dyn IdentityProvider>`. Later
opens get the person key from `Configuration::identity`, and a call that needs
it without one is `Denied` with `IdentityRequired`. `create_with_root(root,
owner)` takes signers the application keeps elsewhere, such as passkey-backed
ones. Rocky never stores a person key: its one persisted form is the 24-word
phrase, which the app keeps in its own storage and reopens with
`IdentityKey::from_phrase`.

## People, access and partitions

`Database::invite(Invite::new(access))` issues an `Invitation` whose `code` a
joiner redeems with `Configuration::join`; admin devices invite, and the joiner
needs the issuing device online when it redeems the code. An invite can add
`partition`s, a `label` for the device and, on the first invitation, the
person, and a validity from one minute to 30 days (`Invite::valid_for`, ten
minutes by default). Every code admits one device once. A default code admits
whoever holds it, as a person key Rocky mints and returns in
`Opened::identity`. A code prints as one URL-safe line of about 250 characters
(`Display`, and `FromStr` to read it back) holding a secret, the database, the
issuing device and its routes at issue time: an admin device that issues codes
for later binds a fixed port or sets `rocky_tcp::Options::advertise` to a
stable URL, while iroh routes are keys and stay valid. The code holds no access
or application; the joiner learns them from the issuing device's answer and
fails with `JoinError::OtherApplication` when its schema is another
application's. The issuing device has then admitted the device, so the join
stays unfinished: repeating it in the same directory with the right schema
resumes it. A device's credentials last 180 days and renew at every sync with
an admin device; a device that meets none for 180 days holds expired
credentials (`status().authority.expired`) until it does.
`Database::invitations` lists open and redeemed invitations, and
`cancel_invitation` ends an unused one.

`Access` is `Full`, `Read`, `Store` or a `Role` the schema declares with
`SchemaBuilder::role`. A role reads, writes, appends to or updates named
columns of the tables it lists, and grants on one table compose: this doctor
reads wards, adds charts and edits only their notes, in the wards they are
granted.

```rust crates/rocky-sdk/tests/integration/readme_examples.rs
fn clinic() -> Result<Schema, rocky_sdk::Error> {
    let charts = Table::replayed("charts")
        .column(Column::new("ward", Affinity::Text).required())
        .column(Column::new("note", Affinity::Text).required())
        .primary_key(["ward", "id"])
        .partition_by(["ward"])
        .partition_group("ward");
    let doctor = Role::new(DOCTOR)
        .read("wards")
        .append("charts")
        .update_columns("charts", ["note"])
        .per_partition("ward");
    Schema::builder("clinic", 1)
        .table(Table::replayed("wards").column(Column::new("name", Affinity::Text).required()))
        .table(charts)
        .role(doctor)
        .build()
}
```

### Partitions

`Table::partition_by(columns)` splits a table into partitions by those
columns' values, each encrypted under its own key. Each partition belongs to a
partition group, which is the table's own name unless
`Table::partition_group` names one that several tables share, so that one
grant covers all of them. `Role::per_partition(group)` takes the name of a
partition group, not of a table: above, `charts` is in the group `ward`. A
role declared `per_partition` holds no partition of that group until an admin
grants one.

An admin device opens a partition in this order: `create_partition` creates
its key, `grant_partition(person, &partition)` gives it to a person whose role
is `per_partition`, and then the first write lands in it. A write before
`create_partition` is `Denied` with `PartitionNotCreated`, and
`Invite::partition` grants a partition with the invitation instead of
`grant_partition`.

```rust crates/rocky-sdk/tests/integration/readme_examples.rs
async fn open_ward(admin: &Database, doctor: PersonId, ward: &str) -> Result<(), rocky_sdk::Error> {
    let partition = Partition::new("ward", params![ward]);
    admin.create_partition(&partition).await?;
    admin.grant_partition(doctor, &partition).await?;
    let admit = "INSERT INTO charts(ward, note) VALUES(?, 'admitted')";
    admin
        .write(async |tx| tx.execute(admit, params![ward]).await)
        .await
}
```

A person whose role is `per_partition` and who holds no partition of the
group is refused every read of the group's tables, a plain `SELECT note FROM
charts` included, with `Denied` and `PartitionNotGranted { group }`, and
`status().data.blockers` lists a matching blocker. Rocky refuses on purpose
rather than return no rows, which would look like a ward with no charts: show
the person that they wait for an admin's grant, and read again when
`status_changes()` drops the blocker. Once the person holds a partition, a
read returns the rows of the partitions they hold, also when its filter names
others, and a write into a partition they do not hold is `Denied` with
`PartitionNotGranted`.

`people()` lists each `Person` with the partitions granted to them in
`Person::partitions`. `Full` and `Read` reach every partition without a
grant, so an admin asking who reads a partition counts them too:

```rust crates/rocky-sdk/tests/integration/readme_examples.rs
async fn readers(
    admin: &Database,
    partition: &Partition,
) -> Result<Vec<PersonId>, rocky_sdk::Error> {
    let people = admin.people().await?;
    Ok(people
        .into_iter()
        .filter(|person| {
            person.partitions.contains(partition)
                || matches!(person.access, Access::Full | Access::Read)
        })
        .map(|person| person.id)
        .collect())
}
```

Grants reach every device of the person, including devices that join or link
later. `revoke_partition` takes one back like any narrowing of access: it
rotates the partition's key and revokes the credentials of each of the
person's devices, which get new ones without the partition at their next sync
with an admin device.

A device that learns its access narrowed, at its next sync with an admin
device, deletes what it can no longer read: the rows of a revoked partition or
of a table its access no longer reads, Rocky's copies of them and their
encrypted history, including its own writes there that never synced, since the
credentials they were written under are revoked. A device that loses its last
partition of a table reopens and reads none until it is granted one again, and
then syncs that partition's rows anew.

### Devices, admins and removal

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
`make_admin` returns once the owner's device recorded the promotion; in
`SyncMode::Live` the promoted person's connected devices receive it at once,
and each observes it on its own `status_changes()`, as in
`while !changes.next().await?.authority.is_issuer {}`, after which it reads
and administers as an admin.
Admin devices raise `AdminDeviceLinked` and `AdminDeviceUnlinked` alerts when
an admin links or unlinks a device, and `AdminStale` when no device of an admin
was heard from for 30 days (`Configuration::admin_stale_after`).

A removed or unlinked device learns of it at its next sync. It then deletes
every row it holds, in local-only tables too, along with its commits that never
synced, and from then on refuses every read, commit and sync with
`ErrorKind::Removed`, also after reopening. It keeps only what reports
`Removed`: its credentials, the schema and its keys.
`logout(Logout::PurgeOnClose)` deletes those at close, and the directory with
them unless it holds files of the application's own. A device that is offline
cannot know yet: it keeps reading and committing locally until it syncs, and
those commits never reach anyone else.

### Data at rest

Rocky encrypts what devices send and what relays hold, not a device's own
store. `rocky.sqlite` in the database directory holds every row the device can
read, and Rocky's copies of them, in plaintext: any SQLite client that can open
the file reads them. The device's keys are under `secrets` in the directory
unless `Configuration::secrets` keeps them elsewhere. Protect the directory as
the operating system allows: full-disk encryption such as FileVault, BitLocker
or LUKS, the platform's data protection on phones, and file permissions. In a
browser the store lives in the origin's private file system, open to that
origin's code.

When a device deletes rows it can no longer read, SQLite overwrites their
content in the file, Rocky releases the freed pages and truncates the
write-ahead log; a read still running at that moment delays the truncation to
the next checkpoint. Rocky cannot reach copies outside the database file, such
as backups, file system snapshots, swap or the application's own copies, and a
device that has not synced since a change still holds what it held.

## Unattended hosts

A fleet of headless hosts, such as kiosks or servers, joins without anyone at
each host:

1. On an admin device that stays online, issue one default invitation per host
   with `Invite::new(access).valid_for(..)` and hand the host its code, one
   line that fits an environment variable or a provisioning file.
2. At boot, the host calls `open`, configured with
   `Configuration::identity(Arc::new(IdentityKey::from_phrase(&phrase)?))`
   once it holds a stored phrase; on `NoDatabase` or `JoinUnfinished` it calls
   `join` with its code instead, which needs the issuing admin device online.
3. `join` returns the person key it minted in `Opened::identity`, once: the
   host stores `identity.phrase()` in its own storage before anything else, and
   every later boot reopens from that phrase.

## How merging works

A transaction is recorded as SQL intents: its statements and their
parameters. Every device replays every transaction in one global order, so a
device's local commit may be reordered when older transactions arrive, which
`observe` reports as a `Rebuild`. A replayed transaction that violates a
constraint, such as a unique index, a foreign key or a `CHECK`, is dropped
whole, on every device; `observe` reports it in `TableChange::dropped` and
`transaction_outcome(token)` says what happened to it. `DropReason::Admission`
means the writer's access narrowed between its commit and the replay; a write
its access never allowed is refused at the statement instead.

A row's primary key is its identity on every device. A table that declares one
with `Table::primary_key` holds exactly its declared columns, and a foreign key
references that key: `check_ins.ticket REFERENCES tickets(code)`. When two
devices create the same key offline, the later insert in the global order fails
the key and its transaction is dropped; `INSERT … ON CONFLICT DO UPDATE` replays
onto the same row, and a deleted key may be created again. A table that declares
no key gets a generated `id` holding a `RowIdentity`, which Rocky fills when an
`INSERT … VALUES` omits it and `RETURNING id` reads back;
`Transaction::next_id(table)` allocates one ahead, for example to reference the
row from child rows, and `next_id_in(table, partition_values)` does so in a
partition. A table partitioned by its own id names
the id `next_id` allocated in every insert. Ids need no coordination; gap-free
numbers come from `rocky-coordinator`. A `RowIdentity` prints as hex and parses
back with `str::parse`, and a `Timestamp` column value prints as RFC 3339 in
UTC. `Transaction::token()` names the transaction for `transaction_outcome`
and drop reports, already inside the body, so an outbox row can store it. A
transaction that ran no write statement records and syncs nothing, so its token
never gets an outcome; a write that matched no row still records an intent.
`flush` makes every write that returned durable.

`Table::local(name)` declares a table whose rows stay on the device: they
never sync, are kept across reopening, and are deleted with the rest when the
device is removed. It holds per-device state, such as a till's parked sales or
its offline switch.

A write must replay identically everywhere, so Rocky refuses some SQL in a
transaction with `ErrorKind::Sql { unsupported, .. }`:

- a clock or random value, such as `datetime('now')` or `random()`, is
  `Unsupported::NonDeterministic(function)`, whose message says to bind the
  value as a parameter instead;
- a `WITH` clause is `Unsupported::With`;
- a write to a primary-key column, including `id` and the partition columns, is
  `Unsupported::KeyUpdate`, since a row's key never changes; delete the row and
  insert it again;
- an `INSERT … SELECT` into a table with a generated id that does not name `id`
  is `Unsupported::SelectWithoutId`; insert the rows with `VALUES`, or in a
  migration with `MigrationStep::insert`, and Rocky fills each id;
- `IS ?` and `IS NOT` comparisons are accepted;
- a statement whose intent is over 128 KiB fails at `execute` with
  `ErrorKind::TooLarge` and `Limit::Intent`, and a transaction whose
  statements fit alone but not together fails the same way when `write`
  commits it. A parameter takes its own size plus a few bytes, so a blob of
  about 120 KiB is the largest one statement carries, and an `INSERT … SELECT`
  records the
  rows it read.

### Indexes, defaults and references

`Table::index(Index::new(name, columns))` adds an index; `unique()` refuses two
rows with the same values, `descending(column)` appends a column in descending
order, and `expression` and `filter` index a deterministic expression or only
the rows a predicate holds for. `Column::default(value)` sets what an insert
that omits the column gets.

`Column::references(parent)` references the parent's single-column primary key.
A reference to a generated `id` is a `Blob` column holding the `RowIdentity`,
not its hex text: `build()` compares each reference column's affinity with the
parent key column's and refuses a mismatch with `SchemaError::ReferenceType`,
naming the column and both affinities. `Table::foreign_key(ForeignKey::new(columns,
parent))` references a multi-column key, such as a partitioned parent's
`(partition, id)`.

```rust crates/rocky-sdk/tests/integration/readme_examples.rs
fn ledger() -> Result<Schema, rocky_sdk::Error> {
    let accounts = Table::replayed("accounts")
        .column(Column::new("name", Affinity::Text).required())
        .column(
            Column::new("currency", Affinity::Text)
                .required()
                .default("EUR"),
        )
        .index(Index::new("accounts_by_name", ["name"]).unique());
    let entries = Table::replayed("entries")
        .column(
            Column::new("account", Affinity::Blob)
                .required()
                .references("accounts"),
        )
        .column(Column::new("cents", Affinity::Integer).required())
        .column(Column::new("booked", Affinity::Integer).required())
        .index(Index::new("entries_by_account", ["account"]).descending("booked"));
    Schema::builder("ledger", 1)
        .table(accounts)
        .table(entries)
        .build()
}
```

A foreign key deletes with `DeleteAction::Restrict` unless
`ForeignKey::on_delete` says `Cascade` or `SetNull`: deleting an account its
entries still reference fails at `execute` with `ErrorKind::Sql`, so a ledger
keeps its history. Replay enforces the same rule, partitioned tables included:
when one device deletes an account offline while another books an entry
against it, whichever transaction comes later in the global order is dropped
whole on every device, as for any other constraint. `ForeignKey::deferred`
lets a transaction write a row before the parent it references and checks the
key at commit; a `Restrict` delete is still refused at once.

## Sync

`Configuration::sync` picks the mode a database opens in and `set_sync`
changes it:

| `SyncMode` | Serves peers | Reaches out |
| --- | --- | --- |
| `Live` (default) | Yes | On its own, and pushes each commit to connected peers at once |
| `Manual` | Yes | Only on `sync_now` |
| `Off` | No | Never |

When a peer drops or closes its live link, for example because it restarts, a
`Live` device that dialed the peer redials it on its own, with a backoff that
starts near a second and doubles up to a minute, until the peer answers or the
database closes.

`sync_now` runs one pass and fails with `ServiceError::SyncOff` when sync is
off and with `NoPeerReached(report)` when no peer reconciled, which is also
what an owner with no other device yet gets. Its `SyncReport` prints as one
line; `complete()` is true when every known peer reconciled or another peer
covered it, and `pulled()` and `pushed()` count encrypted frames, not rows.
`add_peer`, `remove_peer` and `contact` manage peers, each named by its
`DeviceId`. `remove_peer` forgets a peer however the device learned it and
keeps a handover or discovery from bringing it back, also after a restart,
until `add_peer` adds it again; removing an unknown peer succeeds. Each sync
tells the other device this one's current route, so an issuer reaches the
device that joined with its invitation, and peers follow a device that reopens
on another port, without `add_peer`; a dial-only device tells none.

`observe(&["notes"])` returns `TableEvents`, a `Stream` of `TableChange`s for
the named tables, or all tables for an empty list; `TableEvents::next` reads one
without importing `StreamExt`, and the stream ends when the database closes. A
change is a coalesced invalidation, not a row diff: it names the tables to read
again, with their `causes` (`Local`, `Sync`, `Rebuild` or `Authority`, when the
device's access changed what it may read), and every change names a table or a
dropped transaction. `status`
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

    let stamped = "INSERT INTO notes(body) VALUES(datetime('now'))";
    let written = database.write(async |tx| tx.execute(stamped, params![]).await);
    let Err(error) = written.await else {
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
- `Closed`: the call came after `close`, from any clone; open the database
  again.
- `NoDatabase`, `DatabaseExists` and `JoinUnfinished`: the directory holds no
  database, already holds one, or holds a join to repeat.
- `Blocked { table, blockers }`: the data has not arrived; retry after the
  named blockers clear. It always names at least one.
- `Busy`: another write held the database for all of `TRANSACTION_TIMEOUT`,
  or too many calls are pending; retry.
- `TimedOut`: the write body held the database past `TRANSACTION_TIMEOUT` and
  was rolled back; move slow work out of the body.
- `TooLarge { limit, size, max }`: a write exceeds a size limit.
- `SchemaMismatch { installed, declared }`: the database runs a schema version
  this application cannot use. When `installed` is newer, it removed something
  the application declares: upgrade the application. When `declared` is newer,
  an admin device must publish it first, or this device sync with one that has;
  until then every read and write is refused before it runs, while `status` and
  sync keep working.
- `MigrationFailed { version, step }`: a migration step failed and its version
  was withdrawn; a retry does not clear it.
- `Sql { code, message, unsupported }`: SQLite refused the statement, or
  Rocky did for a replayed write when `unsupported` is set.
- `Schema(SchemaError)`: `build()` refused the schema, or the installed version
  cannot become it. `SchemaError::ForeignKey { key, rule }` names a foreign key
  SQLite could not enforce, such as `SetNull` over required columns,
  `SchemaError::ReferenceType` a reference column whose affinity differs from
  its parent key's, and `SchemaError::Change(RefusedChange)` the table, field or
  foreign key a new version may not change that way.

`Invite`, `Join`, `Link`, `Access` and `Service` carry the typed cause of an
invitation, link, access or network failure, and `Internal` is never
retryable.

## Schema evolution

Raise the version in `Schema::builder(application, version)` with every change
and declare the SQL that brings the data there with
`SchemaBuilder::migration(version, steps)`. A step may read the previous
version's rows, under that version's names, as `parent.<table>`, but not
correlated with the row it writes. When a step fails, `MigrationFailed {
version, step }` counts steps from 1. Adding a nullable column needs no step:
existing rows hold `NULL`, or the column's default.

Migration steps run once, when an admin device publishes the version. A write
that a device still on the older version makes afterwards, offline or before it
upgrades, reaches the new version holding only the new column's default. An
application that derives the column from other fields runs its rule again after
sync, for example when `observe` reports a `Sync` cause:

```rust crates/rocky-sdk/tests/integration/readme_examples.rs
fn notes(version: u32) -> Result<Schema, rocky_sdk::Error> {
    let mut notes =
        Table::replayed("notes").column(Column::new("title", Affinity::Text).required());
    let mut schema = Schema::builder("notes", version);
    if version >= 2 {
        notes = notes.column(Column::new("slug", Affinity::Text));
        schema = schema.migration(2, [FILL_SLUGS]);
    }
    schema.table(notes).build()
}

const FILL_SLUGS: &str = "UPDATE notes SET slug = lower(title) WHERE slug IS NULL";

async fn fill_slugs(database: &Database) -> Result<(), rocky_sdk::Error> {
    database
        .write(async |tx| tx.execute(FILL_SLUGS, params![]).await)
        .await
}
```

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
and `SchemaError::Change(RefusedChange::LostEarlierName { .. })`, and an
earlier name cannot be used again.
`cargo run -p rocky-sdk --example renamed_column` walks through it with two
devices. A primary-key column may be renamed this way, but a table's key never
changes: a version that adds, drops or swaps a key column fails to open with
`RefusedChange::ChangedKey`, so declare a new table and copy the rows into it
with `MigrationStep::insert`. Every other change the installed version cannot
take fails to open with a `RefusedChange` too, such as a retyped field, a new
required field without a default, or a foreign key over a field existing rows
already hold. A new required field with a reference fails with
`SchemaError::Change(RefusedChange::AddedReference(key))`, since existing rows
would hold no parent: declare the field optional with `references` and fill it
with an `UPDATE` migration step.

An admin device publishes a new version when it opens with it. A version that
drops tables or fields or narrows a role fails to open with
`ErrorKind::RolloutConfirmation`, whose `RolloutImpact` lists what goes; open
it with `Configuration::open_confirmed(&impact.digest)`. A version that drops a
role someone holds fails with `RoleInUse`.

An application that declares an older version than the database runs
(`DeclaredSchema::Behind` in `status`, and `DatabaseStatus::schema_version`
reads the installed version) keeps working. Only when a newer version removed
or renamed a table or field it declares does open, or a statement using it,
fail with `SchemaMismatch { installed, declared }`: an application still on the
old name has to upgrade, though the writes it made before the device learned of
the rename still land under the new name. An application that declares a newer
version than the database runs (`DeclaredSchema::Ahead`) gets
`SchemaMismatch` from every read and write until an admin device publishes that
version and this device syncs with it.

A write made under an earlier version that a later one can no longer apply,
such as a late write into a removed field, including one that a later version
replaced with a new field declared without `renamed_from`, is dropped on every
device, loudly: `observe` delivers a `DroppedTransaction` with
`DropReason::Schema`, and the writer's device and every admin device raise a
`WriteDropped` alert that survives restarts until `acknowledge_drop(token)`.

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
  network, and runs in the browser through relays; its `relay` takes a
  `rocky_iroh::Relay` and its `lookup` a `rocky_iroh::Lookup`, each `Default`,
  `Disabled` or `Custom(url)`. It claims `iroh:` routes. An
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

```toml
[dependencies]
rocky-coordinator = { git = "https://github.com/BierDav/rocky", tag = "v0.1.0-beta.3" }

[dev-dependencies]
rocky-voter = { git = "https://github.com/BierDav/rocky", tag = "v0.1.0-beta.3", default-features = false }
```

`rocky-coordinator` adds gap-free counters and admit-once claims to a
`Database`. `rocky-voter` runs the coordinator's voters; as a dev-dependency
it gives tests an authority in-process (`LocalCluster`, below), and
`default-features = false` leaves out the relay server its `server` feature
builds into the `rocky-voter` executable.

This till records a sale, then claims the sale's invoice number. Run it
against `scripts/coordinator-local DIR`, which builds `rocky-voter`, starts
three voters on loopback and prints the trust bundle: `cargo run -p
rocky-coordinator --example counters -- DIR/service/bundle.json EMPTY_DIR`.

```rust crates/rocky-coordinator/examples/counters.rs
use rocky_coordinator::{
    Claim, Claimed, ControlReceipt, CounterError, Counters, Name, Tenant, TrustBundle,
};
use rocky_sdk::{Affinity, Column, Configuration, Database, Schema, Table, Timestamp, params};
use std::{path::Path, time::SystemTime};

type Failure = Box<dyn std::error::Error + Send + Sync>;

const USAGE: &str = "usage: counters BUNDLE.json EMPTY_DIRECTORY";
const CONTROL_LIFETIME_SECONDS: u64 = 86_400;

#[tokio::main]
async fn main() -> Result<(), Failure> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [bundle, directory] = arguments.as_slice() else {
        return Err(USAGE.into());
    };
    let bundle = TrustBundle::from_json(&std::fs::read_to_string(bundle)?)?;
    let schema = Schema::builder("till", 1)
        .table(
            Table::replayed("sales")
                .column(Column::new("sale", Affinity::Text).required())
                .column(Column::new("created", Affinity::Integer).required())
                .column(Column::new("invoice", Affinity::Integer))
                .primary_key(["sale"]),
        )
        .build()?;
    let created = Configuration::new(Path::new(directory), &schema)?
        .create()
        .await?;
    let database = created.database;

    let counters = Counters::connect(database.clone(), &bundle).await?;
    let tenant = Tenant::generate();
    let receipt = counters
        .register(&created.root, &tenant, CONTROL_LIFETIME_SECONDS)
        .await?;
    let kept = (tenant.secret(), receipt.to_bytes());
    let receipt = ControlReceipt::from_bytes(&kept.1)?;
    counters
        .renew(
            &Tenant::from_secret(kept.0),
            &receipt,
            CONTROL_LIFETIME_SECONDS,
        )
        .await?;

    let invoices = Name::new("invoices")?;
    record_sale(&database, "sale-1").await?;
    let first = invoice(&database, &counters, &invoices, "sale-1").await?;
    let retried = invoice(&database, &counters, &invoices, "sale-1").await?;
    println!("sale-1 is invoice {first}; its retry is invoice {retried}");
    if retried != first {
        return Err("a retry took a second number".into());
    }

    counters.close().await;
    record_sale(&database, "sale-2").await?;
    match invoice(&database, &counters, &invoices, "sale-2").await {
        Err(closed) if matches!(closed.downcast_ref(), Some(CounterError::Closed)) => {}
        other => return Err(format!("closed counters answered {other:?}").into()),
    }
    database.close().await?;
    Ok(())
}

async fn record_sale(database: &Database, sale: &str) -> Result<(), Failure> {
    let created = Timestamp::try_from(SystemTime::now())?;
    database
        .write(async |tx| {
            tx.execute(
                "INSERT INTO sales(sale, created) VALUES(?, ?)",
                params![sale, created],
            )
            .await
        })
        .await?;
    database.flush().await?;
    Ok(())
}

async fn invoice(
    database: &Database,
    counters: &Counters,
    invoices: &Name,
    sale: &str,
) -> Result<i64, Failure> {
    let rows = database
        .query("SELECT created FROM sales WHERE sale = ?", params![sale])
        .await?;
    let created: Timestamp = rows
        .collect()
        .await?
        .first()
        .ok_or("no sale")?
        .get("created")?;
    let claim = Claim::new(sale, sale, created.system_time().ok_or("sale time")?);
    let Claimed::First(number) = counters.claim(invoices, &claim).await? else {
        return Err("another attempt holds this sale's number".into());
    };
    database
        .write(async |tx| {
            tx.execute(
                "UPDATE sales SET invoice = ? WHERE sale = ?",
                params![number, sale],
            )
            .await
        })
        .await?;
    Ok(number)
}
```

`Counters::connect(database, &bundle).await` reaches the coordinator a
`TrustBundle` names through an iroh client endpoint of its own. That endpoint
is a pure client: it serves no protocol and refuses every inbound connection,
and it never takes part in discovery or sync. iroh still binds wildcard UDP
sockets for it, as for any QUIC client. It uses the relay the voters' routes
name; with none named, it dials their direct addresses with no relay or lookup
when every route has one, and otherwise uses n0's relays and lookup. Clones of
`Counters` share the endpoint, and dropping the last clone stops it.

`SyncMode::Off` stops sync, not claims. `counters.close().await` is the
offline switch: it stops the endpoint for every clone, and every later call
fails at once with `CounterError::Closed`, which is not retryable, so generic
retry code stops instead of waiting for an authority that is not down. To
come back online, connect again. A till keeps its switch in a `Table::local`
row, which stays on the device and holds across restarts.

The owner registers the database once with
`counters.register(&root, &tenant, lifetime)`. `root` is the owner's root key
(`Created::root`), because the coordinator admits a database only on a grant
its root key signed: the first registration holds the database's counters, so
no admin or device may register it under a tenant of its own. `tenant` is the
coordinator account (`Tenant::generate()`). `renew(&tenant, &receipt,
lifetime)` extends the registration before it expires and needs both, so keep
`tenant.secret()` (restored with `Tenant::from_secret`) and the returned
`ControlReceipt` as `receipt.to_bytes()` (read back with
`ControlReceipt::from_bytes`).

A counter is the unit of uniqueness: a key is admitted once per counter, so
every door, till or device that must agree claims against the same name, such
as one `admissions` counter for all doors. A counter per door would admit a
ticket once at each door.

`counters.claim(&name, &claim)` takes a counter's next number for any person
whose access holds insert, update or delete rights; a reader is refused with
`Refusal::ReadOnly` and a removed person with `Refusal::Revoked`. The
application writes the number, an `i64` from 1 up, in its own transaction.
Commit the business row and `flush` before claiming: the claim takes its
number on the coordinator at once, so a device that crashes before its row is
durable consumes a number for a sale it lost.

`Claim::new(key, attempt, created)` names the business key, such as a ticket
or an order id, the application's own id for this try, such as the scan row's
id, and `created`, the time stored with that row. The answer is
`Claimed::First(n)` for the attempt that holds the key, on every retry, and
`Claimed::Taken(n)` for any other attempt while the key is in force. A retry
rebuilds the same `Claim` from the application's rows, so nothing about a
claim needs storing before it is sent. `Claim::new(id, id, created)` takes a
number no other key shares, such as an invoice number.

A key stays in force for seven days from `created` (`CLAIM_WINDOW`), or until
`Claim::expires(at)`, at most 366 days ahead; `at` too comes from the row,
never from the clock at claim time. A claim after its window is refused with
`CoordinationError::Expired`, which is not retryable, and takes no number, so
a device that retries a week late never takes a second number for one sale:
it learns loudly that the sale's number, if its first attempt took one, is
lost. A counter holds 16,384 keys in force (`MAX_CLAIMS`); a new key beyond
them is refused with `CoordinationError::Full` and takes no number, and only
expired keys are ever forgotten. A counter's last number is `i64::MAX`; after
it, a claim is refused with `Exhausted`. A database holds 64 counters, and the
tenant's storage limit may refuse a claim earlier with `Backpressure`.

Each call waits up to 12 seconds (`CALL_DEADLINE`);
`counters.deadline(std::time::Duration)` sets another bound.
`CounterError::retryable` says whether sending the same call again may succeed.
`Unavailable` means no voter accepted the request. `OutcomeUnknown` means the
request may have reached the coordinator without an answer: send the same
`Claim` again, which answers the same `Claimed` the first one took, never a
new one.

`rocky_voter::LocalCluster` is the authority for tests and local development.
The voters log through `tracing`.

- `LocalCluster::start(dir, voters).await` runs 3 to 7 voters in-process on
  loopback, keeping their keys, genesis and state under `dir`, and refuses a
  `dir` that already holds a cluster.
- `cluster.bundle()` is the trust bundle to connect with.
- `cluster.shutdown().await` stops every voter and waits until they stopped.
  Dropping a cluster without it stops them in the background, so call
  `shutdown` before opening the same `dir` again.
- `LocalCluster::open(dir).await` starts the voters `dir` holds again, with
  the same genesis, trust bundle, addresses and counters, so a test stops and
  reopens the authority to exercise an outage and the retries after it.
- `cluster.lose_next_claim_reply()` lets the next claim any voter receives
  commit and loses every reply to it, so that call fails with
  `OutcomeUnknown`; the same `Claim` sent again answers the number it took.

## Browser (wasm)

The same crates build for `wasm32-unknown-unknown`. Rocky must run in a
dedicated Worker, where it keeps each database in the origin's private file
system (OPFS); the directory you pass to `Configuration` names it there. In the
browser, a database dials a WebSocket server with
`rocky_tcp::Options::browser()`, or reaches any peer through `rocky-iroh`'s
relays; see [who can reach whom](#who-can-reach-whom) and
[a browser app](#a-browser-app).

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

### A browser app

[examples/browser-notes](examples/browser-notes) is a notes app to copy. Its
[`src/lib.rs`](examples/browser-notes/src/lib.rs) owns the `Database`, keeps it
in OPFS, dials with `rocky_tcp::Options::browser()` and exports `Notes.open`,
`add`, `list` and `watch` to JavaScript through wasm-bindgen;
[`www/worker.js`](examples/browser-notes/www/worker.js) runs it in a dedicated
Worker, and [`www/index.html`](examples/browser-notes/www/index.html) talks to
the Worker with messages. Its peer is the `websocket_server` example, which
serves the page and its files beside the sync route `/rocky`:

```sh
cargo build -p browser-notes --target wasm32-unknown-unknown
scripts/tools wasm-bindgen
local/bin/wasm-bindgen --target web --out-dir examples/browser-notes/www/pkg \
    target/wasm32-unknown-unknown/debug/browser_notes.wasm
cargo run -p rocky-sdk --example websocket_server
```

Open the `http://127.0.0.1:8080/#…` link it prints. The first visit joins with
the invitation code after `#`; later visits open the database from OPFS and
need no code. The server keeps its database in `local/notes-server` and the
owner's phrase in `local/notes-server.owner-phrase`, so it invites again on
every start. A deployed page sits behind an HTTPS proxy and syncs over
`wss://`, which the same dialer opens. `join` mints the browser's person key in
`Opened::identity` and the example drops it, so the browser reads and writes
but cannot link another device; keep its phrase if it should. Outside this
repository, install the `wasm-bindgen` CLI of the version in your `Cargo.lock`
with `cargo install wasm-bindgen-cli --version <it>`.

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
--create` keeps a notes database, `cargo run -p todo-desktop` runs several
devices of a shared todo app in one window, and
[examples/browser-notes](#a-browser-app) runs one in a browser.

## License

Source-available under the [PolyForm Noncommercial License
1.0.0](LICENSE): read, run, modify and share Rocky for any noncommercial
purpose. Commercial use needs a separate paid license from the copyright
holder; ask [d.bieregger@gmail.com](mailto:d.bieregger@gmail.com).
Contributions are accepted under the inbound grant in [AGENTS.md](AGENTS.md).
