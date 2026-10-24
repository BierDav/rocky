# Manual deployment evidence — 2026-10-24

Runtime base: `nnrprlwq` / `0b0527df`, with fixes in change `pxomrnxr`;
encrypted replica 35, row 12,
checkpoint 17, link 16. Diagnostic build:
`features10-0b0527df-manual-diagnostics2-20261024`.

- Published fresh JVM and Android SDK artifacts to Maven local, including both
  Android native ABIs. Shopping schema qualification, 25 Android unit tests,
  62 JVM tests and both app builds passed. The diagnostics regression checks
  rotation and omission of exception messages.
- `scripts/lint`, `scripts/check` (180 unit and 1,287 integration tests), and
  `sdks/kotlin/gradlew -p sdks/kotlin check` passed. Cached SDK checks do not
  establish new device or distributed-protocol qualification.
- Installed debug package `org.example.examples` (versionCode 1,
  versionName `unspecified`) on `david-pixel`, Pixel 11 Pro, USB serial
  `66121FDKX001MF`, with `adb install --no-streaming -r`. Verified the
  foreground `MainActivity`, living process and private diagnostic file.
  Wireless installation failed before the successful USB installation.
- Two detached JVM processes expose windows titled Shopping A and Shopping B.
  Their isolated data roots are `local/manual-testing/desktop-a` and
  `local/manual-testing/desktop-b`; each has `app.pid`, separate `stdout.log`
  and `stderr.log`, and `logs/diagnostics.log` with three rotated files.
  No household was automatically created or paired by the deployment commands.
- Shopping A's first user-triggered creation took 54.9 seconds. Its subsequent
  reopen preserved node identity and revision and took 1.43 seconds: runtime
  opening 1.39 seconds, Room setup 9 ms, configuration 25 ms. The initial delay
  remains unexplained; startup stages now have timestamps.
- The phone logged a runtime-open failure, then an app-side Resetting → Setup
  transition. Deployment issued no reset, uninstall or data-clearing command.

The shared observer persists SDK activity and status, revisions, peer identities,
network-attempt IDs/stages/outcomes/failure categories, checkpoints and session
transitions. Android also records activity visibility. JVM exceptions retain
classes, cause chains and frames; messages and event payloads are omitted to
avoid exporting row values, invitations or credentials. Each diagnostic file is
bounded to 2 MiB, four files per instance, with UTC timestamps. There is no
native tracing-subscriber initialization API in the current binding: Rust span
traces are not persisted by this change. A process kill or power loss can still
lose an in-progress write; writes close the file but do not fsync each event.

Collect diagnostic files only (no databases, credentials, preferences or raw
stdout/stderr):

```sh
examples/shoppinglist-app/collect-diagnostics --serial 66121FDKX001MF
```

The collector verifies `david-pixel`, uses debug-build `adb run-as`, and writes a
new timestamped directory under `local/manual-testing`. Without `--serial`, it
collects only the two desktop instances. Both modes were exercised successfully.
Logs can contain node identities and operational metadata; row values and
pairing secrets are deliberately excluded. Raw stdout/stderr remain local and
are outside this collection's privacy guarantee.

## Recovery follow-up

Live validation is still in progress. `seal-recovery6` recovered the existing
phone's enrollment bootstrap at 08:30:31 UTC, revision 28. Later observations
showed `bootstrap=false`, revision 41, but SQL writes still refused a pending
verified-seal rebootstrap. Two open links alone did not prove usable replication;
admission, dial and request timeouts continued.

- Room treated the expected verified-seal read refusal as fatal and stopped the
  app's recovery work. Its observer now retries that refusal; writes remain
  subject to the runtime's verified-state checks.
- Full devices now receive the scoped list-insert capability. Own-id partition
  preparation accepts that capability; arbitrary partition creation, invitations,
  list renaming and deletion retain their existing authority requirements. The
  existing phone received a signed scoped grant without changing its identity,
  role or pairing.
- Extending the own-id SDK regression to import a checkpoint reproduced the
  live write refusal. Preparing a new partition introduced unverified scopes.
  Preparation now records the unused own identity and rows referencing it as
  empty through the existing seal, before exposing its key, and preserves
  materialized state. Unrelated group members still require verified state.
  All 11 shared-list SDK tests passed, including restart before the first insert
  and refusal for an unrelated group member.
- Seal recovery now selects peers permitted to serve the scope on the followed
  tip. A peer outside that scope leaves recovery pending instead of receiving a
  request that ends the session. The current user-preserved state is Shopping A
  on `main`, Shopping B on `dev`, and the phone on `main`; A and the phone's
  actual followed tip IDs match. The earlier handover's branch mapping is stale;
  the investigation does not establish who changed it. No branches were switched
  to fit that mapping. Their metadata links may remain connected across branches,
  while branch rows remain separate.

