# 18. Shopping and market on iOS

Status: not started.
Owner: `examples/shoppinglist-app`, `examples/market-app`. Depends on
[phase 14](14-ios-packaging-keychain-lifecycle.md) and
[phase 15](15-market-example.md). Wave 2.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the iOS scope](scope-ios.md), the two examples and
[shopping on devices](../runtime/shopping-devices.md).

Both applications keep their logic and screens in a `shared` Compose
Multiplatform module built for the JVM and Android. Build them for iOS from
the same sources and run them on the simulator and the iPhone.

## Ordered slices

1. `shared` of each example gains the iOS targets. Replace or isolate what
   does not exist there: check each dependency (dependency injection, QR
   generation, icons, lifecycle view models), and keep platform code behind
   the existing expect/actual seams.
2. An `iosApp` per example: the application entry, the Keychain store, the
   lifecycle observer and the starter iroh transport.
3. Pairing on iOS: scanning a QR invitation with the camera and its
   permission; showing one; a paste fallback for the simulator, which has no
   camera.
4. Local network: the usage description and prompt needed by
   [phase 9](09-discovery-and-partner-choice.md)'s discovery when that phase
   has landed; before it, contacts only.
5. A device procedure for iOS beside the Android one: install, pair, share,
   write offline, background, resume, revoke. Each step driven with
   `agent-device` and verified by a named expectation, not a screenshot.
6. A simulator check in CI on a macOS runner that builds both applications
   and runs the procedure's steps that need no second device.

## Completion gate

- [Acceptance 5](scope-ios.md#acceptance) passes on the simulator paired with
  an Android emulator through a custody peer on the host: the shopping
  procedure, and a market reservation in each direction.
- The same build installs and opens on the attached iPhone; pairing by camera
  works there.
- No source file of either `shared` module is duplicated for iOS; the
  platform code added is listed with its reason.
- The JVM and Android applications pass their existing checks.
- [Shopping on devices](../runtime/shopping-devices.md) holds the iOS
  procedure and its first runs.

## Not in this phase

The physical cross-platform pair across networks
([phase 19](19-qualification.md)), a native SwiftUI screen and store
distribution. If [phase 17](17-shopping-without-a-roster.md) lands first, this
phase builds on its schema; otherwise phase 17 reruns this phase's checks.
