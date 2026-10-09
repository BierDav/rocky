# Shopping: Room, ViewModels and direct device sync

A Kotlin-only Compose application for JVM desktop and Android. Room DAOs use
Rocky's own file; Metro builds the application graph and screen ViewModels.
Devices sync over iroh, including a phone on cellular. No custody peer is
required for the app: two devices must be online together to exchange edits.
The separate [qualification harness](../../docs/runtime/shopping-qualification.md)
continues to test offline delivery through custody.

## Run and pair

```sh
./kotlin run -m desktopApp
sdks/kotlin/android-runtime/gradlew -p sdks/kotlin/android-runtime publishToMavenLocal
./kotlin build -m androidApp
```

The desktop keeps its household in `~/.rocky-shopping`, or
`ROCKY_SHOPPING_DATA`. Install the Android debug APK with `adb install -r`;
Android keeps secrets in the Keystore and pauses automatic sync in the
background. Republish the Android runtime after Rust changes.

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
trusted devices can read them. A trusted device chooses:

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

**Remove permanently** on a revoked device deletes its record, identity and
sealed keys at once; it disappears from every device with the next sync. The
signed revocation stays, since it is what refuses the old identity. The
deletions leave tombstones until a [horizon](../../docs/runtime/mergeable-horizon.md)
on `devices` and `identities` is finalized: every other device must acknowledge
it, which it does when it next syncs with a trusted device. The Devices tab
shows how many are outstanding and the deadline, at most 30 days, after which
a silent device counts as behind and must rebootstrap those tables. Declaring
needs a prior sync with each device. Meanwhile both tables refuse writes, so a
device enrolled then is recorded once the horizon is finalized.

## Approvals

The **Approvals** tab shows strong commands. Any device asks to buy something
with `AskApproval`; a trusted device approves or declines it with
`DecideApproval`. The [public coordination service](../../docs/runtime/public-coordination.md)
orders both, so the first decision stands everywhere: when two trusted devices
decide at once, the second gets a typed rejection naming the standing decision.
Members hold `Invoke` on `requests` only, so they cannot decide. The
command-controlled `requests` and `decisions` tables change only through
confirmed results; see [Approvals](shared/src/Approvals.kt) and
[Coordination](shared/src/Coordination.kt).

The founding device creates the household's coordination tenant key in the
`coordination` table, which only trusted devices read. Trusted devices keep the
domain registered: every live device may invoke and every trusted device may
execute, and the control is renewed before its 29 days end. Each device
publishes its command identity in `identities`. The service learns request
identities and commitments, never titles or decisions. A device opened over
direct TCP, as in qualification, has no coordinator and shows the tab as
unavailable. A member's request goes directly to the trusted device the
coordinator assigned as executor, so that device must be reachable; a request
that cannot be delivered stays stored and is retried in the background.
Once its requests settle, a device lets the service retire their identities.
The runtime retires only identities whose results other devices can still
verify, so the app acknowledges unconditionally; see
[coordinator retention](../../docs/runtime/coordinator-retention.md).

Only trusted devices create, rename and delete lists. Each list, its items
and categories form the `list` partition group with one secret. Trusted
nodes seal each list's secret to its recipients in the replicated `keys`
table; recipients install it and bootstrap from a peer checkpoint. No
plaintext secret travels in a row. The `devices` table records names and
kinds. List title conflicts converge per list; deleting a list hides its
children, and deleting a category clears its items' category. Moving an item
keeps an offline tick attached to it.

The **Sync** tab shows readiness, connection state, domain and node identity,
grants, policy epoch, credential expiry, per-table holdings and key epochs.
Its activity feed shows transfers, checkpoint imports, bootstrap, horizon
finalization, author installation, enrollment and key delivery. The feed is
bounded and lasts for the open database's lifetime; see
[SDK activity](../../docs/runtime/activity.md). The top badge also reflects
inbound sync on a trusted device that serves the phone.

The current Room schema is version 5. Earlier application schemas are refused;
there is no migration or fallback. An existing household that cannot open
shows **Retry** and preserves its data.

## Code and SDK usage

Start at [ShoppingNode](shared/src/ShoppingNode.kt) for platform composition,
[ShoppingGraph](shared/src/ShoppingGraph.kt) for Metro,
[ShoppingSession](shared/src/ShoppingSession.kt) for app lifetime,
[ShoppingRepository](shared/src/ShoppingRepository.kt) for DAO calls and
[Household](shared/src/Household.kt) for device enrollment and key delivery,
[TrustedRows](shared/src/TrustedRows.kt) for the tables only trusted devices
read, which Room does not declare, and
[ShoppingLinks](shared/src@android/ShoppingLinks.kt) for Android links.
The [Room entities](shared/src/ShoppingDatabase.kt) and
[Kotlin schema](shared/src/ShoppingSchema.kt) describe the same tables.
Screens and their ViewModels are under `shared/src/ui`.

```kotlin
val platform = ShoppingPlatform(directory, "Desktop",
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