Build `features10-0b0527df-manual-peer-seal-recovery7-20261024` was installed on
all three devices. Their original node identities were observed after reopening.
Branch reconciliation still reported `ciphertext_custody_denied`: the holding
proof acknowledgement required the same tip even for the held-change exchange,
which explicitly supports different tips. A focused regression now checks that
held-change proofs work across tips while ordinary row transfer remains refused.
Build `features10-0b0527df-manual-held-proof-recovery8-20261024` was deployed on
all three nodes with their original identities. The phone still refused writes.
Private metadata comparison found four phone-only partition keys from the failed
creation; a peer unable to verify one of these proofs ended recovery of the other
scopes. Recovery now treats an unaccepted proof as unavailable for that scope;
explicit data requests still require proof. A regression checks continuation to a
shared scope after refusing an unshared scope. The next build is
`features10-0b0527df-manual-unshared-recovery9-20261024`.
Pre-deployment SQLite snapshots used SQLite `VACUUM INTO` on Android and the
SQLite backup API on both desktops; no live WAL files were copied. Private
snapshots and investigation output stay under `local/manual-testing`. Final
phone creation, delivery to Shopping A on `main`, and link stability remain unverified.
The first full check ran 180 unit and 1,292 integration tests successfully, but
exceeded the integration tier's 300-second suite budget while builds contended;
it is not a passing final check. A serialized rerun passed 180 unit and 1,293
integration tests (197.34 seconds against the 300-second integration budget).
The Kotlin SDK check initially timed out waiting for the Cargo lock in its
service-owner test; its serialized rerun passed. These runs precede recovery9's
unshared-proof fix and require final validation of that fix.

## Resumed validation

`recovery9` passed shopping schema qualification, 67 JVM tests, 25 Android
tests and both app builds. Workspace lint passed. Clippy identified a nested
checkpoint condition; it was flattened without changing behavior. The final
workspace check passed 180 unit and 1,294 integration tests; integration took
203.44 seconds against its 300-second budget.

Both desktops closed normally and reopened from the frozen `recovery9` build
with their original data directories and node identities. Read-only database
inspection confirmed A on `main`, B on `dev`; the phone's SQLite snapshot
confirmed `main`. Their metadata link reopened across these branches.

The phone was reachable only over Wi-Fi and reported hardware serial
`66121FDKX001MF` and name `david-pixel`. A consistent SQLite snapshot passed
`quick_check`. A replacement APK transfer timed out. A later TLS transfer reached 246 MiB
in 148.8 seconds, then failed with `failed to read copy response: EOF` and the
wireless transport disappeared. Installation and the phone's running build
remain unconfirmed; USB reconnection is required for further live checks.
No verification list existed in the phone snapshot or either desktop's current
projection. No successful Full-node creation or delivery is claimed.

USB reconnection subsequently allowed `recovery9` installation with `-r` and
confirmed the original phone node in an open foreground session at 12:04:21
UTC. Its create attempt still refused a verified-seal rebootstrap. Manual sync
reported A admission timing out after 10.0 seconds and B reconciliation timing
out on its request channel. The Kotlin SDK check passed (4m 24s); those checks
precede the following optimization.

A 12-second CPU sample of A showed repeated bookmark/ancestry work in
`refresh_closed_held` while serving scoped held-frame requests. The per-refresh
graph context now also caches abandonment boundaries by domain and source tip,
within its existing entry and byte limits; each frame still checks its own
author and sequence. Six refresh regressions and the unrelated-domain header
validation regression passed, including rollback, changed winning seals, lazy
errors and uncached fallback. Lint and targeted Clippy passed. Build
`features10-0b0527df-manual-held-refresh10-20261024` is being prepared for live
validation; no timeout or live-creation recovery is established yet.

`refresh10` was deployed on all three nodes with the existing identities and
branches. A subsequent CPU sample showed the repeated ancestry path much less
prominently, and the phone opened A's link; reconciliation still timed out.
A longer debug probe retained only error metadata and reported native reads,
channel reads and stopped streams. At 12:31 UTC, read-only runtime inspection
found one incomplete direct import of a checkpoint bound to a seal. This is
recovery progress, not successful bootstrap or authoring.

A five-packet LAN probe lost four packets from phone to desktop; the reverse
probe lost one, with a maximum 2.64-second RTT. The phone has an active VPN, but
these observations do not establish VPN causality. VPN/network protection
settings were left unchanged; a controlled VPN-off test awaits user approval.
The final Kotlin SDK check passed in 3m 9s. The first post-optimization workspace
run passed all 180 unit and 1,295 integration tests in 220.29 seconds, but a
format-refusal test exceeded its two-second individual budget when rerun
alone (2.36 seconds), so that run is not a full passing check. A serialized
workspace and shopping check passed: 180 unit and 1,295 integration tests
(209.30 seconds, within all budgets), schema qualification, 67 JVM tests,
25 Android tests and both app builds. Final lint passed.

