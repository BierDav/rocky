# Phase 4 implementation evidence

Status: complete and qualified on 2026-10-23. These are the historical phase 4
v34 measurements; subsequent v35 qualification is recorded in the
[phase 3 evidence](03-incremental-finalization-evidence.md).

## Representation and invariants

Source-schema ordinals expand to name identities before grants, path filtering,
admission or projection. Remote reads require the exact installed source schema.
Stamps carry lamport, numeric author and domain author sequence. Identity and
signer attribution retain keys; the exact used signer set enters row commitments.

Ordinary births derive the canonical creation statement from their signed frame.
The frame binds its birth commitments. Carries retain the original signed header
and author bindings, shared between rows without changing their provenance.
Replay that mints a restored row retains those witnesses atomically; audit checks
their presence, digests and signatures without allocating or repairing state.

Field-name ids are never reused. Stored rows, kept views, overlay, scratch,
rebuild, audit and projection share the record codec. File-local scope, origin,
creation and signer-set ids expand before canonical commitments. Checkpoint
pages have exact-use field, origin and creation legends; origin encoding shares
manifest and binding digests. Missing, duplicate, unordered or unused references
are refused before mutation. Signed page cuts survive resume and forwarding;
each original birth header is transmitted at its first stream reference.

Bundle member ciphertexts use byte strings. Their compact plaintext omits the
scope, which is authenticated in the member AAD along with transaction, index
and count. The same secret in two scopes cannot interchange openings. All
opaque member slots remain present and atomic admission is unchanged.

Current formats are **row 12, checkpoint 17, replica 34, link 16**. Other
versions are refused; there is no migration, backfill or alternate encoding.
TypedRows and native boundaries exchange SQL columns and names.

## Checks on the fixed candidate

| Check | Result |
| --- | --- |
| `./scripts/lint` | passed |
| `./scripts/check` | passed Clippy, 180 unit and 1,283 integration tests |
| Integration tier | 195.39 s / unchanged 300 s budget |
| `./scripts/check --e2e` | 78 passed, including both complete 100k programs |
| Regular e2e suite clock | 526.49 s / unchanged 900 s budget |
| Existing 100k exemptions | additional 596.16 s; total elapsed **1,122.65 s** |
| `sdks/native/check` | passed formatting, Clippy/bindgen build and no-default check; zero native unit tests |
| `sdks/native/check-ios` | passed all four device/simulator builds, with and without iroh |
| Kotlin `gradlew check` | passed in 3m 34s; 202 tasks, 47 executed and 155 up-to-date |
| Shopping application `check` | passed required-schema qualification, 25 Android-target and 61 JVM tests, desktop and Android builds |
| Independent checkpoint vectors | Python verifier passed, 28 fixture files |

Validation ran on macOS arm64, with JVM and Android-target tests and iOS arm64
simulator tests. The iOS device target was compiled. Physical-device runtime,
Linux and Windows were not qualified. The packaged JVM consumer exercises
schemas, TypedRows, row moves, credentials, restoration, generation reopen and
owned/borrowed networks through packaged artifacts without a Rust consumer.
There are no outstanding phase 4 blockers.

The targeted restoration, malformed-legend, resumed/forwarded-header and origin
codec regressions all passed. The source remained fixed for final qualification.
Full suites include wrong carry proofs, tampered chain/birth commitments,
receipts, restoration, consumed and filtered births, source-schema refusal,
remove/re-add convergence, rollback, durability and supported feature variants.
The malformed legend tests use actual signed origins and compare authoritative
state before and after refusal. A real 64-frame bundle passes the body budget;
scope substitution with a shared encryption secret is refused.

The 32-row SQL/TypedRows fixture has seven fields and exactly 50 encoded value
bytes per row. Its final dedicated run measured **3,586 B encrypted body**,
**5,360 B signed header plus body** (**167.5 B/row**) and **181 B maximum stored
record**, below 500 B wire and 300 B record gates. Its 32-row carried body is
**13,406 B plaintext** (13,446 B including encryption overhead); expansion
preserves every original proof, stamp and signer mapping exactly.

## Complete 100k programs

Both programs retain the seed and fill migrations, additive switch, durable
publication to a separate peer before member measurement, closing compaction,
one-sync/exact-closing-base assertions, no covered-frame replay, ordinary and
migration retention accounting, and every-row value checks. The 60 s member
assertion includes open plus catch-up. Hub counters include actual traffic in
both directions. No budgets or existing exemptions changed.

