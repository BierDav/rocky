# Phase 8 evidence

Status: implemented; performance and device qualification incomplete (2026-10-25).
Base: phase 6 (`708230a5`), Jujutsu workspace `rocky-pipelined-sync`.

## Implementation

- Range catch-up requests the next cursor before applying the current page.
  Requests acknowledge only committed frontiers; a final `Held` closes each
  completed or budget-limited scope. No intermediate acknowledgment round trip
  is required. The next request and current apply overlap without skipping
  frames at the page cursor.
- Checkpoint requests carry a window of at most four pages. Replies retain
  their scope, checkpoint and predecessor cursor, are verified off the replica
  lock and staged in order. Windows shrink near the session byte limit, and
  the next window starts before the preceding window's last page is stored.
- Up to four scopes bootstrap or reconcile concurrently, each on its own
  request session with its own byte budget. Graph refresh precedes scheduling;
  completion updates bootstrap intent durably and failures remain per scope.
- Graph reconciliation compares statement IDs over fixed binary ranges of the
  sorted digest space. Equal ranges stop immediately; differing leaves exchange
  bounded ID sets and only missing statements. Dependencies are admitted after
  collecting the complete difference. Candidate and followed-tip checks remain.
- Held-change tokens compare the currently authorized, refreshed held-frame set
  before transferring frames. Equal sets skip both directions. Admission reuses
  the exact local scope tokens sent during the handshake.
- An iroh link owns a dedicated QUIC connection with a control stream and four
  request streams; optimistic row pushes use the first bulk stream. Additional streams bind to the authenticated link and lane
  in their signature labels, with independent sequence counters and bounded
  workers. Channel generations are checked per channel. Final reconciliation
  markers carry delivery watermarks across lanes. Independent FINs drain with a
  deadline so they cannot discard a close or data arriving on another stream.
  Application-owned endpoints remain open. Other byte-stream transports retain
  their existing single-stream path.
- The sole current link handshake is **18**, and the transport ALPN is
  **rocky/link/5**. No legacy reader or format migration was added.

## Profiles and measurements

The starting sampled profiles are the phase 6 issuer-fill and returning-member
samples in `/tmp/rocky-phase6/{baseline,after}-{fill,member}.sample`, summarized
in [phase 6 evidence](06-database-thread-evidence.md). They show signature work,
CBOR encoding, SQLite B-tree and WAL I/O remaining after duplicate frame opening
was removed. Those samples preceded phase 6's final pruning corrections; their
38.560 s / 23.380 s member runs are observations, not a controlled phase 8
baseline or proof of any phase 8 gate.

New host logs, native builds, isolated phone probe and measurements are kept
under `/tmp/rocky-phase8` on the execution host. The probe replaces only the
native library of the separate `org.example.rocky.load100k` package and uses
fresh directories. Its frozen JVM/Android bindings perform their normal ABI
checks. The phone is a physical Pixel 11 Pro running Android 17; its serial
is `66121FDKX001MF`. The probe's local-network permission was disabled and had
to be enabled before a LAN test could connect.

## Device observations

The initial qualification build's native SHA-256 values were
`d7700153b4caae2746c922125ba590dacd7d3e0b497ee92602c4ec1b5a20914d`
(Android, stripped) and
`e0d5b6796b1b090abf3cd9ff1410ae8c0df3e54803b595d1f001b0ab2f76ae69`
(macOS). These observations precede the final invalid-quota guard and bulk-push
lane routing refinement. Final runtime rechecks are recorded below when complete.

Run `pipeline-100000-20261025T164249Z` used two JVM processes and the physical
phone, fresh directories and the isolated SDK migration schema. Relays and lookup
were disabled for LAN. Every key, row count and generated value was checked on
both joining replicas; all three ended readable and writable. Public node IDs:

- Desktop A (issuer): `d91bb97353bcc22bd3ce1d6031caed9b569480588c54ed4f4dd290aefbc77e27`.
- Desktop B (issuer): `26ff8089150d1f8654e7d962049dc706b5314a5657953b4ebcebf9e5c2b2845e`.
- Phone (writer): `1cab47cbb90162db539d3c5259a93dfd6d8f183dce753fed4af2dfc9fdcd1553`.

Observed stages: issuer author/publication **143.273 s**, seal **1.314 s**,
checkpoint **11.026 s**, desktop B bootstrap **24.762 s**, phone bootstrap
**319.364 s**. Each bootstrap completed in one explicit sync pass, and the
returned final reconciliation reports contained no timeout or failed request.
Builds and other local work overlapped the run; starting host load was 34.74 on
18 cores. These are observations, not an isolated before/after comparison.

With automatic live links enabled after bootstrap, 20 alternating one-row
transfers between desktop A and the phone had nearest-rank **p95 440.94 ms** and
**maximum 865.13 ms**, including the synthetic control bridge and read polling.
The 1 s gate was observed for this build. Five empty manual reconciliations of
the 100k phone replica took **2.867–3.608 s on LAN** despite transferring no rows.
That remaining metadata/storage cost is not hidden by the small-fixture result.

Run `pipeline-12-20261025T165240Z` forced relay use for the phone by binding the
endpoints to loopback and disabling local discovery. Five empty reconciliations
took **1.904–1.969 s**. This observes the 2 s gate on that small sealed fixture;
it does not qualify empty reconciliation of the 100k replica over relay.

## Phone profiles and remaining gates

Two five-second `simpleperf` samples during the 100k phone import captured
**17,756** and **13,574** samples. There were **zero lost userspace samples**;
22 and 14 kernel samples were lost. Kernel symbol addresses were restricted;
no device-wide security setting was changed. Native addresses were resolved
against the matching unstripped ELF with NDK `llvm-addr2line`.

Self cycle overhead includes `pread64` **4.55% / 6.43%**, `pwrite`
**3.99% / 3.78%** and `fsync` **3.89% / 4.49%**, alongside scalar Ed25519 field
arithmetic, CBOR decoding, allocation and SQLite page-cache work. These are
sampled cycle percentages, not wall-clock attribution. The issuer and member
also have macOS `sample` captures in the run directory. The remaining per-row,
proof and storage work is substantial; network pipelining did not achieve the
**60 s phone bootstrap gate**, which remains unchanged and unmet.

The **100 ms per-hop / 1.5× 100k catch-up gate is not verified**: this run has no
validated latency shaper for that measurement. The **three-device shopping UI
check is not verified**. The isolated three-node SDK probe is distinct from that
application check and did not alter the user's shopping household.

