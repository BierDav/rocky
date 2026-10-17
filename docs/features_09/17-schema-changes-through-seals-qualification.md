# Phase 17 repair qualification

Status: core code accepted by HIGH43, the author-payload delta by HIGH47 and
the SHA-2 backend delta by HIGH49 (2026-10-21). The latest accepted snapshot
is `1f55a2aa6b1a`. The owner subsequently authorized phase20 to proceed with
the exact migration timing exemption and Linux ARM scope decision below.
Round44 adds private service-test diagnostics. This record
covers phase 17 and the shared phase 14/19 paths in change `pvvozwkw`,
parent `10a4a8bf9ddb`. Replica file v32, link v14 and checkpoint format 16 are
all new in this change and refuse every other version. The
[earlier v31 passes](17-schema-changes-through-seals-evidence.md#before-round-7-v31-baseline)
are historical evidence only.

## Accepted repair contracts

On 2026-10-21 the owner exempted only
`rocky-sdk::migrations a_migration_of_100000_rows_converges_on_every_member`
from the combined e2e clock. Both feature builds still execute it separately;
all assertions, 100,000 rows and the 600 s nextest timeout remain. Other tests
retain the 900 s combined suite budget. No historic failed run becomes a pass.
Linux ARM fallback execution is no longer a release blocker; it remains
unexecuted. Phase20 may proceed. Preserve rollback diagnostics and explain or
reproduce the earlier functional failure if it recurs, without preemptively
adding a readiness loop or weakening its assertions.

Each round's HIGH decision was accepted in full; the decisions are kept with
the writer handoffs in `/tmp`.

- **Round 7.** A follow change persists its cause; a bookmark move holds only
  source-visible frames the target excludes; removal holds only own, unsent,
  target-visible bundles. Backward and sideways switches restore the target's
  own history ([removal](../runtime/removal.md#the-path-filter)). Scoped
  trusted nodes use signed header placements outside their custody; unreadable
  coordinated entries are refused before acknowledgment.
- **Rounds 8 and 9.** Readiness is grant-based and pure; finalization
  materializes every checked scope, including verified empty state. Unknown
  scopes need from-zero verification or a signed base. Coverage anchors, deferred
  keyless scopes and an audited staging transform complete the rule
  ([lazy compaction](../runtime/lazy-compaction.md)).
- **Round 10.** A checkpoint's identity marks cover only its covered births
  (checkpoint format 16). A staged schema switch keeps the source installed and
  is the only authority until it finishes at its target, advances with a
  continuing bookmark, or is withdrawn ([generations](../runtime/generations.md#interrupted-schema-switches)).
- **Round 11.** Every replay keeps its own identity ledger, so a reused birth
  never materializes after a seal drop removed the original row.
- **Round 12.** A keyless held scope with no history of its own is signed as a
  `NoFrames` checkpoint state, imported without pages or keys
  ([no-frames checkpoints](../runtime/encrypted-checkpoints.md#no-frames-checkpoints)).

## Historical focused regressions before admission evidence

These historical runs used `cargo nextest` against the then-current working
copy; runner wall time includes the build. Logs are
`/tmp/rocky-phase17-v32-<label>.log`, with start/end UTC and exit status also in
`/tmp/rocky-phase17-v32-results.jsonl`.

| Label | Scope | Result | Wall |
| --- | --- | --- | --- |
| `r10-staged-3` | `tests::compaction::staged_switches` | 11/11 | 73.808 s |
| `r10-sdk-crash-1` | SDK crash windows in `tests/generations.rs` | 2/2 | 61.499 s |
| `r11-regressions-1` | `tests::compaction::identities`, ledger unit test | 11/11 | 72.278 s |
| `r11-nobase-2` | replay from no base | 1/1 | 71.956 s |
| `r12-checkpoint-2` | `tests::checkpoint` | 19/19 | 80.352 s |
| `r12-noframes-5` | `tests::checkpoint::no_frames` | 9/9 | 70.513 s |
| `r12-clippy-2` | Clippy, secure and SDK, all targets | passed | 10.583 s |

A mutation check disabled the replay identity skip: the consumed-birth,
mixed-frame, paging-with-seal-drop and no-base replay regressions failed as
required (`r11-mutation`, `r11-mutation-3`), and the source was restored.

## Historical matrix before admission evidence

All commands run sequentially from the repository root, with budgets, filters
and skips unchanged. `scripts/check` and `--e2e` each run the workspace and
`rocky-sdk --no-default-features` builds. The 100,000-row gate keeps its 600 s
per-test kill and the e2e suite its 900 s budget.

- `./scripts/check` (`check-r12-1`): failed, 435.195 s wall. Unit 268/269,
  integration 848/851 across both builds. The two failing tests, each counted
  in both builds where it runs, are the round 13 questions below.
- The hash-window coordinated recovery regression is an e2e-tier test; alone
  in the e2e profile it passed in 45.296 s (`hash-window-alone`).
- The contemporaneous `./scripts/check --e2e` run is superseded by the final
  qualification matrix; it is not an active job or a current pass.
- `./scripts/lint`, `./scripts/commitlint --jj`, `./sdks/native/check`,
  Kotlin `check`, `publishToMavenLocal` and `./examples/shoppinglist-app/check`
  must be rerun on the final tree. Their earlier passes, and the earlier
  shopping failure (`IssuerRecoveryTest` line 81), precede rounds 8–12 and are
  provisional only.

## Resolved round 13 questions (historical)

1. A member whose second scope import is interrupted can no longer read its
   completed first scope: round 8 readiness gates the whole workspace, while
   `tests/bootstrap.rs`
   `interrupted_join_resumes_only_incomplete_scopes_after_reopen` requires
   those rows to stay readable.
2. A replica's own frames pruned beyond a losing compacted seal cannot return
   after a listed-checkpoint import: the ownership guard refuses them, so
   `tests::compaction::anchors::a_missing_position_beyond_the_anchor_requires_a_listed_checkpoint`
   cannot finalize the later seal.

Accepted later contracts resolve both questions. Current regression assertions
remain in the tree; these historical failures are not outstanding design holds.

## Admission evidence and own-birth coverage

Rounds 19–24 establish exact local admission receipts, concrete partition
floors, immutable Filtered decisions and their qualified continuations. Round
25 adds permanent O and mandatory signed `CoveredMark.own` inside current
checkpoint16. Carries continue raising P at both concrete and allocation scopes,
but never O, S or L. File32 requires O; link14, row11 and backup2 stay current.
There are no legacy readers or missing-field defaults.

Round27 supersedes the round26 lists/items fixture with a single-table normal
API fixture. Its first author bundle has raw writes src1/dst2 and sorted
members dst/domain0, src/domain1. The mover receives both plus src3/domain2;
the destination-only issuer receives only the mover's destination/domain0.
Its honest first seal has exactly frontier `{2:0}`, and signs sequence3/own0.
The tests assert the actual frame order and destination-only grants and keys.
After following the Continue successor, a fresh receiver admits the late
author/domain0 birth; listed import reapplies the pre-admitted exact birth;
the issuer's next honest seal has `{1:0,2:0}` and signs sequence3/own2. Rebuild
and reopen audits run without widening custody or relaxing coverage. The
separate two-arrival-order guard keeps the arithmetic correction: P=3 before
own birth4 and P=O=4 after it.

Round28 corrects an assertion gap: a bad compaction seed can sign a base with
one row while independently computing the correct own2 mark. The continuation
test checks the new signed base has two rows and no tombstones, audits against
the independently applied live rows before rebuilding, and checks both keys
after rebuild and reopen. Its strengthened compaction-P-seed mutation fails at
the signed row count `(1,0)` versus `(2,0)`; the source was restored. The earlier
survival was a coverage gap, not evidence of equivalence.

Development results on this implementation (logs in `/tmp`, not release gates):

| Run | Scope | Result |
| --- | --- | --- |
| `codex1-resumed-own-focused-2` | Exact returns, identity refusals, partition orders, Filtered/Unresolved restoration and genuine Filtered carry | 19/19 passed |
| `codex1-resumed-secure-all-1` | Secure library development run, excluding the two hash-window names and nine existing ignored helpers/measurements | 399/399 passed |
| `codex1-resumed-own-integrity-1` | Mandatory signed own maxima, row checks before direct exposure/destructive install, fast recovery control | 2/2 passed |
| `codex1-resumed-removed-partition-3` | Pre-admitted removed partition table, audit/reopen and source return | passed |
| `codex1-resumed-carry-checkpoint-4` | Round27 normal Fresh/Exact/next-compaction paths, separate order guard, newly signed kept bytes after live O/R growth and reopen | 4/4 passed; each <0.7 s |
| `codex1-resumed-post-mutations-green-1` | Strengthened carry controls and strict old-state/open-fault controls | 5/5 passed |
| `codex1-resumed-round29-focused-2` | Required reopen/idempotence/exclusion controls, expired-statement loader retry, restore precheck and finite restore cuts | 5/5 passed |
| `codex1-resumed-hashwindow-e2e-1` | Exact real hash-window recovery in the e2e profile, before the round29 retry correction | passed in 45.713 s; final script rerun pending |

All six round27/28 mutations compile and fail the intended controls: carries
counted in covered own, statements raising O/S with sequence, Fresh reading P,
listed import seeding L from sequence, compaction seeding replay from sequence,
and carries raising O. Production source was restored after each mutation.

Round29 requires open to verify stored state under retained revocations before
reconciling supplied statements through the ordinary atomic revocation path.
The caller's supplied set is immutable across transaction retries. Tests cover
the failed-reconcile trigger with a plain reopen while the trigger remains,
strict tampered-L refusal, identical-set and plain reopens after success, and a
new revoked-issuer statement refused before storage. The latter is stricter
than the memo's predicted storage-plus-exclusion path; the original recorded
seal stays excluded. A failed E4 reconciliation preserves logical metadata;
post-commit notification or settling errors can leave the consistent new state
durable. No global file-byte rollback is claimed.

R23 now has actual author-log-fork rollback after candidate intake, installed
early-gate candidate preservation, a truly Held ingress contradiction, and a
schema-unavailable install that drops only its pulled candidate. These focused
passes and the mutation checks do not replace the final script tiers, both
workspace/SDK builds, long hash-window and 100,000-row gates, native/Kotlin
checks, artifact rebuild and shopping SchemaUpgrade/IssuerRecovery tests.

The focused accounting run `codex1-resumed-final-focused-2` preserves the
phase 15 tuple `((1,0),1,0,0,4,1,1)` at cycles 16 and 64 and checks exactly 48
additional receipts, original claims and chain values. Logical evidence grew
from 19,792 to 70,235 bytes; main-file size grew from 1,179,648 to 1,835,008 bytes.
WAL sizes before truncation were 4,260,112 and 4,593,832 bytes, respectively;
each truncation left 0 bytes. Allocated table/index pages include SQLite's
automatic indexes. Encoded graph history is reported separately (1,814 and
2,583 bytes). These are observations of that run, not total-storage plateau
claims or final tier qualification.

## Current matrix and performance repair

The [timing qualification record](17-schema-changes-through-seals-performance.md)
contains actual matrix results and the measured performance repairs. Matrix6
passes all 73 functional e2e tests but exceeds the combined 900 s budget at
1,285.78 s. Native, the full Kotlin rerun, local Maven publication and shopping
checks pass; the earlier Kotlin timeout remains recorded. Timing qualification
and fresh independent acceptance remain open.

## Original-prefix restoration and core prerequisites

The accepted round38 repair and corrected private-stage observation are recorded
in [original-prefix restoration evidence](17-original-prefix-restoration-evidence.md).
That record distinguishes settled original evidence from target projection,
public sync reporting from private stage inspection, and focused prerequisites
from final qualification. Phase 20 application work remains pending.

## Independent review boundary

Phase 17 is not accepted until a fresh independent HIGH review of the final
repairs and matrix. Phase 20 design and physical qualification, phone actions,
publication to remotes and release acceptance are outside this run.


## Matrix10 and round44 diagnostic disposition

[The performance record](17-schema-changes-through-seals-performance.md#matrix10-and-round44-diagnostics)
records both independent holds: P5 failed before its first progress marker, and
combined e2e timing exceeded 900 s. The unchanged isolated P5 pass and the
round44 two-case command do not close either hold. Round44 adds labels, exact
preceding sync-report retention and failure-only read-only file snapshots; it
adds no readiness loop, retry, runtime change or extra sync pass. Its exact
command passes both tests, but nextest's `--no-capture` runs them serially.
The original functional failure is **not reproduced; cause unknown**. Retain
the diagnostics for the next required full catalog with ordinary scheduling.
Phase20 and ALLfeatures09 remain open.

## Round49 acceptance and current-source checks

Fresh HIGH49 accepts the actual backend delta without a P0/P1/P2 finding.
Its performance continuation decision remains STOP: the one comparison does
not justify another large benchmark, profiling campaign or paired e2e run.
See [backend evidence](17-sha2-backend-evidence.md) for byte equivalence,
platform links, the failed continuation rule and the remaining runtime gap.

The following checks ran sequentially against accepted `1f55a2aa6b1a`.
Command logs are `/tmp/rocky-phase17-root-final-<label>-11.log`; command,
UTC start/end, exit and wall times are recorded in
`/tmp/rocky-phase17-codex1-final-matrix-results.jsonl`.

| Label | Actual result | Command wall |
| --- | --- | --- |
| `lint` | passed | 4.150 s |
| `commitlint` | passed, JJ change description | 1.865 s |
| `check` | 175 unit and 1,256 integration tests passed; integration 203.49/300 s at load 1.0; required isolated confirmations passed | 556.169 s |
| `service-functional` | three existing service tests passed; rollback 44.858 s, private migration 5.336 s, registration 1.033 s | 45.299 s |
| `native` | formatting, Clippy, native test command and no-default check passed; native crate reports zero direct tests | 106.617 s |
| `kotlin` | passed, including clean packaged-JVM consumer execution | 214.012 s |
| `maven` | local publication passed | 34.448 s |
| `shopping` | 58 tests passed; required schema, desktop and Android compilation passed | 87.960 s |

The service selection used ordinary e2e capture/concurrency without a thread
override, retries or additional sync calls. Its fourth selector,
`a_device_granted_execute_registers_and_executes_without_being_an_issuer`,
is a stale catalog name with no corresponding current test: this is three
executed passes, not four. P5's earlier failure was not reproduced; its cause
remains unknown. The subset does not establish full-catalog behavior or repair
the earlier failure.

One bounded infrastructure attempt provisioned isolated QEMU 11.1.2 but did
not establish a suitable Linux ARM64 guest or actual AT_HWCAP evidence.
Both normal post-overlay software-fallback executions remain open. No Rocky
fixture build or run followed that unsuccessful feasibility check.

No final-source combined e2e pass, timing repair, phase20 artifact freeze or
phone qualification is claimed. Native links and packaged JVM execution do
not establish execution on every cross-built platform. The earlier combined
990.38/900 s failure remains; the individual 600 s gates remain required.
