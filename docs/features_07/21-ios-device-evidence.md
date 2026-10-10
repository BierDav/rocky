# Phase 21: physical iPhone continuation

This continues the [application evidence](21-ios-applications-evidence.md).
The owner confirmed physical camera preview, QR decode, enrollment and
reservation. Mixed Android/iOS acceptance remains pending; signing and Shopping
persistence have separate passing evidence below.

## Team-selected physical builds, 2026-10-11

After the owner configured signing, `security find-identity -v -p codesigning`
reported two valid identities. The certificate for **David Emanuel Bieregger**
has team ID **G5K3D2J5Y9**; its parenthesized identity suffix is not the team ID.
Both Xcode device builds passed with `DEVELOPMENT_TEAM=G5K3D2J5Y9`,
`CODE_SIGN_IDENTITY=Apple Development` and automatic provisioning. Codesign
confirms that team on both apps. Both installed through agent-device on the
USB-connected iPhone 15 (`00008120-001E28413ED8A01E`). Build/install logs are
`local/phase21-team-{market,shopping}-{build.log,install.json}`.

Market's launch succeeded, but its initial UI snapshot reported
`devtools_security_developer_mode_disabled`: macOS `DevToolsSecurity -status`
reported development-tool access disabled. This is the Mac's authorization
setting, not a finding that iPhone Developer Mode is disabled. The owner chose
to enable it personally; a subsequent status check reported enabled and
physical UI snapshots worked. The owner created **USB groceries** offline and
added **Milk**, then explicitly confirmed that Milk remained after both Home /
reopen and force-close / reopen. A physical named **Milk** expectation passed
in 117 ms (`local/phase21-phone-shopping-evidence.json`). Camera pairing and
fresh two-direction simulator / Android reservations remain pending.

A real three-voter service with publicly reachable iroh relay addresses is
running for the camera test. The simulator owner created **USB Bolt** and
restocked one item. The owner accidentally chose **Create market** on the
iPhone, producing a separate owner rather than joining; that does not count
as camera enrollment. At the owner’s request, iOS now exposes a confirmed
**Reset market** action in the common screen. It unfollows and closes the
current session, removes only Market’s Application Support directory and
rotates the installation’s Keychain namespace before returning to setup.
Other devices retain their market data, and Shopping data is untouched.

The Android build also uses a private Maven-local repository under this
workspace (`-Dmaven.repo.local=…/local/phase21-maven`) so concurrent workspace
publications cannot replace its runtime. The provenance verifier follows the
same standard JVM property. The completed APK’s arm64 and x86_64 native
libraries exactly match the private AAR; no global Maven artifact was replaced
(`local/phase21-team-market-android-private-build.log`).

The reset update’s signed Xcode build and physical installation passed
(`local/phase21-reset-market-build.log`,
`local/phase21-phone-commands.jsonl`). Its first UI runner attempt lost
authorization; a clean retry produced a physical setup snapshot. The owner then confirmed **the reset worked**. Camera permission was
enabled, but the scanner showed a black preview; camera enrollment remains
unverified. Final `./scripts/lint` and
`CI=1 NEXTEST_TEST_THREADS=4 ./scripts/check` exited 0: 146 unit and
842 integration tests, integration suite 192.92 s against 300 s
(`local/phase21-reset-lint-final.log`, `local/phase21-reset-check.log`).

The published qr-kit 3.1.3 iOS source initializes its capture session in both
UIKitView’s factory and its first update, without stopping the first session.
An attempted iOS seam retained qr-kit’s decoder with a single-session UIView
host. Its correctly sized preview remained black on the physical device. A
direct AVFoundation session then reported a running, uninterrupted session,
an active connection and `previewing=true`, but the owner still observed a
black preview with uncovered lenses. Neither attempt establishes camera
acceptance. The source inspected is retained in
`local/phase21-qrkit-camera-source.kt`. The setup column is weighted
so **Reset market** stays visible while its fields and scanner scroll.

