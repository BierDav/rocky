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