## Verification

- The 32 focused protocol regressions passed, including graph differences and
  zero-statement reconciliation, unchanged held-frame skipping, final range
  acknowledgment/pruning, four-page checkpoint windows, failed-page resumption,
  independent streams, delivery barriers and borrowed endpoint ownership.
- All 12 bootstrap tests passed after replacing the serial-import assumption
  with an exact persisted-pending-set assertion across an offline reopen.
- The unit tier passed **199 tests** (184 workspace, 15 SDK without defaults).
- Static lint and workspace Clippy passed. Final combined verification results
  are recorded below.

## Resumed integration and final runtime check

The implementation was combined with main's deterministic simulation and
coordinated-outage work. The stored-header regression still expected an empty
held lookup to decode an unrelated live pending frame. Its assertion now follows
the transactional position index: that lookup stays empty, while header scan
oracles and a full audit reject corrupt stored bytes. No admission or production
validation was removed.

The iroh idle-liveness fixture also froze Tokio time during real QUIC
reconciliation. Independent lanes can need delayed ACK timers before becoming
idle. The fixture now allows those timers to run during reconciliation, then
freezes time for its unchanged no-traffic/no-redial assertions over 60 simulated
idle seconds. Its five-second worker-settlement bound remains unchanged.

Two consumer fixtures exposed scheduling assumptions on a subsequent full run.
The revocation fixture now fetches the signed revocation through renewal before
asserting local sync refusal. The concurrent-renewal fixture disables background
replication and extra issuers and controls reachability of the two intended
issuers, so its unknown-device renewal cannot race a relayed enrollment record.
Both corrected fixtures passed focused checks; security and registry assertions
remain in place.

Run `pipeline-final-reopen-20261025T173148Z` reopened the same three 100k replicas
with the final native builds, checked all three row counts, restored automatic
live links and passed 20 alternating phone/desktop row updates. Nearest-rank
**p95 was 215.68 ms**, **maximum 278.42 ms**. All three ended readable and writable.
This verifies reopen and live transfer, not another bootstrap or shopping UI run.
The starting host load was 90.33 on 18 cores while verification builds ran.

Final native SHA-256 values:

- Android (stripped): `0320810d0bc66577afe940913754301d0cfac947cdd4a640dfa5de4d5d0cfbdf`.
- macOS: `39996fbc162faa95b400cd50c296d8d29384e827573a6be64f41457276ec7a9b`.

Both native artifacts and the reopen result remain under `/tmp/rocky-phase8` on
the execution host, independently of the temporary Jujutsu workspace. The
bootstrap, latency-shaping and shopping UI qualification limits above remain.

## Final combined verification

- `./scripts/lint` and `./scripts/commitlint --jj` passed.
- `./scripts/check` passed all **199 unit tests** and all **1,378 integration
  tests** (1,074 workspace and 304 SDK without default features), including the
  corrected fixtures and deterministic simulation seed set. Integration suite
  wall time was **252.76 s**, within its **300 s** budget.
- **The command still exited 1:**
  `rocky-sdk::tips races::revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner`
  exceeded its **2 s per-test budget** in both builds. Its best isolated reruns
  were **2.31 s** and **2.24 s**. All its assertions passed. No budget exception,
  tier move or weakened assertion was added. Removing a round of fixture syncs
  did not resolve the overrun, and that experiment was reverted.
- Final check output is `/tmp/rocky-phase8/resume-verified.log` on the execution
  host. This is not a fully green check or completed phase qualification.

## Phone qualification resumed on 2026-10-25

A fresh three-node LAN run, `pipeline-100000-20261025T181536Z`, uses the
final native artifacts above, fresh synthetic databases and the isolated
`org.example.rocky.load100k` package. The installed APK's native library and
host library were hashed before starting; both match the final hashes above.
The controller is `/tmp/rocky-phase8/qualification_run.py`. No production code
or shopping household was changed. Initial host load was 2.63 on 18 cores;
unrelated host tests subsequently ran, so this is not an isolated comparison.

### Locating the import cost

The earlier five-second samples were expanded to cumulative caller reports
and resolved against their matching original unstripped ELF. The original
stripped and unstripped artifacts have identical `.text` addresses, lengths
and SHA-256 values. In those samples,
`PublicKey::verify` accounts for 32.75% / 30.94% of sampled cycles and
`sqlite3_step` for 22.52% / 25.57%. These are CPU samples, not wall-clock
attribution; caller percentages overlap and must not be added. Reports are
`/tmp/rocky-phase8/phone{,-late}-children-demangled.txt`.

Code inspection identifies redundant work in the checkpoint path:

- `Origin::from_checkpoint` verifies a creation header when reconstructing an
  origin (`replica/moves/compact.rs`). `open_page` verifies supplied headers
  again, or verifies the known header again when a row first references it
  (`replica/checkpoint/transfer.rs`). `store_page` passes newly supplied headers
  to `put_creation_header`, which verifies them again (`replica/moves.rs`).
  The stored header dictionary does not avoid repeat signature checks when
  the same header is referenced on another page.
- `stage_checkpoint_page_inner` decrypts and decodes the page while holding
  the replica lock to discover header references. `open_page` then decrypts
  and decodes the same page again on a blocking worker.
- `fetch_pages` overlaps network windows, but awaits the complete verify/store
  of each page before opening the next. Per-row dictionary, record, stub,
  index and projection writes remain inside each page transaction.

The desktop member stores 100,000 rows in 766 checkpoint pages with only
3,126 distinct creation headers/origins. The repeated verification is therefore
not limited to one check per distinct signed creation header for the import.
For the item scope specifically, the persisted page cuts contain 765 pages,
3,125 unique origins and **97,830 distinct-origin occurrences summed across
pages**. Thus origin reconstruction alone repeats signature verification about
**31 times per unique header on average**, before the additional header checks.
These counts were read from the completed desktop member; the small registry
scope accounts for the remaining page and header. The count report is
`/tmp/rocky-phase8/pipeline-100000-20261025T181536Z-origin-counts.json`.

A likely optimization boundary is to carry verified, decoded headers through
page opening and storage and reuse immutable signature results by digest,
while retaining authority, digest, provenance and page-order checks. This diagnosis has not yet been validated by an optimized build.

### Final-build fresh LAN results

