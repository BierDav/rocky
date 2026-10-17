# Phase 17 timing qualification

Status: the final combined e2e timing gate remains unresolved (2026-10-21).
This records actual matrix attempts and measured repairs in change `pvvozwkw`,
including the shared phase 14/19 path. Functional passes do not qualify a timing
gate. The [round48 SHA-2 experiment](17-sha2-backend-evidence.md) records the
target-scoped backend, independent byte controls and its bounded comparison.
See [repair qualification](17-schema-changes-through-seals-qualification.md)
for the accepted security contracts and independent acceptance boundary.

The third final-matrix attempt passed lint, commitlint and `./scripts/check`:
169 unit and 1,170 integration tests across both builds. Its e2e run passed
72 tests, including the real hash-window recovery, but the workspace 100k
migration timed out at 600.054 s. The SDK build's 100k test passed in 583.639 s;
the combined suite exceeded its unchanged 900 s budget. These results precede
the performance repairs and are not final qualification.

Round30 accepts verification reuse of immutable admission entries within one
frame and transaction attempt, with fresh SQL tuple comparisons on every lookup.
The two-birth regression changes a SQL claim after the shared entry verifies,
requires refusal, checks rollback, then rebuilds and audits. Its corrected
exact-error assertion passes in 0.056 s. This does not test privileged replacement
of an already verified source or head within the same transaction.

The full 100k measurement then timed out at 600.049 s. A profiling run on the
same source passed in 515.311 s, with seed/additive/fill/member stages of
54.920/37.384/183.908/236.067 s. Both outcomes remain evidence. Live fill-stage
samples show repeated payload decryption and canonical decoding; the subsequent
local-payload refactor passes 27 focused admission/replay regressions. Its
round31 binding checks the full header scope after source-epoch payload opening,
then scans every original write before projection. Duplicate decrypt/decode is
unnecessary after that binding; no current authority is cached. The actual-replay
boundary regression passes in 0.202 s, including empty and multi-write corrupt
headers, a later wrong-scope write, a later path-filtered write, unchanged
SQL/sets/ledger on refusal, and a valid multi-write control. Both existing
epoch-rotation controls and the six accompanying admission controls pass
(`codex1-payload-boundary-5`: 9/9). The SDK small two-step migration also passes
in 0.986 s. These focused results precede the later matrix attempts below.

The fourth matrix attempt passed lint, commitlint and `./scripts/check`:
169 unit and 1,172 integration tests across both builds, including all required
isolated tier confirmations. Its actual `./scripts/check --e2e` ran both builds
and passed all 73 tests. The two 100k migrations passed in 486.609 and 591.612 s,
and real hash-window recovery passed in 63.265 s. The command nevertheless
failed: combined wall time was 1,494.15 s at load factor 1.0, over the unchanged
900 s budget. The workspace build alone took 900.948 s, including the holdings
fixture's 371.380 s. The sequential driver stopped there; native, Kotlin,
Maven and shopping checks did not run. Logs are
`/tmp/rocky-phase17-codex1-final-{check,e2e,pipeline}-4.log`.

Live samples captured staged fill publication and member synchronization.
The member sample shows repeated seal-readiness scans decoding retained headers
before discarding pending keyed frames. Accepted round32 keeps the full scan and
its live predicates, and extends the existing private parsing memo with complete
live-byte equality, immutable parsed fields, an entry cap and 32 MiB byte cap.
Frontier reads still compare the live SQL hash on hits. This caches syntax only;
it grants no authority or readiness. All six new actual-consumer regressions
pass (`codex1-header-memo-focused-5`: 6/6, 0.241 s), including live predicates,
byte replacement, bounded fallback and distinct existence/placement counts.

The fifth matrix passes lint, commitlint and `./scripts/check`: 169 unit and
1,178 integration tests, including required isolated budget confirmations.
Its e2e run passes 72 of 73 tests; the SDK-without-defaults 100k migration times
out at 600.048 s. The combined suite takes 1,475.90 s at load factor 1.0 and
fails its 900 s budget. Header parsing reuse has not qualified performance.

