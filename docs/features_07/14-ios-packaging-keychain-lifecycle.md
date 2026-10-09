# 14. iOS packaging, Keychain and lifecycle

Status: not started.
Owner: `sdks/kotlin` packaging tooling and `rocky-core` iOS sources; the
release workflow. Depends on [phase 2](02-change-driven-sync.md) and
[phase 13](13-kotlin-native-binding.md). Wave 1.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the iOS scope](scope-ios.md), [Android](../runtime/android.md) as the
model for each part, [SDK packaging](../runtime/sdk-packaging.md) and
[releasing](../releasing.md).

After phase 13 the libraries build inside the repository. Make them a
packaged artifact, keep secrets in the Keychain and survive iOS's process
lifecycle.

## Ordered slices

1. Packaging: one command writes the XCFramework with the static runtime for
   device and simulator, the Kotlin artifacts for the iOS targets and a
   SHA-256 inventory, to a local repository. A release publishes them at the
   release version. State the minimum iOS version and Xcode.
2. A packaged consumer for iOS: a project outside the toolchain modules that
   depends only on the packaged coordinates, built with no Rust toolchain.
3. `KeychainSecretStore`: generic-password items scoped to the application,
   device-only, with the accessibility class chosen and stated. `delete`
   removes the item; `logout(purge = true)` leaves none. State what an
   uninstall and a device restore do to the items.
4. Lifecycle: an observer of the application's foreground and background
   notifications drives `enterBackground` and `enterForeground`, as
   `RockyLifecycle` does on Android. Entering the background returns within
   the bound of phase 2.
5. Resume: after a suspension the starter's endpoint and sockets may be
   dead. Foregrounding detects that, rebinds with the stored transport key so
   the address is the same, and syncs. An application-owned endpoint is the
   application's to rebind; Rocky reports it and never closes it.
6. The table of what a killed process loses and what the next open does,
   for iOS, in a new guide beside the Android one.

## Completion gate

- [Acceptance 2, 3 and 4](scope-ios.md#acceptance) pass on the simulator;
  3 and 4 also on the iPhone.
- The packaged consumer builds on a clean macOS host without rustup.
- The identity's files contain no key; with the Keychain items removed the
  identity does not open and reports why.
- A process killed during a write reopens with the write committed or absent
  and `reconcile` tells which.
- After backgrounding for longer than the system keeps sockets, foregrounding
  syncs with no user action and the device's address is unchanged.
- A `RockyEndpoint` hosts a Kotlin protocol next to Rocky's on iOS, across a
  database close and reopen.
- The inventory lists every published file with its checksum.

## Not in this phase

Background sync, push wakeups, App Store distribution and a Swift surface.