- Issuer author/publication: **115.604 s**; seal: **1.573 s**; checkpoint:
  **12.167 s**. Desktop B bootstrap: **27.797 s**.
- Phone bootstrap: **250.719 s**, one explicit pass, with no failed request or
  timeout in the returned report. The **60 s gate fails by 4.18 times**. All
  generated values and keys were checked on both joining replicas; all three
  ended readable and writable.
- Twenty alternating automatic phone/desktop updates: nearest-rank
  **p95 197.86 ms**, maximum **1,173.83 ms**. The **1 s p95 gate passes**.
- Five empty manual phone reconciliations on LAN: **2.666–3.074 s**, with no
  transferred rows. The replica-size-dependent empty reconciliation cost
  remains even without relay latency.
- Public node IDs: desktop A
  `c7bd0b99ff6f59e0e420195a479b9b09d5f16171e73ae614d218b873921959b0`,
  desktop B `0ff5679427ab036f5bc11a262dc16906bccd701177c14907dd9418704be46619`,
  phone `26e98020ea7e37c1ea9f15a13e85245ef9ff40f12cb6f4cdf4d35385c863a0a6`.

New early/late phone samples contain **19,450 / 22,837 samples**, with no lost
userspace samples (20 / 0 lost kernel samples). Self cycle overhead is
`pread64` **4.36% / 5.84%**, `pwrite` **4.17% / 3.99%**, and `fsync`
**3.95% / 3.88%**, consistent with the earlier storage-heavy profile. Native
symbols in these final-build samples have not been resolved against a matching
unstripped final ELF; the cumulative symbol attribution above belongs only to
the earlier matching build.

Read-only `/proc/<pid>/io` observations during the new import show
**2,479,190,016 bytes of physical writes over 203.747 s**. This is an interval
within the import, not its total. Physical write accounting had reached
**2,831,290,368 bytes** at the last sample, while the completed phone directory
occupied about **162 MB**. These process counters include WAL and other writes
and do not isolate a particular SQL statement; they establish substantial
write amplification without attributing all elapsed time to disk.

Artifacts, JSON stage reports, source/member host profiles, phone profiles,
self-overhead reports and process counters have the run name above as their
prefix under `/tmp/rocky-phase8`. The 100 ms latency-shaping and shopping UI
checks remain unverified.

### Full-size relay observation

`pipeline-final-relay-20261025T183106Z` reopened the same three 100k replicas,
checked their row counts, disabled local/topic discovery and bound all endpoints
to loopback. The phone therefore required relay transport to reach either
host. Its relay status reported a connected
`https://euc1-1.relay.n0.iroh.link./` with no error.

Five empty manual reconciliations took **8.747, 5.456, 9.096, 5.764 and
11.555 s** wall time (native durations **8.722, 5.432, 8.943, 5.739 and
11.528 s**). Reports contained no transfer, refusal, waiting scope or timeout;
all replicas ended readable and writable. The **2 s relay gate is not met on
this full-size fixture**, despite the earlier small-fixture observation. Initial
host load was 22.25 on 18 cores; workspace verification overlapped this relay
run. These are loaded observations, not an isolated latency comparison or an
attribution of the relay time to one internal operation.

Two preceding attempts did not reach measurement: the first lost its phone
control connection during reopen, and the second found the local control port
unavailable. The wireless ADB port-forward list was empty. Restoring only
`tcp:19401` made the final attempt complete; neither failed attempt is counted
as a Rocky reconciliation failure. Their logs remain under the corresponding
`pipeline-final-relay-*` prefixes in `/tmp/rocky-phase8`.

### Investigation verification

Only this evidence document changed; no runtime optimization was implemented.
`./scripts/lint`, workspace Clippy and `./scripts/commitlint --jj` passed.
`./scripts/check` passed all **199 unit tests** and all **1,378 integration
assertions** (1,074 workspace and 304 SDK without defaults). The workspace and
SDK integration runs took **167.650 s** and **77.927 s**, respectively. The
workspace integration run started at load 133.3 on 18 cores; slow cases were
rerun in isolation by the existing budget checker.

The command **exited 1** because the same previously recorded
`rocky-sdk::tips races::revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner`
test exceeded its **2 s** budget in both builds: **2.08 s** and **2.30 s** on
isolated reruns. All its assertions passed. No test, budget or tier was changed.
The complete check log is `/tmp/rocky-phase8/qualification-check.log`; static
lint output is `/tmp/rocky-phase8/qualification-lint.log`. Phase 8 remains
unqualified: the fresh phone bootstrap and full-size relay gates failed, while
latency shaping and shopping UI checks remain unverified.


## Checkpoint import optimization (2026-10-25)

Work was isolated in Jujutsu workspace `rocky-sync-opt` under
`~/AgentWorkspaces`, starting from the qualification evidence above.

- Checkpoint pages are decrypted and decoded once on a blocking worker,
  outside the replica lock. Stored-header lookup still checks the stored bytes
  against their digest; ordered storage rechecks the checkpoint and cursor.
- A per-replica cache retains immutable, verified creation headers, bounded by
  8,192 entries and 8 MiB of encoded headers. Origins and newly supplied headers
  reuse that result across pages. Storage accepts the verified header without
  another signature check, and checkpoint serving reuses it when checking
  origins. The cache also serves ordinary creation-header storage.
- Cached results contain no credential authority, revocation decision, local
  dictionary IDs or transaction state. Current admission and checkpoint checks,
  page signatures, row commitments, provenance checks and atomic page commits
  remain. A failed transaction can retain only immutable signature results.
  No persisted or wire format changed.

A regression imports multiple pages sharing one creation header and checks that
exactly one header signature was verified. Another warms the cache, rejects a
changed signed header, accepts its correctly re-signed replacement and refuses
an unsupported row format even with a valid signature. Existing checkpoint
corruption, authorization, crash and reopen tests remain applicable.

Measurement artifacts and the temporary probe controllers are under
`/tmp/rocky-sync-opt`. The host probe uses the existing frozen JVM bindings and
two processes over LAN with remote address lookup and relays disabled; local
mDNS remains enabled. Each run copies the
same sealed 100k source fixture into a new source directory and bootstraps a
fresh issuer member, checks its count and generated values (except the fixture's
previously edited `k000001` title), then runs five empty reconciliations.
The previous native build took **22.193 s**, the first optimized run
**21.156 s**. Initial host loads were **8.13** and **45.24** on 18 cores, with
verification/builds overlapping; this pair is preliminary evidence, not an
isolated speedup claim or a phone/relay qualification. Empty host reconciliations
were **43.5–45.9 ms** and **50.6–55.9 ms**, respectively.


