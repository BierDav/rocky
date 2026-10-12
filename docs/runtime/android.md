# Android

The SDK has the same contract on Android as on the JVM desktop
([SDK packaging](sdk-packaging.md)). An Android consumer needs Gradle and the Android SDK, and no Rust
toolchain, Cargo, Python or NDK. Qualified on emulators and one phone ([qualification](#qualification)).

## The packaged artifact

Contributors run `sdks/kotlin/gradlew -p sdks/kotlin publishToMavenLocal`, which publishes the Android
variants of the libraries to Maven local:

| Coordinate | Content |
| --- | --- |
| `rocky:rocky-core-android:0.1.0` (AAR) | The `rocky-core` API, the UniFFI bindings, `rocky.android` and `librocky_uniffi.so` for `arm64-v8a` and `x86_64`; R8 keep rules for JNA and the bindings |
| `rocky:rocky-sqlite:0.1.0` (jar) | The [`androidx.sqlite` driver](sql-driver.md#the-kotlin-driver) on Rocky's file |
| `rocky:rocky-room:0.1.0` (jar) | [Room on Rocky's file](room-integration.md): the driver's Room profile and invalidation |

The POMs name the public dependencies: JNA 5.18.1 as an AAR, which carries `libjnidispatch.so`,
kotlinx coroutines 1.10.2 (Android), kotlinx serialization JSON 1.9.0 and the Kotlin standard library;
`rocky-sqlite` adds the `androidx.sqlite` 2.6.2 interfaces; `rocky-room` adds `rocky-sqlite` and Room 2.8.4. The minimum SDK is 26 (Android 8).
The runtime is a stripped release build with 16 KiB page alignment, linked by NDK 28.2 for API 26.

Building the runtime needs the Android SDK with platform `android-35` and NDK `28.2.13676358`
(`ANDROID_HOME`, `ANDROID_NDK_HOME`), and rustup. Without rustup on `PATH`, the build provisions a
checksum-pinned rustup under `sdks/kotlin/build/android-tools`; `rust-toolchain.toml` selects the compiler and the
build adds the two Android targets. [Releases](../releasing.md) publish these coordinates, and the
runtime, to GitHub Packages at the release version; locally built ones stay `0.1.0`. `rocky-core`'s Android variant depends on
`rocky-runtime-android`, so apps get the same runtime and JNA's dispatch library transitively.

## Consume it with Gradle

```kotlin
// settings.gradle.kts
dependencyResolutionManagement {
    repositories {
        google(); mavenCentral()
        mavenLocal()
    }
}
// app/build.gradle.kts
dependencies { implementation("rocky:rocky-core-android:0.1.0") }
```

The [shopping example](../../examples/shoppinglist-app/README.md)'s `androidApp` is such a consumer:
its repository and Room DAOs run on Rocky's file, compiled for Android with Room's KSP processor.
It holds no Rust source.

## Lifecycle

Android stops, freezes and kills app processes without asking. The database contract already covers
the kill: a secure transaction is committed and flushed before `write` returns, and an interrupted one is
recovered by its handle with `reconcile` ([SDK packaging](sdk-packaging.md#batches-cancellation-and-reconciliation)).
What remains is not syncing while nobody looks:

- `RockyDatabase.enterBackground()` stops automatic sync. It cancels the native pass and returns within
  one second, so none runs after it returns. It also closes every live link and refuses
  new inbound or outbound links until foregrounding. Sessions, reads and writes continue; the file, the attached network
  service and an owned or borrowed endpoint stay open, so the device keeps its address.
- `enterBackground()` also stops [local-network discovery](discovery.md#lifecycle-and-platforms) and
  `enterForeground()` resumes it. While the path is on, `RockyLifecycle` holds the Wi-Fi multicast lock for
  each followed database, without which the radio filters other devices' announcements; it releases the
  lock in the background and when the database is no longer followed. The SDK's manifest declares
  `CHANGE_WIFI_MULTICAST_STATE`.
- `enterForeground()` restarts automatic sync with an immediate pass, if the database was opened with
  it, reopening live links. A database opened without automatic sync is left to the
  application's own foreground `syncOnce` calls.
- `rocky.android.RockyLifecycle.register(application)` in `Application.onCreate` counts started
  activities; `follow(database)` applies the visibility now and on every change. It never closes a
  database or a network, and `unfollow` leaves the database in its current state.

The JVM stalled-transport test measured a 5 ms pause and a 4 ms foreground
dial on 2026-10-10 ([phase-2 evidence](../features_07/02-change-driven-sync.md#measured-evidence-2026-10-10)).
These timings have not been measured on an Android device. The phase 5
`android-discovery:qualify` probe checks open → background closed → foreground
open with TCP links and enforces the one-second pause bound. Its current run
is blocked by Gradle distribution verification; see the
[phase 5 evidence](../features_08/05-status-kotlin-lifecycle.md#implementation-and-evidence).

What a killed process loses, and what the next open does:

| Lost with the process | On the next `open`/`resume` |
| --- | --- |
| The reactor thread, sessions, subscriptions and readiness waits | Opened again; observers subscribe again |
| The attached network service, its sockets and a starter iroh endpoint | Started again; the transport key is in the secret store, so the identity is the same |
| An application-owned `RockyEndpoint` | The application binds it again with its own secret |
| A sync pass in flight | Repeated; sync is idempotent |
| A `write` that had not returned | Committed or not; `reconcile(handle)` tells; write again only if it did not |
| The development coordinator's in-memory ledger | Gone, as on the desktop |

Committed rows, outbox and credentials are in the database file and survive. A process killed while
backgrounded is replaced by a fresh one; nothing restores open handles.

## Keystore secret store

`rocky.android.KeystoreSecretStore(context)` implements the [key provider contract](credentials.md#key-providers):

- Each secret is sealed with AES-256-GCM under one Android Keystore key (alias
  `rocky.secret-store.v1`), created on the first `store`, non-exportable, with randomized encryption.
  `strongBox = true` requests StrongBox where the device has it.
- Files hold only the sealed form, bound to their name as associated data, in `noBackupFilesDir/rocky-secrets`:
  `RKSS`, format byte `1`, the 12-byte nonce, then ciphertext and tag. They are written atomically and
  synced. A secret is plaintext only in process memory, where the runtime signs and decrypts with it.
- Secret names support the key-provider contract's 255-byte ASCII limit, including
  group addresses and sibling epoch hashes. Temporary filenames are short and
  independent of the secret name.
- A name with no file loads as absent, so `Vault` reports a missing secret and never makes one.
- A file that does not open fails loudly: a lost Keystore key, a file moved to another name or altered
  bytes are errors, never an absent secret. A new key is refused while sealed files exist, since it
  would orphan them; recovering such a device is a new enrollment or a [restore](backup-and-restore.md).
- No user authentication is required for the key, because sync runs without the user present.

It protects keys from a copy of the app's files, a backup and other apps; the Keystore keeps the key
from the app's own process too. It does not protect against code running inside the app process, a
rooted device or an emulator: an emulator's Keystore is software-backed (security level 0). The file
store remains the fallback and the default when no store is passed.

## Qualification

`ROCKY_PHONES=<a>,<b> ./examples/shoppinglist-app/qualify` drives the Compose app on two devices
([shopping devices](shopping-devices.md)). The earlier Gradle consumers and device driver were removed
when examples were reduced to the todo and shopping-list apps; the 2026-10-07 and 2026-10-08 runs used them.

| Run | Covered |
| --- | --- |
| Android 15 (API 35) arm64 emulator, loopback TCP through `adb reverse`, 2026-10-07 | Instrumented packaged consumer, Keystore store, missing-secret refusal and a background/foreground/reopen cycle, in debug and R8-minified release builds; todo and shopping apps joining a host issuer, syncing both ways, pausing sync while backgrounded and resuming the same identity after `am kill` |
| Two Android 15 arm64 emulators, `adb reverse`, 2026-10-09 | Shopping lists shared through the packaged custody peer, a duplicate title and a move keeping an offline tick ([details](shopping-devices.md#emulator-pair-2026-10-09)) |
| Pixel 11 Pro (Android 17), direct LAN TCP, 2026-10-08 | The same steps plus the Compose app; Keystore security level 1 ([details](shopping-devices.md#physical-device-run-2026-10-08)) |

Steps took 0.2–0.6 s on the emulator. A row written while the app was backgrounded was absent after
3 s and arrived once the app returned to the foreground.

Not covered: `x86_64` execution (built only), iroh, relays, NAT and the Internet on a phone, the
cached-app freezer, Doze, low-memory kills and power loss. The emulator's Keystore is software-backed.

Notes for consumers:

- `RockyLifecycle.follow(database, onApplied)` calls `onApplied` on its IO worker after each completed
  transition; `Activity.onStop` alone precedes the asynchronous pause.
- Two issuing apps on one device collide on the default `127.0.0.1:47470`; give each a `listen` address.
- An app targeting API 37 must hold `ACCESS_LOCAL_NETWORK` before opening, or LAN TCP times out.
- The shopping example's [Compose app](../../examples/shoppinglist-app/README.md) builds from the Kotlin
  toolchain modules instead of the AAR and gets the runtime transitively through `rocky-room`.
