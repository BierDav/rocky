# Phase 21: iOS applications evidence

Implementation and qualification in the dedicated Phase 21 jj workspace, based
on main `b9b17694`. No push or main-bookmark movement is part of this phase.

## Implementation

Both application libraries target JVM, Android, arm64 iOS and arm64 simulator.
Screens and logic remain in common sources. Shopping uses string paths, portable invitation sequence selection and a
portable UTF-8 form codec; platform seams retain secure randomness, directory
operations and localized dates. Common time uses Kotlin's clock. Its existing
Android debug qualification actions now have one common implementation.

The existing Metro graph/ViewModels and icon set are retained. The hot-reload
runtime remains in common dependencies. The QR display/scanner moved into `pairing-ui` for
both apps; iOS uses AVFoundation for availability, and EasyQRScan owns mobile
camera authorization and scanning. QRose displays codes. Denial offers paste. No photo-library picker,
local-network description or multicast entitlement is added.

Platform additions and why:

- `shared/src@ios/Platform.kt`: Foundation filesystem/date formatting and
  Security's random bytes. `src@jna/Platform.jna.kt` keeps JVM/Android behavior.
- `shared/src@ios/ui/Permissions.ios.kt`: no network permission request.
- `pairing-ui/src@ios/Codes.ios.kt`: camera availability. `src@mobile/Scanner.kt`
  shares EasyQRScan between Android and iOS and delivers only the first code
  on Main. Showing QR with QRose is common. `Paste.ios.kt` uses a system `UIPasteControl`
  with UIKit's text-only receiver. This host's iOS 27 simulator still
  presents system Paste consent; the procedure accepts that named dialog.
- `rocky-core/src@android/KeystoreSecretStore.kt`: accepts names through 250
  ASCII bytes, retaining room for its atomic-write filename. The old 128-byte
  cap refused current vault group-epoch names; the device run proves the
  delivered group key is stored and used.
- `rocky-core/src@ios/RockyLifecycle.kt`: an explicit scene visibility entry
  point feeds the same ordered worker as UIKit notifications; the existing
  integration regression now covers both sources.
- Each `iosApp` has an Xcode project, plist and a Swift Compose host generated
  from the pinned Kotlin toolchain's iOS template. Swift contains no app screen.
- Each `ViewController.kt`: Application Support paths, Room builder, Keychain
  service and the SDK lifecycle observer. SwiftUI forwards active/background
  scene changes through `RockyLifecycle.sceneVisibility`; inactive is ignored.
  Shopping also confirms external pairing offers and accepts debug-only
  qualification links. Market’s debug-only loopback launch argument selects
  isolated iroh for the host-voter procedure; ordinary launches use starter
  iroh. Qualification uses the common manual pairing address selection without
  changing node identity or invitation token.

Shopping's application upkeep follows visibility alongside SDK sync. Market's
common upkeep and same-ticket retries pause in the background. No background
execution facility is added. Secrets are explicitly stored in Keychain, with a UUID in app defaults selecting
a service per installation. Reinstalling cannot inherit an earlier market
owner's tenant or control. The
phase 18 accessibility and restore-refusal contract is unchanged.

The [earlier paste investigation and runs](21-ios-applications-history.md)
retain the initial failure logs and takeover results.

## Test tiers

`PairingCodeTest` is common and covers UTF-8, reserved bytes and malformed links.
`IosShoppingTest` exercises real Room persistence across offline writes,
background/foreground and reopen. `IosMarketTest` runs the common Kotlin
handlers, accepts one last-item reservation, rejects the second and reopens both
durable tickets. These two are integration tests with the unchanged 2 s budget.
The agent-device procedure is e2e, with a 15 min suite budget after build.
No test-tier exception is added. The existing JVM/Android checks retain their
platform selection; iOS has its own macOS gate.

## Resumed run, 2026-10-11

The public shopping bundle’s CBOR transcription omitted the `i` in
`window_bytes`, despite carrying the correct 12-byte key length. Restoring that
byte in `PublicCoordinator.kt` and `PUBLIC_COORDINATOR.md` makes
`TrustBundle::genesis()` verify the original root and signature. No root,
certificate, format reader or validation rule changed. The Rust
`shopping_bundle` regression validates both published copies and compares their
genesis IDs (`local/phase21-resume-bundle-test.log`). The earlier `Corrupt`
probe remains historical failure evidence.