### Paired physical-phone probe

`phone-baseline2` and `phone-optimized` use the same source snapshot and a fresh
phone directory each time, on the physical Pixel 11 Pro. Each has one desktop
source and one phone. The phone uses the issuer profile in both runs, unlike
the preceding three-node qualification with a writer phone. Installed
baseline Android native SHA-256 was checked against the pulled APK and matched
`0320810d0bc66577afe940913754301d0cfac947cdd4a640dfa5de4d5d0cfbdf`.
The optimized APK changes only its native library and retains the frozen
bindings' ABI checks. Optimized native hashes:

- Android (stripped): `7cc8e5114cad3fad328501c8c3c3ee81bde14a44a5b9a97811d1f548e32cab70`.
- macOS: `b415cb62440f188f7cc6ae1c035dae6f902bf1a845c611e38510c22cdbb9909e`.

Bootstrap completed in one explicit sync pass in **174.940 s** before and
**123.396 s** after, an observed **29.5% reduction**. Both checked 100,000 rows
and generated values with the same exception for the source's edited title.
Initial host loads were **52.19** and **69.80** on 18 cores; workspace
builds overlapped both runs. One five-second CPU sample overlapped the optimized
import. This is a paired observation with load and device variability, not a
repeated controlled benchmark. The **60 s bootstrap gate remains unmet**. The optimized final reconciliation
report includes one peer-closed-link attempt followed by a successful reconnect
and reconciliation; its elapsed time is included.

Five empty phone LAN reconciliations took **1.103–1.335 s** before and
**1.148–1.262 s** after. On the optimized build, 20 alternating live phone/desktop
row transfers had nearest-rank **p95 273.12 ms**, maximum **300.87 ms**, including
control calls and polling. Both replicas ended readable and writable. This
observes the 1 s one-row gate for this fixture; LAN empty timings do not qualify
the relay gate.

The optimized phone CPU sample recorded **20,228 samples, zero lost**. Its
resolved cumulative caller report has `PublicKey::verify` at **3.53%** versus
**30.94–32.75%** in the previous qualification's samples. `sqlite3_step` is now
**41.20%**; self overhead includes `pread64` **9.69%**, `pwrite` **5.69%** and
`fsync` **5.77%**. These are overlapping sampled cycle percentages, not
wall-clock attribution or a controlled profile pair. Kernel symbol access
remained restricted. The optimized process's post-bootstrap `/proc/<pid>/io`
reported **2,888,527,872 physical bytes written**; this process counter includes
open/join and WAL activity, not just row storage. Remaining storage work is
substantial. The sample, resolved report and matching unstripped ELF are in
`/tmp/rocky-sync-opt`.

The first baseline controller connected to the forwarded port before the phone
listener was ready and received EOF before joining; `phone-baseline` is excluded.
The successful controllers wait for the listener. No shopping application or
household was changed. Relay, 100 ms latency-shaping and shopping UI qualification
were not repeated; their previous unmet or unverified status remains.


### Final verification

- `./scripts/lint`, workspace Clippy with warnings denied and
  `./scripts/commitlint --jj` passed. Release native builds passed for macOS
  and Android; the frozen consumer bindings opened and exercised both artifacts.
- The final `./scripts/check`, with `NEXTEST_TEST_THREADS=8` and the shared host
  Cargo target directory, passed **200 unit tests** and all **1,379 integration
  assertions** (1,075 workspace and 304 SDK without default features). Workspace
  and SDK integration runs took **152.845 s** and **78.338 s**; the checker
  reported **232.45 s**, within the unchanged **300 s** suite budget.
- **The final command exited 1** on three unchanged **2 s** per-test budgets:
  `rocky-sdk node::revocation_tests::reopen_completes_each_revocation_secret_boundary`
  (**2.62 s / 2.22 s** isolated in the two builds);
  `rocky-sdk::tips races::revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner`
  (**2.37 s / 3.03 s**); and
  `rocky-sdk::tips trusted::a_trusted_identity_opens_a_branch_that_devices_on_main_never_see`
  (**2.22 s** in the SDK build). Assertions passed. Only the tips-race overrun
  was already recorded above; the other two overruns are also reported here,
  without attributing them to a cause or claiming a fully green check.
- An earlier full run failed the new fixture's too-small page allowance and
  timed out the existing SDK offline-peer liveness test. The fixture now uses
  the standard 64 KiB allowance; the test build's 16-row page cut still forces
  multiple pages. Its focused run passed in **0.86 s**, and its final workspace
  run in **0.67 s**. The offline-peer test passed a fresh isolated run in
  **0.63 s** and the final two builds in **1.39 s / 1.37 s**; its earlier timeout
  and failed isolated rerun are retained as an intermittent verification issue.
  No existing assertion, tier or budget was weakened.

Final check output is `/tmp/rocky-sync-opt/check-final.log`; the earlier run is
`check.log`, and static lint output is `lint-final.log` in the same directory.
These artifacts remain outside the temporary workspace. This work improves the
measured bootstrap but does not complete phase 8 qualification or produce a
fully green required check.

## Further SQLite experiments (2026-10-25)

Work used Jujutsu workspace `rocky-sync-opt-next` under `~/AgentWorkspaces`,
on top of the verified-header optimization above. Artifacts, controllers and
JSON results remain under `/tmp/rocky-sync-opt-next`. The starting profile is
the matching optimized phone sample above: SQLite accounted for 41.20% of
sampled cycles, with substantial reads, writes and checkpoint work. These
experiments use the same sealed source snapshot, frozen consumer bindings,
fresh synthetic directories and issuer-profile members. They do not change
the shopping application or its household.

### Rejected SQLite cache experiment

Writable connections were tested with a 32 MiB suggested SQLite page cache
and 1,024 prepared statements instead of the default page cache and 128
statements. The original artifact's phone imports took **102.571 s** and
**133.772 s**; the cache variant took **119.843 s**, between those observations.
Post-bootstrap process counters showed bytes read through system calls falling
from **5.975 / 5.993 GB** to **2.922 GB**, but physical writes stayed near
**2.89–2.91 GB**. This did not establish a wall-clock benefit. The cache changes
were removed; no additional SQLite cache allocation is retained.

