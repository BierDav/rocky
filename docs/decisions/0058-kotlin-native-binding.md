# 0058: Kotlin/Native binding mechanism

Status: accepted; phase 17 binding qualification complete.
Date: 2026-10-10.

## Context

[Phase 17](../features_07/17-kotlin-native-binding.md) requires the same
`rocky.internal.native` contract on JVM, Android and iOS. JNA does not run
on Kotlin/Native. The preferred released generator must support the pinned
UniFFI 0.32.2 without its Gradle plugins or a pin upgrade.

`cargo info gobley-uniffi-bindgen` resolves release 0.3.7. Its published
manifest pins `uniffi_bindgen` and `uniffi_meta` to `=0.29.4`, so it cannot
consume the current metadata. No compatible released Gobley generator was
found. The brief's fallback therefore applies and the phase grows to XL.

## Decision

`rocky-bindgen generate-native LIBRARY OUT_DIR` loads UniFFI 0.32.2 metadata,
emits the Swift generator's C header, and renders Rocky-owned Kotlin/Native
sources over cinterop. It writes `RockyTypes.kt`, `RockyNative.kt` and
`rocky_uniffiFFI.h` and an adapted UniFFI JNA implementation. Public names and
package follow the existing Kotlin binding. The plugin uses this single task
for both platforms; ordinary UniFFI generation remains available.

The fallback derives its rendering/templates from Gobley 0.3.7 under MPL 2.0;
[attribution](../../sdks/native/src/bin/kotlin_native/NOTICE.md) identifies
that source and the port. Gobley is not a build dependency. The port uses
UniFFI's current integer handles, clone/free callbacks, foreign-future dropped
callback, and unsigned C carriers. It checks the contract version and every
metadata checksum at initialization. Unsupported metadata is an error.
There is one ABI version, no legacy reader, authored C ABI or authored unsafe
Rust. The runtime also emits a static library with bundled SQLite.

## Evidence and consequences

The common SDK, SQLite driver and Room use the same generated contract on
JVM/Android and both iOS targets. The public path API uses `String`, retaining
JVM/Android `File` overloads. Platform time, secure randomness and portable
exact decimal rendering replace JVM-only calls in common sources.

The [binding qualification guide](../runtime/kotlin-native-binding.md) records
simulator callbacks, protocol hosting across database reopen, cancellation,
handle counts, resident memory and regression checks. SDK closure releases
owned generated handles explicitly; cleaners supply a collection fallback.
Callbacks follow the documented Rust-thread/default-dispatcher contract.

The iroh build required a [narrow dependency overlay](../../vendor/README.md):
netdev 0.46.3 and netwatch 0.19.3 use missing iOS Darwin declarations exposed
by libc only on macOS. The overlay reexports pinned upstream libc and adds
those ABI declarations with layout assertions, preserving upstream network
implementations and the locked iroh 1.3.0 admission path. It has no authored
unsafe code. Both manifests/locks select the same arrangement.

A macOS CI job qualifies both bindings and the target matrix after phase 1's
build job. Phase 18 owns consumer packaging, Keychain and lifecycle; this
binding decision does not claim those qualifications or physical-device use.
