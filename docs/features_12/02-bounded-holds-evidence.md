# Bounded holds: inventory and blocking findings

Status: stopped after inventory (2026-10-27); no runtime implementation.
Phase start: `ynnvllrn` (`4e4e2497`); investigation: `wxutrylm`, workspace
`bounded-holds`. Source, manifests and workspace metadata were inspected.
This is a design investigation, not new performance evidence.

## Execution contract

The brief requires durable bounded steps, an atomic bounded switch for changes
that readers must see together, and no persisted format change. The roadmap
requires stopping when a settled premise does not match code. The existing
projection-plan example does not have a bounded final switch: `plan_step` calls
`switch` inside one transaction; `switch` calls `verify_shadow` for every table;
`verify_shadow` loops over all stored rows before the table replacement. It
also creates indexes at the switch. `PLAN_ROWS` bounds each fetched batch, not
the acquisition or transaction containing that final verification.

This is a demonstrated mismatch with the brief's proposed example. It is not a
proof that every format-preserving solution is impossible. The investigation
stops before selecting a different architecture or changing persisted state.
The phase is not complete and its completion gates have not been attempted.

## Slice 1: measured holder inventory

Source: [phase 1 measurements](01-see-the-wait-measurements-evidence.md), release
`uuzywvqz`, physical Pixel 11 Pro, Android 17, 600 probe samples per scenario,
50 warm-up writes, in-memory transport. Acquisition counts vary by site.
The source records commands, sample counts and complete p50/p95/p99/max tables.
Sites below retain that revision's line numbers. Each row gives the largest
phone p99 and max across the recorded scenarios, in milliseconds. This union
includes every background site whose p99 exceeds 25 ms or max exceeds 100 ms,
including bootstrap preparations. It does not establish that these are all
possible expensive sites: phase 1 reports only the top ten per scenario.

All listed APIs can execute while a database is open; setup-only observations
are not evidence that an API is open-only. Units are identified from code;
phase 1 did not measure a per-unit cost for every category. No numerical bound
is proposed where the required sizing evidence is missing.