The remaining checks were then run separately on the same source. Native
checks pass. Kotlin `check` fails the iOS simulator's unchanged 1,000-cycle
open/close handle-lifetime test at its 180 s timeout; 28 other simulator tests
pass. An isolated rerun also times out. Maven local publication passes, and
the shopping check passes all 58 tests, including `SchemaUpgradeTest` and
`IssuerRecoveryTest`, plus Android compilation. Logs are
`/tmp/rocky-phase17-codex1-final-{check,e2e,native,kotlin,maven,shopping}-5.log`.
The unchanged lifetime test subsequently passes in 37.580 s with zero live
handles and RSS increasing from 106,233,856 to 108,085,248 bytes. A complete
Kotlin `check` rerun then passes in 1 min 44 s
(`/tmp/rocky-phase17-codex1-final-kotlin-6.log`). The earlier timeouts remain
observations of runtime variability; no code or timeout repair is claimed.
The e2e timing gate remains unresolved, so these passes do not qualify the
final source or phase 20.

A subsequent unchanged, properly tiered SDK 100k profiling run passes in
498.002 s (seed/additive/fill/member 51.443/26.466/133.034/283.894 s).
Its live samples identify repeated graph queries during held-frame refresh.
Accepted round33 reuses only graph facts within that refresh, leaving each
header's author/sequence checks and all later live checks intact. The expanded
focused selection passes 33/33 in 3.236 s; two strengthened rollback/lazy-body
controls then pass in 0.243 s. Exact header syntax reuse also serves this scan.

An external 30,000-row comparison retains the migration helper's data,
accounting and one-sync assertions. Member convergence falls from 59.249 to
49.678 s; total test time falls from 120.78 to 113.31 s. The external harness
has a different dependency feature graph, so only its before/after results are
comparable. This modest improvement is not qualification of either the original
100k gate or the combined 900 s budget on the new source. Logs are
`/tmp/rocky-phase17-codex1-small-30000-{before,after-graph}-1.log`.

Accepted round34 retains each final bundle member's opened payload for that
application attempt, preserving author/non-bundle decode positions and all live
checks. A new final-member held-resolution disagreement control passes in
0.076 s, after proving both members ready, with all post-intake SQL unchanged
except rejection statuses. The selected 61 small controls pass; an accidentally
selected long hash-window test was interrupted in the integration profile and
then passes in its proper e2e profile in 45.911 s. The interrupted run is not a
passing suite. The same external 30k harness then passes in 108.62 s, with member
convergence at 45.239 s versus round33's 49.678 s. Data/accounting and one-sync
assertions remain intact. This further modest gain still leaves final timing
qualification pending.

Accepted round35 keeps one borrowed historical binding-chain success within
one ordinary-birth materialization call. Every proof's own signature and
metadata checks precede lookup; carry and replay use the uncached wrapper.
Five focused controls pass in 0.292 s, covering actual two-birth reuse, a fresh
next frame, a corrupted second proof, the later exact-frame-stamp check, full
key/error-order differentials and rollback/retry with fresh verification.
The retained selection passes 46/46; the exact nested row-move proof control
passes separately in 0.161 s. Secure all-target Clippy passes. The same external
30k comparison passes in 108.34 s versus 108.62 s after round34, with a slower
49.875 s member stage and a peer-closed-link report before one-sync convergence.
This is no meaningful overall gain. Verification counters establish the intended
reuse; they do not establish seconds saved or a final timing pass.

Accepted round36 adds an optional fresh-file covering index on operation
status/hash/header. The complete readiness scan still decodes every selected
header. Duplicate existence obligations now retain their maximum, and placement
fallback completes its scan and prefers a conflicting hash over an equal match.
These close scan-order dependencies before allowing the new query plan.
The focused retained selection passes 41/41; all six isolated confirmations
pass within the unchanged 2 s budget. Five format controls and secure all-target
Clippy also pass. Logs are `/tmp/rocky-phase17-root-round36-*.log`.

Accounting now reports operation tables and indexes separately from permanent
admission evidence. A one-operation sample has 182 pages, no free pages, a
745,472 B main file and compact backup, and WAL of 935,272 B before truncation
and zero afterward. The new index occupies one 4,096 B page in that sample.
Churn retains its original count tuple and admission-growth assertions; freed
index pages do not imply that the main file shrinks.