Current deployment: frozen `features10-0b0527df-manual-held-refresh10-20261024`
under `local/manual-testing/builds`; desktop PIDs 56054 and 56055, with data
roots `local/manual-testing/desktop-a` and `desktop-b`. Their `app.pid` files
are authoritative if restarted later. USB installation and the original phone
identity were verified. A remains on `main`, B on `dev`, phone on `main`;
no branches were switched during this resumed work. No verification list was
created or delivered. The follow-up 15-packet phone-to-desktop probe lost 11
packets (73%). Recovery, Full-node creation/delivery and sustained three-node
link stability remain unresolved pending a controlled network test; VPN
settings remain unchanged under the user's explicit instruction to await
approval. The code and automated checks do not establish a completed live fix.

## VPN-off continuation

The user independently disabled the VPN before this continuation. No protection
or VPN setting was changed or restored. A new 15-packet LAN comparison lost
6/15 phone-to-desktop packets (40%; earlier 11/15, 73%). Desktop-to-phone lost
0/15, RTT 4.96–258.10 ms. This small ICMP comparison neither proves QUIC health
nor establishes VPN causality. A controlled application sync still timed out
in admission to both desktops (12.51 and 12.75 seconds); bootstrap remained
pending with one incomplete direct checkpoint import. Branches remain A `main`,
B `dev`, phone `main`; the phone reports `Full` and `canCreateLists=true`.

Live A logs also exposed `checkpoint page unavailable`. A read-only consistent
phone snapshot found that its pending checkpoint's signer is A's saved branch
author, rather than A's current node author. A retains that author's line mapping
but no imported page signatures for its locally formed checkpoint. A new
regression reproduces the exact refusal after compacting on `dev` and returning
to `main`. The proposed fix searches retained line signers for page signatures
and restores saved line signers at SDK open without selecting them for writes.
The focused transfer regression passed; the SDK restart regression also passed
with a fresh member bootstrap and subsequent incremental write. Workspace lint and check passed (180 unit and 1,298 integration tests;
204.87 seconds within the 300-second integration budget, including individual
budget confirmations). Both native SDK publications succeeded; the Kotlin SDK check passed in three
minutes. Shopping qualification and deployment are pending; no live recovery
is claimed.

The user additionally requested an isolated 100,000-row end-to-end load and
real-phone bootstrap, count/value validation and incremental sync, with time,
wire-byte and storage measurements. This remains pending behind the existing
household's recovery. The existing household must not receive the load fixture;
a separate debug application identity/data root is required on the phone.

The first `line-signer11` shopping check passed schema qualification and 66/67
JVM tests but failed an immediate `bootstrapPending=false` assertion after both
trusted peers had already received the Full member's new list. Delivery does
not imply completion of every bootstrap scope. The test now waits, with its
existing 60-second bound, on the status flow for bootstrap completion. This
retains the completion requirement and avoids an arbitrary sleep; its rerun and
app packaging are pending. That first shopping run is not counted as passing.

`line-signer11` subsequently passed shopping schema qualification, all 67 JVM
and 25 Android tests, and both builds. The two desktops closed normally and
reopened in their original directories (A PID 99547 on `main`, B PID 99548 on
`dev`). USB `-r` installation on the original phone succeeded. All three report
`features10-0b0527df-manual-line-signer11-20261024` and their original identities.
The phone's staged checkpoint import completed, and bootstrap became false at
13:46:10 UTC. A consistent follow-up snapshot has no staged imports. This is a
live improvement from the retained-signer fix, but authoring remains blocked.
The phone still reports Full, issuer=false, canCreateLists=true and `main`.
Creation of `Full node verification 2026-10-24 signer11` refused the verified-seal
gate before creating a list; neither desktop has that verification list.
A separate preparation-only call hits the same gate. A focused SDK regression
for an unused partition prepared before a later seal passed; that simple case
alone does not reproduce the remaining live state.

A later VPN-off 15-packet phone-to-desktop probe lost 10/15 packets (66%). A
manual sync recorded dial, native-read and channel failures. A two-minute native
error probe additionally caught `link channel queue quota`. An eight-second CPU
sample of A shows control/registry SQL traversing `partition_statement`,
`table_complete` and repeated global scope-readiness checks for unrelated
partitioned tables. The proposed `scoped-read12` change loads partition proofs
only for conservatively parsed SQL relation names, including nested view
expansion and write targets. It keeps the existing checker and authoring gate.
Parser regressions passed; a focused secure regression and final validation are
pending. Seal-gate failures now distinguish unsealed import, lost compaction and
remaining replay scopes using short scope hashes, without application payloads.
The isolated 100,000-row harness is prepared under `local/manual-testing/load100k`;
no scale validation or real-phone 100,000-row result has been claimed.