| Measured site | Phone p99 / max (ms) | Work, current bound and proposed investigation |
| --- | ---: | --- |
| `replica/sql.rs:1098:18` | 115.046 / 416.196 | Post-flush maintenance and WAL work; separate the phases and acquisitions; see maintenance/WAL rows below. |
| `replica/sql.rs:181:14` | 3.495 / 343.019 | SQL connection planning over schema tables; no work bound; measure planning cost and isolate metadata work. |
| `replica.rs:1498:36` | 177.444 / 434.701 | Deferred maintenance over scopes plus checkpoint; one seal can still scan/rewrite a scope; resumable seal steps needed. |
| `replica/group.rs:147:32` | 3968.132 / 4051.561 | Receive group; 64 requests / 4 MiB plus pending pass; split by independently measured admission and application work, retaining one flush. |
| `replica/schemas.rs:502:14` | 501.349 / 531.941 | Required-schema metadata/history traversal; no work bound; bounded scan with a stable snapshot or revision validation. |
| `replica/bulk.rs:453:32` | 514.861 / 516.542 | Missing-publication discovery plus required-schema traversal; no work bound; bounded metadata scan and revision validation. |
| `replica/group.rs:185:36` | 167.346 / 171.389 | Post-flush maintenance/checkpoint; includes multiple work categories; isolate before sizing. |
| `replica/blockers.rs:510:14` | 3.290 / 369.896 | Blocker computation over stored state; no total bound; bounded scans with coherent result validation. |
| `replica/tips.rs:1502:36` | 1.401 / 352.798 | Tip statement page; output at most 32, but prerequisite checks can scan more; bound prerequisites separately. |
| `replica/held/late.rs:485:36` | 10.086 / 361.404 | Held refresh then frame page; output at most 32, refresh scans operations; durable or recomputable refresh cursor needed. |
| `replica/compaction/rebootstrap.rs:318:32` | 5.286 / 355.849 | Rebootstrap discovery over scopes/history; no total bound; bounded validated discovery. |
| `replica/compaction.rs:1977:36` | 89081.073 / 89081.073 | Finalization plus compaction before pruning; replay batches 64 frames / 256 rows but enclosing acquisition unbounded; staged scope state needed. |
| `replica/tips.rs:1319:36` | 54879.111 / 54879.111 | Tip admission and workspace rebuilding; statement input is finite but induced row work unbounded; investigate atomic staged workspace transition. |
| `replica/publication.rs:344:36` | 995.679 / 995.679 | Publication listing and progress scans; no total bound; paginate internal scans and validate result consistency. |
| `replica/tip_observation.rs:198:36` | 221.224 / 221.224 | Earlier-epoch history scan; no total bound; resumable scan with commit/tip invalidation. |
| `replica/tip_observation.rs:187:14` | 146.001 / 146.001 | Seal frontier computation; no total bound; resumable scan with commit/tip invalidation. |
| `replica/schemas.rs:1260:36` | 31.956 / 31.956 | Schema retention discovery and deletion; no total bound; stage/validate kept references before bounded deletion. |
| `replica/compaction.rs:2024:36` | 36.619 / 36.619 | Finalize-path entry; unlimited seals and row work inside each; bound both levels, preserving domain invariants. |
| `replica/checkpoint/transfer.rs:1715:36` | 39520.465 / 39520.465 | Checkpoint installation; whole-scope live row/projection/history replacement; blocked on atomic staged-switch design below. |
| `replica/compaction/rebootstrap.rs:327:36` | 155.047 / 155.047 | Replay-at-seals reconstruction; whole-scope restoration; same staged row/projection/history design needed. |
| `replica/checkpoint/transfer.rs:1661:36` | 208.811 / 256.917 | Checkpoint page storage; 256-row producer steps, signed pages committed together; investigate bounded storage with safe replay of partial pages. |
| `replica/schemas.rs:1152:14` | 60.128 / 60.128 | Live schema switch; existing projection plan bounds fills but final verification/index creation are unbounded; settled-pattern mismatch. |
| `replica/placements.rs:632:36` | 88.214 / 88.214 | Placement headers/custody page processing; finite page plus induced metadata work; measure and bound each category. |
| `replica/incoming.rs:15:40` | 290.558 / 290.558 | Settle pass; GROUP_REQUESTS=64 applications per pass; separate batch costs from fixed work before resizing. |
| `replica/roles.rs:958:36` | 124.205 / 124.205 | Restoration completion check and guard removal; history-dependent work; bounded validated completion scan. |
| `replica/roles.rs:1103:14` | 11.882 / 320.297 | Custody frontier recording/validation; no measured per-unit bound; investigate bounded validation preserving atomic acknowledgement. |

Additional required responsibilities not independently sized in those tables:

| Responsibility | Current work limit | Next requirement |
| --- | --- | --- |
| Pruning | `STEP_OPERATIONS=64` production, 8 in tests | Measure selection/retention/custody work as well as deleted frames; release between committed steps. |
| Page reclamation | `RECLAIM_STEP=4096` pages after freelist reaches 1024 | Measure incremental vacuum separately; phase 1 has no reclamation per-page histogram. |
| Passive WAL checkpoint | Whole currently available log, while holding replica | Dedicated connection and blocking task as brief specifies; preserve size-trigger and shutdown ownership. |
| Truncating WAL checkpoint | 64 MiB trigger, 50 ms reader busy timeout | Retain writer exclusion and report measured target misses explicitly. |
| Checkpoint export | Row batches and signed page construction | Count all work per acquisition, including headers/provenance, rather than just emitted rows. |
| Projection rebuild API | Whole scope inside one transaction | Available on an open replica; not exempt merely because rebuilding also happens during open. |

Open-only exemption applies to work reachable exclusively before
`Database::open` returns, including startup recovery, startup rebuilding and
migration publication. Shared methods still need bounds on their live paths.
No exemption for the live schema-switch or replay APIs above was established.

## Why the checkpoint switch is not a row-limit edit

`finish_rebootstrap` verifies staged completion and the signed summary, then
calls `switch_to_staged` inside one `transaction!`. The switch:

- checks provenance and identity claims and classifies retained operations;
- deletes and replaces live `secure_row_records` for a scope when needed;
- rebuilds row indexes, move indexes, constraints and typed projections;
- replaces base rows, logs, page metadata, stubs and identity ledger;
- replays retained operations above the incoming checkpoint;
- clears staging and advances the revision.

Direct imports already expose committed pages where that mode is permitted.
They are not a general substitute: `rewrite = !direct || applied_any(id)`
forces replacement for non-direct imports and direct imports with applied
operations. The atomic-replacement concern below applies to those paths.

