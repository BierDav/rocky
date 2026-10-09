# Scope: iOS on Kotlin Multiplatform

Status: agreed 2026-10-09. Implemented by phases 13, 14 and 18 of
[the roadmap](README.md).
Owner: `sdks/native`, `sdks/kotlin` and its tooling, the example applications.

## Problem

The Kotlin SDK reaches the runtime through UniFFI's Kotlin bindings, which
call the native library with JNA. JNA exists on the JVM and on Android only,
so `rocky-core`, `rocky-sqlite` and `rocky-room` declare
`platforms: [jvm, android]` and nothing runs on an iPhone. The application
code is already portable: the shopping example's `shared` module is Compose
Multiplatform with Room and holds no Rust source.

## What I need

The same Kotlin API on iOS, with the platform pieces
[Android](../runtime/android.md) has:

- **A Kotlin/Native binding.** `rocky-core`, `rocky-sqlite` and `rocky-room`
  gain the iOS targets (device and simulator, `arm64`). Common code keeps one
  API; only the binding below it differs per platform.
- **Room.** Room's multiplatform runtime runs on Rocky's
  [`androidx.sqlite` driver](../runtime/sql-driver.md#the-kotlin-driver) on
  iOS as it does on Android. Rocky's file and bundled SQLite are the only
  database; the system SQLite is not linked for Rocky's tables.
- **Packaging.** An iOS consumer needs Xcode and the Kotlin toolchain, and no
  Rust toolchain or Cargo. The runtime ships as an XCFramework holding the
  static library for device and simulator, with a checksum inventory, beside
  the Kotlin artifacts.
- **Keychain.** A `SecretStore` backed by the Keychain, with the accessibility
  class stated and device-only items, as the
  [Keystore store](../runtime/android.md#keystore-secret-store) is on Android.
- **Lifecycle.** iOS suspends a backgrounded process within seconds and
  reclaims its sockets. Entering the background ends sync and effect dispatch
  without waiting on the network; returning rebinds the network and continues
  from durable state. Nothing in flight is lost that
  [Android's lifecycle contract](../runtime/android.md#lifecycle) keeps.
- **An application endpoint.** `RockyEndpoint` hosts Kotlin protocols next to
  Rocky's on iOS as on the JVM
  ([transports](../runtime/sdk-transports.md#attach-to-your-endpoint)).
- **Both example applications** build for iOS from their shared sources.

## Decided 2026-10-09

- **Kotlin Multiplatform only.** A Swift surface, hand-written or generated,
  is a separate topic and not in this roadmap.
- **One iPhone and simulators.** Qualification uses the iOS simulator and one
  physical iPhone, driven with `agent-device`; the cross-platform pair is that
  iPhone and one Android phone.

## Left to the phase

- **The binding mechanism.** Generated Kotlin/Native bindings, cinterop over
  the C scaffolding, or a narrower C surface. [Phase 13](13-kotlin-native-binding.md)
  chooses with a decision record; the constraint is one contract for all
  Kotlin targets and no second hand-maintained API.
- **Whether the JVM and Android keep JNA** or move to the same binding.

## Not requested

- Swift, SwiftUI or CocoaPods consumers; macOS, watchOS and tvOS targets;
  Intel simulators.
- Background execution: no background tasks, push wakeups or background
  sync. A backgrounded application does not sync.
- App Store distribution, signing automation and privacy manifests beyond
  what a development build on the attached iPhone needs.
- iCloud Keychain synchronization of secrets. Items stay on the device.

## Known platform constraints

Verify each against the current SDK before relying on it:

- Kotlin/Native has no JNA; callbacks from Rust (`SecretStore`, transports,
  protocol and command handlers) and async calls are the difficult part of
  the binding.
- Local-network access needs a usage description and prompts the user; raw
  multicast needs an entitlement. This bounds the local discovery of
  [phase 9](09-discovery-and-partner-choice.md) on iOS.
- The simulator shares the host's network; a physical iPhone does not reach
  the host's loopback, so device runs need a LAN route or a relay.
- iOS builds and checks run only on macOS hosts.

## Acceptance

1. `rocky-core`, `rocky-sqlite` and `rocky-room` pass their existing checks on
   the iOS simulator.
2. A consumer project with no Rust toolchain builds and runs against the
   packaged artifact.
3. Secrets survive an application restart and are absent from the
   application's files; a device restore without the Keychain cannot open the
   identity.
4. A write committed before the process is suspended or killed is present on
   reopen; sync resumes after foregrounding without user action.
5. The shopping and market applications run on the simulator and on the
   iPhone, paired with an Android device.
