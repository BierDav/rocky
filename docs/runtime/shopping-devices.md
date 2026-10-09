# Shopping on Android devices

Device runs of the [shopping qualification](shopping-qualification.md). The
JVM harness proves the scenarios; this page records what ran on Android.

## Device kinds

The current app pairs directly over iroh. A trusted device chooses the new
device's name and kind in **Devices → Add device**: trusted (issuing and full),
full (all lists, without issuing) or partial (only explicitly shared lists).
The [app guide](../../examples/shoppinglist-app/README.md#run-and-pair) describes
the grants and automatic sealed-key delivery. The Sync tab exposes domain,
holdings, key epochs and [background activity](activity.md).

The custody procedure below is a separate qualification topology. The
interactive app needs no custody peer; devices exchange edits whenever they
are online together. Android driver extras are implemented in
[Driver](../../examples/shoppinglist-app/androidApp/src/Driver.kt), which also
accepts raw invitations for the qualification harness.

## Two-phone procedure

The [Compose app](../../examples/shoppinglist-app/androidApp) takes driver
extras (see `Driver`), so the harness drives it with `adb` and reads the
results from logcat. A host issuer enrolls both devices as issuing devices of
the user and enrolls the packaged custody peer; then the host closes. Both
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
| 2026-10-09 | Two Android 15 (API 35) `arm64-v8a` emulators, `adb reverse` to the host | The procedure above, group schema; [below](#emulator-pair-2026-10-09) |
| 2026-10-08 | Pixel 11 Pro, one host, LAN | Version-2 unpartitioned schema; [below](#physical-device-run-2026-10-08) |

No two-phone run exists: only one phone was attached, and it was in personal
use. The emulators run the same APK, but not a phone's radio, Keystore
hardware or power management.

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
  Timeout`) right after a device came online, most likely while background sync
  held the replica past the 5 s write limit. The write was rolled back; the driver retries such
  a step up to three times and records it in `retried` ([fix later](../fix_later/README.md#a-room-write-times-out-behind-background-sync)).

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