## Resumed after usage-limit interruption

The interrupted focused Room regression completed successfully (one test).
The earlier scoped-read workspace run passed 180 unit and 1,300 integration
tests overall but recorded a Room housekeeping assertion failure: forbidden
`PRAGMA foreign_keys = OFF` returned code 1 instead of 23. It is not counted as
a passing check. The current change includes the unpartitioned-schema fast
path; a serialized full rerun is in progress. Lint passed on resumption.

The phone remains Full, nonissuer, on `main`, with list creation enabled. Its
status still reports an unverified scope despite completed checkpoint import.
A remains on `main`, B on `dev`, and both have zero verification lists. A new
15-packet phone-to-desktop LAN probe lost 11/15 packets (73%); VPN settings were
not changed. This does not prove the cause of application-level failures.

The serialized resumed workspace check passed 181 unit and 1,301 integration
tests, with integration at 198.68 seconds within the 300-second budget and all
individual budgets satisfied. The Room housekeeping regression passed. Fresh
JVM and Android native publication is now in progress. A consistent live phone
snapshot again passed `quick_check` and had zero staged checkpoint imports.

Fresh JVM and both Android ABI runtime publication passed (4m 16s). Kotlin SDK
checks and shopping qualification/builds are running serially before the
scoped-read12 deployment. No new live creation or scale result is claimed.

`scoped-read12` passed the Kotlin SDK check (3m 20s), shopping schema
qualification, 67 JVM and 25 Android tests, and both app builds. The frozen
artifacts' JVM and Android native hashes were checked against the published
runtime. Both desktops closed normally and reopened in their existing roots:
A PID 87073 on `main`, B PID 87074 on `dev`, with original identities. USB `-r`
installation succeeded; the phone reports the same build and original identity.

At approximately 16:47 UTC, own-list preparation succeeded. The subsequent
identity-checked probe created `Full node verification 2026-10-24 scoped12` and
`Verification item` on the same Full nonissuer phone on `main`. A consistent
phone snapshot passed integrity checking, confirmed both rows and zero staged
imports. The old private error file was still present after successful
preparation; it is stale and not evidence of a new refusal. The timing alone
does not isolate the scoped-read optimization from background recovery.

A bounded 45-second delivery observation found neither list nor item on A,
and no list on B (`dev`). A subsequent native probe still found native-read
and channel-read timeouts. The desktop CPU sample is mostly idle; it does not
prove another CPU bottleneck. Successful local authoring is established; list
delivery, sustained link stability, and isolated 100,000-row validation remain
pending.

A separate debug package `org.example.rocky.load100k` was installed without
changing the shopping installation. Its first harness attempts failed before
replication (an invalid synthetic application identifier and an IPv6 control
listener with an IPv4 ADB forward); neither is a Rocky sync result. After those
harness corrections, direct-only QUIC enrollment timed out. With the standard
relay-enabled transport, a fresh isolated phone joined in 4.84 seconds and
completed an empty reconciliation in 10.43 seconds. A second isolated run joined
in 5.03 seconds and received one authored row in one sync pass (6.62 seconds),
validating its key, completed flag and title on the phone. These are small
transport checks, not 100,000-row qualification. Active Android networks were
Wi-Fi and cellular; the default was Wi-Fi, with no active VPN transport.

The new shopping list's four checkpoint imports completed on A around 17:02
UTC, followed by two received frames. They remained pending, so imported bases
and `bootstrap=false` still did not establish row delivery. A normal reopen
from the same frozen runtime did not resolve it. An offline, private consistent
copy with networking disabled applied both frames when its keys loaded. A
temporary JVM-only diagnostic runtime reproduced initial application on the
live A, followed by online recovery returning both frames to pending and
removing their rows again. No database or keys from the offline copy were
installed into the household. The temporary probes printed only static waiting
categories and are now removed from source.

A's current diagnostic process is PID 3473, using its original directory and
identity, with a separately frozen JVM probe artifact under
`local/manual-testing/builds/pending-probe13`. B remains PID 87074 on `dev`;
the phone remains on `scoped-read12`, Full and nonissuer on `main`. The extended
sealed-branch shopping regression delivered the Full member's list to the main
follower but stalled on the dev follower's list query; the initial unbounded
run was terminated and is not passing evidence. That query is now bounded.
A focused reset-replay regression is running: `replay_at_seals(reset=true)`
marks retained frames pending but does not currently run pending application
afterward. This is an implementation hypothesis pending the regression.

