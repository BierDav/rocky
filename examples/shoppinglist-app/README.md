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
Android keeps secrets in the Keystore and pauses automatic sync in the
background.

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

The Devices tab starts with **This device**: its name, kind, node and credential
expiry, also before a trusted device's record of it has synced.

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

Members' **Devices** tab shows **This device** and only peers met here, with local labels.
**My contact code** shows the code another member scans. **Scan contact** keeps a
route and a local label; it grants no access and replicates no label. Pairing
supplies one issuer contact; authenticated sessions hand over other contacts,
so no reciprocal contact entry is needed. Discovery is not used by these flows.

Trusted devices manage every device in the issuer registry, including devices
enrolled elsewhere. The trusted-only `devices` table records name and kind;
the same two fields form the opaque registry label, available before that table
syncs. **Clear name** clears a revoked device's application description; its
registry tombstone and signed revocation remain. No roster horizon is declared.

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
direct TCP, as in qualification, has no coordinator and shows the tab as
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
In **Lists → Share list**, a member
scans or pastes the recipient's contact code; a trusted node chooses from the
registry, including a device it has never contacted. List title conflicts converge per list; deleting a list hides its
children, and deleting a category clears its items' category. Moving an item
keeps an offline tick attached to it.

The **Sync** tab shows readiness, connection state, domain and node identity,
grants, revocation digest and count, credential expiry, per-table holdings and key epochs.
Its partner list shows each known peer's source, ranking reason and last connection state.
Its activity feed shows transfers, checkpoint imports, bootstrap, horizon
finalization, author installation, enrollment and key delivery. The feed is
bounded and lasts for the open database's lifetime; see
[SDK activity](../../docs/runtime/activity.md). The top badge also reflects
inbound sync on a trusted device that serves the phone.

The current Room schema is version 6. Earlier application schemas are refused;
there is no migration or fallback. An existing household that cannot open
shows **Retry** and preserves its data.

## Code and SDK usage

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

## iOS

`shared` builds unchanged screens and logic for `iosArm64` and
`iosSimulatorArm64`; `iosApp` supplies the Compose host and scene lifecycle,
and `shared` binds the Room path and Keychain store. Build and test both examples with
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

The Sync and Devices tabs show **Live** per peer while its authenticated link is
open. The top badge shows **Live** whenever any link is open, using the existing
readiness flow. Connected without Live describes an earlier successful pass.
The activity feed includes link open and close reasons. See
[live-link status](../../docs/runtime/live-links.md#status-and-lifecycle).
