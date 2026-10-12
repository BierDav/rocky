# iOS

The Kotlin Multiplatform SDK targets arm64 iPhones and arm64 simulators, with
one common API and bundled SQLite. A consumer needs Xcode and Kotlin, without
Cargo, rustup, Rust sources or a manual native linker flag.
The [binding guide](kotlin-native-binding.md) covers callbacks and threading.

## Package and consume

Contributors run `sdks/kotlin/gradlew -p sdks/kotlin packageIos`. It writes
`sdks/kotlin/build/packaged-ios`, containing:

- `RockyRuntime.xcframework`: device and simulator static archives and the
  generated UniFFI header.
- `maven/`: `rocky:rocky-core`, `rocky:rocky-sqlite` and
  `rocky:rocky-room` (their `-iosarm64` and `-iossimulatorarm64` target modules), with common metadata, target klibs, cinterop klibs
  and dependency metadata. The cinterop klibs embed their target's static
  archive; Kotlin consumers need no additional link configuration.
- `BUILD.json`: version, minimum iOS (13.0), Kotlin (2.4.20), Xcode version
  and native build profile. These artifacts currently use the debug runtime.
- `SHA256.json`: every other distributed file and its SHA-256 digest.

`-ProckyVersion` selects the publication version (default `0.1.0`).
[Releases](../releasing.md) publish the Maven repository and a tarball of the
whole package. Local packages can be consumed from the `maven/` directory as a
Gradle repository (`maven { url = uri("/path/to/packaged-ios/maven") }`). The
[independent consumer](../../sdks/kotlin/qualification/packaged-ios/build.gradle.kts)
depends only on these coordinates. `sdks/kotlin/gradlew -p sdks/kotlin qualifyIosPackage`
verifies the full inventory, then builds and runs the consumer on the simulator with Rust tools absent from `PATH`.

## Secrets

The SDK ships no iOS secret store. The application passes its own `SecretStore`,
backed by storage it chooses, or none for the
[file fallback](credentials.md#key-providers). The runtime reads secrets often,
so the store answers loads from memory. The shopping example uses KSafe.

Whatever the store, secrets are absent from Rocky's identity directory and
SQLite file, open never manufactures a missing secret, and
`credentials.logout(purge = true)` deletes the installation's secrets after the
publisher-custody check permits logout.

## Visibility and sockets

Register `rocky.ios.RockyLifecycle.register()` on the main thread at startup.
`follow(database, onApplied, onError)` applies current visibility and observes
`UIApplicationDidEnterBackgroundNotification` and
`UIApplicationWillEnterForegroundNotification`. Each database has one ordered
worker and a conflated pending state: rapid changes finish in the latest state.
For a SwiftUI or other scene-based host, forward the combined scene visibility
with `sceneVisibility(true)` on active and `sceneVisibility(false)` on
background. Ignore inactive transitions; with multiple scenes, remain visible
while any scene is active. This feeds the same ordered worker.
Callbacks run on that worker. Handle errors explicitly, especially when the
application owns the endpoint. `unfollow` leaves the database in its current
state; `unregister` removes observers and workers. Neither closes a database,
a network or an endpoint.

`enterBackground()` cancels automatic sync, including a stalled dial, within
one second. Local reads and writes remain available and committed writes remain
durable. There are no background tasks, push wakeups or background sync.
`enterBackground()` also stops [local-network discovery](discovery.md#lifecycle-and-platforms),
which runs on the simulator alone: a device build reports the path
unavailable with the missing multicast entitlement as its reason.
`enterForeground()` refreshes the starter's sockets and resumes an immediate
sync pass when automatic sync was enabled at open. A dead starter endpoint is
rebound with the original transport key, retaining its node ID. Live starter iroh
endpoints receive a network-change notification to refresh reachability.
An application-owned dead endpoint produces an error; its owner must bind a
replacement and attach it. Rocky never closes it as part of foregrounding.
Android uses the same runtime call and keeps its existing lifecycle contract.

## Process death

| Lost with the process | What the next open does |
| --- | --- |
| Reactor, sessions, subscriptions and waits | Creates new handles; the application subscribes again |
| Starter endpoint and sockets | Binds with the stored transport key, retaining the node ID |
| Application-owned endpoint | The application binds it with its own secret and attaches it |
| An in-flight sync pass | Retries from durable state; sync is idempotent |
| A write whose reply was lost | `reconcile(handle)` reports whether its atomic local commit happened |
| Development coordinator's memory ledger | Starts empty; use a persistent service for durable coordination |

Persist the mutation handle before submitting a write. Rows, operations, outbox,
revision and handle evidence commit atomically. A process killed before commit
reopens without the write; a process killed after commit reopens with it,
including when it never received the reply. Retry under the same handle only
when reconciliation says it did not commit.

## Qualification

Run `./sdks/native/check-ios`, `sdks/kotlin/gradlew -p sdks/kotlin -Procky.qualification=true iosSimulatorArm64Test`,
`./sdks/native/qualify-ios-lifecycle`, `sdks/kotlin/gradlew -p sdks/kotlin packageIos`, then
`sdks/kotlin/gradlew -p sdks/kotlin qualifyIosPackage` on macOS/Xcode. CI owns these simulator and
artifact gates. The lifecycle gate checks secrets through a supplied store, byte-searches
for them, deletion and purge, notifications, the stalled-dial pause, automatic
sync after injected socket closure and subprocess death at commit boundaries.
The standalone simulator test harness embeds a test Keychain entitlement;
applications use their own signing identity and access group.
Only contributor simulator builds enable the `qualification` feature; published
packages contain neither socket-fault calls nor commit-exit hooks.

[Phase 18 evidence](../features_07/18-ios-evidence.md) records exact results and
limits. Physical iPhone qualification additionally requires the locked-device
behavior of the application's secret store, restart and restore refusal, and foregrounding after ten
minutes in the background. Simulator results do not establish those properties.
