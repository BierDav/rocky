# Module boundaries

This document maps the current implementation. Future boundaries and phase order
are governed by [V4](proposals/architecture-proposal-v4.md),
[design rationale](runtime/coordinator-retention.md#design-rationale) and
[features_06](features_06/README.md), not by the SDK's current composition choices.

The target separates reusable credentials, managed database/replication, strong
commands and optional iroh services. `rocky-sdk` becomes language bindings plus
an optional starter composition; it does not require ownership of networking.
Consumers define schemas and handlers in their own language without a Rust build.
There is no command crate: contracts stay in `rocky-core` and execution in the
runtimes. [Design rationale](runtime/sdk-composition.md#design-rationale)
records the dependency direction, ports and resource ownership.
The public coordinator retains only bounded generic metadata and receives no
application schema/handlers/payloads. The [bounded retention protocol](runtime/coordinator-retention.md) supplies safe
retirement and explicit expiry. [Public coordination](runtime/public-coordination.md)
adds tenant registration and an iroh client lane.

Core protocol, todo semantics, persistent replicas, two-replica synchronization,
strong publication and native desktop UI are implemented. See
[local replicas](runtime/local-replica.md), [two-replica sync](runtime/local-replica.md#replication-port)
and [strong publication](runtime/strong-publication.md).

## rocky-core

Implements shared identities, checked counters, canonical operation envelopes,
versions, dependency readiness, applied history/frontiers, strong commitments and
protocol errors. Its generic coordinator port carries metadata only.
It defines unsigned canonical manifests, publisher-bound definition IDs and scoped
operation references. It defines the shared `Application` reducer and `StoredApplication` compiled
projection contracts with optional registered command handlers and the tables a checkpoint carries, independent of storage engines.
Scoped grants and the `RowApplication`/`RowCommandApplication` contracts define
precise row transitions and deterministic encrypted-command handlers.
`CommandDefinitions` builds the canonical descriptors of typed commands with
their argument and outcome codec, `CommandHost` is where handlers run, `HandlerReads` gives immutable primary-key
reads and bounded key-range scans, and
`TypedCommands` is the row application of typed tables with commands
([typed commands](runtime/typed-commands.md)).
`SqlWrites` and the captured-row, intent and transaction-token types are the
evidence of a SQL transaction ([SQL sessions](runtime/sql-sessions.md)).
`DatabaseBinding` names the tables one session sees and the domain of each
([database bindings](runtime/database-bindings.md)). `RowWrite` adds the
write mask to a secure row change, and `TypedRows` is the secure row contract
of a logical schema's typed tables ([secure SQL](runtime/secure-sql.md)).
`ProjectionSchema` is the owned SQL an installation creates; compiled adapters
supply it through `CompiledSchema`. `LogicalSchema` is the canonical descriptor
of typed tables with its fingerprints, the scalar codec and the canonical
business-key encoding ([logical schemas](runtime/logical-schema.md)).
`SchemaDefinition` builds one from portable definition data, and `Profile`
diagnostics refuse what a profile cannot install ([schema definitions](runtime/schema-definitions.md)).
`net` is the network port: the `Transport` and `Connector` traits, the
versioned protocol names and the stream type, with `tokio` for its I/O traits
only ([SDK transports](runtime/sdk-transports.md)).
It has no dependency on the other workspace crates, Dioxus, a database or a
network implementation. Todo fields and application rules do not belong here.

Start at `crates/rocky-core/src/lib.rs`.

## rocky-credentials

Verifies and issues credentials without a database or endpoint: the signed
manifest, capability chain and policy types with subject revocation, the
distinct identities, raw grants and chain delegation, author pseudonyms and range verification,
grant profiles built on
raw grants with the invoke and execute rights of declared commands,
`Authority` issuance, the `KeyProvider`
contract with its file fallback, transport-free invitation, pairing and
renewal messages, and the coordination grant and tenant admission control
tokens of a coordination service. Runtimes enforce what it verifies. See
[credentials](runtime/credentials.md), [credential primitives](runtime/credential-primitives.md) and
[design rationale](runtime/credentials.md#design-rationale).

Depends on `rocky-core` only. `rocky-secure` and `rocky-sdk` depend on it;
`rocky-runtime` does not need it yet.

Start at `crates/rocky-credentials/src/lib.rs`.

[Partition credential primitives](runtime/partition-credentials.md) add selector
attenuation and recipient-bound partition key envelopes. Canonical tuple/address
types belong to `rocky-core`; `rocky-secure` owns authenticated partition frames,
checkpoints and SQL enforcement, `rocky-storage` owns SQL completeness proofs,
and `rocky-sdk`/Kotlin own consumer key lifecycle and schema overlays.

## rocky-storage

The base both runtimes build on: the bundled SQLite adapter in WAL mode
([design rationale](runtime/write-concurrency.md#engine)), scoped connections, row
capture and set digests, the database file owner, the shared `Error` and the
SQL session option, state and error types. `statement` checks application
SQL on a SQLite-dialect syntax tree and resolves its table names, `cursor`
runs snapshot cursors on their own connections and `observe` holds the
committed table events ([queries and observation](runtime/queries-and-observation.md)). It is an internal boundary: other
crates reach it through the runtime that re-exports what they need. The
`test-support` feature exposes fault-injection hooks to dependent tests.

Depends on `rocky-core` only. It knows no replica, operation log or credential.

Start at `crates/rocky-storage/src/lib.rs`.

## rocky-runtime

Owns the trusted profile: mutation admission, atomic transactions, operation
history, the durable outbox, incoming processing and committed query observation.
The storage adapter comes from `rocky-storage`. Incoming operations commit durable receipt before
bounded dependency-ready apply transactions. Named-peer acknowledgments are
persistent; exact receipt sets and bounded retained-history pages drive reconciliation.

Durable command requests, immutable preparations and verified result application
use a separate strong head and publication outbox. Coordinator calls stay outside
the storage worker and transactions.

`SqlSession` runs ordinary statements and transactions on a domain's typed
tables. A transaction leases the file writer; one row hook feeds digests and
capture, and the commit step turns the changed rows into operations
([SQL sessions](runtime/sql-sessions.md)). `Runtime::open_bound` records
bindings in the placement catalog; a `Database` session commits the rows of
several co-located domains in one transaction, and `migrate_placement` moves
a domain between file slots offline
([database bindings](runtime/database-bindings.md)). Sessions open snapshot
cursors and catalogs, and replicas and databases register table observers
published after each group commits ([queries and observation](runtime/queries-and-observation.md)).

The language-neutral `sdk::Session` wraps an already authorized compiled domain
with precise handles, local/strong reads and cancellable bound-query subscriptions.
The Kotlin/JVM process consumer is qualified separately; see [SDK contract](runtime/sdk-contract.md).

Depends on `rocky-core` and `rocky-storage`. It must not depend on
`rocky-secure`, `todo-app`, `rocky-coordinator` or Dioxus in production.
Database ownership is separate from scoped domain scheduling.
The manifest-bound `Runtime` installs domain namespaces spread over up to 16
files in the automatic layout, one by default, with one writer thread per file.
[Explicit placement](runtime/wide-schemas.md) supports 4,096 domains, up to 256 file
slots and opening a selected subset. Wide schemas bootstrap in resumable bounded
transactions, and non-unique indexes have an additive hook; legacy `Replica::open`
remains single-domain and single-file. Worker memory holds counters and a frontier; stored rows are the
state. Bound reads run application-declared authorized views over stored rows on
read-only connections, paged and index-served
([design rationale](runtime/local-replica.md#design-rationale)); strong reads can fetch matching payloads through
`StrongPayloadSource`. See [app runtime](runtime/app-runtime.md). Each domain's
worker serializes author work; the file writer group-commits projections,
history, outbox and revisions atomically, buffered by default and flushed before
anything leaves the device ([design rationale](runtime/write-concurrency.md#durability-and-exposure)),
and supplies bounded snapshot queries with committed watch observations
([write concurrency](runtime/write-concurrency.md)). Additional
backends are not an MVP requirement.

History is bounded: each trusted domain keeps a verified checkpoint copy of its
projection, prunes operations behind a horizon and rebootstraps a peer that fell
behind it ([checkpoints](runtime/checkpoints.md),
[design rationale](runtime/checkpoints.md#design-rationale)).

Start at `crates/rocky-runtime/src/lib.rs`.

## rocky-secure

Owns the secure profile around `EncryptedReplica`: admission of the signed
capabilities `rocky-credentials` verifies, reader key epochs and rotation,
encrypted row operations, a framework-transaction p2panda store adapter and bounded
authenticated native TCP synchronization, encrypted command scheduling and
scoped strong projections with [bounded strong history](runtime/strong-history.md),
and signed per-scope checkpoints, pruning and
rebootstrap ([encrypted checkpoints](runtime/encrypted-checkpoints.md)), and
replica roles: scope subscriptions, frontier range sync, eviction under full-node
custody and remote reads ([replica roles](runtime/replica-roles.md)).
Handlers run outside its lock and transactions, one command at a time, and
durable tickets record what a device knows of each command it submitted,
apart from whatever delivers it ([typed commands](runtime/typed-commands.md)).
`SignedCommandRequest` authenticates a node key before a trusted replica admits
the compiled command. Its files are separate from trusted legacy files; see
[permissions and sync](runtime/permissions-and-sync.md). Multi-scope transaction
bundles and authority-bound sessions are in [secure sessions](runtime/secure-sessions.md);
typed tables and SQL sessions over them in [secure SQL](runtime/secure-sql.md),
read-only command-controlled tables in [confirmed tables](runtime/confirmed-tables.md),
with availability-checked cursors and table events in
[queries and observation](runtime/queries-and-observation.md).
Secure history range access, redundant recovery archive pruning and same-definition
shadow rebuilds are described in [scaling and rebuilds](runtime/scaling-and-rebuilds.md).
Existence logs, puppets and virtual rows for references across reader scopes
are in [existence logs](runtime/existence-logs.md); `rocky-core` owns the
descriptor entry and `rocky-storage` the support-row SQL filter.

Depends on `rocky-core`, `rocky-credentials` and `rocky-storage`, never on
`rocky-runtime`: the two profiles share storage code, not replicas or admission.

Start at `crates/rocky-secure/src/lib.rs`.

## rocky-iroh

Implements the network port on iroh. `Attachment` registers Rocky's ALPN
handlers on a router the application owns and dials with its endpoint, without
ever closing either; `Options` is the starter that binds, routes and shuts
down its own. `Attachment::serving` answers another protocol set, such as a
coordinator voter's. It re-exports the iroh version it attaches to. See
[iroh networking](runtime/sdk-transports.md#iroh).

Depends on `rocky-core` only, never on a runtime, a database or `rocky-sdk`.

Start at `crates/rocky-iroh/src/lib.rs`.

## rocky-coordinator

Owns generic executor assignment, leadership epochs, expected-head validation,
active generations and confirmation deduplication. The deterministic in-memory state
machine supplies fixture registration, acquire/takeover, lookup and receipt
verification.

Depends on `rocky-core` and `rocky-credentials`, and on `rocky-iroh` for the
voter's client lane behind its default `iroh` feature. It receives metadata and
commitments, not application payloads, reducers or UI types. The legacy in-memory
fixture remains separate. The certified profile owns root bootstrap verification,
generic control certificates, persistent OpenRaft metadata/log/snapshot storage,
authenticated TCP and trusted signed receipts. A genesis with a `PublicService`
policy adds tenant registration, per-tenant quotas and request lanes, the
`Protocol::Coordinate` client lane, linearizable reads, voter backup and fenced
restore, and the `TrustBundle` clients pin; `rocky-voter` provisions and
administers voters. It loads no Directory or Alice application code in
production; application crates are test-only dependencies. See
[shared coordination](runtime/shared-coordination.md),
[public coordination](runtime/public-coordination.md) and
[multiprocess evidence](runtime/coordinator-evidence.md#coordination-and-crash-boundaries).

Start at `crates/rocky-coordinator/src/lib.rs`.

## directory-app

Owns deterministic Directory registration, node/deployment ownership, scoped
delegation and explicit user-deletion disposition. It implements the ordinary
application command/projection boundary; it grants no other app data rights.
See [Directory registration](applications/directory/registration.md).

## todo-app

Implements the compiled version-1 todo definition, payload codec, static admission,
field-register reducer, permanent tombstones, sorted visible rows and canonical
replay. The PublishListSnapshot policy and canonical immutable row codec support
exact-cut runtime replay. It is independent of desktop rendering.

Depends on `rocky-core`. Its `StoredApplication` implementation supplies the
todo projection schema, bound changes, retained stamps/tombstones and row decoding.
The runtime invokes this contract without a production dependency on this crate.

Start at `examples/todo-app/src/lib.rs`. See the
[implemented semantics and phase 2 boundary](applications/todo/operation-semantics.md).

## employees-app

Implements the signed-manifest row-policy example: public employee names and
separately encrypted 1:1 payroll companions. Depends on `rocky-core` only.
The runtime owns authentication, encryption and storage; the app owns row shapes
and permission-controlling fields. See [secure row qualification](runtime/permissions-and-sync.md).

## inventory-app

Implements the manifest-bound second application: LWW draft notes merge locally;
stock reservation executes against the confirmed predecessor, retains business
rejection and projects fulfillment job intents. Version 2 of the reservation
takes typed arguments beside the retained bytes of version 1. Depends on `rocky-core` only.
The typed SDK/composition examples live in `rocky-runtime/examples`; no UI SQL
writes or package construction is required. See [qualification](runtime/app-runtime.md#examples-and-qualification).

## accounting-app

Its `Alice` application owns observed-event checkboxes and deterministic review-intent jobs. `Accounting`
owns versioned event conditions and downstream review authorization. It depends
on `rocky-core` only. The runtime owns durable effect attempts, authenticated
observations and encrypted recovery artifacts; the coordinator owns explicit
acknowledged-loss domain replacement metadata. See
[recovery and effects](runtime/recovery-and-effects.md) for workflow and fault evidence.

## todo-desktop

Owns desktop composition, UI state and development controls. It instantiates
independent Laptop and Phone replicas with separate database files, and owns
the bounded, fault-controllable in-process transport adapter. It composes the
trusted coordinator, routes strong requests/results and exposes publication,
coordination availability and explicit Phone takeover controls.
It consumes observation and mutation APIs rather than writing SQL directly.

Depends on `rocky-runtime`, `rocky-secure`, `rocky-coordinator`, `todo-app` and `employees-app`. The coordinator
dependency is for composing the development setup, not placing its rules in UI.
Dioxus 0.7 renders both editable panels from runtime observations. The separate
`employees-desktop` executable composes secure row/command clients with an opaque
relay over authenticated native TCP; see [its guide](runtime/encrypted-commands.md).
The default todo executable accepts `--data-dir`; its window-close path drains
both runtimes before exit.

Start at `examples/todo-desktop/src/lib.rs`.

## Kotlin integration and shoppinglist-app

`crates/rocky-sdk` also packages the configuration-only `rocky-peer` custody
launcher and its container; see [always-on peer](runtime/always-on-peer.md).
`crates/rocky-sdk` owns the native reactor over `EncryptedReplica`: a schema's
typed secure tables, enrollment with grant profiles, issuer devices, renewal,
revocation with key rotation, supplied credentials, durable pseudonym assignment
and per-table signing keys, the `SecretStore` key
provider, contact records and sync with configured peers
([SDK enrollment](runtime/sdk-enrollment.md)). Its transports are TCP, the
in-process hub and the chunk adapter for transports defined outside Rust;
iroh comes from `rocky-iroh` behind the default `iroh` feature
([SDK transports](runtime/sdk-transports.md)). `Database` opens without a
network, `Session` handles share it, `Connection` serves a database
driver's writer and readers ([SQL driver](runtime/sql-driver.md)) and a `NetworkService` owns or borrows its
transport; `Engine` is the starter composition of the three
([SDK composition](runtime/sdk-composition.md)). `Commands` composes an
application's coordinator, or a coordination service its trust bundle names
([public coordination](runtime/public-coordination.md)), and a handler: the handler runs on its own thread
with a deadline, and command calls submit, run, deliver and settle tickets
([typed commands](runtime/typed-commands.md)). Its `owner` module signs a domain
owner's tenant-key registration, control and activation calls. It also accepts
portable definition tables through the SDK/native schema input and owns `TypedApp`, the typed-table
application with business keys and row incarnations, and its translation of
captured SQL rows; the Kotlin builder and Room importer share this definition path. The legacy
Room/policy importer preserves existing descriptors ([logical schemas](runtime/logical-schema.md)). It uses the existing
runtime, independent of example application reducers. `sdks/native` is the
excluded UniFFI ABI crate; generated FFI scaffolding is isolated from the safe
workspace, with no authored unsafe code. Besides the session it exports the
typed [driver connections](runtime/sql-driver.md) and table events, the
foreign transport contract, the foreign command handler, an in-memory
development coordinator and, with iroh, an endpoint handle that hosts the
application's protocols next to Rocky's. In-process UniFFI bindings are the
accepted consumer adapter ([design rationale](runtime/sdk-packaging.md#design-rationale)).

`sdks/kotlin/rocky-core` owns the [definition builder and Room JSON import](runtime/schema-definitions.md#build-in-kotlin), typed commands and their handlers, the ORM-neutral suspend/Flow SDK,
durable runtime port, transport and secret-store selection, peers, `RockyEndpoint` and JVM native lifecycle. `RockyDatabase` owns independently closable sessions and optional
network services; `Credentials` and `CommandPort` provide separate capabilities.
Signed generation lineage, backup archives and packaged resources are covered in
[SDK packaging](runtime/sdk-packaging.md) and [generations](runtime/generations.md). `sdks/kotlin/rocky-sqlite` is the `androidx.sqlite` driver on Rocky's file, with no Room
dependency ([SQL driver](runtime/sql-driver.md)). `sdks/kotlin/rocky-room` binds Room to
it: the pinned Room profile and invalidation from table events.
Neither depends on shopping-list rules or Compose. The Kotlin toolchain modules build
JVM desktop and Android. In `rocky-core`, `src/*.kt` is the platform-neutral client; `src/native` binds the
runtime through JNA on both platforms; `src@android` adds the Android Keystore store
and lifecycle binding. The `rocky-android` plugin ships the same sources as an AAR for
[Android](runtime/android.md). `sdks/kotlin/rocky-runtime-android` brings Android apps the native
runtime that the [`android-runtime`](../sdks/kotlin/android-runtime/README.md) Gradle build
publishes. `sdks/kotlin/tooling` holds the toolchain plugins that
build, package and qualify the SDK ([build tooling](../sdks/kotlin/README.md#build-tooling)).

`examples/shoppinglist-app` is a Kotlin-only Compose Multiplatform consumer whose typed Room entities
and DAOs run on Rocky's file, with a small repository, shared screen and QRKit helper;
`androidApp` is its Android app. Its exported Room
schema qualifies Kotlin import; `rocky-policy.json` remains a legacy Rust
generation fixture. `sdks/kotlin/room-qualification` records Room 2.8.4's statements as the
driver's pinned acceptance fixture and runs the same DAOs on Rocky's file.
`sdks/kotlin/schema-qualification` supplies Room-generated
market-shaped and compound-key scalar fixtures and DAO value tests. The SDK supplies all
native behavior. The walkthrough and tests showcase the API; full UI work is
paused. See [Room on Rocky's file](runtime/room-integration.md).

## Finding details

Search the owning crate first, then follow definitions and callers. Use specific
module names such as `outbox`, `observation`, `replication` and `confirmation` as
those responsibilities are implemented.

```sh
rg --files crates/rocky-runtime -g '*.rs' -g 'AGENTS.md'
rg -n 'observe|Observer|Revision' crates/rocky-runtime/src
cargo metadata --no-deps --format-version 1
```

Package descriptions, manifests and source targets form the live detailed map.
This document describes responsibilities, not an inventory of every source file.