The cache variant's three-node data checks passed and 20 phone/desktop updates
had p95 **138.90 ms**. Its relay observations were **2.894–23.390 s**. These
results belong to the rejected artifact, not the final code.

### Retained WAL checkpoint thresholds

The secure replica now attempts passive checkpoints at **16 MiB** instead of
4 MiB, and truncating checkpoints at **64 MiB** instead of 16 MiB. This gives
checkpointing more committed pages over which to combine writes. The nominal
truncation threshold grows by 48 MiB per replica; it is not an absolute file
size cap when a transaction overshoots or a reader prevents checkpointing.
Per-page transactions, cursor updates, explicit durable flushes, synchronous
mode and full device-flush settings are unchanged. No stored or wire format
changed.

The sequential two-desktop probes `host-baseline` and `host-wal` completed
fresh 100k bootstraps in **17.605 s / 11.562 s**, an observed **34.3% reduction**.
Both checked every generated row and value, allowing the source fixture's
previously edited `k000001` title. Starting host loads were **9.14 / 8.88** on
18 cores, with no verification build overlapping either probe. Five empty
reconciliations were **36.0–38.4 ms / 35.8–37.8 ms**. This is one host pair,
not repeated device qualification.

`phone-wal-retry-three-relay` used the physical Pixel 11 Pro and the final WAL
artifact. Its phone bootstrap took **107.029 s**, within the original
artifact's **102.571–133.772 s** range. This does not establish a phone
wall-clock speedup, and the **60 s gate remains unmet**. Its post-bootstrap
`/proc/<pid>/io` counter reported **2,472,243,200 physical bytes written**,
versus **2,909,671,424 / 2,910,425,088** in the two baseline runs: about
**15% fewer writes**. These process counters include open/join and WAL work,
not only checkpoint row storage. Starting host loads for the baseline,
baseline repeat and WAL runs were **17.73 / 6.34 / 11.30**; builds did not
overlap their measurements. Device and network variability remain.

All 100,000 rows and generated values were checked on the source and phone.
Twenty alternating automatic row transfers had nearest-rank **p95 130.24 ms**,
maximum **130.59 ms**, observing the **1 s p95 gate** on this fixture. Five
empty phone LAN reconciliations took **1.145–1.450 s**.

A second desktop then joined and checked all rows, values and the latest
edited title. Its bootstrap took **14.551 s**, including peer-closed-link
attempts against another peer; that peer remained marked for retry in its
report. The data checks passed and all three replicas ended readable and
writable. Public node IDs:

- Source: `c7bd0b99ff6f59e0e420195a479b9b09d5f16171e73ae614d218b873921959b0`.
- Phone: `78739251608c2773d623617f94cecf5ee05b92394c8a401e3e4f36a3578c8929`.
- Second desktop: `cf3ce5ac96893d04c6935538a09736e1d565882225a45a7937a7920e60729730`.

The same three replicas were reopened with loopback-only endpoint bindings
and local/topic discovery disabled. The phone reported a connected relay at
`https://euc1-1.relay.n0.iroh.link./`. Five empty reconciliations took
**7.395, 6.587, 2.860, 8.258 and 4.301 s**. They transferred no rows and
reported no refusal, waiting scope or timeout; three reports included
peer-closed-link attempts, whose elapsed times are included. The **2 s relay
gate remains unmet**. All three replicas again ended readable and writable.

The first WAL probe, `phone-wal-three-relay`, lost its control connection
before returning a bootstrap timing. Its ADB control forward was absent
afterward. It is excluded from timing comparisons; the cause was not
established. Restoring the forward and using a fresh directory made the retry
above complete.

Final native SHA-256 values; the Android library was checked against the installed APK:

- Android, stripped: `139705410c6d25797831ac95856619e0b483e3c9ed4b893c41355713c3bbea36`.
- macOS: `7e56f31ec005654169c6047552763ead0c4077c40ee0409712a172edb5b79a35`.

The final artifacts are in `/tmp/rocky-sync-opt-next/wal`; the rejected cache
artifacts remain in its parent directory. The **100 ms latency-shaping** and
**shopping UI** checks were not repeated and remain unverified. This work
reduces measured checkpoint I/O and host import time; it does not qualify
phase 8.

### Final verification of the retained thresholds

- `./scripts/lint`, `./scripts/commitlint --jj`, both release native builds
  and workspace Clippy with warnings denied passed.
- `./scripts/check --build` created workspace and SDK-without-defaults
  archives; `./scripts/check --from` ran the full checks from those artifacts,
  with eight nextest workers. All **200 unit tests** and **1,379 integration
  assertions** passed. Workspace and SDK integration runs took **156.853 s**
  and **85.658 s**; the checker reported **255.84 s**, within **300 s**.
- **The final check exited 1** on unchanged **2 s** per-test budgets:
  `rocky-sdk node::revocation_tests::reopen_completes_each_revocation_secret_boundary`
  (**2.24 s / 2.13 s** isolated in the two builds);
  `rocky-sdk::tips races::revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner`
  (**2.26 s / 2.48 s**); and
  `rocky-sdk::concurrent_rotation two_revoked_list_readers_open_the_opposite_sibling_but_not_the_superseding_rows`
  (**2.30 s** in the SDK build). Assertions passed. The first two overruns
  were already recorded above; the concurrent-rotation overrun was not.
  No assertion, tier or budget was changed, and no cause is attributed to the
  new overrun. This is not a fully green required check.

Final build and check logs are `wal/check-build.log` and `wal/check-final.log`
under `/tmp/rocky-sync-opt-next`. The earlier cache experiment's full check
is `check.log`; it also passed all assertions but exceeded the previously
recorded revocation/reopen and tips-race budgets. All artifacts remain outside
the temporary workspace.

## Page-batched checkpoint row storage (2026-10-25)

Work used Jujutsu workspace `rocky-sync-opt-20261025` under
`~/AgentWorkspaces`, based on the retained WAL thresholds above. Raw logs,
controllers, native libraries and fresh synthetic databases are in
`/tmp/rocky-sync-opt-20261025` on the execution host.

Checkpoint import now checks for existing rows once per page and inserts
encoded rows in bounded multi-row statements. Direct imports use the already
resolved integer scope ID to write `secure_rows`, retaining its normal row
triggers, rather than resolving the scope through the writable view for
every row. Both direct and staged imports batch the checkpoint row copy.
The maximum is 256 rows (768 bound parameters) per insert. Row dictionaries,
creation clocks, provenance, partition and identity checks, projections and
page cursors remain in the same transaction. Signatures, durability settings,
wire and stored formats are unchanged.

