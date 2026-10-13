# Shopping: Room, ViewModels and direct device sync

A Kotlin-only Compose application for JVM desktop and Android. Room DAOs use
Rocky's own file; Metro builds the application graph and screen ViewModels.
Devices sync over iroh, including a phone on cellular. No custody peer is
required for the app: two devices must be online together to exchange edits.
The separate [qualification harness](../../docs/runtime/shopping-qualification.md)
continues to test offline delivery through custody.

## Run and pair

The app is its own Kotlin toolchain project and depends on Rocky only by Maven
coordinates (`rocky:rocky-room`). In this repository they resolve from Maven
local, so publish the SDK first, and again after SDK or Rust changes:

```sh
../../sdks/kotlin/gradlew -p ../../sdks/kotlin publishToMavenLocal
./kotlin run -m desktopApp
./kotlin build -m androidApp
```

After republishing the local `0.1.0` Android artifacts, Kotlin CLI 0.13 may
reuse an older extracted AAR. If Android alone cannot resolve new SDK members,
remove the derived `<artifact>-0.1.0/` directory and `<artifact>-0.1.0.aar.flag`
inside `~/.m2/repository/rocky/<artifact>/0.1.0/` for the Rocky Android artifacts,
then build again. Keep the published `.aar`, POM and module metadata.