The owner explicitly requested replacing qr-kit with
[EasyQRScan](https://github.com/kalinjul/EasyQRScan), overriding the brief’s
initial library-retention choice. `pairing-ui` now pins EasyQRScan 0.7.2 on
Android and iOS; its one `src@mobile` scanner uses QR-only decoding and a
permission-denied paste fallback. QRose 1.3.1 supplies common QR display,
since EasyQRScan does not generate codes. qr-kit and its Android context
initializers are removed, together with the unsuccessful custom camera host
and diagnostics. The owner subsequently confirmed the physical live preview
and successful QR scan. The black image was observed through Device Hub
mirroring; that observation does not establish a failed physical camera.

The EasyQRScan Market device build and installation passed
(`local/phase21-easyqr-market-build.log`,
`local/phase21-phone-commands.jsonl`). Market's existing Android/JVM check
passed all nine tests and compiled its desktop and qualification applications.
Shopping's existing check passed all 25 tests
(`local/phase21-easyqr-{market,shopping}-check.log`). Lint exited 0.

The next physical screenshot failed with CoreDeviceError **4016**: the device
could not fulfill the requested power / trusted-connectivity / device-services
assertions (`local/phase21-phone-commands.jsonl`). The owner then checked the physical screen and confirmed the EasyQRScan
camera preview and QR decode. The physical snapshot contains the scanned
pairing invitation, but its Coordination bundle is empty, so **Join market**
is correctly disabled. The owner was instructed to copy the real service
bundle into that field; enrollment is still pending. Earlier mirrored black
preview reports and the capture error remain historical observations, without
being treated as successful enrollment.

The EasyQRScan iOS check exited 0: both device and simulator common libraries
and both simulator applications built. The native Shopping persistence test
took **1.153835750 s** and the Market reservation/reopen test
**213.854583 ms**, each within its unchanged 2 s budget
(`local/phase21-easyqr-ios-check.log`). The full root check exited 0: 146 unit
and 842 integration tests, integration suite **184.72 s against 300 s**
(`local/phase21-easyqr-check.log`). The final documentation lint also exited 0
(`local/phase21-easyqr-lint-final.log`). Earlier misses remain recorded.

A new actual invitation was issued after the simulator owner reopened; its
current public UDP route is `185.129.174.22:61266`. The cropped QR is
`local/phase21-phone-market-qr-fresh.png`. The owner was asked to scan it and
join using the service bundle and that direct address; no enrollment or
reservation result is assumed.

The signed Shopping build with EasyQRScan also exited 0 and installed as an
update on the attached iPhone; its application data was preserved
(`local/phase21-easyqr-shopping-build.log`,
`local/phase21-phone-commands.jsonl`). Market was left foregrounded for the
owner's enrollment attempt.

## Join feedback and rejected invitation

The owner filled the bundle and direct address and reported that **Join
market** seemed to do nothing. The physical snapshot instead showed **Could
not open: invalid invitation**, above the scrolled input fields. Reading the
full invitation from the native text view confirmed the older token
`653e9ab3…`, which had been replaced on the issuer by a newer invitation. The
button had initiated an actual enrollment request; enrollment was rejected.
The SDK persists a pending join for safe retries and refuses a different
invitation while that intent is unresolved. The owner was instructed to use
**Reset market**, then scan the newly issued QR, retain the service bundle
and re-enter the current direct address. No successful enrollment is claimed.

The shared screen now keeps status below the scrolling setup fields, shows
progress while opening, and disables duplicate create/join/reset taps during
that operation. iOS observes its opening flag; Android exposes an explicit
opening state to the same screen. This changes feedback, not enrollment or
invitation validation. Build and check logs use
`local/phase21-join-status-{build,android-build,check,lint}.log`.

The signed feedback build exited 0 and was installed/opened on david-iphone.
The physical snapshot places `market-status` outside the scrolling fields,
above **Reset market**. A named status expectation passed through agent-device.
The Android APK compilation also exited 0 using the private Maven repository.
The update cleared only transient setup field values; the pending join remains
on disk until the owner confirms **Reset market**. Enrollment is pending.

At the owner's request, the same update was rebuilt for the simulator and
installed on **rocky-p21** (`F9287254-90B4-4411-B8FF-FB4363494C71`), preserving
its existing owner data. The simulator build exited 0
(`local/phase21-join-status-simulator-build.log`). The agent-device named
**Reset market** expectation passed in 80 ms. The **USB Bolt** expectation
timed out after 5 s with the setup fields visible. The subsequent physical
simulator snapshot reports **Could not open: secret transport is missing; the
device logged out or lost its keys**. The database remains present, but that
old installation cannot resume with its current Keychain namespace. The owner
was instructed to reset and create a fresh market for the end-to-end test.
This reopen failure is retained; its cause is not assumed.
The owner will perform the remaining end-to-end steps; no passing result
is assumed. The final full root check exited 0: 146 unit and 842 integration
tests; the integration suite took **180.27 s against 300 s**
(`local/phase21-join-status-check.log`). Earlier budget misses remain recorded.

## Owner-confirmed camera enrollment and reservation

After resetting the old simulator market, the owner created a fresh market
and explicitly confirmed both physical-iPhone steps: joining by scanning the
simulator's QR, then successfully reserving an item. This establishes actual
camera enrollment and a real reservation, not only QR decode. The simulator
snapshot shows issuer controls, listings **hu** / **4 in stock** and **test** /
**6 in stock**, and **1 confirmed holds, 1 items**
(`local/phase21-phone-commands.jsonl`). These are the owner's actual labels and
quantities; they are not relabeled as the earlier **USB Bolt** fixture.

Physical Shopping lifecycle persistence, camera pairing and Market reservation
now have passing owner observations. Fresh simulator/Android reservations in
both directions and the corresponding shopping foreground-delivery gate are
still unverified. The Android emulators used by Phase 20 remain untouched.
The latest complete root checks are the preceding passing run (146 unit,
842 integration; 180.27 s against 300 s), with no code changes since that run.

The named simulator **1 confirmed holds, 1 items** expectation timed out
after the owner continued testing; its current surface showed **Accepted: 6
left**. The earlier snapshot still records the actual hold count at that
moment. The
concurrent physical snapshot attempt failed when the UI runner lost
authorization (**Not authorized for performing UI testing actions**,
xcodebuild exit 65 / `IOS_RUNNER_CONNECT_TIMEOUT`). That automation failure
is retained in `local/phase21-phone-commands.jsonl`; the iPhone result above
is explicitly the owner's physical observation, not a passing UI-runner
expectation. No system permission or shared runner cache was changed.

## Shopping reset

At the owner's request, iOS Shopping now offers **Reset Shopping** with
**Cancel** / **Reset** confirmation. It closes the session and debug drivers,
unregisters the old lifecycle observer, removes only its Shopping directory,
clears its debug qualification settings, selects a new Keychain namespace and
builds a fresh application graph. The common UI owns a ViewModel store per
graph so a reset cannot retain ViewModels for a closed session. Local unsynced
changes are removed; other devices' copies are untouched. The signed iPhone and simulator builds exited 0 and both updates installed
and opened. Agent-device found **Reset Shopping** on the iPhone in 468 ms
and on rocky-p21 in 80 ms. The owner explicitly confirmed that the reset
worked. Shopping's existing JVM/Android check exited 0 and the desktop app
built. Logs are `local/phase21-shopping-reset-{device-build,simulator-build,
app-check}.log` and `local/phase21-phone-commands.jsonl`.

The final root check after the Shopping reset implementation exited 0: 146
unit and 842 integration tests, integration suite **190.61 s against 300 s**
(`local/phase21-shopping-reset-check.log`). Lint exited 0. Android's host also
wires the common reset action: it closes its link drivers and session,
unfollows databases, removes its own Shopping/ciphertext files, selects a
fresh Keystore alias and recreates the activity. Shopping's JVM/Android
check passed again after these changes.

At the owner's request, the APK installed and opened on the remotely connected
**Pixel 11 Pro** through agent-device. Both native ABI libraries match the
private runtime AAR. The strict button-role selector timed out, although its
current surface included **Reset Shopping**; the corrected named text
expectation passed in **224 ms**. The phone's data was not cleared and the
reset confirmation was not accepted. Evidence is
`local/phase21-shopping-reset-android-{build.log,install.json,open.json,
button.json,text.json,artifact-hashes.json}`. The owner requested committing
the current implementation; the mixed Android/iOS gates remain unverified.