The same external 30k migration passes in 100.57 s versus post35's 108.34 s;
member convergence takes 40.683 s versus 49.875 s. All original assertions and
one-sync convergence remain. A closed-fixture copy is made after these stage
measurements. Bundled SQLite 3.53.2 uses the new cover for both full header
scans and the original scoped cover for the frontier. Pending and held queries
also select the new index, while placement serving scans the operation table.
At 1,877 retained operations, the new cover occupies 451 pages (1,847,296 B)
without overflow; the old cover remains and occupies 2,149 pages including
1,876 overflow pages. These are observed plans and costs, not planner contracts
or universal no-overflow guarantees. Logs are
`/tmp/rocky-phase17-root-small-30000-after-index-1.log` and
`/tmp/rocky-phase17-root-round36-query-plans-6.log`.

The subsequent unchanged SDK-without-defaults 100k e2e test passes in
487.103 s, below its 600 s timeout. Seed/additive/fill/member take
54.596/32.588/134.746/262.833 s; one-sync convergence and all original
data/accounting assertions pass. Session-budget and peer-closed-link reports
precede successful completion. The log is
`/tmp/rocky-phase17-root-round36-100k-profile.log`. This isolated pass does not
qualify the combined 900 s e2e suite.

Matrix6 passes lint, commitlint and the ordinary check (169 unit and 1,191
integration tests; integration wall 173.02 s within 300 s). All 73 e2e tests
also pass: the two real 100k runs take 429.647 and 447.402 s. The combined
e2e suite still fails its 900 s budget at 1,285.78 s. Workspace wall is
836.869 s, including the isolated holdings fixture at 364.946 s; the SDK
without defaults takes 447.866 s. Logs are
`/tmp/rocky-phase17-root-final-{check,e2e}-6.log`.

Native checks pass in 92.573 s. Kotlin's first full check fails the JVM
background-link activity assertion at its unchanged 1 s timeout; both lifecycle
tests subsequently pass in isolation, and the complete unchanged Kotlin check
passes in 42.914 s. This is runtime variability, not a claimed source repair.
Local Maven publication passes in 233.791 s, and shopping passes all 58 tests
plus Android compilation in 94.525 s. Logs are
`/tmp/rocky-phase17-root-final-native-6.log` and
`/tmp/rocky-phase17-root-final-{kotlin,maven,shopping}-7.log`. The original
Kotlin failure is preserved in `root-final-kotlin-6.log`. The combined e2e
timing failure and independent whole-change acceptance remain open.

A subsequent isolated full holdings profile passes in 387.911 s. Its unchanged
200-partition, four-table fixture retains 801 scopes, all row/reference checks,
checkpoint/reopen/audit checks and its 10 s idle proof assertion. Measured idle
is 4.210 s, catch-up 25.758 s and reopen 0.969 s. Short live samples after
checkpoint creation repeatedly identify the final 801-scope audit loop, where
each scope audit rechecks whole-replica admission evidence and identity ledgers.
The log is `/tmp/rocky-phase17-root-holdings-profile-1.log`. This motivated the
round37 HIGH decision to share production checks in a private test composition.