The focused compaction regression now explicitly requests reset replay with
all retained frames. It failed before the change with recovery still pending.
Running pending application alone cleared recovery but left the restoration
guard; adding the existing exact restoration check made the regression pass
(0.52 seconds). Replay now applies retained frames, checks restoration and
rechecks seal recovery without requiring another incoming frame. Missing
chains still keep their guard. A separate bootstrap-planning guard preserves
its existing import intent when the admitted peer follows another tip, after
metadata exchange and before ordinary scoped offers. The cross-tip proof
regression checks that held exchange remains allowed and current rows remain
refused. Full validation for `replay-recovery14` is in progress.

`replay-recovery14` passed lint, Clippy and the full workspace check: 181 unit
and 1,301 integration tests, integration 195.68 seconds within the 300-second
budget and all individual budgets satisfied. Native publication, SDK/app
qualification and live deployment are next; the running JVM probe is not the
final corrected deployment.

## Three-node continuation

`replay-recovery14` native publication passed for JVM and both Android ABIs
(4m 8s); the Kotlin SDK check passed (3m 11s). Shopping qualification and all
25 Android tests passed, but the JVM suite passed only 66/67 tests. The extended
sealed-branch regression timed out reading the trusted `dev` follower's lists
after the Full member created a list on `main`. This is not a passing app check.
The main follower received the new list successfully.

A focused reproduction found no staged checkpoint imports in a consistent
snapshot, despite runtime readiness reporting "a scope is being imported".
The SDK's future import intent was mistaken for missing historical coverage
when the new partition had not materialized. Recovery now checks frame and
existence coverage independently of that intent; ordinary finalization and
coordinated readiness retain their import guards. Validation is in progress.
Temporary test probes were removed. Existing device data and branches remain
unchanged. A pre-deployment consistent phone snapshot passed `quick_check`
and confirmed the earlier verification list and item, absent on both desktops.

The focused app regression passed in 10.03 seconds with the new JVM runtime:
the Full member's new list reached the trusted main follower, the trusted dev
follower's list query completed without that list, and following main then
bootstrapped the new list and item. The return uses `follow`, retaining the
branch's evidence; attempting `leave` correctly refused to discard required
frames. The Rust coverage regression passed before its missing-history case
was added; the complete regression and workspace rerun are pending.

The complete coverage regression now passes (0.14 seconds), with assertions
that both replicas have finalized a base before testing the missing-history
case. It uses path finalization, independently of coordinator finality.

The trusted desktops reopened normally with their original identities and
branches. A recovered the phone's earlier verification list and item; repeated
read-only storage checks found both present and no pending frames. B on dev
has neither row. This established storage recovery, while A's list query still
waited and the desktop link repeatedly closed. The app's Sync screen waited
for list/device names before showing any events; it now renders activity before
that metadata becomes available. Lists also distinguish the first pending read
from an empty household.

Deployment inspection found that compiling the app did not rebuild its shared
JVM JAR. The deployment now explicitly runs the shared and desktop JAR tasks
and checks the diagnostic build constant in the packaged shared class, in
addition to comparing native hashes. The updated Sync screen displayed the
actual reconnect cause. `frame-budget16` identifies an oversized placement
response: 354,150 bytes against the 262,144-byte native frame limit. The page
bounded header count but not the credential chains it carries. Placement
responses now account for both, reserve framing space, and choose earliest
author positions first so shortened pages can advance. Validation and Android
deployment of these changes remain pending.

## Continued scale result and household verification

The prior thread stopped before reporting the isolated run's final result.
Its controller completed successfully after that interruption:
`load100k-20261024T185105Z`. This uses the SDK migration fixture, a separate
`manual_load100k` domain and Android package `org.example.rocky.load100k`;
the household did not receive the fixture.

The host authored and published 100,000 rows in 94.380 seconds, validated
100,000 distinct keys with zero invalid values, sealed in 1.562 seconds and
formed a replay base in 11.563 seconds. The phone joined in 4.812 seconds
and completed bootstrap in one sync pass in 323.519 seconds. Its complete
SQL validation also found 100,000 rows, 100,000 distinct keys and zero invalid
values. The phone changed `k000001`; the host received its exact title and
completed flag in one pass (7.560 seconds). The host changed `k100000`; the
phone received both values in one pass (9.045 seconds). Both counts remained
100,000. These are real-phone SDK results, not a 100,000-row shopping UI test
or a three-replica scale test.

The phone UID counters increased by 51,530,358 received and 3,256,848 sent
bytes during join and bootstrap. They include transport overhead and control
traffic, not just database payload; the controller separately counted 4,271
bytes sent and 3,535 received on its control connection. UID counters do not
measure all physical network overhead. After normal close, host SQLite was
412,647,424 bytes and phone SQLite 161,673,216 bytes. Total data directories
were 412,671,001 and 161,696,063 bytes respectively. The host retained sole-copy
history: compaction formed a replay base but pruned zero frames.

