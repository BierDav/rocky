# Kotlin/Native template attribution

The render module and templates derive from `gobley-uniffi-bindgen` 0.3.7,
published by the [Gobley project](https://github.com/gobley/gobley) under the
[Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
Original source: [published crate](https://crates.io/crates/gobley-uniffi-bindgen/0.3.7).

Rocky maintains this derivative for UniFFI 0.32.2. Changes replace Gobley’s
C++ adapter with UniFFI’s generated Swift C header, use the current integer
object handles and clone callbacks, check ABI versions/checksums, and remove
JVM/Android and alternate adapter generation. This is source reuse, not a
dependency on Gobley or its Gradle plugins. The generator entry point is
Rocky-authored; the derived source files retain their MPL notices.
