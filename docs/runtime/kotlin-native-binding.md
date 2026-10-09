# Kotlin/Native binding qualification

`rocky-core`, `rocky-sqlite` and `rocky-room` share their Kotlin API and common
sources across JVM, Android, `iosArm64` and `iosSimulatorArm64`.
[Decision 0058](../decisions/0058-kotlin-native-binding.md) records the XL
fallback over UniFFI 0.32.2. Consumer packaging, Keychain storage and application
lifecycle qualification belong to [phase 18](../features_07/18-ios-packaging-keychain-lifecycle.md).

## Generate and qualify

Use an Apple silicon macOS host with Xcode, the pinned Rust toolchain and
both `aarch64-apple-ios` and `aarch64-apple-ios-sim` standard libraries:

```sh
./sdks/native/check-ios
./sdks/native/qualify-ios-binding
./sdks/native/qualify-ios-sdk
./kotlin test -p jvm -m rocky-core -m rocky-sqlite -m rocky-room -m room-qualification
./kotlin check packaged -m packaged-consumer
./kotlin build -p android -m rocky-core -m rocky-sqlite -m rocky-room
```

The matrix builds the bundled-SQLite static runtime on both targets, with and
without iroh, at deployment target 13.0. It reports every failed configuration.
The isolated binding fixture qualifies an async session call, Rust-thread
secret callbacks and Kotlin transport/ByteStream enrollment without iroh.
The SDK check runs the common core, SQLite and Room qualification tests with
iroh, plus public SecretStore/custom transport enrollment, endpoint, command,
cancellation and lifetime tests on the simulator. The public callback fixture
also checks zero binding handles after both databases and transports close.
The repository `kotlin` launcher supplies the absolute source fixture directory
through `ROCKY_TEST_ROOT` and `SIMCTL_CHILD_ROCKY_TEST_ROOT`, including for plain
`./kotlin check`; no arbitrary sleep synchronizes a new test.

The `rocky-native` plugin's single `bindings` task emits common records,
callback contracts and expect objects, UniFFI's JNA implementation for the
JVM/Android, and Rocky's cinterop implementation for iOS. It adapts JNA's
platform declarations to the common contract without replacing JNA's ABI
implementation. Compiling common consumers on both platforms checks drift.
Per-target tasks build the static archive before cinterop. Generated artifacts
remain under ignored `build/` or `local/` directories.

The host native library is a generated `@jna` resource, so JVM tests and
Android unit tests running on the host JVM can both load it. Android consumer
packaging uses the per-ABI libraries from `rocky-runtime-android` instead.

The locked iroh dependencies need missing iOS Darwin network declarations.
[The dependency overlay](../../vendor/README.md) preserves upstream netdev,
netwatch and iroh behavior, reexports libc 0.2.190, and adds the shared Darwin
IPv6 ioctl types and routing constants. Compile-time assertions check the
IPv6 ioctl layout. No endpoint admission or feature is bypassed.

## Threads, cancellation and ownership

Synchronous secret-store and command callbacks execute on the calling Rust
runtime thread, not an iOS UI dispatcher. Implementations must be thread-safe,
bounded and synchronous. Command handlers remain deterministic and must not
perform external effects. Async foreign traits run Kotlin coroutines on the
default dispatcher; dropping their Rust future cancels the Kotlin job.

