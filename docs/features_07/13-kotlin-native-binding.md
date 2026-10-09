# 13. Kotlin/Native binding

Status: not started.
Owner: `sdks/native`; `sdks/kotlin/rocky-core`, `rocky-sqlite`, `rocky-room`
and the `rocky-native` tooling plugin. No dependency. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the iOS scope](scope-ios.md), [SDK packaging](../runtime/sdk-packaging.md),
the [SDK contract](../runtime/sdk-contract.md), the
[Kotlin driver](../runtime/sql-driver.md#the-kotlin-driver),
`sdks/native/src` and `sdks/kotlin/rocky-core/src/native`.

The Kotlin SDK calls the runtime through UniFFI's Kotlin bindings over JNA,
which Kotlin/Native lacks. The exported surface is small but not trivial:
objects with async methods (`NativeDatabase`, `NativeStatement`,
`NativeTableEvents`, `NativeEndpoint`) and callbacks from Rust into Kotlin
(`NativeSecretStore`, `ForeignTransport`, `ByteStream`, `ProtocolHandler`,
`NativeCommandHandler`). Give the three libraries iOS targets on one contract.

## Ordered slices

1. Decision record for the mechanism, after a spike that proves the two hard
   cases on the simulator: one async call and one callback from a Rust
   thread into Kotlin. Options are in
   [the scope](scope-ios.md#left-to-the-phase). Record whether the JVM and
   Android keep JNA; if they do, one definition must generate or check both
   bindings so they cannot drift.
2. Build the runtime as a static library for `aarch64-apple-ios` and
   `aarch64-apple-ios-sim`, with bundled SQLite and with and without the
   `iroh` feature. Authored Rust stays free of unsafe code.
3. `rocky-core`: move the JNA binding behind the platform boundary and add
   the iOS one. Common code, the API and its tests do not fork.
4. `rocky-sqlite` and `rocky-room` on iOS: the driver, Room's profile and
   invalidation, with Room's multiplatform runtime and its compiler.
5. Threads and memory: callbacks arrive on runtime threads; state the
   threading contract, and show that handles are released when Kotlin objects
   are closed or collected.
6. Run the libraries' tests on the simulator in a macOS check and add it to
   CI.

## Completion gate

- [Acceptance 1](scope-ios.md#acceptance) passes: the existing `rocky-core`
  tests, the driver tests and the Room qualification run on the simulator.
- A Kotlin `SecretStore`, a Kotlin transport and a Kotlin command handler are
  each called from Rust on iOS.
- A cancelled coroutine cancels its native call; a closed database releases
  its handles, shown by a leak check over 1,000 open and close cycles.
- The JVM, Android and packaged-consumer checks pass unchanged.
- The build without `iroh` compiles for both iOS targets.
- [SDK packaging](../runtime/sdk-packaging.md) names the iOS binding and its
  limits.

## Not in this phase

Packaging for consumers, the Keychain and lifecycle
([phase 14](14-ios-packaging-keychain-lifecycle.md)).