The desktop keeps its household in `~/.rocky-shopping`, or
`ROCKY_SHOPPING_DATA`. Install the Android debug APK with `adb install -r`;
Android keeps secrets in [KSafe](https://github.com/ioannisa/KSafe), encrypted
at rest and cached in memory once read, and pauses automatic sync in the
background. The Android lifecycle tracker is registered in `Application.onCreate`,
before any activity starts, so a lazily created graph cannot miss foreground entry.

For a desktop reached over SSH, the running trusted app can write a one-use
pairing code to a private file, and the new instance can read that file:

```sh
./kotlin run -m desktopApp -- --invite-file /private/path/pairing.txt "MacBook Air"
# Transfer this file privately to the other computer, then launch there:
./kotlin run -m desktopApp -- --join-file /private/path/pairing.txt
```

The receiving app joins as a full device. Remove both copies of the file after
pairing. An already configured receiver opens its existing household.

Start a household on the first device. In **Devices → Add device**, choose a
name and kind, then scan the QR code with the joining phone's camera app, scan
it in the app, or paste the code. The code is a `rocky-shopping://pair?…` link:
Android opens it in the app, which asks before joining. Each trusted device has
one open invitation, which works once for ten minutes; a new code replaces it.
**Devices → Open invitations** lists every trusted device's open code; only
trusted devices can read them. The issuer removes consumed codes using persisted
SDK state, retries failed removal and resumes cleanup after reopening; other
trusted devices see the removal after sync. A trusted device chooses:

| Kind | What it does |
| --- | --- |
| Trusted | Issues credentials, creates lists, adds devices and receives every list; a full replica |
| Full | Receives every list automatically and edits its items and categories |
| Partial | Receives only lists shared with it from a trusted device's Devices tab |

The Devices tab starts with one compact **This device** card: its name and kind,
**Networking enabled** toggle, committed revision, last reconciliation with
attempted-peer count and last transfer. Bootstrap progress, verified revocation
and credential expiry appear independently. **Identity and access** expands to its full selectable node/domain IDs,
grants, revocation count/digest, credential expiry, renewal route and reset
control. This works before a trusted device's record of it has synced.

In **Devices → Revoke device**, a trusted device can revoke a device it manages.
Revoked devices collapse into their own section; the badge persists across restarts. Revocation stops future access and
rotates list keys; remaining recipients receive new keys automatically. Data
already decrypted on the removed device stays there.

When the removed device next contacts an issuer, it verifies the signed policy
and shows **This device was revoked**. **Join network** accepts a fresh pasted
code or **Scan QR code** on Android. Rejoining archives the old local copy and
creates a new device identity; it does not undo the old identity's revocation.
Trusted devices also record their own enrollment. Each recipient's key delivery
runs independently, so a refused recipient cannot block the remaining devices.

Members' **Devices** tab shows **This device** and discovered peers, using short
node identifiers as names. Rocky finds peers through mDNS and topic discovery;
pairing supplies an issuer route and authenticated sessions hand over additional
contacts. Members read no issuer registry, and no peer names or routes enter
replicated application tables. The app needs no manual contact codes.

Trusted devices manage every device in the issuer registry, including devices
enrolled elsewhere. The trusted-only `devices` table records name and kind;
the same two fields form the opaque registry label, available before that table
syncs. **Clear name** clears a revoked device's application description; its
registry tombstone and signed revocation remain. No roster horizon is declared.

Each peer's **Devices** card shows one connection summary. Live links have a
per-device duration timer, updated every second from the SDK's link opening time;
reconnecting resets it. Closed links show when the last attempt reached the peer
or failed. The next row shows the discovery/contact source, local-network and
issuer flags, and the selection reason. An error appears once below these rows;
a stale failed attempt does not hide an open link.

An open link also shows its network paths: collapsed, the path count, selected
path's kind and round trip; expanded, each path's kind, addresses and round trip.
Tapping a path shows its MTU, congestion window, traffic, loss and congestion
events. **Identity and routes** expands to the full copyable node ID, exact last
check and link opening times, round trip, raw source/selection values, saved-contact
flag and dialing routes. Devices refreshes measurements every two seconds; see
[link status](../../docs/runtime/live-links.md#status-and-lifecycle).

Copy and paste use Klip's system clipboard; iOS may request paste consent.
Credential dates use kotlinx-datetime in the device's time zone, displayed as
`yyyy-MM-dd HH:mm`. SHA-256 and secure random bytes use KotlinCrypto on every target.

## Approvals

The **Approvals** tab shows strong commands. Any device asks to buy something
with `AskApproval`; a trusted device approves or declines it with
`DecideApproval`. The [public coordination service](../../docs/runtime/public-coordination.md)
orders both, so the first decision stands everywhere: when two trusted devices
decide at once, the second gets a typed rejection naming the standing decision.
Members hold `Invoke` on `requests` only, so they cannot decide. The
command-controlled `requests` and `decisions` tables change only through
confirmed results; see [Approvals](shared/src/data/coordination/ApprovalCommands.kt) and
[Coordination](shared/src/data/coordination/Coordination.kt).

The founding device creates the household's coordination tenant key in the
`coordination` table, which only trusted devices read. Trusted devices keep the
domain registered: every live device may invoke and every trusted device may
execute, and the control is renewed before its 29 days end. Command identities come from Kotlin `devices()` on trusted nodes; members never
query the registry. Requests and decisions retain command identities, which members
label **This device** or **Another device**. Trusted nodes show the registry name. The service learns request
identities and commitments, never titles or decisions. A device opened over
isolated loopback iroh, as in qualification, has no coordinator and shows the tab as
unavailable. A member's request discovers the assigned executor through contacts, or is forwarded
once by an admitted peer; a request
that cannot be delivered stays stored and is retried in the background.
Once its requests settle, a device lets the service retire their identities.
The runtime retires only identities whose results other devices can still
verify, so the app acknowledges unconditionally; see
[coordinator retention](../../docs/runtime/coordinator-retention.md).

Only trusted devices create, rename and delete lists. Each list, its items
and categories form the `list` partition group with one secret. Trusted
nodes share each list with trusted and full devices, and partial devices get
the lists sent to them from the **Devices** tab. Sync carries each sealed
secret to its recipient alone, directly or through the custody peer, which
holds it without learning the list; the recipient installs it and bootstraps
from a peer checkpoint. A revocation's rotation reaches the remaining devices
the same way. No row records who holds which list. `keys` and `identities` are removed;
members have no physical `devices` table. SQL against any removed member table
returns `no such table`, including after reopen. A byte search of a member's
directory finds no other device's node key; see
[Phase 20 evidence](../../docs/features_07/20-shopping-without-a-roster-evidence.md).
In **Lists → Share list**, a member chooses a discovered peer; a trusted node
chooses from the registry, including a device it has never contacted. List title conflicts converge per list; deleting a list hides its
children, and deleting a category clears its items' category. Moving an item
keeps an offline tick attached to it.

The **Sync** tab is the searchable activity log. **All**, **Problems**,
**Transfers**, **Connections**, **Keys**, **Access**, **Devices** and **History**
chips show counts for the current search. Select several categories to include
any of them; search terms must all match the title, detail or event ID, ignoring
case. Failed connections appear under Problems and Connections; ordinary link
closures appear only under Connections. Controls stay above the scrolling log,
and new events do not clear the query. **Clear filters** restores the complete
feed. Each entry's title, details, millisecond-precision local timestamp and event ID
are selectable.

The feed shows transfers, checkpoint imports, bootstrap, horizon finalization,
author installation, enrollment and key delivery. It is bounded and lasts for
the open database's lifetime; see [SDK activity](../../docs/runtime/activity.md).
The top badge shows **Live** only while an authenticated link is open. Raw
inbound connection counts do not imply successful synchronization.

The current Room schema is version 6. Earlier application schemas are refused;
there is no migration or fallback. An existing household that cannot open
shows **Retry** and preserves its data. A failure in Room’s background invalidation
tracker also closes the open node and shows **Retry**; retry opens fresh SQL
sessions without deleting the household.

## Code and SDK usage

The [list screen architecture and controls](../../docs/runtime/shopping-screen.md)
separate synchronous text editing from committed Room observations.


Start at [ShoppingNode](shared/src/data/session/ShoppingNode.kt) for platform composition,
[ShoppingGraph](shared/src/di/ShoppingGraph.kt) for Metro,
[ShoppingSession](shared/src/data/session/ShoppingSession.kt) for app lifetime,
[ShoppingRepository](shared/src/data/ShoppingRepository.kt) for DAO calls and
[Household](shared/src/data/household/Household.kt) for device enrollment and key delivery,
[TrustedRows](shared/src/data/db/TrustedRows.kt) for the tables only trusted devices
read, which Room does not declare, and
[DeepLinks](shared/src/presentation/navigation/DeepLinks.kt) for app links.
The [Room entities](shared/src/data/db/entity) and
[Kotlin schema](shared/src/data/db/ShoppingSchema.kt) describe the same tables.
`shared/src` follows one package layout: `data` holds the database (`data/db`),
household, coordination and session; `di` the Metro graph; `util` platform
helpers, links and permissions. `presentation` holds the app host, one navigation3
host whose root follows the session state and whose scene decorators add the
tabs' bar or rail and the snackbar, reusable components (including QR pairing)
and one package per screen with its ViewModel.

```kotlin
val platform = ShoppingPlatform(directory.absolutePath, "Desktop",
    room = { path -> Room.databaseBuilder<ShoppingDatabase>(path) })
val node = ShoppingNode.create(platform)
try {
    val list = node.repository.createList("Groceries")
    val dairy = node.repository.addCategory(list, "Dairy")
    node.repository.add(list, "Milk", category = dairy)
    node.repository.observe(list).collect { items -> /* typed rows */ }
} finally {
    node.close()
}
```

The SDK installs authors at create/join, so the application does not carry
pseudonym requests. `buildOnRocky` builds Room and follows committed table
events; `settledOnRocky` retries transient DAO read failures. Closing the Room
wrapper leaves Rocky open; `ShoppingNode.close` closes both in order. These
helpers have no application or Compose dependency; see
[Room integration](../../docs/runtime/room-integration.md).

## Desktop JAR

`./package-desktop` writes `build/dist/shopping-desktop.jar`, which runs with a
compatible JDK and the platform's graphics libraries:

```sh
./examples/shoppinglist-app/package-desktop
java -jar examples/shoppinglist-app/build/dist/shopping-desktop.jar
```

Amper's `executableJarJvm` names each entry of `BOOT-INF/lib` after its bare
file name. JetBrains publishes Compose, Lifecycle and SavedState twice, as an
empty relocation stub whose POM defers to the androidx artifact holding the
classes and as that androidx artifact, under one file name. The flattening keeps
the stub, drops the other, and the JAR then fails on startup with
`NoClassDefFoundError: androidx/compose/runtime/Composer`, while `./kotlin run`
is unaffected because a classpath tells the two apart by full path. The script
puts the dropped artifacts back under names that cannot collide and loads their
classes through the finished JAR, so a missing dependency fails here.

The script builds and packages Rocky and Skiko for macOS and Linux on arm64
and x86_64, and Windows on x86_64. The complete cross-build runs on macOS with
GNU Linux toolchains and MinGW, without Docker or Zig. See
[desktop cross-build setup](../../docs/runtime/desktop-cross-build.md) for the
prerequisites, separate builds and verification limits. It checks all native
resources and loads both runtimes on the build host before reporting success.

Desktop sync, connection, key-delivery and renewal failures are also printed to
the console, even with the Sync tab closed. Household failures include their
exception stack traces. To capture a Windows failure, launch from PowerShell:

```powershell
java -jar build/dist/shopping-desktop.jar *> shopping.log
```

## Verify

```sh
./examples/shoppinglist-app/check
./examples/shoppinglist-app/qualify
./examples/shoppinglist-app/demo
```

`check` compares the hand-written schema with Room's export, installs both,
runs repository, household and approval tests and compiles desktop. Approval
tests use an in-process coordinator in place of the public service. Household tests
cover automatic full-node key delivery, revocation and delivery of rotated keys, explicit partial sharing, lists two trusted devices create apart, writes by
joined members, immediate reads after reopen, and retry after a held directory
lock. `demo` runs a console walkthrough over the same node and repository.
The multi-process qualification uses manual sync and a keyless custody peer;
Android device qualification is documented in
[shopping devices](../../docs/runtime/shopping-devices.md).

`qualification` is one JVM application containing the process driver and live-link agent.
`qualify-live-links` measures links between independent JVM processes. Start two agents
in separate terminals, then drive them from a third:

```sh
./qualify-live-links agent 47120
./qualify-live-links agent 47110
./qualify-live-links prepare
./qualify-live-links pair /tmp/rocky-live-desktop /tmp/rocky-live-peer
./qualify-live-links latency 10
./qualify-live-links lifecycle 5
./qualify-live-links finish
```

The lifecycle scenario calls the SDK's background/foreground hooks. It does not
qualify Android activity handling, cellular connectivity, battery cost or radio
wake-ups. The shopping Android app still builds separately.

## iOS

`shared` builds unchanged screens and logic for `iosArm64` and
`iosSimulatorArm64`; `iosApp` supplies the Compose host and scene lifecycle,
and `shared` binds the Room path and KSafe store. Build and test both examples with
`./examples/shoppinglist-app/check-ios`. Pair by scanning with camera permission
on a device, or paste on a simulator.
See [the device procedure](../../docs/runtime/shopping-devices.md#ios-procedure)
and [evidence and signing limits](../../docs/features_07/21-ios-applications-evidence.md).

On every platform, **Reset Shopping** on the Sync and setup screens asks for
confirmation, then closes this device's session and returns to **Start a
household** / **Join a household**. It removes
local lists and household setup, including unsynced changes, and selects a
fresh secret namespace. Other devices keep their copies. **Cancel** keeps
the current household open.

A member without an application device description is treated as partial.
Full and partial members have the same SDK grants; only an explicit full
description enables automatic sharing of every list.

The Sync and Devices tabs show the native link lifecycle and dated operation
outcomes per peer. The top badge derives a peer-link count, disabled networking,
suspension or bootstrap progress from `observeStatus()`. A past successful
reconciliation does not imply a currently open link.
The activity feed includes link open and close reasons. See
[live-link status](../../docs/runtime/live-links.md#status-and-lifecycle).
