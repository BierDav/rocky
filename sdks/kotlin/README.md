# Kotlin libraries

The APIs below describe the current implementation. [V4](../../docs/proposals/architecture-proposal-v4.md) governs future composition:
language-owned schemas/handlers, optional transports and packaged consumption
without a Rust project. The bounded JVM desktop APIs and packages are implemented; see [packaging](../../docs/runtime/sdk-packaging.md) and its platform evidence.

`rocky-core` provides Kotlin schemas, table actions, typed row access and the
ORM-neutral suspend/Flow client. `RockyDatabase` owns a bundled UniFFI native
engine, device [enrollment](../../docs/runtime/sdk-enrollment.md), peers and background
sync over a selectable [transport](../../docs/runtime/sdk-transports.md), iroh by
default. A transport can be written in Kotlin, and `RockyEndpoint` is an
[iroh endpoint the application owns](../../docs/runtime/sdk-transports.md#iroh) for its
own protocols next to Rocky's. [Coordinated writes](../../docs/runtime/coordinated-writes.md) run ordinary SQL
at a committed log head, with a metadata service or an in-process `DevelopmentLog`
for development. `rocky-sqlite` is an `androidx.sqlite`
driver on Rocky's own file, without a Room dependency ([SQL driver](../../docs/runtime/sql-driver.md)). `rocky-room` runs Room's DAOs on that driver
and refreshes their observers after sync ([Room](../../docs/runtime/room-integration.md)). The modules build JVM desktop and
[Android](../../docs/runtime/android.md), plus arm64 iOS device/simulator
through the [Kotlin/Native binding](../../docs/runtime/kotlin-native-binding.md).

The application defines its schema in Kotlin. No Rust, native host path or
application-specific operation codec is needed. Start with the
[shopping-list example](../../examples/shoppinglist-app/README.md) and
[integration contract](../../docs/runtime/room-integration.md).

`RockyDatabase.local` opens without networking. `openSession()` gives an independent
`RockySession`, with credentials, coordinated SQL and native `observeStatus()` Flow.
`attach` returns a separately closable network service; borrowed endpoints stay
application-owned. Starter `create/join/resume` share this implementation.
`ServiceCoordinator` pins a certified service trust bundle. Independent credential
issuers use `Credentials.prepare` and `RockyDatabase.install`.

Applications depend on `rocky:rocky-core`, `rocky:rocky-sqlite` or `rocky:rocky-room`;
the Android variant brings the native runtime. They need no Rust tools.
[Releases](../../docs/releasing.md) publish the libraries to GitHub Packages.

Partitioned schema declarations, Room overlays, scoped keys/grants and
transactional per-partition IDs are described in
[partitioned scopes](../../docs/runtime/partitioned-scopes.md).
`RockyDatabase.partitions` and `SqlWrite.nextId(table, partitionValues...)` use the
same packaged bindings as ordinary database requests.

## Build

A [Gradle](settings.gradle.kts) build. Building from source needs the pinned Rust
toolchain: Gradle runs Cargo for the native runtime and generates the UniFFI bindings.

| Directory | Contents |
| --- | --- |
| `rocky-core`, `rocky-sqlite`, `rocky-room` | The published libraries for the JVM, Android and arm64 iOS |
| `rocky-runtime-android` | The native runtime for Android, which `rocky-core` brings to applications |
| `qualification/` | Tests and conformance checks; never published |
| `build-logic/` | Convention plugins and the Cargo, binding and packaging tasks |
| `gradle/libs.versions.toml` | Every dependency version |

```sh
./gradlew publishToMavenLocal   # what the examples build against
./gradlew jvmTest               # JVM tests
./gradlew check                 # every test and qualification of this host
```

The [shopping-list example](../../examples/shoppinglist-app/README.md) is a separate
Kotlin toolchain project that resolves `rocky:rocky-room` from Maven local, as a released
application resolves it from the registry. Publish again after SDK or Rust changes.

| Task | Checks or produces |
| --- | --- |
| `:qualification:inventory:check` | The original inventory contract on its legacy process-host transport, and that each mismatched typed command in `rejected/` is a type error |
| `:qualification:schema:check` | Hand and Room definitions are equivalent and install natively |
| `:qualification:packaged-jvm:check` | A [packaged](../../docs/runtime/sdk-packaging.md) JVM consumer runs with only a JVM |
| `:qualification:android-discovery:qualify` | On an emulator, the Wi-Fi multicast lock follows [local discovery](../../docs/runtime/discovery.md#lifecycle-and-platforms) |
| `packageIos`, `qualifyIosPackage` | The [iOS package](../../docs/runtime/ios.md) and its consumer without Rust tools |
| `-Procky.qualification=true iosSimulatorArm64Test` | The simulator tests, with the runtime's fault injection |

Android needs the SDK with platform `android-35` and NDK 28.2 ([Android](../../docs/runtime/android.md));
without `ANDROID_HOME`, Android Studio's default location is used. The native ABI has its
own [verification script](../native/check).
