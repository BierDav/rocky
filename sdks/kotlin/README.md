# Kotlin libraries

The APIs below describe the current implementation. [V4](../../docs/proposals/architecture-proposal-v4.md) governs future composition:
language-owned schemas/handlers, optional transports and packaged consumption
without a Rust project. The bounded JVM desktop APIs and packages are implemented; see [packaging](../../docs/runtime/sdk-packaging.md) and its platform evidence.

`rocky-core` provides Kotlin schemas, table actions, typed row access and the
ORM-neutral suspend/Flow client. JVM `RockyDatabase` owns a bundled UniFFI native
engine, device [enrollment](../../docs/runtime/sdk-enrollment.md), peers and background
sync over a selectable [transport](../../docs/runtime/sdk-transports.md), iroh by
default. A transport can be written in Kotlin, and `RockyEndpoint` is an
[iroh endpoint the application owns](../../docs/runtime/sdk-transports.md#iroh) for its
own protocols next to Rocky's. [Typed strong commands](../../docs/runtime/typed-commands.md)
are declared with `Command` and decided by a `Handler` written in Kotlin,
with a `DevelopmentCoordinator` for one process. `rocky-sqlite` is an `androidx.sqlite`
driver on Rocky's own file, without a Room dependency ([SQL driver](../../docs/runtime/sql-driver.md)). `rocky-room` runs Room's DAOs on that driver
and refreshes their observers after sync ([Room](../../docs/runtime/room-integration.md)). The modules build JVM desktop and
[Android](../../docs/runtime/android.md).

The application defines its schema in Kotlin. No Rust, native host path or
application-specific operation codec is needed. Start with the
[shopping-list example](../../examples/shoppinglist-app/README.md) and
[integration contract](../../docs/runtime/room-integration.md).

`RockyDatabase.local` opens without networking. `openSession()` gives an independent
`RockySession`, with `credentials`, `commands` and committed `readiness()` Flow.
`attach` returns a separately closable network service; borrowed endpoints stay
application-owned. Starter `create/join/resume` share this implementation.
`ServiceCoordinator` pins a certified service trust bundle. Independent credential
issuers use `Credentials.prepare` and `RockyDatabase.install`.

Contributors build the package with `./kotlin do packageSdk -m packaged-consumer` and
qualify it with `./kotlin check packaged -m packaged-consumer`. Consumers use only the
resulting JVM jars/resources; Rust generation tools are not required at launch or for
application schemas and handlers. [Releases](../../docs/releasing.md) publish them to
GitHub Packages.

Partitioned schema declarations, Room overlays, scoped keys/grants and
transactional per-partition IDs are described in
[partitioned scopes](../../docs/runtime/partitioned-scopes.md).
`RockyDatabase.partitions` and `SqlWrite.nextId(table, partitionValues...)` use the
same packaged bindings as ordinary database requests.

## Build tooling

Building from source needs the pinned Rust toolchain; the build runs Cargo through
the [Kotlin toolchain plugins](https://kotlin-toolchain.org/latest/user-guide/plugins/topics/structure/)
in `tooling/`, which share helpers in `tooling/build-logic`. Their output is under
`build/` or `local/`. The native ABI has its own [verification script](../native/check).

| Plugin | Enabled on | Run with |
| --- | --- | --- |
| `rocky-native` | `rocky-core` | Every build: UniFFI bindings and the host's JNA library resource |
| `rocky-qualification` | `inventory-qualification`, `schema-qualification` | `./kotlin check qualify -m <module>` |
| `rocky-package` | `packaged-consumer` | `./kotlin do packageSdk`, `./kotlin check packaged`, `./kotlin do qualifyPackageOnLinux` |
| `rocky-android` | `rocky-runtime-android` | Every build: the [Android runtime](android-runtime/README.md) is current; `./kotlin do packageAndroid` |

`inventory-qualification` keeps the original inventory contract on its legacy
process-host transport, and checks that each mismatched typed command in
`rejected/` is a type error. It is not part of the native application API.
`schema-qualification` checks hand/Room definition equivalence and native installation.
`./kotlin check` runs both, the packaged consumer and every module's tests. Android
apps depend on `rocky-runtime-android`, whose native libraries a small
[Gradle build](android-runtime/README.md) publishes first.
