# Phase 2 evidence

Status: complete (2026-10-23). Every phase-2 gate passed. Work stops at phase 2.

## Implementation

Range pages carry optional `StateAdvice` under `rocky.link.v15`. Advice shares
bootstrap eligibility, covers every named requester log, and requires at least
64 retained covered frames, excluding void positions. Frames and mandatory
`behind` handling remain intact, including serving retained ciphertext without
read keys. Checkpoint, frame and replica formats are unchanged by phase 2.

The SDK imports the exact advised offer, defers to another signer's active
staged import, and suppresses optional imports after three failures for that
scope/checkpoint in this process. Fallback uses transactional staging undo and
settlement; mandatory rebootstrap remains mandatory. Tests verify local writes
committed after the frontier request are reapplied and converge.

The issuer closes a published migration with a same-schema continue, bookmark
move, finalization, compaction, existence signing and checkpoint listing.
Migration status clears only after listing. Its retained own checkpoint record
is the completion journal, including after later schema changes.

Final-seal maintenance runs at sync start, after exchanges and after local
statement publication. Trusted and member devices compact within one pass,
including members without a composed log. Nonfinal compaction remains explicit
except migration completion. The real-voter rollback fixture verifies automatic
compaction and a nonempty pruned prefix before restoration.

Successful writability and resolved seal paths share the graph cache's context
invalidation. The writability key includes domain, exact tip and followed
bookmark; hits still require an enrolled node. Credential/revocation and live
workspace-base checks still run per frame. Graph loading, statement admission,
revocation resolution, workspace/schema changes and rollback invalidate cached
facts. Tests compare paths with uncached traversal across switches, failed
admission, same-size revocation replacements and graph reload, and refuse a
warmed writer after bookmark deletion without changing rows.

Credential schema traversal builds a successor lookup once per resolution,
preserving first-match order, winner checks, errors and cycle rejection.
Bookmark resolution reuses its selected target. Canonical tips return after
successful active ancestry validation, which also proves structural ancestry;
losing tips keep the existing resolution. Finality/revocation tests check both
winning and losing continuations before and after revocation.

## Gates and measurement boundaries

The history fixture authors 50,000 real rows through one seed migration. Its
98 historical seals plus migration and closing seals give 100 seals. It keeps
older listed bases, imports the newest closing base, verifies every row and
both files' exact base, reports one member sync and no covered frames. Empty
historical states are explicitly compacted before authoring; history creation
and compaction remain timed inside the complete gate.

Both 100k builds retain the full seed migration, additive schema switch and
fill migration. A separate publication peer durably acknowledges bundles before
the returning member is measured. Publication includes that peer's enrollment
and transfer; closing compaction is measured separately. Member opening is
reported separately from catch-up. Complete test times include setup, closing,
verification and every other cost outside individual stage timers. No budget,
timeout or exemption was added or relaxed.

The integration regressions also cover one-page/no-advice behavior, exact
coverage and unknown-log refusal, failed-offer matching, staged-signer deferral,
three-failure scope/checkpoint isolation, local-write reapplication, automatic
final compaction and explicit nonfinal compaction.

## Final qualification

- `./scripts/lint`: passed, including eight lint-script tests.
- `./scripts/check`: formatting and Clippy passed; 177 unit tests in 94.2 ms
  against 10 s; 1,279 integration tests (1,002 workspace, 277 SDK without
  defaults) in 192.55 s against 300 s.
- Targeted secure frontier/advice checks: 18 passed. Credential tests: 72 passed.
  The final full Rust checks include the updated regressions.
- `NEXTEST_SUCCESS_OUTPUT=immediate NEXTEST_NO_OUTPUT_INDENT=1 ./scripts/check --e2e`:
  **78 passed** (53 workspace, 25 SDK without defaults). Regular wall time
  **599.00 s against 900 s**, at load factor 1.0. The two pre-existing 100k
  exemptions added **898.22 s**; both retained their 600 s timeout. Total tier
  elapsed was **1,497.22 s**, not 599 s. Invocation elapsed including Cargo/list
  overhead was approximately **1,499.85 s**.
- `sdks/kotlin/gradlew -p sdks/kotlin check`: passed in **4m27s**; 202 tasks,
  47 executed and 155 up-to-date. JVM, packaged JVM, Room, Android lint and
  iOS simulator checks passed.
- `./examples/shoppinglist-app/check`: required-schema qualification, **25
  Android-target tests**, **61 JVM tests**, desktop build and Android build passed.
- `scripts/commitlint --jj`: passed. Edits remain in change `nnrprlwq`, with
  parent `2ec156e9`; no push, bookmark or separate commit was created.

## Final scale measurements