Market’s failed mixed-device invitations combined addresses from different
networks: host loopback, emulator-private and observed public NAT routes. A
fresh baseline reached the issuer, consumed the invitation and committed the
buyer’s registry rows, but timed out waiting for the enrollment reply
(`local/phase21-resume-market-baseline3.log`). A real UDP forwarding probe and
then a single direct host route both completed enrollment, retaining the
issuer’s endpoint identity and invitation credentials
(`local/phase21-resume-udp-pair-probe.log`,
`local/phase21-resume-direct-pair-probe.log`). A fresh full run still timed out
with the single direct route (`local/phase21-resume-pair-final.log`), so the
warm probes alone did not establish a fix.

The shared market now offers **Direct pairing address (optional)** and applies
that selection in `MarketNode.open`, before every platform opens the database.
It selects one iroh IP route and retains relay fallback without changing endpoint identity, publisher,
token, expiry or grants. Android preserves this choice through its launch
intent. Common tests check credential preservation and reject other invitation
versions and invalid address syntax. The device procedure enters this actual
field rather than rewriting the invitation in the test. Both iOS hosts
explicitly disable mDNS; camera or pasted invitations need no discovery prompt.
No simulated enrollment result is used.

The native transport now disables gateway port mapping when `local` is false
or unavailable: gateway discovery also uses multicast. Enrollment previously
woke return-route synchronization before writing its sealed answer. The issuer
now wakes synchronization after the caller receives that answer and closes the
request stream. A deterministic duplex-stream regression checks both sides of
that boundary (`local/phase21-resume-enrollment-order-test.log`).

The fresh direct run passed the iOS-owner reservation, but the reverse failed
before consuming Android's invitation. A native host join failed the same way.
The UDP probe observed replies from a different host port than the emulator's
configured redirection (`local/phase21-resume-reverse-udp-proxy.log`), so that
redirection alone is not a usable symmetric route. The reusable
`examples/market-app/emulator_udp.py` utility forwards actual UDP datagrams and
normalizes their source port. Its localhost address is entered into the app's
manual field; the utility owns and removes only its own emulator redirection.
The apps retain isolated transports for this local procedure. A public-relay
attempt enrolled but failed stock convergence and remains failure evidence
(`local/phase21-resume-pair-wildcard-final.log`). Ordinary manual selection
retains any relay fallback; its regression checks that selecting an IP cannot
discard it. Device expectations are unchanged, and no successful protocol
response is manufactured.

The common screen now exposes peer connection errors. This revealed a separate
runtime provenance failure: the live iOS endpoint accepted native session 12,
but the installed Android library accepted 13. The APK matched the replaced
global Maven-local AAR, not this workspace's own Gradle output
(`local/phase21-resume-alpn-probe.log`,
`local/phase21-resume-android-alpn-probe.log`). The verification task had cached
its empty output while another publication replaced that mutable artifact.
It now runs every time, checks the current source digest in both the POM and
the AAR's embedded provenance, requires both ABI libraries, and contributes the
AAR content hash as a generated resource so dependents invalidate. Other wire
versions are refused; no compatibility reader or test exception is added.

Disk space was checked before builds. When free space fell to 2.9 GiB, only
this workspace’s regenerable native Rust incremental caches were removed;
source, application state and retained logs were preserved.

## Resumed verification

- Ordinary shopping startup with the corrected public bundle passed both named
  expectations in 6.85 s (`local/phase21-resume-public-startup.log`,
  `local/phase21-evidence/public-shopping.json`). No fixture bundle was injected.
- Shopping's JVM/Android checks passed; the final market check passed nine
  tests, including manual-route credential and relay preservation.
- Both arm64 iOS libraries and simulator apps built. The native shopping test
  missed its unchanged 2 s budget at 3.57 s during concurrent builds
  (`local/phase21-resume-ios-check-settled.log`). After linking finished, it
  passed at 1.33 s; market passed at 252 ms. The final market common-test run
  also passed all three pairing regressions and its integration test at 664 ms.