Rust futures use cancellable Kotlin continuations. Cancellation invokes the
native cancel function; the future is freed in `finally`, including cancellation
before dispatch. As on the JVM, cancellation abandons a wait and does not undo
an admitted write. Persist stable intent handles and reconcile committed work
([SDK packaging](sdk-packaging.md#batches-cancellation-and-reconciliation)).

Generated objects support explicit `destroy()` and Kotlin/Native cleaners.
The cleaner retains a separate disposable and an atomic guard frees it once.
SDK database/session, driver connection/statement, observer and handler-read
closures release their owned handles. Shutdown precedes destruction where
required. Temporary network wrappers are released after composition;
application-owned endpoints remain borrowed across database close and reopen.
A Kotlin protocol handler releases its stream wrapper after completion without
resetting a finished stream.

`bindingLiveHandles()` counts owned Rust objects, callback-map entries,
pending continuations and Rust futures. `bindingPendingContinuations()` lets
the cancellation fixture wait for a registered native poll. The lifetime
fixture reopens one durable installation 1,000 times and checks zero live binding
handles after every close, before any collection. It warms the collector after
creation and collects every 100 cycles. The Mach RSS samples at cycles 100 and
1,000 therefore follow equal allocation windows. Both samples also report
Kotlin heap allocation, retained-object counts and Darwin native allocation. The final RSS must be at
most 110% of the cycle-100 RSS.

The uncached release iOS test executable uses Kotlin/Native's
[per-object allocation policy](https://kotlinlang.org/docs/native-memory-manager.html#disable-allocator-paging)
(`-Xbinary=pagedAllocator=false`); application binaries keep their default
policy. Debug tests also exercise the toolchain's cached runtime and retain
the same RSS assertion. The default paged allocator produced RSS growth of 18.92% while
Kotlin heap capacity fell, retained objects changed from 3,329 to 3,336 and
Darwin live allocations changed by only 4,240 bytes. All native handles were
zero. Reclaiming freed Darwin malloc caches returned zero bytes, excluding
those caches as the cause. RSS was measuring Kotlin allocator page residency,
not accumulating database ownership.

Per-object allocation removes that paging noise without changing the 1,000
cycles, per-close handle assertions or 10% RSS limit. Collection every 100
cycles compares equal transient allocation windows; it occurs after the
zero-handle assertion and cannot mask an unclosed native object. A targeted
release run measured 83,771,392 bytes at cycle 100 and 83,820,544 at cycle
1,000 (+0.059%), with Kotlin live heap 150,696 to 150,792 bytes. These are
binding ownership measurements under the stated allocator policy, not a
claim about production workload memory.

The standalone native crate uses the same optimized dependency development
profile as the workspace, so host and simulator integration tests do not pay
for unoptimized crypto and SQLite.

## Test placement

The SQLite driver suite uses the `sqlite-test-host` fragment on JVM and iOS.
Android host tests supply mocked framework exceptions with no message and an
Android-only bundled SQLite JNI library; those hosts cannot run this reference
suite. Every original assertion runs on the JVM and simulator instead.

Common simulator/JVM tests cover `HandlerReadsTest`, driver scalar/coercion,
statement/snapshot and unsupported-SQL behavior, Room's recorded statements,
trace replay and DAOs, moves/deletes and committed invalidation between two
databases. The pinned Room 2.8.4 compiler generates the common implementation;
Room's bundled SQLite driver supplies the reference behavior. Its native text
getter dereferences null pointers for SQL NULL and empty blobs. The common
reference check normalizes those known cases to empty values. The native
reference also uses SQLite's [`%!.15g` formatter](https://www.sqlite.org/printf.html)
to request the SDK's existing 15-digit real text contract explicitly; its
default getter renders more digits. Rocky's getters still run directly, and
the JVM reference calls remain intact. A deterministic JVM `BigDecimal`
oracle checks the portable rounding against the original implementation.

| JVM-only test | API keeping it on the JVM |
| --- | --- |
| `PortableNumericRenderingTest` | `java.math.BigDecimal` oracle over deterministic random doubles |
| `WriteLeaseTest` | `System.nanoTime`, `Thread.sleep`, JVM scheduling probe |
| `RoomGateTest` | Java `File`/`ServerSocket` fixtures, `ProcessBuilder` running the `sqlite3` CLI and raw native metadata-file reads |
| `ConfirmedRowsTest` | Java `File`/`ServerSocket` fixtures and raw native metadata-file inspection |

Those tests are retained and still run on the JVM. Test-only Foundation/Java
file adapters live in `platform-test` on all SDK targets; they are not a new product filesystem
surface. Public database/credential openers take `String`; JVM/Android callers
retain `File` overloads. Common randomness uses the platform CSPRNG and time
uses Kotlin's clock. SQLite numeric rendering retains exact binary-double
rounding through portable integer limbs instead of `BigDecimal`.

The two original JVM Room failures came from `installAuthors` member lookup
shadowing the qualification extension. Tests installed authors in schema order
but generated IDs for lists-first order. The explicitly named
`installRoomAuthors` fixture restores that agreement; creation-log validation
and transaction atomicity remain enforced.

After rebasing onto the issuer-registry implementation, the horizon fixture
compares the declared application scope's digest, row count and cleared
tombstones, plus Room's parent/child relation. A writer's private registry
is not expected to match the issuer's registry.
The packaged consumer's exact author expectation is now 4: the registry owns
1 and the executor's delegated range owns 2 and 3. The delegate's pseudonym
is therefore 3. Its consumer API calls remain unchanged; retry identity, owner
privacy and delegation stay checked.

## Measured evidence, 2026-10-10

Host: Apple silicon macOS 27.0.1, Xcode with iOS SDK 27.0, official Rust
1.99.0, Kotlin CLI 0.13.0 and Kotlin/Native 2.4.20. Homebrew Rust rejected the
official iOS standard libraries with E0514 despite matching release/hash.
A checksum-verified official compiler and libraries under ignored
`local/p17/official-toolchain` resolved the mismatch; local target checks use
`RUSTC` pointing there. CI installs the official pinned toolchain through
rustup. This local setup is not a product dependency.

| Check | Result |
| --- | --- |
| Runtime matrix: both arm64 iOS targets, bundled SQLite, with/without iroh | All four static-library builds passed |
| Isolated no-iroh callback fixture | Passed: async call, Rust-thread secrets, Kotlin transport/ByteStream enrollment |
| Common SDK/driver/Room simulator qualification | 15 tests passed: core 6, SQLite 4, Room qualification 5 |
| Public Kotlin callbacks and endpoint ownership | SecretStore/custom transport enrollment, command handler and application protocol across close/reopen passed |
| Native cancellation | Registered observer call cancelled; future handles released; observer remained usable |
| 1,000 open/close cycles | Zero live handles after every close; RSS100 = 137,216,000 bytes, RSS1000 = 136,970,240 bytes (-0.18%) |
| JVM core/SQLite/Room qualification | 20 tests passed, including both original Room failures and the deterministic decimal oracle |
| Clean packaged JVM consumer | Passed outside the repository with only Java on PATH |
| Android native runtime and SDK packaging | Both arm64-v8a/x86_64 runtime libraries built; AAR/core/SQLite/Room packaging passed with compiler warnings treated as errors |
| Android Kotlin and arm64 device Kotlin builds | All three SDK libraries and applicable test fragments compiled; Room qualification and its tests also compiled for the device target |
| `./scripts/lint` | Passed |
| `./scripts/check` | Strict Clippy, 691 workspace tests, doc tests and 154 no-default-feature SDK tests passed; 15/4 ignored tests skipped |
| `./sdks/native/check` | Formatting, strict bindgen Clippy, 2 ABI tests and no-iroh compile passed |

Shared-check reruns during concurrent SDK work failed the coordinator
disk-latency assertion (means 54.98 and 74.00 ms against a 50 ms limit).
The final run after SDK work settled passed all shared checks. No threshold
or test was weakened. The original iroh and Room blockers are resolved.
The change is rebased onto main `fbbf0f80`; local transcripts are under the
ignored `local/p17/` directory (`rebased-*` target/simulator checks and
`final-*` regression, packaging and shared checks).

### Rebase onto main `1aa1edae`

The JVM-only regression command passed unchanged, including with fresh build
output. The full Kotlin check reproduced the missing host library in Android
unit tests: the common test move introduced Android host execution, but the
library resource still selected only `@jvm`. Attaching it to `@jna` puts it
in both JVM and Android host classpaths. The rebase also introduced a Java
clock call into common `observeSync`; it now uses the existing portable clock.

After these repairs, the four-module JVM command passed all 23 tests in
44.86 s. The Android runtime was rebuilt successfully in Maven local.
`./kotlin check` still failed (449.70 s): Android host SQLite tests lose
error messages through mocked framework exceptions and cannot load the
Android bundled engine's `sqliteJni`; simulator Room tests lack
`ROCKY_TEST_ROOT`; the 1,000-cycle lifetime gate reported zero handles but
RSS grew from 136,429,568 to 168,230,912 bytes, exceeding its 10% limit.
The dylib is present in both Android variant jars, and the missing-library
failure and iOS clock compile failure no longer occur. Tests and budgets
are unchanged. `./scripts/lint` passed (6.22 s); `./scripts/check` passed
(455.68 s including builds): strict Clippy, 133 unit tests, 719 integration
tests and the tier budgets. Transcripts are under `local/p17/rebase-repair/`.

### Completion verification on parent `1aa1edae`

The Android SQLite reference tests now run on the JVM and iOS host fragment,
with every assertion retained. The launcher supplies the Room fixture root
for plain checks. The lifetime measurement uses equal collection windows and
per-object allocation in the uncached release test runtime, as described
above; neither the 1,000 cycles nor the 10% limit changed. Two consecutive
full Kotlin checks passed, including debug and release simulator tests.

The last required sequence produced:

| Command | Exit | Wall time | Result |
| --- | --- | --- | --- |
| `./kotlin test -p jvm -m rocky-core -m rocky-sqlite -m rocky-room -m room-qualification` | 0 | 16.14 s | All 23 tests passed |
| `./kotlin check` | 0 | 152.97 s | Full gate passed, including both simulator variants |
| `./scripts/lint` | 0 | 6.68 s | All lint modules passed |
| `./scripts/check` | 1 | 148.77 s | Strict Clippy and all 133 unit / 719 integration tests passed; one integration budget failed |

Debug RSS was 123,256,832 to 122,585,088 bytes (-0.55%); release RSS was
108,789,760 to 108,838,912 (+0.045%). Every close left zero binding handles;
Kotlin retained objects and native allocations fell in both variants.

The remaining blocker is
`rocky-coordinator::public tenants_register_isolated_domains_and_cannot_starve_each_other`:
4.42 s alone against 2 s, following a previous 4.24 s budget failure. Its
separate isolated network run also exceeded the unchanged 3 s fairness
assertion. Preconnecting its clients did not resolve that failure, so the
experimental fixture edit was removed. No coordinator behavior, assertion,
tier, budget or exception was changed. Phase 17 remains incomplete until
the shared gate passes. Final transcripts and diagnostic samples are under
ignored `local/p17/completion/`; earlier failed sequences remain in its
`first-verification`, `second-verification` and `third-verification` directories.

No physical iOS/Android device execution, iOS consumer package, Swift wrapper
or remotely hosted CI run is claimed here. Those limits do not prevent the
phase 17 simulator/binding gate; iOS packaging is phase 18 work.

The baseline workflow now has an arm64 macOS `ios` job after phase 1's `build`
job. It runs the complete target matrix, both simulator qualifications and
JVM regressions, and uploads simulator logs even on failure. The Android
job compiles the Kotlin SDK before packaging its native runtime.
The Android job builds and packages its native AAR on pull requests as well
as pushes. These definitions do not configure hosting branch protection.