[Exact stage times, checkpoint identities, accounting and complete 100k member
`dbstat` output](02-states-as-default-catch-up-measurements.txt) retain every
object's allocated bytes, payload bytes and page count: **182 objects per
100k build, 364 total**. Main-file lengths and live `dbstat` page totals are
reported separately; no vacuum or storage-normalization step is applied.

### 100,000 rows: Default features

- Seed open: **40.193287833s**; additive open: **25.893813583s**;
  fill open: **119.356371834s**.
- Publication: **157.20981775s**; closing compaction: **85.696323792s**.
- Member opening: **30.368875ms**; catch-up: **17.061114333s**,
  below 120 s; **one sync**, exact closing checkpoint and no covered frames.
- Complete gate: **450.974s**. Member main file: **534,945,792 bytes**.
  Live `dbstat`: **536,002,560 allocated bytes**, **422,799,687 payload
  bytes**, **130,860 pages** across all **182 objects**.
- Additive switch: **8 SQLite changes**, equal to the
  eight-row reference; used data grew only **12,288 bytes**.

Both issuer and member hold:

```text
seal       a11b07e7a9812b26b0468575aa0e32fd7118137b5c05aaa50df20900f08ef80f
checkpoint 47816ebac188601fe749ed03b7b7d9a3a12dafb598eb72040037463d8944ff2d
```

### 100,000 rows: SDK without default features

- Seed open: **38.02241375s**; additive open: **24.148841667s**;
  fill open: **114.29983925s**.
- Publication: **163.609079666s**; closing compaction: **83.32603675s**.
- Member opening: **29.344208ms**; catch-up: **17.137730959s**,
  below 120 s; **one sync**, exact closing checkpoint and no covered frames.
- Complete gate: **446.004s**. Member main file: **535,433,216 bytes**.
  Live `dbstat`: **536,014,848 allocated bytes**, **420,799,536 payload
  bytes**, **130,863 pages** across all **182 objects**.
- Additive switch: **8 SQLite changes**, equal to the
  eight-row reference; used data grew only **12,288 bytes**.

Both issuer and member hold:

```text
seal       8626cbe77ce7243a39764840e4076577930b07924e93a7d5b5b9c6a7160c7e92
checkpoint 67b320979c90c7d5874cc977279369ba3b0f17cb9ca945a3e1a4652c878b6faf
```

### 50,000 rows and 100 seals

- **Default features**: complete gate **240.039s**; history **7.21837675s**;
  seed **64.943879333s**; publisher reopen **11.225668667s**;
  publication **126.953695375s**; closing compaction **19.235549333s**;
  member opening **28.361417ms**; catch-up **7.992553958s**.
  One sync, newest listed base, exact base in both files and zero covered frames.

- **SDK without defaults**: complete gate **187.581s**; history **5.377727167s**;
  seed **34.398047959s**; publisher reopen **8.145332125s**;
  publication **109.318922041s**; closing compaction **19.354124167s**;
  member opening **23.306709ms**; catch-up **8.018409125s**.
  One sync, newest listed base, exact base in both files and zero covered frames.

## Investigation and limits

The first 50k attempt timed out at 600 s before seed publication; a CPU sample
attributed 1,201/1,406 samples to repeated writability/bookmark resolution.
Another sample attributed 363/612 active samples to workspace-base checks,
including 311 rebuilding the same sealed path. The caches above retain only
graph facts; database readiness and credential checks remain live.

A later standalone run still timed out at 600.035 s in publication-peer
enrollment (history 70.868 s, seed 71.121 s, additive 47.478 s, fill 133.759 s).
Graph traversal improvements subsequently passed the standalone history gate
in 386.162 s. Stopped investigation runs are not counted as full-suite passes.

An intermediate full run passed all 78 cases functionally but missed the
regular budget: **1,035.87 s regular**, **915.94 s exempt**, **1,951.81 s total**.
The history fixture had unnecessarily reused the entire two-migration program,
authoring 100k row changes for its 50k state. Focusing it on its specified one
50k-row population retained every required row/seal/base/frame assertion and
all costs inside its clock. Both 100k programs stayed complete. The final run
above passes the original 900 s budget without another exemption.

During early overlapping checks, shopping's coordinated decision returned
`Undecided` and a seal test read during required rebootstrap. Both classes
passed a focused four-test rerun, and full serialized shopping runs passed.
An overlapping Rust integration run was stopped and was not counted as a pass.
Final qualification ran serially.

Host: macOS 27.0.1 (26A434), Apple M5 Pro, 18 cores, 48 GiB RAM, Rust 1.99.0.
The Android SDK and booted `rocky-p21` iOS simulator were available. Generated
binding annotation, Gradle deprecation and the existing `block` future-Rust
warnings remain. Physical devices, Linux and Windows were not exercised here.
There is no remaining phase-2 blocker. Publishing/compaction costs and the
current row representation remain visible in these measurements; later phases
were not implemented.
