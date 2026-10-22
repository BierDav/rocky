# Phase 3 implementation and qualification

Status: complete and qualified on 2026-10-24. All phase 3 gates passed.
Current formats: row 12, checkpoint 17, replica 35, link 16.

The mandatory replica format is v35. Stored records have a local liveness byte and a canonical variable-width shape id;
the virtual generated `secure_rows.live` column and partial dead-row
index follow every writer, including bulk restore and direct import. This
prefix does not enter canonical commitments or wire encodings. A shape
legend, per-scope shape counts and `(scope, shape, row)` index identify
implicit SQL defaults after an additive switch. The switch retains its
constant write count and does not rewrite rows to maintain this index.

`secure_row_positions` records original source-schema storage identities at
application. Excluded-operation renumbering preserves these entries; deleting
an applied position releases them. A compact birth-position index preserves
global identity-retirement order during row-only replay. Both indexes are
checked against authenticated retained frames by the audit. Current-format
schema checks refuse missing or altered mandatory objects before writes.

Projection changes maintain indexed seal candidates. Quiet finalization reads
those candidates and indexed tombstones. A changed scope restores selected
rows over their base, applies included positions and prior seal drops, resolves
the constraint graph, records drops and replays the excluded tail in application
order. Canonical row counts and digests change in the same transaction.

The test switch defaults to comparing with the original full finalizer in a
rolled-back savepoint. It compares canonical stored rows, scope accounting and
all typed tables. Its time is separate from incremental finalization time.
The explicit audit retains full canonical replay and checks a rebuilt typed
projection; interrupted schema installation keeps its existing staged-row audit
until its partial typed projection is installed.

## Focused 100k gate

The final-format regular e2e gate passed on 2026-10-24 through
`./scripts/check --e2e` (standard Cargo profile). The earlier standalone
command was `cargo test -p rocky-secure --lib finalization_100000 -- --nocapture`.
It seeds 100,000 real SQL rows, signs both seals and checks exact full-rebuild
canonical state, typed tables, applied/row/birth positions and support bookkeeping.
Temporary SQL triggers assert zero authoritative row writes for the quiet seal
and exactly 300 distinct authoritative rows for ten excluded 30-row operations.
The fixture checks those operation and row counts before sealing.

| Stage | Quiet seal | Ten operations / 300 rows |
| --- | ---: | ---: |
| Incremental finalizer | 18.631 ms / 200 ms | 392.077 ms / 1,000 ms |
| Explicit rebuild oracle and comparison | 31.128906 s | 31.125572 s |
| Authenticated publication through finalization, including oracle | 32.883529 s | 33.244088 s |

Seeding took 41.956631 s. Complete fixture wall time, including setup, assertions
and the final complete audit, was 120.845 s. The finalizer timing includes scope
cuts, renumbering, affected-row restoration, drops, tail replay, ledger updates
and bounds inside the caller transaction; the core timer ends before the
caller commits. Publication totals additionally include commit, readiness and its existing
phase 1 test oracles. Checkpoint creation/compaction remains separately exposed
by the complete SDK migration programs; it is not included in the millisecond
finalizer claim.

The seven-field, 50-byte SQL row fixture passed at 5,363 B signed frame
(167.59 B/row), 3,586 B encrypted body and 183 B maximum stored record. Its carried
body was 13,417 B plaintext. The unchanged gates are 500 B/row wire and 300 B
stored record. The independent Python verifier passed all 28 checkpoint fixture
files without changing their bytes.

## Complete migration and catch-up qualification

