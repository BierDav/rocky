# Shopping list: Room example and phase-16 preflight

This is a Kotlin-only consumer of the desktop UniFFI SDK whose Room DAOs read
and write Rocky's own file. Start with the small
[Room database](shared/src/ShoppingDatabase.kt),
[repository and schema](shared/src/ShoppingRepository.kt), and
[desktop composition](shared/src@jvm/ShoppingDesktop.kt).

A list, its categories and its items form the `list` partition group: each list
is its own partition with one secret, so one envelope shares all three tables.
An item title is unique per list, the later write winning; deleting a list hides
its categories and items, and deleting a category nulls only `items.category`.

```kotlin
val app = ShoppingDesktop.create(dataDirectory)
try {
    val list = app.repository.createList("Groceries")
    val dairy = app.repository.addCategory(list, "Dairy")
    app.repository.add(list, "Milk", category = dairy)
    app.repository.observe(list, search = "milk", includeCompleted = false)
        .collect { items -> /* typed ShoppingItem values */ }
} finally {
    app.close()
}
```

Create and join are explicit; the SDK handles native loading, enrollment and
background sync. Devices connect through iroh unless another
[transport](../../docs/runtime/sdk-transports.md) is passed:

```kotlin
val invitation = creator.runtime.pairing(GrantProfile.Writer(ShoppingTables.toSet()))
val peer = ShoppingDesktop.join(peerDirectory, invitation.uri)
// A writer that does not issue inserts under authors an issuer assigns it.
peer.runtime.installShoppingAuthor(issuer = creator.runtime)
// PairingQrCode(invitation) renders the QRKit painter. For the user's own second
// device, creator.runtime.pairDevice() pairs an issuer that survives the creator's loss.
peer.runtime.syncOnce() // the join's own bootstrap finishes first
val node = peer.runtime.session().node
peer.repository.accept(list, creator.repository.share(list, listOf(node)).getValue(node))
peer.repository.check(list, item, completed = true)
peer.runtime.disconnect() // local edits and Room queries continue
peer.runtime.connect()
```

`share` is one call for the whole list and needs an issuer; the application
delivers each recipient's envelope. The screen's **Share** button shares with
every other enrolled device and shows a `list:<id>:<envelope>` code per device,
which **Open** accepts there. A device new to the list starts it from a peer's
checkpoint ([sharing](../../docs/runtime/partitioned-scopes.md#group-secrets)).
`ShoppingDesktop.resume(dataDirectory)` opens the persisted domain. Give each
instance its own directory and close it after use.

The repository calls ordinary DAOs. Each DAO write is one Rocky commit and one
signed bundle; `followRocky` refreshes Room's observers after sync writes. The
replicated schema is declared with the Kotlin builder and equals the import of
the exported Room schema, version 3, with `shoppingRoomOverlay`; Room never
migrates Rocky's tables. Rocky issues each row's `id`: `createList` prepares the
new list's secret with `partitions.next("lists")`, and the DAOs reserve ids in
the transaction that inserts the row. The creator and devices paired with
`pairDevice()` assign their own authors when they create or join. See
[Room integration](../../docs/runtime/room-integration.md).

From the repository root:

```sh
./examples/shoppinglist-app/check
./examples/shoppinglist-app/demo
```

The SDK's [build plugins](../../sdks/kotlin/README.md#build-tooling) prepare its native
library and bindings automatically; source builds require Rust. This application
contains no Rust source, Cargo manifest or Cargo invocation of its own. This project builds
JVM desktop, with macOS ARM64 tested, and Android through Compose Multiplatform.

`androidApp` shows the shared [`ShoppingScreen`](shared/src/ShoppingScreen.kt) on Room DAOs
over Rocky's file, with keys in the Android Keystore and sync paused in the background:

```sh
sdks/kotlin/android-runtime/gradlew -p sdks/kotlin/android-runtime publishToMavenLocal
./kotlin build -m androidApp
```

The first command publishes Rocky's [Android runtime](../../sdks/kotlin/android-runtime/README.md)
once, and again after Rust changes; `androidApp` depends on the SDK's `rocky-runtime-android`
module, which packages it. The runtime is built for `arm64-v8a` and `x86_64`; the app packages only
those ABIs. On Android 17 it asks for local-network access, which direct LAN TCP needs.
The desktop screen remains the starter UI.

The [phase-16 required definition](requiredSchema/src/RequiredSchema.kt) is the same schema.
`./kotlin run -m requiredSchema`, the first step of `check`, builds it by hand and from the
Room export to identical bytes and installs either natively. It is a preflight, not acceptance
evidence: device qualification of custody-mediated sharing, horizons and moves is
[phase 10](../../docs/features_06/10-qualification.md).