`placement-pages17` passed the prior full workspace run (181 unit and 1,303
integration tests, integration 202.67 seconds within budget), Android native
publication for both ABIs, the Kotlin SDK check, shopping schema qualification,
67 JVM and 25 Android tests, and application builds. Both packaged Android
native libraries matched publication. The original Full nonissuer phone created
another verification list and item after installation.

On continuation, both trusted desktops reopened in their existing roots.
Direct inspection found both already following `main`; neither was switched
during this continuation. Both displayed recovered lists and items.
A fresh consistent offline copy, opened with networking disabled, read six lists
and finalized through the current main seal with no recovery wait. The phone
still reported missing historical coverage in its initial status. A persistent
foreground probe successfully edited its verification item; delivery and link
stability remain under live observation. No protection or VPN setting changed.

The first continued workspace check passed all 1,303 integration tests but
exceeded the 300-second tier budget (317.00 seconds); it is not a passing
check. Its rerun was superseded when another transport regression was added.
Temporary admission tracing was removed from source and frozen separately
for desktop A. Its diagnostic runtime logs sizes and static phases only.

The phone's charging screensaver/lock screen triggered `activity background`
and cancelled both live reconciliations. A bounded instrumentation session
explicitly activated SDK sync independently of application visibility and
restored the SDK to the actual visibility state afterward. This reproduced
channel read timeouts despite successful admission, so background cancellation
is not a complete explanation. The same session read six lists; a consistent
snapshot passed integrity checking and retained two incomplete imports. One
channel queue refusal also occurred; its cause is not established.

A paused-clock regression reproduced a request-channel timeout while a reply
was still arriving: three 8-KiB fragments at six-second intervals failed at
ten seconds. Request-channel reads now renew their deadline only after another
full 8 KiB on that channel. Silent peers, sub-fragment trickles and traffic on
other channels still time out. All six channel tests passed, including three
new progress/stall regressions, in less than 0.01 seconds. No wire encoding,
message-size limit or queue bound changed. Build `channel-progress19` is in
qualification; no live recovery from this change is claimed yet.

The isolated two-node scale measurements are also recorded in
[manual-sync-100k-2026-10-24.json](manual-sync-100k-2026-10-24.json).

## Frame-progress continuation

`channel-progress19` passed native JVM and both Android ABI publications,
the Kotlin SDK check, shopping schema qualification, 67 JVM and 25 Android
tests, and both app builds. The final archived workspace check passed 184
unit and 1,303 integration tests (217.53 seconds within all budgets; format
refusal 1.93 seconds alone). A prior check passed all tests but exceeded that
one test's budget, so only the final check is passing evidence.

With the phone SDK explicitly active, a fresh Full nonissuer list and an
item rename/check succeeded on build 19 but did not reach either desktop in
three minutes over the default transport. A reversible diagnostic reopened
the same phone installation with iroh bound to loopback, retaining standard
relay service and disabling local discovery for that experiment. Desktop path
observations confirmed relay use. Both desktops then received the new list
and its checked, renamed item with zero pending frames. The phone completed
its imports, finalized through the current main seal and reported no recovery
wait. Four stale-checkpoint-page refusals occurred during that recovery;
this does not establish a failure-free link. The original transport factory
and actual application visibility state were restored afterward. No VPN or
protection setting changed.

On the restored default transport, the Full phone's new title and unchecked
flag reached both desktops. A new item entered through Desktop A's actual UI
reached B within the first one-second storage observation and remained present
through a 70-second observation. It did not reach the phone in the two-minute
SDK-active observation. That session reported foreground, worker enabled and
worker running at its final readiness read. Normal unlocked foreground UI
behavior is not established by these instrumentation sessions.

The physical native and live-link readers/writers also used a single deadline
for an entire frame. They now share bounded 8-KiB progress operations, retaining
header, byte-quota, signature and sequence checks. Idle live links still wait
without a deadline; a started header or stalled chunk remains bounded. All 33
networking tests passed in 1.09 seconds, including four new paused-clock cases
for slow native reads/writes, trickles and a signed link frame spanning multiple
deadlines. `frame-progress20` native publication passed for JVM and both Android
ABIs; the desktop apps reopened normally with matching frozen runtimes. Full
validation and phone deployment are in progress; no build-20 live result is
claimed yet.

## Three-node scale and foreground upkeep

Build `frame-progress20` passed lint and the archived workspace check: 187 unit
and 1,304 integration tests, with integration taking 208.81 seconds and all
configured budgets passing. The Kotlin SDK check passed. Shopping's Android
25 tests passed; its concurrent JVM run hung in two cases. A focused rerun of
all five Approvals/Seals cases passed, but that does not erase the first hang.
A native sample captured a reactor blocked publishing a revision while status
observation held a watch read guard across replica awaits. The status snapshot
now copies the revision before constructing the asynchronous fields. A
concurrent status/commit/shutdown regression and build `status-lock21` are in
qualification.