Final `./scripts/lint` passed, including eight script tests and formatting.
The independent Python verifier passed all 28 frozen checkpoint files again.
`./scripts/check` passed Clippy, 180 unit tests and 1,287 integration tests.
Integration wall time was 188.79 s at load x1.0, within 300 s. Unit wall time
was 96.0 ms (44.9 ms at the runner's existing load x2.1), within 10 s.
`./scripts/check --e2e` passed all 79 tests: regular 744.85 s / 900 s;
additional existing 100k exemptions 1,064.14 s; total elapsed 1,808.99 s.
The new finalization fixture remains in the regular budget. No limit,
exemption, fixture population or assertion was relaxed.

Both complete 100k programs retain seed, additive schema switch, fill,
durable publication and closing/compaction. Both members import the exact
closing base in one sync, with zero covered frame replay and zero member
frame bytes. The additive switch performs eight SQL changes. Both 50k programs
retain one population plus 100 seals and assert the exact newest base.

| 100,000-row stage or measurement | Default | No default features |
| --- | ---: | ---: |
| Seed open | 80.758124 s | 84.983764 s |
| Additive open | 8.310156 s | 8.514884 s |
| Fill open | 297.137748 s | 317.574356 s |
| Durable publication | 68.238504 s | 69.825458 s |
| Closing and compaction | 38.376756 s | 40.700913 s |
| Member open | 26.280 ms | 25.733 ms |
| Member pull | 23.400961 s | 23.388237 s |
| Member open plus pull | 23.427241 s | 23.413970 s |
| Member traffic | 45,224,887 B | 45,219,752 B |
| Member main file | 170,319,872 B | 170,471,424 B |
| Member WAL | 15,318,192 B | 10,300,032 B |
| Member `secure_rows` | 29,413,376 B | 29,413,376 B |
| Member `secure_operations` | 4,096 B | 4,096 B |

Source population and fill costs increased relative to the phase 4 evidence;
they are not included in the finalizer latency claim. Source publication moves
106,268,309 / 106,346,822 B; closing moves another 4,007,137 / 4,019,624 B.
The 6,251 retained publisher frames average 12,355 B of body in both variants.
Member catch-up stays within 60 s, 50 MB traffic and 200 MB main-file gates;
`secure_rows` stays within 30 MB. The shape index uses 4,558,848 B on each member.
Both members' row-position and birth-position objects are empty 4,096 B objects
after exact-base import. Complete stage accounting, base identities and **every**
member `dbstat` object for all four migration runs are in the
[raw measurements](03-incremental-finalization-measurements.txt).

The workspace run passed the two-device late-frame comparison, insert/delete
plateau, kept-seal visibility and next-finalization tests, parent exclusion
with cascade/SET NULL children, existence puppet/support restoration, injected
precommit failure with exact authoritative rollback and retry, corrupted index
and shape audits, version refusal and missing mandatory-object refusal.
The full-rebuild switch remains on by default for all secure tests.

## Platform qualification and limits

Heavy qualifications ran serially after e2e:

| Command | Result | Wall time |
| --- | --- | ---: |
| `./sdks/native/check` | format, Clippy, bindgen build/test command and no-default check passed; native test binaries contain zero tests | 97.260 s |
| `./sdks/native/check-ios` | four builds passed: aarch64 device/simulator, each with and without default iroh networking | 263.754 s |
| `sdks/kotlin/gradlew -p sdks/kotlin check` | passed; 47 tasks executed, 155 up to date, including configured iOS simulator, JVM/Room and eight schema descriptor/fingerprint qualification pairs | 211.978 s |
| `./examples/shoppinglist-app/check` | required-schema qualification, 25 Android-target tests, 61 JVM tests, desktop and Android builds passed | 50.669 s |

This qualifies the shared runtime on the tested macOS host and the configured
Kotlin iOS simulator tasks; the four native iOS checks alone are compile-only.
Physical device execution, Linux and Windows remain unqualified. Generated
Kotlin `@Throws` and Gradle deprecation warnings remain nonfatal. No public API
or binding contract changed. No implementation blocker remains.

The first complete e2e attempt exposed a storage regression: fixed-width shape
prefixes plus stored generated columns grew `secure_rows` to 31,559,680 B,
above the unchanged 30,000,000 B limit. Qualification was not accepted. The
implementation now uses canonical variable-width ids and virtual generated
columns, preserving indexed maintenance without duplicating the prefix in each
row. The interrupted run's retained JUnit reported member pull at 23.590690 s;
that is diagnostic evidence only, not final qualification. The final reruns above passed with `secure_rows` at 29,413,376 B in both
100k variants; no storage budget was relaxed.