All sizes are bytes. The [raw measurements](04-compact-row-representation-measurements.txt)
include every member `dbstat` object (page sizes, payload and page counts), main
and WAL lengths, source file lengths, frame sums, stage times and transfer costs
for both 100k programs and both 50k/100-seal gates.

| Measurement | Workspace/default | SDK/no-default |
| --- | ---: | ---: |
| Complete program | 303.805 s | 290.978 s |
| Seed open | 35.709486292 s | 35.353645875 s |
| Additive open | 9.608777166 s | 8.922225625 s |
| Fill open | 97.769197875 s | 92.378128375 s |
| Durable publication | 71.020155917 s | 63.234941459 s |
| Publication traffic | 106,201,119 | 106,348,054 |
| Closing compaction | 64.126790541 s | 64.600235833 s |
| Closing traffic | 3,990,887 | 4,019,432 |
| Member open | 0.025141500 s | 0.024438958 s |
| Member catch-up | 24.476700166 s | 25.575486459 s |
| Open plus catch-up | **24.501841666 s** | **25.599925417 s** |
| Member traffic, limit 50,000,000 | **45,183,855** | **45,237,423** |
| Member main, limit 200,000,000 | **163,844,096** | **164,474,880** |
| Member WAL | 15,544,792 | 4,507,312 |
| `secure_rows`, limit 30,000,000 | **29,409,280** | **29,409,280** |
| `secure_operations`, limit 100,000,000 | **4,096** | **4,096** |
| Source main after closing | 643,919,872 | 643,891,200 |
| Source frames | 6,251 | 6,251 |
| Total frame body bytes | 77,232,410 | 77,232,698 |
| Average body, limit 16,000 | **12,355** | **12,355** |
| Maximum body | 16,111 | 16,117 |

The frame gate is the average body, not the maximum. Source publication and
closing costs are exposed separately; the 200 MB file limit applies to the
member main file. WAL sizes are reported separately. Final seals compact and
allowed nodes list the exact bases as in phase 2.

## One 50k population and 100 seals

Both feature configurations preserve the complete 50k population and 100-seal
history, then take the exact newest closing base in one member sync.

| Measurement | Workspace/default | SDK/no-default |
| --- | ---: | ---: |
| Complete gate | 129.146 s | 132.580 s |
| History | 3.230190583 s | 3.470126625 s |
| Seed open | 25.739176750 s | 27.368318667 s |
| Publisher open | 3.271948084 s | 3.484913792 s |
| Publication | 75.208525542 s | 76.148055959 s |
| Closing compaction | 10.763572667 s | 10.675709125 s |
| Member open | 0.024697084 s | 0.025646291 s |
| Member catch-up | 10.245153833 s | 10.662587417 s |
| Member traffic | 20,717,054 | 20,742,964 |
| Member main file | 78,213,120 | 78,397,440 |

## Development failures retained

These were development failures, not qualified runs:

- First 100k: **200.654 s**, seed 47.041309542 s, additive 23.747673208 s,
  fill 129.031272708 s; 6,251 bodies totaling 316,148,055 B, average 50,575 B,
  maximum 54,523 B. It stopped before publication or member measurement.
- Shared carried witnesses alone still failed: **187.041 s**, seed
  43.820375833 s, additive 22.786641708 s, fill 119.623727250 s; 317,134,574 B,
  average 50,733 B, maximum 54,664 B. Repeated bundle openings were the remaining
  frame overhead.
- Compact bundle openings passed the frame/storage/time numbers but failed
  transfer: **264.94 s**, 51,876,542 B member traffic. Publication was
  63.116902583 s / 106,175,285 B; closing 54.895233792 s / 3,995,992 B;
  member open 25.390833 ms, catch-up 25.173832750 s; main 163,794,944 B,
  `secure_rows` 29,409,280 B, `secure_operations` 4,096 B. Page-origin
  dictionaries remove that repeated metadata without dropping witnesses.
- A development full check failed an injected-frame helper that lacked the
  signer-set interning real materialization installs. A later run failed the
  new empty-origin test setup and two existing restoration audits missing
  retained witnesses; it also exceeded 300 s at 332.20 s with rebuilds during
  the run. Helpers and witness retention were corrected. The fixed candidate
  passes the complete checks and budgets above.