A regression injects an existing row at the end of an incoming page, checks
that the page is refused and compares every authoritative SQL table before
and after refusal. Existing checkpoint fault, process-exit, resume and
concurrent-write checks exercise page atomicity and recovery.

### Verification

- `./scripts/lint`, `./scripts/commitlint --jj`, workspace Clippy with
  warnings denied and both release native builds passed.
- The focused checkpoint run passed 75 tests (one ignored measurement).
  After correcting the new regression's privileged injection to copy valid
  stored record bytes, its isolated nextest run passed in **265 ms**. The
  final full run also passed it in **834 ms**.
- The final `./scripts/check` used eight nextest workers and passed all
  **200 unit** and **1,380 integration** tests, including the SDK
  without default features. Integration runs took **166.606 s / 134.576 s**.
- **The check exited 1**: the checker measured **303.07 s** against the
  **300 s** integration suite budget. The unchanged **2 s** test budgets
  also rejected the tips race
  `revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner`
  (**2.09 s / 2.41 s** isolated, scaled by host load) and concurrent rotation
  `two_revoked_list_readers_open_the_opposite_sibling_but_not_the_superseding_rows`
  (**2.20 s**). These tests' overruns were already recorded above; their
  assertions passed. No budget or tier was changed. Other Rocky compilation
  was active on the shared host. This is not a fully green required check.

Final verification logs are `check-final-fixed.log`, `lint-verified.log`,
`commitlint.log` and `duplicate-regression.log` under the artifact directory.
The earlier `check-verified.log` contains the invalid regression-fixture
injection and was stopped during redundant timing reruns after the corrected
full check began; it is diagnostic evidence, not the final result.

### Host observations

The frozen JVM consumer, sealed 100k source snapshot, schema and value checks
are the same as the preceding WAL experiment. Each probe cloned that source
and joined a fresh member, checking all 100,000 rows and every generated
value on both replicas. The source's previously edited `k000001` title remains
allowed. No verification from this workspace overlapped these measurements.
Other work in the recovery workspace was running CPU-intensive tests.

The first baseline, before the verification builds, took **11.121 s**, with
host load **11.17** on 18 cores. After verification, the adjacent baseline /
batch pair took **30.620 s / 21.260 s**, starting at loads **29.80 / 41.00**.
The reverse-order batch / baseline pair took **17.609 s / 38.804 s**, at
**26.95 / 25.01**. Both adjacent pairs favor batching, but the baseline spread
shows that these are shared-host observations, not an isolated causal speedup
or phone qualification. Five empty reconciliations per later run ranged from
**47.5–256.7 ms** with batching and **54.6–126.7 ms** without it. The first
baseline's empty reconciliations were **35.2–38.1 ms**.

### Phone observations and qualification limits

The physical Pixel 11 Pro (`66121FDKX001MF`) lost its USB connection before
the first baseline began. A wireless-ADB retry was interrupted by the user's
phone restart before returning a bootstrap timing. Both attempts are excluded.
After reconnecting to the same verified hardware serial over wireless ADB,
`phone-baseline-reboot` completed a fresh 100k import in **65.047 s**, checked
all rows and generated values, and passed 20 alternating automatic updates
with nearest-rank **p95 308.67 ms**. Five empty LAN reconciliations took
**529.2–601.4 ms**; both replicas ended readable and writable.

The candidate `phone-batch-three-relay` returned a fresh bootstrap time of
**107.247 s**. This is slower than the immediately preceding post-reboot
baseline, and does **not** demonstrate a phone improvement. The **60 s gate
remains unmet**. Shared-host observations favor batching, but neither these
runs nor the earlier baseline spread establish an isolated device speedup.
No further optimization was attempted after the owner's request to finish.

The candidate native libraries were built from the retained source and the
installed APK's library was checked byte-for-byte. SHA-256 values:

- Android (`--strip-debug`): `c39247b141d6d26dcd310c07699764859a6a10a0f69f6e6ab0cd47e748ae9e87`.
- macOS: `5ea459c521626dad0814c8cb3395fb6a1854453f65a111643dae15acae98f41c`.

Both phone runs use the wireless control bridge; their one-row timings
include that bridge and read polling. All data belongs to fresh synthetic
fixtures and `org.example.rocky.load100k`, not the shopping household. The
**100 ms latency-shaping** and **three-device shopping UI** checks were not
repeated and remain unverified. Phase 8 is not qualified by this work.

The candidate probe completed successfully after the merge. It checked all
100,000 rows and generated values, passed 20 alternating phone/desktop
updates with nearest-rank **p95 150.68 ms**, maximum **176.69 ms**, and measured
five empty LAN reconciliations at **477.8–592.1 ms**. The second host JVM
replica bootstrapped in **12.308 s**, checked every row/value and the latest
edited title, and reported no retry-pending peer, refusal or timeout. All
three replicas ended readable and writable.

Public node IDs for the candidate probe:

- Source: `c7bd0b99ff6f59e0e420195a479b9b09d5f16171e73ae614d218b873921959b0`.
- Phone: `99fbfb1485fa9f730e66366e16df872e5896ae1efe8410d38595a74d87caac72`.
- Second host JVM: `68c0008851a8fa53be6c6f42b78b527da0b672dd034b428cb405be4a6274825d`.

Reopening the same three replicas with loopback-only endpoint bindings and
local/topic discovery disabled connected the phone to
`https://euc1-1.relay.n0.iroh.link./`. Five empty reconciliations took
**9.202, 2.454, 2.552, 2.412 and 8.816 s**, with no row transfers. These
include all attempts in each reconciliation; the **2 s relay gate remains
unmet**. All three again ended readable and writable. The one-row **1 s p95**
gate was observed on the candidate; the bootstrap and relay gates were not.

The retained changes were squashed into the original working change, and
Jujutsu workspace `rocky-sync-opt-20261025` and its directory were removed.
Artifacts remain in `/tmp/rocky-sync-opt-20261025` independently of that
workspace. No further optimization was made after the owner's stop request.


## Page-batched creation stubs (2026-10-26)

Work uses Jujutsu workspace `rocky-sync-opt` under `~/AgentWorkspaces`,
based on `02b50e55`, preserving the main checkout's pending checkpoint-write
optimizations. Artifacts are in `/tmp/rocky-sync-opt-20261026` independently
of the temporary workspace.

