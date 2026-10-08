# 5. Android platform

Status: implemented and qualified on one emulator on 2026-10-07; required result
met within the [evidence limits](#measured-evidence). Owner: `sdks/kotlin`
packaging, `sdks/native` build and the `SecretStore` adapters. Depends on
nothing; wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [section 9, item 1](agreed-scope.md#9-platform-and-operations) of the
scope, [SDK packaging](../runtime/sdk-packaging.md), the key storage choice in
[resolved choices](../features_04/resolved-choices.md#phase-3-credentials) and
the file secret store's threat model in [credentials](../runtime/credentials.md).
Only JVM desktop is configured today and keys are plaintext files.

Make the packaged SDK run on Android with the same contract as desktop, and
keep secrets in the platform keystore. iOS is
[not in scope](agreed-scope.md#10-not-in-scope).

## Ordered slices

1. Native packaging: build the runtime for Android ABIs and ship it in the SDK
   artifact; a consumer needs no Rust toolchain.
2. Lifecycle: open, suspend and resume without losing an open transaction's
   durability or an owned network endpoint; document what closes when the
   process is killed.
3. A Keystore-backed `SecretStore` implementing the existing adapter
   contract; the file store remains the documented fallback.
4. Qualification on one Android device or emulator: the packaged checks of
   `sdks/kotlin` run there and record limits.

## Completion gate

- The todo and shopping examples run on Android from the packaged artifact.
- Keys never leave the Keystore in plaintext; a missing secret fails loudly
  and never silently generates a replacement.
- Sync survives a background and foreground cycle and a process restart.
- The support matrix in [SDK packaging](../runtime/sdk-packaging.md) lists
  Android with its evidence.

## Measured evidence

The guide is [Android](../runtime/android.md). Each slice is one commit:

1. **Native packaging.** `./sdks/kotlin/package-android` cross-builds
   `sdks/native` unchanged for `aarch64-linux-android` and `x86_64-linux-android`
   (release, stripped, NDK 28.2, API 26) and writes `rocky:rocky-core-android`
   (AAR, 28 MB) and `rocky:rocky-room` (jar) as a local Maven repository with
   POMs and an inventory. No Rust change was needed. The Gradle consumers in
   the then-maintained Android consumers resolved it with no Rust, Cargo, Python or NDK.
2. **Lifecycle.** `RockyDatabase.enterBackground`/`enterForeground` and
   `rocky.android.RockyLifecycle`. Background waits for a running native sync
   pass and starts none; the file, sessions, network service and endpoint stay
   open, and acknowledged writes are already flushed. What a killed process
   loses is a table in the guide.
3. **Keystore `SecretStore`.** `KeystoreSecretStore` seals each secret with
   AES-256-GCM under a non-exportable Keystore key, name-bound, in
   `noBackupFilesDir`. Absent loads as null; a lost key, moved or altered file
   fails, and a new key is refused while sealed secrets exist. The file store
   remains the fallback and default.
4. **Qualification (historical; device gate removed 2026-10-08).** The Android device gate on an Android 15 (API 35)
   arm64 emulator, five consecutive passes:
   - the unchanged packaged consumer of `sdks/kotlin`, the Keystore store, the
     missing-secret refusals and an in-process background/foreground/reopen
     cycle, in a debug build and, in the last two runs, an R8-minified release build;
   - the todo app (ORM-neutral API) and the shopping example's repository and
     Room database compiled for Android: join a host peer on the packaged JVM
     SDK over TCP through `adb reverse`, sync both ways (0.2–0.6 s), no sync
     while backgrounded and catch-up at foreground, then `am kill` and a cold
     start that resumes from the files and the Keystore and syncs again;
   - four sealed secrets per app and no plaintext `secrets/` directory.
   - `./sdks/kotlin/check-packaged` and `./examples/shoppinglist-app/check`
     (21 tests) still pass on the desktop, `./scripts/check` passes with
     the Android example excluded from the Cargo workspace, and the packaging
     script provisions rustup and the Android targets from scratch.

Completion gate: todo and shopping run on Android from the packaged artifact;
the Keystore key is not exportable and secrets are sealed at rest; a missing
secret or Keystore key fails the open without a replacement; sync survives the
background cycle and the restart; the
[support matrix](../runtime/sdk-packaging.md#build-and-consume-a-package) lists
Android with this evidence.

Limits: one emulator with a software Keystore (security level 0), arm64 only
(`x86_64` built, not run), loopback TCP rather than iroh or a real network, and
`am kill` rather than the freezer, Doze, low-memory kills or power loss. Secrets
are plaintext in process memory while the runtime uses them; the Keystore does
not sign. The Rust gate `./scripts/check` does not cover Android.

Slack for phase 16: the shopping example already builds for Android as a Gradle
KMP library with Room's KSP processor (the since-removed Android shopping consumer),
compiling its shared sources unchanged.