Round37 keeps public single-scope audits fresh and adds a `cfg(test)` batch
under one replica guard, with two complete global passes and every original
per-scope check. Ordinary Result exits restore flags and attempt scope-only
scratch cleanup; verification errors take precedence over cleanup errors.
The [checkpoint contract](../runtime/encrypted-checkpoints.md#verifying-a-file)
records the boundaries, including strict Check, partial keyless reports and
the absence of cancellation or whole-batch rollback guarantees.

One subsequent isolated full holdings run passes in **93.729 s**, with the
same 801 scopes, 200 partitions, four tables and all original setup, sync,
row/reference, frame, checkpoint, reopen and idle assertions. The final audit
alone takes **1.235 s**. All 801 ordered reports match actual retained history
(one operation each), one stored row, saved checkpoint IDs and zero suffix
replay. Idle is **6.195 s < 10 s**, catch-up 25.709 s, authoring 32.024 s,
checkpoint creation 16.359 s and reopen 0.945 s. The original E2E 600 s timeout
and isolation remain; `/tmp/rocky-round37-full-holdings.log` contains the run.
These are observed fixture timings, not whole-suite qualification or a
universal speedup claim. Matrix6's combined budget failure remains historical
evidence; final gates and independent whole-change HIGH acceptance remain open.


## Round38 bounded prerequisite repair

The [original-prefix repair](17-original-prefix-restoration-evidence.md) passes
40 selected secure integration controls in 7.090 s, each <=0.438 s; its 15
focused controls each take <=0.323 s. All six real-service prerequisites pass
on final production source: three integration cases in 3.025 s, and existing
service/private migration/original P5 in the e2e profile in 51.023 s. Original
P5 takes 45.919 s with 258 transactions, unchanged retention256/observation30s,
automatic recovery and its original final read/write/reopen assertions. Private
migration takes 4.574 s and proves the accepted committed-stage/exact-close
observation without a new public status API.

Logs: `/tmp/rocky-round38-implementation-controls-final.log` and
`/tmp/rocky-round38-implementation-sdk-{integration,e2e}.log`. No full matrix,
100,000-row or holdings rerun was performed in this bounded repair. These
focused passes do not settle the combined900 s gate, final platform/artifact
qualification or fresh independent HIGH acceptance.


## Matrix8 on the completed round38 repair

Lint, commitlint and the ordinary check pass. The check runs 170 unit and
1,221 integration tests; integration wall time is 188.14 s within its 300 s
budget, including the required isolated confirmations. Both actual e2e builds
pass all 75 tests. Their 100k migrations take 446.565 and 477.403 s, each below
600 s. The workspace suite takes 556.356 s and the SDK without defaults takes
477.863 s. The combined gate nevertheless fails: 1,035.29 s at load factor1.0
exceeds the unchanged 900 s budget. No test failure or individual timeout is
being hidden by that functional-pass count.

The remaining checks run sequentially on the same source: native passes in
105.250 s, full Kotlin check in 310.069 s, local Maven publication in 226.738 s,
and shopping in 86.848 s. Logs are
`/tmp/rocky-phase17-root-final-{lint,commitlint,check,e2e,native,kotlin,maven,shopping}-8.log`.
These platform passes leave combined timing and fresh whole-change HIGH
acceptance open. Phase20 application/device work has not started.

The subsequent [round39 descriptor parsing experiment](17-migration-descriptor-parsing-evidence.md)
records its bounded byte-only fact, actual-consumer controls and separate
within-harness comparison. It does not qualify the combined timing gate.


The unchanged proper SDK-without-defaults 100k gate on round39 source then
passes in 428.249 s under its original 600 s limit. Seed/additive/fill/member
stages take 42.874/25.176/125.053/233.048 s. One-sync convergence and all original
data/accounting assertions pass; session-budget and peer-closed-link reports
precede completion. Five short live samples cover all stages plus late member
work. Log: `/tmp/rocky-phase17-root-round39-100k-profile.log`. This isolated pass
supports a full qualification attempt; it does not settle combined900.


## Matrix10 and round44 diagnostics

On HIGH43's accepted production `4d70adb986bb`, matrix10 lint/commitlint/check
pass in 4.046/1.779/524.461 s. The check covers 175 unit and 1,246 integration
tests; integration wall time is184.65 s within300 s. E2E passes74/75: P5 fails13.873 s with
retryable `missing_dependencies`, `counts is not available: Incomplete`, before
`rollback: final seals and 258 rows received`. Neither of its two early reads
was labelled, and no failure database was retained. This does not establish
the previously repaired round38 guard as the cause.

Workspace50/51 takes529.159 s; SDK without defaults24/24 takes460.194 s.
Combined990.38 s exceeds900 at load1.0, independently of P5's functional failure.
Both unchanged 100k cases pass425.841/459.726 s under600. Same-source native,
Kotlin, local Maven and shopping checks pass105.337/325.026/244.890/82.790 s.
Logs use `/tmp/rocky-phase17-root-final-*-10.log`. No budget is waived.

The one unchanged isolated P5 then passes44.355 s (command45.085 s), reaching
all original phases including restoration, exact held original, target write
and subsequent M reopen. Its log is
`/tmp/rocky-phase17-root-final-p5-isolated-10.log`. This is no cause/fix proof.

Round44 adds only private fixture diagnostics, retaining the original 258 transactions,
retention256, sync calls/order, real voters,20 s final-seal wait and30 s automatic
restoration observation. Production remains byte-identical to HIGH43. Exactly
one authorized two-case command runs: `cargo nextest run -p rocky-sdk --test
service --locked --profile e2e -E 'test(=rollback::a_compacted_member_waits_for_the_owners_kept_rollback_seal) | test(=migration::real_voters_release_a_private_migration_without_reopening_the_publisher)' --no-capture`.
It passes2/2: migration4.529 s, P5 48.136 s, runner52.666 s, command66.038 s
including compilation. Log: `/tmp/rocky-round44-loaded-subset.log`.

**Not reproduced; cause unknown.** Nextest documents `--no-capture` as serial
execution; the log shows migration finishing before P5 starts. No explicit
thread override was supplied, but this run provided no competing-test load
and is not a full-catalog or qualification result. There was no new failure
fixture/snapshot. Failure-only SQL/public-status diagnostic branches therefore
have compilation/static verification only. After this run, query-result error
labels, expected-refusal report printing, cross-file pending-head lookup and
exact public cursor output were completed; original calls/assertions/deadlines remain unchanged and the two
cases were not repeated. Keep instrumentation for ROOT's next required ordinary
full catalog; functional and combined timing holds remain separate.

## Round46 author payload retention

One author invocation retains its complete final frame payload from the existing
materialization opening through storage, then drops it. The shared body preserves
all checks and their order; existing callers keep their pair-returning wrapper.
Incoming bundle payloads stay borrowed, and replay/ledger code is unchanged.
This production delta still needs independent source acceptance.

The final small selection passes 87 secure integration cases (85 plus two explicit
removal/re-addition controls), each <=0.498 s. Five new author cases cover final
unsupported-version refusal, original-mask and live-O error precedence, two
numeric births and a genuine update witness, and late SQL-COMMIT rollback with
same-replica/same-handle retry. An opt-in test-only trace checks one opening of
each final authored frame, fresh on the next invocation and retry. Stored proofs,
bindings, original ordinal claims, complete SQL/memory rollback and audit/reopen
are checked. Foreign-carry and held-reauthor controls inspect the final envelopes.
Secure all-target Clippy, lint and commitlint pass; SDK no-default two-step
migration passes1.035 s and default-iroh private-service migration passes4.672 s.
Logs use `/tmp/rocky-round46-*.log` and `-commands.jsonl`.

Exactly one unchanged external30k comparison on `ebd5bbe05a08` passes97.36 s,
with seed/additive/fill/member11.984007167/7.694335500/37.677526333/38.894320083 s,
one member sync and all original data/generation/fingerprint/accounting checks.
The unchanged CODE43 baselines were107.07 s (12.801521750/7.883367750/36.888397375/
48.359616333) and96.41 s (12.438401333/7.424431250/37.199207834/38.334470458).
Fill is slower than both, and total is slower than the faster baseline. This
fails the continuation threshold (fill<=34.888 s and total<96.41 s); no useful
performance gain is established. The earlier baseline difference was unchanged
source variability. Log: `/tmp/rocky-phase17-root-after46-fill-30000-1.log`.

The same three short samples still reach opening replay, real author storage,
and finalization/reprojection/replay. Source and focused counts establish removal
of the author's second opening; inclusive samples do not measure saved seconds.
There is no30k repeat or wider reuse. ROOT next owns fresh delta review and one
proper canonical SDK-no-default100k600 profiling run to separate authoring from
opening verification and finalization/replay costs. It was not run here. Matrix10's
P5 cause remains unknown, combined990.38>900 remains failed, and phase20 is untouched.

## Round48 upstream SHA-2 backend

The [bounded backend experiment](17-sha2-backend-evidence.md) passes independent
pre/post byte controls, scoped checks and all available native links. Its ONE
30k run passes all assertions in 96.53 s: fill 33.122666291 s improves, while
member 44.055295292 s offsets it. Total misses <96.41 s. STOP applies: no repeat,
100k or full matrix follows, and no combined timing repair is claimed.