The starting profile is the matching optimized phone sample in
`/tmp/rocky-sync-opt`, summarized above: `sqlite3_step` accounts for 41.20%
of sampled cycles. Inspection found that every imported row repeated a
creation-stub lookup and, for a direct import, a stub upsert, including rows
sharing one creation operation. Import now collects distinct creations per
page, checks their clocks against one another and the previously staged
stubs with one bounded query, and batches the staging and direct stub writes.
The page's creation set counts only newly seen operations. At most 256 rows
enter a production page, bounding inserts to 768 parameters. Row checks,
projection, stubs and cursor remain in the same transaction. Durability,
credentials, signatures and stored/wire formats are unchanged.

The regression uses rows sharing one creation across multiple pages, for
both direct and staged imports. Pages signed by the legitimate source with
conflicting clocks within a page or against a preceding page are refused;
every authoritative SQL table is compared before and after refusal. Valid
pages still finish, audit and retain exactly one checkpoint creation stub.


### Verification

- `./scripts/lint`, `./scripts/commitlint --jj`, workspace Clippy with warnings
  denied, and release native builds for macOS and Android passed. The installed
  load-probe APK's library was pulled and checked byte-for-byte after each
  installation. Frozen consumer ABI checks passed for both builds.
- The final `./scripts/check`, using eight nextest workers, passed **200 unit**
  and **1,381 integration assertions**: **1,077** workspace and **304** SDK
  without default features. The new regression passed in **0.771 s**.
  Integration runs took **149.684 s / 79.958 s**; the checker reported
  **230.81 s**, within the unchanged **300 s** suite budget.
- **The required check exited 1** on unchanged **2 s** per-test budgets:
  `rocky-sdk::service prerequisites::real_voters_finalize_an_owner_seal_after_a_trusted_write`
  took **4.03 s** isolated, and
  `rocky-sdk::tips races::revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner`
  took **2.22 s / 2.21 s** in the two builds. Assertions passed. The tips-race
  overrun was previously recorded; no cause is attributed to the service
  overrun. No assertion, tier or budget was changed. This is not a fully green
  required check. Full output and tier reports are in `check.log` and
  `test-tiers-final/` under the artifact directory.

### Device observations and limits

Both builds used the same frozen JVM/Android consumer, sealed synthetic 100k
source snapshot and schema as the preceding experiments. Each import joined a
fresh directory and checked all 100,000 keys and generated values, allowing the
source fixture's previously edited `k000001` title. No verification build or
host benchmark overlapped the phone measurements. Wireless ADB controlled the
physical Pixel 11 Pro (`66121FDKX001MF`); the source and joining phone both used
the issuer profile. The first phone attempt reused an older fixture path and
was refused before import; it is excluded. Successful runs use date-specific
fresh paths.

The adjacent phone baseline / candidate bootstraps took **169.213 s /
158.185 s**, an observed **6.5%** decrease in one pair. This does not establish
an isolated causal improvement given the previously recorded device spread;
the **60 s gate remains unmet**. Post-bootstrap process counters were
**2,473,472,000 / 2,475,184,128 physical bytes written**, including open/join.
The change does not demonstrate reduced physical I/O.

Each run passed 20 alternating automatic phone/desktop updates. Nearest-rank
p95 was **155.72 ms / 160.01 ms**; the candidate maximum was **186.84 ms**.
These timings include the wireless control bridge and read polling. Five empty
LAN reconciliations took **581.7–684.8 ms / 537.0–570.4 ms**. The candidate's
second host JVM joined in **11.705 s**, checked every row/value and the latest
edited title, and all three replicas ended readable and writable.

The candidate then reopened those three replicas on loopback-only endpoint
bindings with local/topic discovery disabled. All five empty relay attempts
are included: **10.058, 2.450, 6.753, 2.691 and 2.663 s**. All three remained
readable and writable. The **2 s relay gate remains unmet**. The one-row p95
gate was observed for this fixture; the **100 ms per-hop catch-up** and
**three-device shopping UI** checks were not repeated and remain unverified.
This work does not qualify phase 8 and does not change the shopping household.

Candidate public node IDs:

- Source: `c7bd0b99ff6f59e0e420195a479b9b09d5f16171e73ae614d218b873921959b0`.
- Phone: `59f82c3a3394ec7d5cae9e4dc755331eb45c7e2d340eef60e33db7d7eefb17fd`.
- Second host JVM: `11bb0b95efe1c0ad7d8d679105258a6a38ba92c126bccc6e0bc70db40e332e19`.

Candidate native SHA-256 values:

- Android (`--strip-debug`): `485fdb745459072b95052fc24e608a0db48da07ae58c3b9bc8afd790f4d8e939`.
- macOS: `6e8dc1e666f666f717d328471c7eef552d3d68bea7f874fa6da4e21662b8c1d3`.


### Host observations

The clean baseline / candidate pair took **10.545 s / 10.711 s**. The
reverse-order candidate / baseline pair took **11.567 s / 10.700 s**. Both
checked every generated row and value, without overlapping verification or
phone runs. Starting host loads were **3.14 / 3.42 / 2.98 / 5.52** on 18
cores. These pairs do **not** demonstrate a host speedup. Five empty host
reconciliations per run ranged from **33.5–37.0 ms**. An earlier exploratory
baseline took **11.769 s** while test compilation was active and is excluded
from the clean comparison.

The retained change reduces repeated SQL statements, with clock-conflict and
atomicity coverage; these runs do not establish a causal wall-clock benefit.
The device observation is one pair and the host pairs did not favor batching.
The substantial bootstrap and relay qualification gaps remain explicit.


The retained edits were squashed into the original working change
`lqnpvvoy`, preserving its preceding checkpoint optimizations. File hashes
were checked after the merge. Jujutsu workspace `rocky-sync-opt` and its
directory were removed; artifacts remain under `/tmp/rocky-sync-opt-20261026`.


## Four-page commit experiment (2026-10-26)

Jujutsu workspace `sync-quota` (`~/AgentWorkspaces/rocky-sync-quota`)
started from working change `lqnpvvoy` (`79b848d5`), preserving the preceding
checkpoint optimizations. The candidate collected each existing four-page
network window, verified its pages off the replica lock, then stored the
window in one transaction. It retained ordered cursors and resolved creation
headers supplied by earlier pages within the window. The hypothesis was
fewer repeated WAL writes of index pages across page commits; no format or
durability setting changed.

