# Shopping list: Room example and phase-16 preflight

This is a Kotlin-only consumer of the desktop UniFFI SDK whose Room DAOs read
and write Rocky's own file. Full UI work is paused for developer-experience
review. Start with the small
[Room database](shared/src/ShoppingDatabase.kt),
[repository and schema](shared/src/ShoppingRepository.kt), and
[desktop composition](shared/src@jvm/ShoppingDesktop.kt).

```kotlin
val app = ShoppingDesktop.create(dataDirectory)
try {
    app.repository.add("Milk")
    app.repository.observe(search = "milk", includeCompleted = false)
        .collect { items -> /* typed ShoppingItem values */ }
} finally {
    app.close()
}
```

Create and join are explicit; the SDK handles native loading, enrollment and
background sync. Devices connect through iroh unless another
[transport](../../docs/runtime/sdk-transports.md) is passed:

```kotlin
val invitation = creator.runtime.pairing(GrantProfile.Writer(setOf("shopping_items")))
val peer = ShoppingDesktop.join(peerDirectory, invitation.uri)
// A writer that does not issue inserts under an author an issuer assigns it.
peer.runtime.installShoppingAuthor(issuer = creator.runtime)
// PairingQrCode(invitation) renders the QRKit painter. For the user's own second
// device, creator.runtime.pairDevice() pairs an issuer that survives the creator's loss.
peer.repository.check(item.id, completed = true)
peer.repository.rename(item.id, "Oat milk")
peer.runtime.disconnect() // local edits and Room queries continue
peer.runtime.connect()
```

`ShoppingDesktop.resume(dataDirectory)` opens the persisted domain. Give each
instance its own directory and close it after use.

The repository calls ordinary DAOs. Each DAO write is one Rocky commit and one
signed bundle; `followRocky` refreshes Room's observers after sync writes. The
replicated schema is declared with the Kotlin builder and equals the import of
the exported Room schema, version 2; Room never migrates Rocky's tables.
Rocky issues each item's `id`: the DAO reserves it in the transaction that
inserts the item. The creator and devices paired with `pairDevice()` assign
their own author for `shopping_items` when they create or join. See
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

The [phase-16 required definition](requiredSchema/src/RequiredSchema.kt) declares `lists` by its
own `id`, `items` by `list`, a per-list title uniqueness policy and a list reference. Run
`./kotlin run -m requiredSchema` before migrating the Room entities.
It currently fails because both Kotlin and native installation refuse partitioned constraints.
This is a failing prerequisite gate, not acceptance evidence. The existing `check` still
qualifies the version-2 unpartitioned example; it does not establish per-list isolation,
custody-mediated shopping, partitioned relinking or horizon convergence for the required app.