The live row view maps directly to `secure_rows` using the scope id. It has no
active generation selector. `secure_rebootstrap_rows` can hold an incoming
checkpoint invisibly, but the installation still copies its rows into live
storage. `secure_projection_plan` stages typed projections from live row
records; it is not a shadow for the full row/history/operation state. Its
final verification is also unbounded, as noted above.

Committing that switch every N rows would expose mixed row/projection/history
states. A crash could retain an intermediate mixture without a durable install
phase explaining it. Keeping the transaction open while unlocking would allow
another caller onto the same transaction-bearing connection and would keep the
SQLite writer occupied. Splitting only internal SELECT batches bounds memory,
not lock hold or SQLite transaction size.

An implementation needs a reviewed state machine for invisible staging,
concurrent writes and invalidation, resumable replay and provenance checks,
atomic publication, and bounded cleanup. A generation/bank selector plus a
durable install journal is one possible design, but would change persisted
shape and is prohibited in this phase. That is a candidate solution, not an
implemented design or proof that a format-preserving design cannot exist.
Before resuming, either establish such a design using the current format, or
revise the brief's format constraint and coordinate its single version change
with phase 5. Do not silently overload existing checkpoint/projection cursors
with a new persisted meaning.

## What a solution must establish

A design proposal must identify the complete staged state, its stable source
cut, how writes during preparation are mirrored, replayed or invalidate work,
and the exact bounded transaction that makes the replacement visible. Merely
moving projection verification earlier creates a race unless intervening writes
maintain the verified invariant. Index construction belongs before publication,
with its own work bound; old generations need bounded cleanup after publication.

For each durable step, progress and its row/history changes must commit together.
Reopening must distinguish incomplete preparation, a fully prepared replacement,
a committed switch and unfinished cleanup. Existing checkpoint commitments,
identity/provenance checks, handle retries, revision and flush ordering must
retain their meaning. Scope ids are cached and documented as never reassigned;
a scope-id swap is not an established shortcut.

Tests must interrupt after each step and compare resumed rows, projections,
operation statuses, identities, logs, base summaries and audit results with an
uninterrupted run. Interleave local writes, incoming frames, grants/revocations
and tip changes at deterministic boundaries. Count actual work per acquisition,
including final verification, index work, replay and cleanup. Then rerun the
brief's performance and sync qualification; row-batch tests alone do not qualify
the latency target.

## Measurement constraints

Desktop base installation holds for 2.356266 seconds on 100k rows. Pixel records
2.635374 seconds and 39.520465 seconds for 100k rows; a separate one-row install
holds for 149.181 milliseconds. Pixel L3 compaction acquisition p99/max reaches
89.081073 seconds. These are baseline measurements, not new runs.

Phone per-frame costs in phase 1 charge the entire acquisition to each frame:
Pixel L2 Flushed p99 is 68.565666 ms/frame, exceeding the brief's 12.5 ms sizing
budget even at one frame. That does not prove that applying one frame costs
68 ms: the acquisition includes admission, settlement and other fixed work.
Similarly, L3 aggregate replay/base-row costs cannot size an individual replay
step. Measure the newly separated steps before deriving constants. A zero
bound or a wall-clock cutoff would not satisfy the brief.

Truncation intentionally keeps a 50 ms reader wait; Pixel L4 Flushed truncation
p99 is 58.044 ms and max 149.388 ms. The brief explicitly retains that wait and
requires reporting misses. This is a qualification constraint, not a reason to
silently change the timeout or a proof that all bounded work is impossible.

## Ordered slices and validation

1. Inventory recorded; unresolved units/designs called out above.
2. Group/settle splitting and shared flush: not implemented.
3. Dedicated passive checkpoint connection: not implemented.
4. Other resumable holders: not implemented; stopped on settled-pattern mismatch.
5. L0–L4, paired sync, equality/crash and 100k migration qualification: not run.

No runtime, persisted format, admission, durability or wire behavior changed.
Validation from the main workspace:

- `./scripts/lint` passes; the isolated workspace initially lacked the local
  `ktlint` installation, so lint was rerun successfully after integration.
- `./scripts/commitlint --jj` passes.
- `./scripts/check` passes: 204 unit tests and 1,438 integration tests, including
  SDK without default features. These baseline tests do not qualify phase 2.
- The inventory/status change is integrated in the main workspace; the
  `bounded-holds` Jujutsu workspace and its temporary directory are removed.
