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
- Static lint and workspace Clippy passed. Final full verification is running.
- The full run reproduced the phase 6 baseline failure
  `tests::tips::frontiers::headers::excluded_and_other_domain_headers_still_decode_after_warming`
  (expected rejection “invalid secure frame”, got success). It is not waived.