- All four shopping and all six market harness scenarios passed with the
  rebuilt runtime (`local/phase21-resume-shopping-harness-settled.log`,
  `local/phase21-resume-market-harness-settled.log`).
- `CI=1 NEXTEST_TEST_THREADS=2 ./scripts/check` exited 0: 146 unit and
  842 integration tests. Integration wall time was 336.95 s; the existing
  checker's load-adjusted time was 268.48 s against 300 s. All 74 raw timing
  overruns passed fresh isolated checks, retained in
  `local/phase21-resume-budget-overshoots.tsv`. No budget or exception changed.

## Earlier physical-device signing blocker

The requested `security find-identity -v -p codex` rejects `codex` as an
unsupported policy. Both valid-only and complete `codesigning` identity checks
find zero identities, and the certificate search for **David Emanuel
Bieregger** finds none. A device build explicitly selecting that name exits 65
with Xcode’s **Signing for "app" requires a development team** error
(`local/phase21-resume-device-signing.log`). A Developer Key alone is not a
usable signing identity on this host. Enrollment/team provisioning and a valid
development certificate are still required before installing and opening the
same build on the attached iPhone. Its camera/manual pairing and device
lifecycle gates remain unverified; simulator results do not substitute for them.

The earlier signing blocker is resolved by the later team-selected build. No
hosted CI run or complete physical-device acceptance is claimed.

## Run 3 continuation, 2026-10-11

The existing `trxxskro` change and all earlier evidence were preserved. The
fresh `security find-identity -v -p codesigning` check still reports **0 valid
identities** (`local/phase21-run3-signing-identities.log`). A fresh market device
build selecting **David Emanuel Bieregger** exits 65 with **Signing for "app"
requires a development team** (`local/phase21-run3-device-signing.log`). Physical
installation, camera pairing and lifecycle acceptance remain blocked.

The latest retained Android-owner market procedure enrolled the iOS buyer but
failed **1 in stock** after 69.09 s (`pair/expectations-market.json`). There is
no passing fresh two-direction market gate after the runtime provenance repair.
This continuation leaves the emulators occupied by Phase 20 untouched, so it
does not claim a new mixed-device pairing result. mDNS remains disabled in the
iOS hosts; the documented direct-address path remains the qualification route.

The initial lint rerun found one missing blank line between annotated properties
in the Android provenance task. That formatting error was corrected. The
previous native shopping miss of **3.57 s against 2 s** remains recorded above;
the subsequent 1.33 s pass does not erase it. Budgets and exceptions are unchanged.

`./scripts/lint` and `./scripts/commitlint --jj` exited 0 after the spacing fix.
The two-worker `CI=1 NEXTEST_TEST_THREADS=2 ./scripts/check` exited 1: all
146 unit and 842 integration tests passed, including fresh isolated checks of
the timing overruns, but the integration suite took **433.57 s against 300 s**
(`local/phase21-run3-check.log`, `local/phase21-run3-budget-evidence.log`). This
suite-budget miss is retained. After compilation completed, only this
workspace's `target/debug/incremental` cache was removed when free disk space
fell to 527 MiB; source, logs and device state were preserved.

The full warm `CI=1 NEXTEST_TEST_THREADS=4 ./scripts/check` rerun exited 0:
146 unit and 842 integration tests passed. Integration wall and load-adjusted
time were both **143.56 s against 300 s**. Fresh isolated timing checks passed
(`local/phase21-run3-check-four-workers.log`). Final `./scripts/lint` exited 0
(`local/phase21-run3-lint-final.log`). The change remains `trxxskro` on
`b9b17694`; no push, bookmark creation or main movement was performed.

## Physical device continuation

[Physical iPhone evidence](21-ios-device-evidence.md) records the later
team-selected signed builds, the owner's confirmed Shopping persistence and
Market reset, the failed camera investigations and the owner-requested
EasyQRScan replacement. The owner confirmed the physical camera preview and
QR decode, then camera enrollment and a Market reservation after a fresh reset.
Fresh two-direction simulator/Android market acceptance remains pending.
