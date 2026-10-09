# 17. Kotlin/Native binding

Status: not started.
Owner: `sdks/native`; `sdks/kotlin/rocky-core`, `rocky-sqlite`, `rocky-room`,
their qualification modules and the `rocky-native` tooling plugin. No
dependency; slice 3 lands after [phase 2](02-change-driven-sync.md) and
[phase 4](04-small-fixes.md) on the `rocky-core` native sources. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the iOS scope](scope-ios.md), [SDK packaging](../runtime/sdk-packaging.md),
the [SDK contract](../runtime/sdk-contract.md), the
[Kotlin driver](../runtime/sql-driver.md#the-kotlin-driver),
`sdks/native/src`, `sdks/native/src/bin/bindgen.rs`,
`sdks/kotlin/tooling/rocky-native` and `sdks/kotlin/rocky-core/src/native`.

The Kotlin SDK calls the runtime through UniFFI's Kotlin bindings over JNA,
which Kotlin/Native lacks. The exported surface is:

- objects, most with async methods: `NativeDatabase`, `NativeDatabaseSession`,
  `NativeSession`, `NativeNetwork`, `NativeNetworkService`,
  `NativeConnection`, `NativeStatement`, `NativeTableEvents`,
  `NativeEndpoint`, `NativeHandlerReads`, `DevelopmentCoordinator`;
- synchronous callbacks from Rust into Kotlin: `NativeSecretStore`,
  `NativeCommandHandler`;
- async foreign traits: `ByteStream`, `ForeignTransport`, `ProtocolHandler`.

Common code imports only the generated package `rocky.internal.native`. Give
the three libraries the iOS targets (`iosArm64`, `iosSimulatorArm64`) on that
one contract.

## Settled

- Preferred mechanism: the existing `rocky-bindgen` generates the
  Kotlin/Native binding with Gobley's `gobley-uniffi-bindgen` crate, never
  its Gradle plugins. It applies only if a Gobley release supports the pinned
  UniFFI 0.32.2; the last release checked on 2026-10-09 named 0.29.4.
- Fallback: a Rocky-owned Kotlin/Native template in `rocky-bindgen` over
  cinterop of the C header UniFFI's Swift generator emits, with the same
  package and names. The phase is then XL; record that in the status line.
- Excluded: a hand-written C surface (it needs authored unsafe code) and any
  change of the UniFFI pin.
- The JVM and Android keep UniFFI's JNA binding. One `bindings` task emits
  both; common code compiling against both is the drift check.
- Public openers take a `String` path on all targets; the JVM keeps a `File`
  overload. `SecureRandom`, `System.currentTimeMillis` and `BigDecimal` in
  common sources are replaced by portable calls with unchanged results.
- Tests that use only portable APIs move to common; process, socket and
  file-walking tests stay in the JVM fragment and are listed.

## Ordered slices

1. Check Gobley against UniFFI 0.32.2 first. Then a spike on the simulator
   with the mechanism that results: one async call, one synchronous callback
   from a Rust thread and one async foreign trait (`ByteStream`). Write
   decision record 0058 with the mechanism and the evidence.
2. Build the runtime as a static library for `aarch64-apple-ios` and
   `aarch64-apple-ios-sim`, with bundled SQLite, with and without the `iroh`
   feature. Authored Rust stays free of unsafe code.
3. `rocky-core`: the `rocky-native` plugin emits the JNA binding for the JVM
   and Android and the cinterop binding for iOS. Apply the path and portable
   API changes above. Common sources do not fork.
4. `rocky-sqlite` and `rocky-room` on iOS: the driver, Room's profile and
   invalidation, with Room's multiplatform runtime and its compiler.
5. Port the tests: `HandlerReadsTest`, the driver test and the Room
   qualification tests gain a common part that runs on the simulator.
6. Threads and memory: callbacks arrive on runtime threads; document the
   threading contract, and release handles when Kotlin objects are closed or
   collected.
7. A macOS check that runs the simulator tests, added to CI after
   [phase 1](01-test-speed.md) has restructured the workflow.

## Completion gate

- [Acceptance 1](scope-ios.md#acceptance) passes: the common tests of the
  three libraries and of the Room qualification run on the simulator; the
  tests left on the JVM are listed with the API that keeps them there.
- A Kotlin `SecretStore`, a Kotlin transport with its `ByteStream` and a
  Kotlin command handler are each called from Rust on iOS.
- A `RockyEndpoint` hosts a Kotlin protocol next to Rocky's on the
  simulator, across a database close and reopen.
- A cancelled coroutine cancels its native call. After 1,000 open and close
  cycles the count of live native handles the binding reports is zero and
  resident memory has not grown by more than 10% since cycle 100.
- The JVM, Android and packaged-consumer checks pass; their only source
  change is the path overload.
- The build without `iroh` compiles for both iOS targets.
- [SDK packaging](../runtime/sdk-packaging.md) names the iOS binding, its
  threading contract and its limits.

## Not in this phase

- Packaging for consumers, the Keychain store and lifecycle
  ([phase 18](18-ios-packaging-keychain-lifecycle.md)).
- A Swift or Objective-C wrapper, macOS, watchOS, tvOS or Intel simulator
  targets.
- Moving the JVM or Android off JNA, or upgrading UniFFI.
- New exports in `sdks/native`; the surface is bound as it stands.
- Rewriting JVM-only tests around a new portable process or socket layer.