The earlier phone instrumentation activated only the SDK. Shopping separately
uses the application's foreground flow to run household subscriptions and key
upkeep; that flow stayed false while Android's keyguard was showing. A corrected,
bounded build-20 probe activated both application upkeep and SDK sync on the
original default transport. The phone then received Desktop A's new item and
kept it through repeated observations; both peers were connected at the final
read, with Full nonissuer authority, main selected and no compaction/rebootstrap
wait. The probe restored the original visibility state. A consistent live-file
snapshot passed SQLite integrity checking and contained exactly one each of
`Desktop A verification 19`, `Desktop A verification 20`, and the phone-authored
`Phone default route verification 20`, all unchecked. Both desktops had those
same items. All six application tables matched exactly across these three
snapshots (7 lists, 11 items, other tables empty); no operation was pending.
The phone still had one incomplete staged list checkpoint and reported
bootstrap pending, so complete recovery was not established by that snapshot.
Normal unlocked UI lifecycle behavior remains a separate check;
these instrumentation observations do not establish that the keyguard was open.

Run `load100k-three-20261024T223838Z` succeeded using build-20 packaged SDKs,
two JVM Trusted replicas and the real phone as a Full nonissuer replica. This
isolated fixture did not replace or seed the Shopping household. A migration
authored/published 100,000 synthetic items on A; B and the phone received their
rows through checkpoint bootstrap. Each validated 100,000 distinct keys and
zero invalid baseline values. Final counts stayed 100,000 on all three.

Authoring/publishing took 96.717 seconds; checkpoint creation took 11.491.
B bootstrapped in 24.820 seconds and the phone in 215.272 seconds, each in one
pass. Subsequent edits from every node converged to both other nodes: phone to
A/B in 3.156/0.775 seconds; B to A/phone in 2.709/8.755; A to B/phone in
2.578/2.310. The phone UID received 50,676,748 bytes and sent 2,518,461 during
bootstrap, including control and transport overhead. Closed SQLite files were
412,700,672 bytes on A, 161,751,040 on B, and 161,722,368 on the phone. The replay
base retained sole-copy history and pruned zero frames. Other qualification ran
concurrently; this is functional evidence, not a controlled benchmark or a test
of rendering 100,000 Shopping UI rows. Sanitized results for this run and the
earlier two-node run are in the adjacent JSON evidence file.

## Foreground check on October 25

The user made Shopping visible again. Android reported keyguard not showing,
Shopping resumed, and Wi-Fi connected. The normal application foreground flow
and SDK foreground/worker flags were true without instrumentation overrides.
Build 20 did not receive a fresh Desktop A UI item in the first minute and
reported no connected peers. Build 21 was deployed in place: the original
desktop installations closed normally, and the phone update preserved its data
and identity. A fresh item authored through the phone UI reached both desktops;
the reverse edit was still absent while checkpoint recovery continued. The
user reported LTE has no data allowance; subsequent testing uses Wi-Fi and
changes no OS network or protection settings.

Build 21 passed the Kotlin SDK check, 67 JVM and 25 Android Shopping tests, and
both app builds. Final lint and the archived Rust check passed 187 unit and
1,306 integration tests, integration 215.84 seconds. The status concurrency
regression passed in 169.6 ms alone. Its first version used an overly short
one-second timeout and failed under concurrent qualification load; the final
small concurrent workload has a ten-second failure bound and remains within
the normal two-second integration budget. A first workspace run also exceeded
the existing format-refusal test's budget; the passing final run measured it
at 1.92 seconds alone.

A temporary desktop-only native trace recorded message family, sequence,
request duration and error, without payloads. Original production source was
restored immediately after building the isolated diagnostic JARs. Trace showed
checkpoint offer/page requests answered in single-digit milliseconds. Recovery
advanced to other scopes, but request-channel read timeouts still repeatedly
closed live links. The phone's staged scope changed across consistent snapshots,
and one snapshot had no stage before later recovery opened another; this is
progress, not proof that recovery has completed. Reopening the phone normally
also reproduced transport dial/admission retries. No relay-only policy, VPN
change or data reset is used as a fix.

Serving an authenticated request channel now waits while idle before its next
request, allowing the requester to apply the previous answer. Once a request
starts, its header/fragments retain bounded progress deadlines; client replies
and admission remain bounded. The native stream path has the same distinction.
Five paused-clock cases cover delayed requests, started headers/fragments that
stall, and a partial request queued before the handler starts. Existing client
silence/trickle regressions remain in place. Build `request-idle22` is in
qualification; no successful final live result is claimed yet.

## Loading and shared recovery slot

