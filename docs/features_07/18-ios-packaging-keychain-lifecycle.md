# 18. iOS packaging, Keychain and lifecycle

Status: not started.
Owner: `sdks/kotlin` packaging tooling and `rocky-core` iOS sources; the
check and release workflows; `rocky-sdk`, `rocky-iroh` and `sdks/native` for
the socket rebind of slice 5. Depends on
[phase 2](02-change-driven-sync.md) and
[phase 17](17-kotlin-native-binding.md). Wave 1.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the iOS scope](scope-ios.md), [Android](../runtime/android.md) as the
model for each part, [SDK packaging](../runtime/sdk-packaging.md),
[releasing](../releasing.md), the `rocky-package` tooling plugin and
`sdks/kotlin/packaged-consumer`.

After phase 17 the libraries build inside the repository. Make them a
packaged artifact, keep secrets in the Keychain and survive iOS's process
lifecycle.

## Settled

- The XCFramework with the static runtime for device and simulator is the
  stated artifact. Embedding the static library in the cinterop klib as well
  is this phase's call; the constraint is that a Kotlin consumer links with
  no manual linker flag.
- The pause bound is the 1 s of phase 2. This phase adds no second bound.
- "Longer than the system keeps sockets" means 10 minutes in the background
  on the iPhone; on the simulator the test closes the endpoint's sockets by
  fault injection instead of waiting.
- Rebinding after suspension is a runtime change, not a Kotlin workaround. It
  serves Android through the same call. If it changes the exported surface,
  both bindings are regenerated
  ([hazards](README.md#shared-format-hazards)).
- Keychain items are generic passwords with
  `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`: sync runs while the
  device is locked, and the items leave neither in a backup nor through
  iCloud Keychain.

## Ordered slices

1. Packaging: one command writes the XCFramework, the Kotlin artifacts for
   the iOS targets and a SHA-256 inventory to the local repository. A release
   publishes them at the release version. Record the minimum iOS version and
   Xcode version used.
2. A packaged consumer for iOS: a project outside the toolchain modules that
   depends only on the packaged coordinates and builds with no Rust
   toolchain. Its job joins CI on a macOS runner, after
   [phase 1](01-test-speed.md) restructured the workflow.
3. `KeychainSecretStore` in the `rocky-core` iOS sources, under the
   [key provider contract](../runtime/credentials.md#key-providers). An
   absent item loads as absent; any other Keychain status is an error, never
   an absent secret. `delete` removes the item; `logout(purge = true)` leaves
   none. Document what an uninstall and a device restore do to the items.
4. Lifecycle: an observer of the application's foreground and background
   notifications drives `enterBackground` and `enterForeground`, with the
   contract of `RockyLifecycle` on Android: ordered per database, latest
   state wins, never closing a database or a network.
5. Resume: after a suspension the starter's endpoint and sockets may be
   dead. `enterForeground` detects that, rebinds with the stored transport
   key so the node ID is unchanged, and syncs. An application-owned endpoint
   is the application's to rebind; Rocky reports it dead and never closes it.
6. A new guide `docs/runtime/ios.md` beside the Android one: the artifact,
   the Keychain store, lifecycle, and the table of what a killed process
   loses and what the next open does.

## Completion gate

- [Acceptance 2, 3 and 4](scope-ios.md#acceptance) pass on the simulator;
  3 and 4 also on the iPhone.
- The packaged consumer builds and runs in CI on a macOS runner on which
  `cargo` and `rustup` are absent from the path.
- No file under the identity's directory contains a secret, shown by a byte
  search for each stored secret. With the Keychain items deleted, `open`
  fails with the missing-secret error and creates none.
- A process killed at each point of a write (before, during and after the
  commit) reopens with the write committed or absent, and `reconcile`
  reports which.
- `enterBackground` returns within 1 s while a dial is stalled.
- With the sockets closed by fault injection on the simulator, and after 10
  minutes in the background on the iPhone, foregrounding syncs with no user
  action and `contact()` reports the same node ID.
- The Android lifecycle checks pass unchanged.
- The inventory lists every published file with its checksum, and the
  release workflow publishes the iOS artifacts.

## Not in this phase

- Background tasks, push wakeups or background sync.
- App Store distribution, signing automation and privacy manifests.
- A Swift surface, CocoaPods or Swift Package Manager packaging.
- Closing or rebinding an application-owned endpoint.
- Changing Android's lifecycle contract or its Keystore store.
- The example applications ([phase 21](21-shopping-and-market-on-ios.md)).