The host baseline / candidate pair imported the same sealed synthetic 100k
source into fresh members, checking every key and generated value. Bootstrap
took **13.653 s / 22.017 s**, starting at host loads **29.18 / 41.39** on
18 cores. Five empty reconciliations ranged **38.4–43.6 ms / 51.3–61.0 ms**.
Verification and other phase work shared the host. This pair does not show
a benefit and is not an isolated causal comparison.

The installed phone baseline library matched the preceding candidate's
SHA-256 (`485fdb745459072b95052fc24e608a0db48da07ae58c3b9bc8afd790f4d8e939`).
The fresh baseline import started on the physical Pixel 11 Pro, but its
control connection closed without a protocol result. No bootstrap timing
or row verification was returned, so that attempt is excluded. The new
candidate was not installed or measured on the phone. No physical-I/O
comparison, relay qualification, latency-shaping or shopping UI check was
completed in this experiment. The shopping household was untouched.

Static lint, workspace Clippy with warnings denied, and release native
builds for macOS and Android passed for the candidate. Frozen JVM consumer
ABI checks and both host probes passed. The candidate's full check was
stopped during the second unit-tier feature build when the experiment was
rejected; it is not a completed test result. Its new rollback/reopen
regression is retained only in the saved experimental patch.

**The candidate code was discarded.** It did not demonstrate a speedup,
and there is no evidence it can halve bootstrap time. Phase 8 remains
unqualified. Further storage optimization is deferred to phase 9, after
phase 4's recovery contracts land; base/overlay storage, compact indexes
and page-cache tuning directly address the remaining storage cost.

Source patch, logs and native artifacts remain under
`/tmp/rocky-sync-quota-20261026`, independently of the temporary workspace.

Final retained change: this evidence entry only. `./scripts/lint` and
`./scripts/commitlint --jj` passed. A fresh `./scripts/check` passed Clippy
for the retained source, then was stopped during test compilation amid
concurrent phase builds. Neither full-check attempt completed; no green
unit/integration result is claimed for this experiment.

The evidence entry was integrated back into `lqnpvvoy`; workspace
`sync-quota` and its directory were removed. Earlier checkpoint optimizations
and the independent recovery and lighter-replica workspaces were preserved.


## Checkpoint row-index batching experiment (2026-10-27)

Jujutsu workspace `pipelined-perf` under `~/AgentWorkspaces` started from
working change `wuxmrmrs` (`19d49e85`), including its pending Rust SDK work.
Artifacts are retained in `/tmp/rocky-pipelined-perf-20261027` independently
of that workspace.

The starting hypothesis used the historical matching phone sample above, with
`sqlite3_step` at 41.20% of sampled cycles; it is not a profile of the current
v39 build. Inspection found two more index
statements per directly imported row: row-key upsert and move-marker
maintenance. The candidate batched those updates within the existing page
transaction and memoized the serving page's first-reference header lookups.
It preserved authority, signatures, row validation, page ordering, commit
boundaries, durability settings and all persisted/wire formats.

### Host measurements

A temporary release-mode probe expanded the existing checkpoint import
fixture to 100,000 rows. Its test page cap matched production's 256 rows;
its source capture allowance and transaction timeout were explicitly enlarged
for fixture creation only. The probe timed serving and page staging, plus total transfer through the final
switch, excluding source creation. Every run compared all source/target
rows through paginated reads and passed a full audit. These are in-process
host observations without network latency, using fresh equivalent logical
fixtures with newly generated identities, not device or end-to-end sync gates.

| Order | Build | Pages | Serving | Page staging | Transfer and final switch |
| --- | --- | --- | --- | --- | --- |
| First pair, 1 | Baseline | 765 | 2.578 s | 6.322 s | 10.175 s |
| First pair, 2 | Candidate | 766 | 2.470 s | 7.516 s | 11.252 s |
| Reverse pair, 1 | Candidate | 766 | 2.473 s | 7.495 s | 11.253 s |
| Reverse pair, 2 | Baseline | 766 | 2.568 s | 6.303 s | 10.148 s |

No verification build overlapped these four runs. A five-second macOS sample
on the first baseline captured source fixture creation, outside its timed
transfer interval; it is saved as `baseline.sample` and is not an import
profile. Starting host loads for the reverse pair were 8.28 and 3.36 on
18 cores. The small serving decrease did not offset the import regression:
the candidate was approximately **11% slower overall** and **19% slower in
page staging** in both orderings. This does not demonstrate a path toward
halving phone bootstrap time.

**The candidate was discarded.** No runtime or regression-test edit is
retained. The experimental patch, probe source snapshots, release binaries
and logs remain in the artifact directory. All temporary capture, timeout,
page-size and instrumentation edits were removed; normal tests and budgets
remain unchanged. The candidate release probes passed their data and audit
checks. A new rollback assertion initially selected an authoring-only fault
hook and failed to inject an import error; its corrected checkpoint-hook
version is saved in the experimental patch but was not rerun before rejection.
This is not a claim that the discarded candidate passed all required checks.

### Qualification limits

The connected physical Pixel was discovered, but the saved device fixture
uses `rocky.encrypted-replica.v37`; current source requires v39. That fixture
was inspected read-only and neither migrated nor opened with the candidate.
No new phone, relay, 100 ms latency-shaping or three-device shopping UI run
was made. Those gates remain unverified by this experiment; phase 8 remains
unqualified. The shopping household was untouched.

Final retained changes consist only of this evidence entry, squashed into
`wuxmrmrs` while preserving its existing SDK edits. Comparison against the
starting working tree confirms that only this evidence file changed.
Jujutsu workspace `pipelined-perf` and its directory were removed; the
independent lighter-replica workspace was preserved.

`./scripts/lint`, `./scripts/commitlint --jj` and workspace Clippy with warnings
denied passed on the retained source. The final `./scripts/check` exited 0:
all **203 unit tests** and **1,419 integration tests** passed (1,104 workspace
and 315 SDK without default features). Integration runs took **154.929 s**
and **81.755 s**; the checker reported **237.76 s**, within the unchanged
**300 s** suite budget. Its isolated slow-case reruns also passed their
unchanged per-test budgets. Final check output is `check-final.log` in the
artifact directory. This green source check does not qualify any device,
relay or latency-shaping performance gate.