Build 22 passed 38 networking regressions, native JVM/two-ABI Android
publication, the full Kotlin SDK check, 67 JVM/25 Android Shopping tests and
both apps. Its archived Rust check passed 192 unit and 1,306 integration tests;
integration took 234.44 seconds within the 300-second tier. Final budget reruns
passed. The deployments contained no temporary native trace strings.

The user continued interacting with the apps; UI automation and temporary
screen-keepalive taps were stopped to avoid competing with that input. No
subsequent test assumes a quiet household. The user's phone then showed
`Loading your lists…` while links had previously been open. List state waited
for the selected list's categories, items and links before emitting even its
list tabs. It now emits metadata immediately, keeps the tabs usable and shows
a per-list loading state while those details are unavailable. The deterministic
flow regression passed; build `list-loading23` passed 68 JVM/25 Android tests,
both app builds, lint and commit lint. All three installations updated in place.
A screenshot confirmed the phone showing list tabs and items again. A consistent
phone snapshot and Desktop A had identical values in all six application tables
(7 lists, 15 items); Desktop B had one newer item change during the user's
interaction. The previously missing Desktop A verification item was present
on all three. All integrity checks passed, all three followed main, and the
phone had no pending operations but still had a staged checkpoint. This proves
row convergence at that observation, not finished recovery. The phone-authored
foreground verification item was created in the existing `aef` list, not in the
separate verification list.

Ordinary sync's later checkpoint recovery was outside the shared bootstrap
mutex. Parallel exchanges with two peers could both mutate a replica's staged
checkpoint state. The guard now covers initial bootstrap and ordinary row
reconciliation, after transport/admission/envelope delivery. The redundant
second marker existence check was removed. A controlled regression holds the
shared slot with no bootstrap marker and starts ordinary sync: the old code
bypassed the slot and failed the assertion in 0.17 seconds; the fixed code waits,
then receives all four authored rows after release (1.19 seconds including the
one-second wait, within the integration budget). A preliminary large two-peer
case passed before the fix and was replaced; it is not claimed as reproduction
of the race. Build `import-slot24` is in full qualification and deployment.


Build 24 subsequently passed native publication, 68 JVM/25 Android Shopping
checks and both builds, lint and commit lint. The final archived Rust run passed
192 unit and 1,308 integration tests (210.03 seconds). The first run passed all
behavior assertions but exceeded one two-second test budget; the final budget
rerun passed. The Kotlin SDK JVM checks passed on rerun; an initial lifecycle
check timed out under concurrent load, then its focused and full JVM reruns
passed. A consistent phone snapshot had integrity OK, 80 applied operations,
zero pending operations and zero staged imports. Later admission/read retries
continued, so this is a recovery observation, not sustained connectivity proof.

## Cold reopen and held-history scan

The successful three-node 100k fixture was copied into isolated directories and
reopened with build 24. Both desktops remained Trusted and the real phone Full
(nonissuer); each reopened with 100,000 distinct rows. Open took 2,035/1,506/2,318
milliseconds for A/B/phone. A phone edit committed in 136 milliseconds, but did
not converge to Desktop A within the 300-second limit. This failed incremental
run is retained as `load100k-reopen24-20261025T072518Z-result.json`, and does not
replace the earlier successful bulk-sync result. An earlier controller attempt
connected through ADB before its listener was ready and failed with BrokenPipe;
the controller now waits for the phone listener.

A native sample of Desktop A during the failed run showed held-frame page
requests rescanning operation headers, sorting in SQLite and decoding headers.
An empty held page did this even when all history was live or continued. The
new regression fails against the old implementation (eight unrelated header
decodes for an empty page). Classification now uses the existing transactional
position index; only candidate held frames, or recovery-seal cases requiring
payload inspection, load the original signed header/body. Frames are still
rechecked before holding. On a read-only copy of the closed 100k fixture, the
old SQL took 501 ms and the indexed SQL 20 ms for 3,130 operation frames. This
isolated query comparison does not establish end-to-end convergence; build 25
qualification and the repeated cold incremental run remain pending.


The build-25 cold retry `load100k-reopen25-20261025T074740Z` passed. Open took
2,055/1,630/772 ms for A/B/phone. Phone authoring took 89 ms; its edit arrived
at A/B in 9,079/78 ms. B authoring took 8 ms; delivery to A/phone took
174/3,631 ms. A authoring took 10 ms; delivery to B/phone took 97/3,145 ms.
Every delivery completed in one explicit sync pass and checked the exact
updated title and completion value. All three ended with 100,000 rows; initial
row/distinct counts and Trusted/Trusted/Full roles were verified on reopen.
The same real phone ran the isolated instrumentation without taking Shopping's
activity. The successful cold run and preceding failed cold run are included
in the public JSON alongside the earlier bulk-sync evidence. Concurrent app
qualification was running; these are functional timings, not a controlled
benchmark. Live Shopping recovery and final checks remain separate gates.
