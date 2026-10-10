# Shopping on devices

Device runs of the [shopping qualification](shopping-qualification.md). The
JVM harness proves the scenarios; this page records Android and iOS procedures.

## Device kinds

The current app pairs directly over iroh. A trusted device chooses the new
device's name and kind in **Devices → Add device**: trusted (issuing and full),
full (all lists, without issuing) or partial (only explicitly shared lists).
The [app guide](../../examples/shoppinglist-app/README.md#run-and-pair) describes
the grants and automatic sealed-key delivery. The Sync tab exposes domain,
holdings, key epochs and [background activity](activity.md).

The custody procedure below is a separate qualification topology. The
interactive app needs no custody peer; devices exchange edits whenever they
are online together. Pairing codes are `rocky-shopping://pair?…` links, so a
phone's camera app opens a scanned code in the app, which asks before joining.
Debug builds also accept `rocky-shopping://drive?…` links, implemented in
[ShoppingLinks](../../examples/shoppinglist-app/shared/src@android/ShoppingLinks.kt);
they accept raw invitations for the qualification harness. Browsers cannot open
drive links, and release builds ignore them.

Invitation choices are matched by the native invitation/enrollment event order,
so a delayed enrollment keeps its chosen name and kind. Closing that invitation
checks its code and cannot delete a newer invitation. The pending choices share
the activity feed's 512-event lifetime; they are discarded when the app closes.

## Two-phone procedure

The [Compose app](../../examples/shoppinglist-app/androidApp) opens drive
links (see `ShoppingLinks`), so the harness drives it with `adb` and reads the
results from logcat. A host issuer enrolls both devices as issuing devices of
the user and enrolls the packaged custody peer; then the host closes and its contact is removed from both phones. Both
devices reach the custody peer through `adb reverse` and never each other.

```sh
sdks/kotlin/android-runtime/gradlew -p sdks/kotlin/android-runtime publishToMavenLocal
./kotlin build -m androidApp
ROCKY_PHONES=<serial A>,<serial B> ROCKY_APK=<androidApp APK> \
    ./examples/shoppinglist-app/qualify
```

The run installs the app, clears its data, then:

1. Phone A creates "Weekly" with Milk and "Weekend" with Eggs and shares both
   with the driver's envelope codes; phone B opens the codes.
2. A publishes to custody while B is offline, then B syncs from custody alone.
3. Offline from A, B ticks Milk and adds Bread. A adds Bread and moves Milk to
   "Weekend". Each publishes while the other is offline.
4. Both show "Weekly": Bread and "Weekend": Eggs, Milk ticked.

Evidence goes to `local/shopping-qualification/phones.json` with each device's
model, API level and ABI, and the app's log lines under `phones/`.

## Runs

| Date | Devices | Result |
| --- | --- | --- |
| 2026-10-10 | Two Android 15 (API 35) `arm64-v8a` emulators, `adb reverse` to the host | 20 complete passes, zero write retries; [Phase 3](#emulator-pair-2026-10-10) |
| 2026-10-09 | Two Android 15 (API 35) `arm64-v8a` emulators, `adb reverse` to the host | The procedure above, group schema; [below](#emulator-pair-2026-10-09) |
| 2026-10-08 | Pixel 11 Pro, one host, LAN | Version-2 unpartitioned schema; [below](#physical-device-run-2026-10-08) |

No run with two physical phones exists: only one phone was attached, and it
was in personal use. The emulators run the same APK, but not a phone's radio, Keystore
hardware or power management.

## Emulator pair, 2026-10-10

[Phase 3](../features_07/03-catch-up-cost.md) rebuilt the Android runtime for
`arm64-v8a` and `x86_64` and the Compose APK in its assigned workspace. The APK's
native libraries match that workspace's AAR byte for byte. The same two
Android 15 / API 35 arm64 AVDs, `rocky-p03-a` and `rocky-p03-b`, completed
20 procedures in 28.427–89.354 s, with no write timeout or retried write.
Every pass checked both shared lists, the duplicate Bread and Milk's move
with the offline tick. The host issuer was closed and its contact removed;
the phones relayed through the packaged TCP custody launcher.

The final series has 21 attempts, all with zero write retries. Attempt 18
failed convergence: B could not reach custody, and its `adb reverse` list
was empty after the run while A's mappings remained. Three further full
procedures passed. Earlier attempts also found a failed `adb logcat -c`;
unique per-run step markers now delimit logs without clearing them. The
[aggregate, artifact hashes, failed attempts and workspace check logs](../../benchmarks/runtime-scaling/evidence/catch-up-2026-10-10/README.md)
retain the evidence. This qualifies the emulator/TCP procedure, not physical
radios, hardware Keystore, power management or WAN behavior.

## Emulator pair, 2026-10-09

Two AVDs, each created with `avdmanager create avd -n <name> -k
"system-images;android-35;google_apis;arm64-v8a" -d pixel_6` and started with
`emulator -avd <name> -port <port> -no-window` (emulator 37.2.12), ran the
debug APK of the qualified sources against a host issuer and the packaged
`rocky-peer` on macOS arm64. Five consecutive runs passed in
18.3–20.0 s each: enrollment about 7.5 s, B receiving both lists through
custody alone after about 12 s, and the move with B's offline tick converged on
both devices after about 19.5 s. No write needed a retry in those runs.

Findings fixed on the way:

- The Compose screen's DAO flows ended, and the app crashed, when a read
  transaction met an authority change or a list whose received bundle was
  still incomplete. The repository now observes again
  ([Room observation](room-integration.md#observation)).
- A Share code lists only devices this issuer enrolled, so the procedure names
  phone B, which the host enrolled.
- Two of nine runs failed a write with code 4 (`transaction was rolled back:
  Timeout`) right after a device came online. [Phase 3](../features_07/03-catch-up-cost.md)
  reproduced a settings/replica lock cycle: renewal waited for a replica held
  by a Room transaction whose next statement waited for settings. The Room
  profile now reads the schema version between transactions. Historical
  timed-out steps were rolled back and retried by the device harness.

## Physical-device run, 2026-10-08

The since-removed Android device driver passed on a Pixel 11 Pro (Android 17,
API 37, `arm64-v8a`) using direct TCP to a macOS arm64 host peer on the packaged
JVM SDK over home Wi-Fi. Three apps ran: the Gradle `todo` and `shopping`
consumers of the packaged AAR, and the Compose app built by the Kotlin toolchain.

| Check | Result |
| --- | --- |
| Instrumentation, debug and R8 release | Packaged consumer, Keystore store (non-exportable key, security level 1), missing-secret refusal and lifecycle reopen passed |
| todo: join / phone→host / background then foreground / kill then restart | 7.2 s / 2.3 s / 7.3 s / 7.3 s |
| shopping (Room DAOs on Rocky's file): same steps | 3.7 s / 0.2 s / 3.7 s / 1.9 s |
| compose (same DAOs, Compose UI): same steps | 3.7 s / 2.5 s / 2.5 s / 1.3 s |
| Secrets | 7 sealed secrets per app under `no_backup/rocky-secrets`; no plaintext directory |

During the background step, the host wrote a row and the paused app did not
receive it within 3 s; it received the row after returning to the foreground.
Timings are single samples on a lossy home network, not latency measurements.

Findings fixed on the way: each issuing app claimed the SDK's default
`127.0.0.1:47470`, so the examples now listen on an ephemeral loopback port; and
Android 17 gates LAN connections behind the runtime `ACCESS_LOCAL_NETWORK`
permission, which the Compose app requests before opening.

One run failed while the Wi-Fi was degraded (phone→host pings 100% then 50%
lost): the 30 s pairing exchange elapsed and the app reported `open failed`. The
same build passed every step in under a second over `adb reverse`, so these are
network failures. The examples do not retry a failed join; such timeouts surface
as `deadline has elapsed` rather than a classified unreachable error.

## iOS procedure

Both applications build their shared Compose sources for arm64 simulators and
arm64 iPhones. Run `./examples/shoppinglist-app/check-ios` on macOS/Xcode, then
install `Shopping.app` and `Market.app` with `agent-device install`. The apps use
[iOS Keychain and lifecycle](ios.md), with starter iroh endpoints. Their only
usage-description permission is the camera. Local discovery on an iPhone is
unavailable; neither app carries a multicast entitlement or a local-network
usage description. Both iOS hosts explicitly disable mDNS and pair using
invitations. A simulator without a camera offers a paste field. System Paste consent may
appear on this host's iOS 27 simulator; the procedure accepts only the named
example's clipboard dialog.

The reproducible single-simulator gate is
`./examples/shoppinglist-app/qualify-ios --device SIMULATOR_UDID --bundle BUNDLE`.
It requires fresh example installations and three loopback voters from
`scripts/coordinator-local`. It writes named expectations and command output
under `local/phase21-evidence`, and enforces the 15-minute e2e budget after
build. CI builds both apps and owns this procedure in its macOS job.

1. Open Shopping with `agent-device open org.example.rockyshopping --foreground`.
   Expect **Start a household**. Choose **Start**; expect **Create your first list**.
2. Choose **Sync**, disable **Online**, and return to **Lists**. Create **Offline
   groceries**, add **Milk**, and expect **Milk** in the list. The automated gate
   uses the debug drive link for these same repository writes and waits for
   **done offline-write** and **Milk**.
3. Run `agent-device home`, then open Shopping in the foreground. Expect **Milk**.
   Relaunch the example and expect **Milk** again. These establish local
   persistence; they do not establish cross-device convergence.
4. For a simulator/emulator pair, enroll both through the existing trusted
   device's **Devices → Add device** flow (full or trusted as the procedure
   needs). Paste the pairing code on the simulator; scan it on a signed iPhone.
   Expect the named list on both devices before disconnecting either.
5. Run `./examples/shoppinglist-app/qualify-ios-pair` with `--ios-device`,
   `--android-device`, both `--ios-shopping` / `--android-shopping` artifacts,
   both `--ios-market` / `--android-market` artifacts, `--peer` and `--bundle`
   from `emulator-service`. Add `--emulator-routes` for market: it enters
   the host/emulator NAT address in the market’s **Direct pairing address
   (optional)** field, leaving identity, credentials and token intact, and
   removes its UDP redirects afterward. Android's raw UDP redirection replies
   from a different NAT port; the reusable `market-app/emulator_udp.py` utility
   normalizes that route while forwarding the real datagrams. The shared app
   selects one IP route and retains any issuer relay fallback.
   Market uses isolated iroh for this procedure: Android’s existing
   launch extra and the iOS debug-only `-qualification-loopback YES` argument. Shopping
   configures both reverse and forward TCP routes so custody can reach the
   emulator’s listener and push new rows without polling.
   `--scenario shopping` and `--scenario market` run the slices independently;
   `--market-owner ios|android` isolates one reservation direction. Each slice
   records its own expectation file. It removes only the example installations
   so both markets use fresh identities. Its host enrolls both shopping devices,
   stops, and verifies named expectations through `agent-device`.
   First expect **Offline groceries** on both devices through the host custody
   peer. Then take the simulator offline and add **Milk** to that shared list.
   Restore connectivity while it is backgrounded, then foreground it. Without
   pressing **Sync now**, expect **Milk** on the Android emulator. Keep the
   enrollment host stopped and remove its contact from both devices so the
   result proves custody delivery. Record both named row expectations.
6. On a signed iPhone, repeat install, pairing by the camera, offline write,
   background and resume. Expect the named list and row after each transition.
   Device signing is required; simulator success cannot replace this step.

Phase 20 has not landed in this workspace. The pair/share/revoke iOS procedure
on its roster-free schema remains owned by the later landing phase, as the
[Phase 21 brief](../features_07/21-shopping-and-market-on-ios.md) specifies.
Debug simulator configuration accepts a certified local service bundle. Long
qualification links are staged in bounded chunks because agent-device truncates
its iOS URL entry. These controls are disabled in release builds. The bundled
public service certificate validates against its original root and signature;
see the evidence for the corrected CBOR transcription.

First runs and unverified gates are in [Phase 21 evidence](../features_07/21-ios-applications-evidence.md).
