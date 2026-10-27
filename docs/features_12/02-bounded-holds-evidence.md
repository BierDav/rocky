# Bounded holds: inventory and blocking findings

Status: partial implementation in `rocky-bounded-holds`, change `pkslrrwp`.
Updated 2026-10-29 under the owner's revised sizing and scope below.
Compiled defaults are 128 checkpoint rows, eight immutable metadata records
and four one-row receive frames. They pass the aggregate paired member-pull
gate against phase start. Fresh phone calibration retains unresolved storage
outliers; it does not establish uniform latency qualification. Workspace Clippy,
207 unit tests and 1,466 integration tests pass. Surviving holders and final
phase qualification remain unfinished.
Phase start: `ynnvllrn` (`4e4e2497`); investigation: `wxutrylm`, workspace
`bounded-holds`. Source, manifests and workspace metadata were inspected.
The initial inventory is a source investigation. Later dated sections record
implementation and measurements separately.

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

## Replanning (2026-10-27)

The owner accepted a re-scope after reviewing this inventory against the
phase 1 probe breakdown. In most scenarios, phase 1's lock wait and hold
account for little of a write's time. Examples: Pixel L0 Buffered write
p95 359.850 ms against lock wait 0.009 ms and hold 25.841 ms, and Desktop
L3 Buffered write max 132.867 s against lock wait max 0.032 ms. The
database reactor serves all tasks with `spawn_local` on one `LocalSet`
thread, and holders run synchronous SQLite on it. A call that is never
polled records no lock wait. This is an inference from code and numbers,
not yet measured; [phase 1b](01b-see-the-reactor.md) measures it.

Resulting plan:

- Phase 1b measures reactor stalls and removes interactive-path detours.
- Phase 2 covers steady-state holders only, with no format change.
- The atomic whole-scope replacement described above becomes a
  [transition](07-transitions.md), deferred behind an evidence or product
  trigger. A format change is allowed there.
- Phase 3 keeps transitions from starting while interactive work waits.

Slice 1 continues: re-label each row above as steady state or transition,
with its reason, once phase 1b's poll table is available.

## Resumed inventory (2026-10-27)

Implementation workspace: `rocky-bounded-holds`, change `pkslrrwp`, based on
`qyxtuntm` / `0f08fe4b` (phase 1b integrated). Source review confirms named
reactor polls, Interactive SQL status, deferred SQL maintenance, and the
maintenance watermark gate. No nested AGENTS instructions apply to the secure
crate or roadmap.

The classifications below supersede the original investigation's proposed
scope. A mixed acquisition must separate its steady-state work even when it
also invokes a transition. Classification alone does not bound that work.

| Inventory responsibility | Classification and atomicity reason |
| --- | --- |
| SQL post-flush work (`sql.rs:1098`) | Steady state; phase 1b moved it onto the maintenance driver. |
| SQL connection planning (`sql.rs:181`) | Steady state; read-only schema planning does not publish a replacement. |
| Deferred maintenance (`replica.rs:1498`) | Mixed: stale metadata cleanup and WAL work are steady state; seal finalization replaces a scope's replay state atomically and is a transition. |
| Receive group (`group.rs:147`) | Steady state; requests already have independent savepoints and stable retry identities. Multi-frame bundle application remains indivisible. |
| Required schemas (`schemas.rs:502`) | Steady state; computes schema references and can validate its source state. |
| Missing publications (`bulk.rs:453`) | Steady state; discovery does not replace live rows or history. |
| Group post-flush work (`group.rs:185`) | Mixed, with the same maintenance split as the deferred driver. |
| Blocker computation (`blockers.rs:510`) | Steady state; a read-only result requiring source validation. |
| Tip statement prerequisites (`tips.rs:1502`) | Steady state; read-only discovery, separate from tip admission. |
| Held refresh (`held/late.rs:485`) | Steady state; individual closed bundles can be classified independently, retaining each bundle's atomicity. |
| Rebootstrap discovery (`compaction/rebootstrap.rs:318`) | Steady state; lists needs without switching scope state. |
| Compaction (`compaction.rs:1977`) | Mixed: finalization and compaction replace replay bases/history atomically (transitions); subsequent pruning is steady state. |
| Tip admission (`tips.rs:1319`) | Transition; admission can rebuild the live workspace and must publish its graph and rows together. |
| Publication progress (`publication.rs:344`) | Steady state; computes listing/progress metadata. |
| Earlier epochs (`tip_observation.rs:198`) | Steady state; scans history to compute coverage. |
| Seal frontier (`tip_observation.rs:187`) | Steady state; computes a frontier rather than replacing scope state. |
| Schema retention (`schemas.rs:1260`) | Steady state; discover references, validate them, then independently delete unreferenced catalog entries. |
| Finalize path (`compaction.rs:2024`) | Transition; finalized scope state and replay history must change atomically. Read-only eligibility discovery remains steady state. |
| Checkpoint installation (`checkpoint/transfer.rs:1715`) | Transition; replaces live rows, projection, identities, base and retained history together. |
| Replay at seals (`compaction/rebootstrap.rs:327`) | Transition; restores whole-scope rows and replay state atomically. |
| Checkpoint page storage (`checkpoint/transfer.rs:1661`) | Steady state; signed pages have independent durable identities and partial imports already exist. |
| Live schema switch (`schemas.rs:1152`) | Transition; readers must see one schema and its matching projection/workspace. |
| Placement headers/custody (`placements.rs:632`) | Steady state; independent metadata steps, with validated custody results. |
| Settle (`incoming.rs:15`) | Steady state; independent requests/bundles apply atomically, without a wholesale scope replacement. |
| Restoration completion (`roles.rs:958`) | Mixed: completion discovery and guard removal are steady state; `settle_seal_rebootstrap` can install pulled checkpoints or rebuild a workspace, both transitions. |
| Custody frontier (`roles.rs:1103`) | Steady state; validate history before acknowledging the recorded frontier. |
| Pruning | Steady state; deleted prefixes are independently valid after each committed step. |
| Page reclamation | Steady state; each incremental vacuum step is independently valid. |
| Passive WAL checkpoint | Steady state; does not replace application-visible state. |
| Truncating WAL checkpoint | Steady state with the brief's explicit writer exclusion and retained 50 ms reader wait. |
| Checkpoint export | Steady state; read pages and signed export metadata require coherent validation. |
| Projection rebuild API | Transition; replaces a whole scope's derived live projection atomically. |

Phase 1b adds task-level poll attribution, not a new per-acquisition holder
inventory. Its final desktop and Pixel tables identify `sql`, `call`,
`settle_commits`, `recovery`, `connect`, `database`, `synchronize`, `live_link`
and `publish_links` polls. Those task names cannot alone distinguish transitions
from steady-state work. The observed L3 compaction/finalization transitions
remain separately recorded in phase 1b; their aggregate polls must not be
reclassified wholesale as exempt. Pending scans, frontier/schema/blocker
queries, and checkpointing reached through those tasks remain in phase 2.

## Receive and WAL implementation in progress

`pkslrrwp` splits a drained receive/author cohort into homogeneous, bounded
transactions. The leader yields after releasing the replica, keeps exposed
replies across the transactions, and flushes through the last commit before
answering them. Buffered replies retain their original commit identity.
Pending replies resolve after the complete cohort settles, so a bundle's early
members are not prematurely reported as waiting while later members in the
same cohort have yet to arrive.

Pending settlement now discovers one log head per step through the existing
partial pending index, looks up quarantine state by frame identity, and applies
one indivisible request per step. Discovery, waiting-result cleanup and cursor
reset release between steps too. Its in-memory cursor validates both the commit
and an invalidation counter; another writer restarts discovery. A restart drains
its work sets incrementally. Read-only discovery does not open a write transaction.
Move-following after settlement still needs its own bound and is not qualified.

The passive WAL connection is opened with the replica, verifies the existing
flush settings, and uses the blocking pool. A short state mutex suppresses
duplicate jobs. An epoch prevents an old passive job from restoring the previous
WAL's size mark after truncation. The file owner remains alive through the
blocking task. Truncation retains the replica writer and 50 ms reader wait.
The existing owner `optimize` step waits outside the replica for passive work.

Focused group tests passed (17 executions before pending discovery pagination):
several commits share one flush, mixed exposed/buffered replies flush through
the last commit, a fault between commits keeps earlier handles and retries later
ones, process exits preserve whole requests, a passive checkpoint finishes while
the replica is held, and truncation waits for the writer. Larger tests are being
rerun after pending pagination. These are not the phase completion gates.

Separating custody from application moves the receipt rollback fault to the
application transaction. The receipt fixture stages custody before capturing its
before-state, then injects a failure before application commits. Its existing
receipt, identity, row and audit equality assertions remain. Incoming application
transactions explicitly retain the `Sync` observer cause across the new boundary.

## Receive-step sizing

Change `pkslrrwp`; physical Pixel 11 Pro, Android 17, arm64 release native
`rocky-secure` unit-test binary, one test thread, foreground shell execution.
Phone power state and other system activity were not controlled. This is sizing
of separated steps, not a before/after speedup or a completed L0–L4 matrix.
The fixture uses one-row and 32-row bundles plus a maximum 64-frame / 2,048-row
bundle. Twelve fresh targets per large-bundle profile; 64 fresh one-row targets,
then 64 updates on a warmed target. Admission uses `transaction!(@commit)` and
application is a separate atomic `apply_frame` transaction. WAL work, maintenance
and explicit device flush are outside those timers. Empty-settle timing covers
one metadata step after the application. Validation checks row counts and every
member's final status; large snapshots use SQL counts rather than exceeding the
public snapshot API's 1,024-row quota.

```sh
cargo test --release -p rocky-secure --lib --target aarch64-linux-android --no-run
# Push the produced native test binary; run with TMPDIR=/data/local/tmp:
rocky-bounded-holds-tests --exact tests::group::measure_separated_receive_steps \
  --ignored --nocapture
```

Durations below are microseconds, using nearest-rank percentiles. Each admission
sample is one transaction, not a whole receive acquisition divided by its frame
count. Batch sizes are frames admitted in that transaction.

| Profile | Samples | p50 | p95 | p99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: |
| One-row admission, fresh | 64 | 5,201 | 7,040 | 35,502 | 35,502 |
| One-row atomic application, fresh | 64 | 11,068 | 13,954 | 343,005 | 343,005 |
| Empty settlement step, fresh | 64 | 710 | 1,079 | 334,802 | 334,802 |
| One-row admission, warm updates | 64 | 3,027 | 3,917 | 14,296 | 14,296 |
| One-row atomic application, warm updates | 64 | 4,889 | 6,337 | 8,466 | 8,466 |
| Empty settlement step, warm | 64 | 791 | 1,101 | 1,122 | 1,122 |
| 32-row admission | 12 | 5,205 | 8,287 | 8,287 | 8,287 |
| 32-row atomic application | 12 | 19,952 | 384,264 | 384,264 | 384,264 |
| Maximum bundle atomic application | 12 | 226,935 | 1,060,734 | 1,060,734 | 1,060,734 |
| Admission batch 1, maximum-bundle frames | 768 | 970 | 1,821 | 3,663 | 154,704 |
| Admission batch 2 | 384 | 1,478 | 3,236 | 7,314 | 11,672 |
| Admission batch 4 | 192 | 2,732 | 7,225 | 360,476 | 368,631 |
| Admission batch 8 | 96 | 4,164 | 7,823 | 37,744 | 37,744 |
| Admission batch 16 | 48 | 7,428 | 25,137 | 370,250 | 370,250 |
| Admission batch 32 | 24 | 16,507 | 39,493 | 406,427 | 406,427 |
| Admission batch 64 | 12 | 30,329 | 425,886 | 425,886 | 425,886 |

The two-frame admission bound has measured p99 7.314 ms and max 11.672 ms,
below the 12.5 ms sizing budget in that profile. A 32 KiB cap can cut it sooner;
a larger indivisible request still runs alone. One nominal application row per
step uses the warm one-row p99 8.466 ms. Each step processes at most one atomic
application; larger frames/bundles are explicitly counted as oversized, with all
materialized member frames and rows counted rather than one bundle trigger.

The fresh one-row application and empty-settle tails **miss** the phone hold
and sizing targets. Warm one-row admission also misses the 12.5 ms sizing
budget. These are not transitions or oversized-row exemptions. The near-350 ms
outliers are observed wall time; their cause is not established by these
measurements. No universal phone latency claim is made and no target is relaxed.
The ignored measurement passed in 78.32 s. Earlier exploratory runs that charged
the complete pending pass to application, or failed on the snapshot quota, are
not used to size a bound.

## Preliminary paired desktop sync guard

Apple M5 Pro (`Mac17,8`), 18 logical CPUs, arm64 macOS, release,
`rocky-sdk --no-default-features --features qualification`, one full 100k fixture
per revision. Baseline `rzszssxw` / `0b097aa0` has the phase-start crate sources.
Candidate `pkslrrwp` here used one-request admission steps before selecting the
two-request bound and before move-following pagination. This is an intermediate
regression guard, not final phase qualification. Both row/fingerprint, frame,
wire and file assertions passed unchanged.

```sh
cargo test --release -p rocky-sdk --no-default-features --features qualification \
  --test migrations a_migration_of_100000_rows_converges_on_every_member \
  -- --exact --nocapture --test-threads=1
```

| Measure | Phase-start sources | Intermediate candidate |
| --- | ---: | ---: |
| Samples | 1 | 1 |
| Member pull (p50/p95/p99/max, one sample) | 9.555557167 s | 9.785910416 s |
| Member main file | 68,149,248 B | 69,947,392 B |
| Member WAL | 33,554,432 B | 45,748,512 B |
| Publication | 49.268548375 s | 53.162554583 s |
| Issuer main file | 125,571,072 B | 111,493,120 B |
| Full fixture | 256.91 s | 261.93 s |

Member-pull ratio is 1.024107, below 1.10; publication ratio is 1.079036.
Builds/checks overlapped fixture preparation, so issuer-open times are not a
controlled performance comparison. No other host builds ran during either
member-pull sample. One pair does not establish a throughput distribution.

## Continuation: checkpoint regressions and induced work (2026-10-28)

The implementation remained in `~/AgentWorkspaces/rocky-bounded-holds`,
change `pkslrrwp`, after the previous run stopped. The main workspace has no
implementation changes. The phase-start comparison workspace remains available
as `rocky-bounded-holds-baseline`.

Repairs made while resuming:

- Validate shared creation clocks over the entire authenticated page before
  storing metadata or rows. Check stored creation clocks and direct-import
  collisions in read-only, `STORE_ROWS`-sized acquisitions before any writes.
  Validate the commit and authority across those acquisitions and through the
  storage steps; intervening writes return `RetryExhausted` for a safe retry.
- Look up durable import prefixes in `secure_checkpoint_rows` for direct
  imports. Live `secure_rows` can also contain local writes made while an
  import is incomplete; those are not rows of its signed checkpoint.
- Charge the test-only interruption budget to transactions that apply an
  operation. A transaction that merely rechecks a waiting log head must not
  consume it: otherwise reopening with a one-application budget repeatedly
  stops before the first ready log and the crash-resumption test never advances.

Focused validation before adding the fan-out regression: all 54 active
`tests::checkpoint` tests passed (one process helper ignored), and
`tests::sql::catch_up::an_interrupted_catch_up_resumes_and_applies_every_operation_once`
passed. The latter includes reopening and retrying receipts with faults between
application transactions. These results repair the three failures and timeout
left by the interrupted run; they do not qualify the whole phase.

The remaining concern is induced constraint work. A direct storage step calls
`project` for its imported records and then `resolve(None)` in the same
transaction (`replica/checkpoint/transfer.rs`, `store_page`). The resolver
(`replica/constraints.rs`, `resolve` and `Pass::dependents`) can fetch every
child of a parent and update all affected typed rows before that transaction
commits. Neither its dependency query nor its propagation queue has a row bound.
The imported record count alone therefore does not bound this acquisition.

A deterministic regression,
`tests::constraints::one_checkpoint_parent_reveals_all_previously_imported_children`,
imports child rows before their parent, then imports a signed checkpoint with
exactly one parent record. It checks the children before `finish_rebootstrap`,
so the whole-scope installation transition cannot explain the induced work.
The regression passes: 32 previously hidden child rows become visible while
importing the single parent record, before `finish_rebootstrap`. This is a
behavioral regression test, not a holder-latency measurement. The fixture
uses 32 children, but the schema places no bound on their number and the
resolver fetches and processes every dependent.

This is a new mismatch between the proposed input-row bound and the actual
steady-state holder, separate from the whole-scope transitions deferred earlier.
The same propagation is reachable from an ordinary received parent update.
Neither an input-record bound nor the existing frame/bundle size exception
establishes a bound on these induced typed-row changes. Exposing partial
constraint resolution would change the existing atomic row/projection contract.

Per the roadmap execution contract, further phase implementation stops here
rather than silently extending the oversized-request exception or moving
ordinary constraint propagation into phase 7. Resumption needs a design that
bounds dependency discovery and the final atomic publication, or an explicit
revision of the scope/exception. This is evidence that the current row-limit
implementation is insufficient, not proof that every format-preserving design
is impossible. Remaining metadata holders and final performance qualification
are still unfinished. The incomplete change is not integrated and neither
temporary workspace is deleted.

The full workspace check found additional unresolved failures. All 204 unit
tests pass. The workspace integration run completed with 1,122 passed, six
failed and one timed out out of 1,129 executions. Five failures report
`Sql(Unavailable { table: "items", reason: Incomplete })` in ordinary sync
scenarios:

- `a_stalled_bootstrap_peer_does_not_hold_the_checkpoint_import_slot`
- `two_writers_push_concurrently_and_converge_on_the_same_scope`
- `two_automatic_workers_converge_and_then_stop_dialing`
- `a_burst_exceeds_push_bound_and_finishes_by_link_catch_up_in_log_order`
- `peers_progress_before_stalled_dial_and_signals_respect_peer_backoff`

`interrupted_parallel_join_preserves_pending_scopes_after_reopen` fails its
pending-scope readiness assertion.
`three_failed_advised_imports_fall_back_only_for_that_scope_and_checkpoint`
times out at the configured 30-second limit. Expectations and budgets are
unchanged. The ordinary-worker failure also reproduces alone:

```sh
cargo nextest run --workspace \
  -E 'test(two_automatic_workers_converge_and_then_stop_dialing)' \
  --test-threads 1
```

The candidate commits custody before application and releases between them,
creating an additional reader interleaving. Whether that alone explains all
readiness failures has not been established; the exact cause and the import
fallback timeout remain unresolved. They must be repaired while preserving the
existing read/admission contract before integration. The standalone reproduction
means the readiness failure cannot be dismissed merely as parallel-suite load.

`./scripts/lint` passes with the main workspace's installed `ktlint` on `PATH`;
`./scripts/check`'s Clippy stage and `./scripts/commitlint --jj` pass. The SDK
without default features completed with 315 passed, the same five ordinary-sync
readiness failures, and three timeouts out of 323 integration executions. Its
timeouts are the advised-import fallback above,
`a_cut_join_resumes_its_pages_and_keeps_edits_from_both_sides`, and
`edits_relayed_in_any_order_and_repeated_converge`. Across both configurations,
the initial integration runs have 1,437 passes, 11 failures and four timeouts;
`./scripts/check` exits 1. After its budget reruns, it additionally reports
`a_deferred_branch_switch_pulls_a_kept_seal_and_survives_reopen` failing alone
in both configurations, and
`a_trusted_identity_opens_a_branch_that_devices_on_main_never_see` taking
2.26/2.28 s against its unchanged 2 s integration budget. The integration
suite's 283.58 s wall time is within the unchanged 300 s suite budget; the
failures and individual budget misses still fail the check. There is no new
paired baseline for these timing results.

L0–L4, new phone calibration, final paired sync and the 100k migration gate were
not rerun in this continuation. No SDK or binding implementation was edited;
Kotlin and shopping app checks were not rerun. The main workspace remains clean.

## Readiness repair after handover (2026-10-28)

The owner requested continued implementation. Work remains in `pkslrrwp`;
no scope exception, format change, integration or workspace deletion is implied.
The induced-work finding above remains unresolved.

`run_group` now attempts a newly admitted (or retried pending) frame in the
same SQLite transaction as custody. A ready request therefore cannot expose
its newly pending status between custody and application. Application shares
the pending pass's quarantine handling, but does not scan unrelated logs.
Previously blocked work still uses incremental discovery/application steps.
Application failures roll back their savepoint while retaining custody, and
return the failure; injected interruption budgets cover this attempt too.

Receive transactions now contain one request. Author transactions retain their
two-request bound, and a cohort still has one shared flush. The receive-page
test expects ten transactions for ten valid frames and still requires one
flush. A new concurrent-reader regression checks every observed stored status
of a ready single frame, including interleavings before the receive reply.

These combined admission/application acquisitions require new cost calibration.
The earlier separate-step measurements do not qualify this implementation.
The format-preserving fan-out repair must also bound final publication, not
only dependency discovery: with the current materialized typed tables, making
N hidden children visible requires N typed-row inserts in the atomic commit.
Precomputing the desired images across acquisitions does not bound that final
work. No replacement representation or wider oversized-request exception has
been selected.

## Approved dependent fan-out exception (2026-10-28)

The owner explicitly approved a narrow exception for the constraint resolver's
induced reveal/hide of children when a parent is received, imported or updated.
It preserves atomic row/projection commit and permits no partial child reveal.
Every other holder keeps its input-row bound; unique-peer work and unrelated
metadata scans are not added to this exception. Test and qualification support
record `constraint_dependent_row` per acquisition, excluding direct input slots.
Deterministic tests compare this count with the parent's dependent count.

Measure and report cost per induced row, including 1k/10k-child cases, separately
from steady-state hold targets and overlapping interactive latency samples.
Phase 3 starts a fan-out step exceeding the normal bound only when no
interactive write is waiting. This exception ends in **features_13**, when
constraints move into replay. It must not become a permanent or broader
oversized-request exemption. The phase 2/3 briefs and roadmap now record this
owner decision; the earlier stop remains historical evidence, not current scope.

## Dependent fan-out counts and desktop measurements (2026-10-28)

Change `pkslrrwp`, arm64 Apple M5 Pro (`Mac17,8`), 18 logical CPUs, macOS,
release native `rocky-secure` unit-test binary. A concurrent debug SDK build
was running during these measurements (host load 38.09/68.31/84.28 observed
near the run). These are loaded-host exception samples, not phone calibration,
paired speedup evidence or qualification of ordinary steady-state holders.

```sh
cargo test --release -p rocky-secure --lib \
  tests::constraints::measure_checkpoint_parent_fanout \
  -- --exact --ignored --nocapture
```

Three fresh source/target fixtures per size. Children are imported first and
remain hidden; one authenticated parent checkpoint row reveals all children
in one storage acquisition before `finish_rebootstrap`. Every sample asserts
that the acquisition's `constraint_dependent_row` count equals the child count.
The reported cost is the full parent-storage acquisition divided by that count,
including transaction/verification/publication overhead, not isolated resolver
CPU. Metadata-only acquisitions and fixture construction are outside the timer.

| Children | Samples | Hold p50 / p95 / p99 / max (ms) | Per induced row p50 / p95 / p99 / max (µs) |
| --- | ---: | --- | --- |
| 1,000 | 3 | 30.426 / 52.878 / 52.878 / 52.878 | 30.426 / 52.878 / 52.878 / 52.878 |
| 10,000 | 3 | 184.227 / 222.059 / 222.059 / 222.059 | 18.423 / 22.206 / 22.206 / 22.206 |

Individual holds (µs): 1k = 18,411, 52,878, 30,426; 10k = 184,227,
163,899, 222,059. The largest observed hold is **222.059 ms for 10k children**;
this is an observed fixture maximum, not a universal maximum fan-out. These
samples are excluded from the ordinary steady-state targets under the approved
exception and reported here explicitly. No ordinary target is relaxed.

Test/qualification instrumentation counts only changed dependent typed slots,
excluding direct input slots and unique-index peers. The count is taken at
acquisition entry/exit through the existing lock recorder. Oversized dependent
fan-out has its own `dependent_fanout_us` histogram; the ordinary hold histogram
excludes those acquisitions. The write probe merges and prints this separate
exception series, and preserves per-unit costs. Checkpoint storage supplies its
normal row bound to the recorder. The deterministic 32-child reveal and
received-parent hide tests pass; all eight active constraint tests passed in
the focused run (one ignored measurement). Later instrumentation assertions
are still awaiting the final workspace checks.

The ordinary automatic-worker regression and the other four prior `Incomplete`
SDK cases pass in focused runs after ready requests apply with custody. The
advised-import fallback, cut-join and relayed-edits cases also pass in focused
runs. The latest targeted parallel-bootstrap check passes; deferred branch
switching still times out and is under investigation. These targeted results
do not replace a full final check. The first check after handover passed
Clippy and 204 unit tests before it was stopped to rebuild consistently after
the approved fan-out instrumentation changes.

## Scope-specific checkpoint validation (2026-10-28)

The deferred branch-switch reproduction showed `RetryExhausted` while importing
a kept seal, with its item scope failed and its registry scope synced. Page
preflight used the global commit count, so another scope's metadata transaction
could invalidate a page between any two of its bounded acquisitions. The fix
uses physical-row change counters selected by the imported scope id, plus the
authority and followed workspace. Unrelated cache invalidations do not count as
semantic changes. Each write step still checks the current signed statement.

The storage adapter's integer-key watches include trigger writes, scope moves
and rolled back mutations. Shared counters are indexed by table/column/key;
a row write does not walk all active import consumers. Dropping the last watch
removes its keys. The watch is in-memory only; no persisted or wire shape changes.

A deterministic pause after one preflight chunk verifies all three cases:
an unrelated metadata commit (including cache invalidation) proceeds; an import
state write or authority change returns `RetryExhausted` before any checkpoint
row commits. Retrying the unchanged page then completes and passes audit. The
storage watch regression passes. Five focused secure tests pass, including the
preflight test, exact reveal/hide counts, exclusion of unique peers, and separate
exception histograms. The deferred branch reproduction and final checks are
being rerun against this change.

## Maintenance cleanup in progress (2026-10-28)

The deferred branch-switch regression now passes with scoped page validation
(1.641 s in its targeted run). Workspace Clippy and lint passed before starting
the next holder slice. The temporary SDK diagnostic print has been removed.

Maintenance now walks pulled headers, orphan payload scopes/rows and quarantine
entries in independently committed steps, each visiting at most one input
record. A stale pulled header is removed first, making its payload unreachable
through the supported read APIs; payload rows, stubs and page signatures are
then reclaimed incrementally. A new completed pull's header protects its fresh
payload. No deletion cursor or format change is needed: orphaned payload is
recomputable from the absence of its header after a crash. Each scope/domain
is processed across separate replica acquisitions; wholesale finalization stays
a transition.

The SQL maintenance driver may continue its own cleanup commits after their
source state was flushed. A different writer still forces it to stop until
that newer state is flushed. Test/qualification support records
`maintenance_record` per acquisition. A new deterministic test checks both
input/deletion bounds and reopening during orphan cleanup; the existing
flush-race and recovery tests are being rerun. `CLEANUP_ROWS=1` is a provisional
work count, not a calibrated phone bound. Held refresh, other metadata holders
and final qualification remain unfinished; no integration or deletion has run.

## Held refresh and token pagination in progress (2026-10-28)

All nine focused maintenance/recovery tests pass, including flush-race behavior,
one-seal finalization, bounded orphan cleanup and reopen. Maintenance's count
remains provisional pending per-step calibration.

Held refresh now discovers signers through the existing position index and
reads at most 64 operation metadata records per step through the existing
(author,scope,sequence) index. It preserves the previous signer/log ordering,
opens at most one candidate body per acquisition, and commits a held bundle
atomically. A bounded metadata buffer is reloaded when operation writes change
its source. Retries restore the bounded cursor before another transaction
attempt. Existing generation checks still restart after authority/graph/key
changes. The generation-fingerprint computation and other metadata scans still
need their own bound; this change alone does not qualify the holder.

The approved inventory already classified closed bundles as independently valid.
The former whole-refresh rollback fault test now requires the committed first
bundle to survive a later bundle's failure, with unchanged live projection, no
partial failed bundle, and the same retry/reopen result. All 11 focused held
tests pass, including a 160-record live scan that opens no signed bodies.
Actual member frames and rows are counted separately from the one trigger
inspection, so a maximum bundle is not misreported as one row.

Held tokens are being changed to two validated bounded scans: count qualifying
hashes, then stream their existing CBOR array encoding through the unchanged
SHA-256 commitment algorithm. The incremental commitment builder has a fixed
reference vector. Pages bound raw input records before filtering obsolete held
headers, and advance across empty spans. Raw frame blobs are loaded only for
visible records that are considered for the byte quota. The new token tests
check the legacy digest, raw-row bounds and invalidation on an intervening
write. These additions and the complete workspace check are running now.

Qualification histograms also separate the existing oversized indivisible
request exception from both ordinary holds and the dependent fan-out exception.
Receive acquisitions count their application work after the eager-readiness
repair, and the fan-out classifier includes both direct input rows and induced
typed changes against the acquisition's normal row budget. No exception was
broadened. Final holder inventory, calibration, L0–L4, paired sync and migration
qualification remain unfinished; the isolated work has not been integrated.

## Combined receive calibration and latest checks (2026-10-28)

The ready-receive repair must be sized as one acquisition: custody and immediate
application commit together, preventing a reader from seeing artificial pending
custody. The earlier separated admission/application measurements above do not
size this repaired path. `RECEIVE_REQUESTS=1` is still provisional.

Physical Pixel 11 Pro, Android 17, arm64 release native test binary, one test
thread, foreground shell execution with `TMPDIR=/data/local/tmp`. The Device
panel was closed before measuring; power, temperature and other phone activity
were not controlled. `tests::group::measure_combined_receive_steps` measures the
actual `run_group` transaction for one ready one-row frame, through guard release,
excluding explicit flush, WAL maintenance and later empty settle work. It checks
that the receipt is applied and the final warm row has the expected image.
Fresh means 64 freshly opened targets; warm means 128 updates on one target.

| Combined one-row receive | Samples | p50 µs | p95 µs | p99 µs | Max µs |
| --- | ---: | ---: | ---: | ---: | ---: |
| Fresh target | 64 | 13,891 | 14,751 | 15,597 | 15,597 |
| Warm updates | 128 | 6,968 | 8,259 | 9,246 | 10,291 |

The test passed in 12.12 s. Both profiles are below the phone p99/max hold
thresholds, but the fresh profile **fails the 12.5 ms sizing gate even at one
input row**. This is ordinary work, not a transition or induced-work exception.
No target is relaxed and this measurement does not qualify L0–L4.
Log: `/tmp/rocky-bounded-holds-phone-receive.log`.

The preceding full `scripts/check` run passed Clippy and 207 unit tests; its
integration builds recorded 1,456 passes within 255.57 s of the 300 s wall budget.
It failed the held-refresh scan assertion and timed out the advised-import
fallback in both SDK feature builds. The isolated fallback reproduction on the
rebuilt code passes (1.852 s), which does not dismiss the suite timeouts. A
redundant empty end-of-signer query in held refresh has been removed. Readiness
now reads its own table/mutation counters, so another scan registering a wider
watch cannot invalidate it through unrelated operation writes. A new complete
check is running against the settled source. The concurrent-rotation budget
rerun also exceeded its unchanged 2 s individual budget (2.28 s). Lint and
Conventional Commit lint pass; these are not completion evidence for the phase.

## Dependent fan-out phone measurements (2026-10-28)

This is the approved **dependent fan-out exception**, reported separately from
ordinary steady-state hold targets. Physical Pixel 11 Pro, Android 17, arm64
release native test binary; same deployment conditions as combined receive
above. Three fresh databases per size. The ignored
`tests::constraints::measure_checkpoint_parent_fanout` test imports the children
hidden, then imports exactly one parent. Every sample asserts that its induced
typed-row count equals the parent's dependent count, all children are visible
before finishing rebootstrap, and audit agrees. No partial publication is used.
The timed value is the whole parent-storage acquisition, including its fixed
cost, divided by induced rows for the per-row figure; it is not isolated resolver
CPU time. There is no paired baseline or speedup claim.

| Induced children | Full holds µs (three samples) | p50 ms | p95/p99/max ms | p50 µs/induced row | Worst µs/induced row |
| --- | --- | ---: | ---: | ---: | ---: |
| 1,000 | 58,336 / 90,532 / 94,024 | 90.532 | 94.024 | 90.532 | 94.024 |
| 10,000 | 343,568 / 345,828 / 345,100 | 345.100 | 345.828 | 34.510 | 34.583 |

Nearest-rank percentiles with only three samples: p95/p99 are the maximum,
not a tail estimate. The 10k case exceeds the ordinary phone 100 ms maximum;
it is retained here explicitly and excluded from those targets under the named
exception. The test passed in 174.48 s, including fixture construction outside
the measured parent hold. Log: `/tmp/rocky-bounded-holds-phone-fanout.log`.

Phase 3 must delay such an oversized dependent step until no interactive write
is waiting; its starvation guard cannot bypass that requirement. This exception
ends in **features_13**, when constraints move into replay. These samples do not
extend it to unique-index peers, metadata scans, request rows or other holders.

## Held-change listing steps (2026-10-28)

The public held-change listing now reads one held header per acquisition,
decodes its stored bundle outside the replica, then reads one current row per
acquisition. It preserves held-id order and each bundle's row order. Before
returning the whole result, it validates watched held/row-table counters plus
authority, workspace tip and manifest. An unrelated metadata commit does not
invalidate it; relevant writes return `RetryExhausted` before exposing a mixed
listing. `LISTING_ROWS=1` is provisional pending phone calibration.

The new regression includes a three-row held bundle, checks one input per
acquisition (three headers and five current rows), and pauses after a header to
check unrelated metadata writes, held mutations and current-row mutations.
The listing retry must match the original result and leave the live projection
unchanged. The focused tests and a fresh full check are being run. A prior check
that overlapped module creation failed to compile the intermediate source; its
log is not qualification evidence. Remaining graph/generation metadata scans,
L0–L4, paired sync and the 100k migration gate are still unfinished. Neither
implementation nor comparison workspace has been integrated or removed.

The final focused held-refresh/listing/token, unchanged-history and histogram
run passed all 15 selected tests (1.657 s total). The three-row bundle regression
passed with eight separately acquired input records; relevant source mutations
were refused and the original listing was recovered on retry. Its projection
comparison checks the fixture's current pre-switch image, which remains
unchanged by listing. The full check and qualification-feature build are still
running; this focused result does not complete either gate.

`cargo check -p rocky-sdk --features qualification --all-targets` passes after
the new listing work counter. The full check's Clippy stage and all 207 unit
tests pass; integration feature builds are still being rebuilt. No L0–L4,
paired sync or 100k migration result is implied by these checks.

## Latest verification continuation (2026-10-28)

The complete check after listing changes has passed Clippy and 207 unit tests.
Its workspace integration run completed 1,137 executions: 1,132 passes and five
30 s timeouts (235.873 s wall). The timeouts were the runtime retention failure
reopen/audit test, advised-import fallback, cut-join resume, checkpoint seal
compaction/backpressure and compaction insert/delete churn. The new fan-out,
integer-watch, held-token and held-listing tests all passed in this run. The
without-defaults build also reproduced the fallback and cut-join timeouts.
The full script is still completing budget reruns. Kotlin qualification was
building native artifacts concurrently; observed runner load reached 131.3 on
18 cores. This identifies interference, not the cause of any timeout, and does
not excuse or remove the failed checks.

Five isolated, instrumented fallback runs passed (7.892 s total). The full
without-defaults failure reached the first `peer.sync` for the frame-fallback
peer: source compaction, enrollment, follow, link, page staging and recording
three failures had all completed. It did not reach the return from that sync.
Temporary `fallback:` stage prints in the SDK test record this; they must be
removed after diagnosis. Log: `/tmp/rocky-bounded-holds-fallback-diagnostic.log`.
The timed-out full run is `/tmp/rocky-bounded-holds-check-listing.log`.
This is unresolved; an isolated pass is not a repair or a passing phase gate.

The required Kotlin check is running (`/tmp/rocky-bounded-holds-kotlin-check.log`).
The shopping-app check was stopped during startup and must run after Kotlin,
so its empty startup log is not verification. No source changes were made by
that stopped check. The implementation and comparison workspaces are intact,
unmerged and still incomplete.

## Consumer artifact provenance correction (2026-10-28)

Kotlin `check` passed (202 tasks, 12m37s). The shopping-app check uses the
`rocky:rocky-room:0.1.0` artifacts installed in Maven Local, not the SDK source
checkout. I had not verified that native artifact before running it. Its JVM
native library SHA-256 is
`6e655a086937116e0432e219ea42c47d128e416f5d28638e478f746c9b8006e7`;
the candidate built by Kotlin qualification is
`47456f9391f85de04f26d9d9161485f3d1464771243067eb34c1dd7d16fd6b64`.
Consequently, neither the shopping failures above nor a run from the comparison
workspace establish a candidate regression. Both resolved the same installed
SDK. The comparison run had 68/70 JVM tests pass, with different failures in
migration/reopen tests; its checkout remained clean. It is not a paired native
runtime comparison.

The SDK-only instrumented suite passed all 150 tests (30.737 s), including the
fallback under concurrent tests. The shopping revocation test passed alone
(21.002 s). A second full shopping run again timed out rejoining a revoked
device and then stalled in the trusted-bootstrap test. Java/native thread
samples were captured, and only that hung check's processes were stopped.
These are observations about the installed artifact, not fixes to this phase.
Samples: `/tmp/rocky-bounded-holds-shopping-java-threads.log` and
`/tmp/rocky-bounded-holds-shopping-sample.txt`.

All temporary SDK trace statements and flags have been removed; both SDK
implementation/test diagnostic files have no diff. A unique candidate JVM
package is now being staged under the workspace's build directory, with no
write to Maven Local. A tracked-source consumer copy resolves only that
explicit repository and unique Rocky version. Its configuration is recorded
at `/tmp/rocky-bounded-holds-consumer.json`. Consumer qualification against the
actual candidate is still pending. Desktop and Android application builds have
not passed this phase's verification. The phase remains incomplete and unmerged.

## Continuation: migration accounting and consumer packages (2026-10-28)

Resumed change `pkslrrwp` in the preserved `rocky-bounded-holds` workspace,
following the source thread's opening handover, its last ten reasoning entries,
and its later owner approvals. Main has since advanced through documentation
changes, including removal of `features_12`; it has not received this candidate.
The isolated phase evidence remains here until integration is resolved.

The first resumed consumer attempt verified the staged JVM native digest but
failed before executing tests: Kotlin CLI 0.13 rejects `file://` repositories.
Switching the consumer copy to Maven Local and installing only the six unique
candidate-version directories preserved the existing `0.1.0` packages. That
attempt also failed dependency resolution, because KMP resolution requires the
Android and iOS variant publications even for the JVM check. All candidate
platform publications are now being staged with the same unique version. No
consumer result from either attempt qualifies the implementation.

`migration_storage` also held the replica while aggregating all migration
publications, frames, proof nodes and supporting existence history. It now
reads indexed keyset pages across separate acquisitions and computes charges
outside the lock. One validated watch covers mutations to all four source
tables, including intra-transaction and rolled-back writes; a relevant mutation
returns `RetryExhausted` before any mixed totals escape. Unrelated metadata
writes do not invalidate accounting. Supporting existence history is scanned
once, retaining the previous per-publication charge for overlapping ranges.
This changes neither quota admission nor durable counters or formats.

`STORAGE_RECORDS` is provisionally 32 (one in tests), with a separate
`migration_storage_record` per-acquisition work counter. Its new deterministic
test compares all three totals against direct SQL accounting, counts every raw
input row, checks mutations to each source table, and cancels and retries a
partially read scan. Focused tests and package staging are in progress. The
remaining holder bounds, phone sizing, failed full-suite gates, L0–L4, paired
sync and 100k migration qualification are still open. Nothing is integrated
or deleted.

The focused migration suite passes all 20 tests (1.102 s), including the new
accounting regression. Its initial fixture had existence logging disabled;
that run passed the 19 existing tests but could not exercise existence-record
mutation. The corrected fixture enables real existence logs before authoring
and the final test counts those records too. Lint and Conventional Commit lint
pass. The full Rust check is now running against the settled source, and a new
unique `20261028b` package version is being built for consumer verification.

The final package rebuild includes the JVM host, both Android ABIs and both
iOS targets. The Rust check's driver was paused while preparing its second
feature configuration, before further test execution, to let package builds
finish first. The source remains stable. This pause is scheduling only, not
a test result or a resolution of the earlier suite timeouts.

The full `./scripts/check` completed and exited 1. Workspace Clippy and all
207 unit tests pass. Workspace integration ran 1,138 tests: 1,137 passed and
one timed out (advised-import fallback), wall 176.626 s. The SDK without default
features ran 323 tests: 321 passed and two timed out (the same fallback and
cut-join resume), wall 112.076 s. Combined integration wall was 290.12 s within
the unchanged 300 s budget; all individual budget reruns passed. The accounting
regression passed in the workspace run (1.057 s). The three 30 s timeout
executions remain failures. Package builds had finished before integration
execution. This run does not establish their cause or resolve them.
Log: `/tmp/rocky-bounded-holds-check-storage.log`.

All platform publications of the final unique candidate version
`0.1.0-bounded-holds-pkslrrwp-20261028b` staged successfully (245 tasks,
11m24s). The consumer now resolves that exact version through Maven Local;
only unique candidate-version directories were installed, preserving the
existing `0.1.0` artifacts. Byte equality was checked against generated/build
outputs for the JVM host library and both Android native libraries. SHA-256:

- JVM darwin-aarch64: `dfc24b73518b6306891d168c6c7243d7035543c64dfc2a672271963f49077bc4`.
- Android arm64-v8a: `992262a85615c7828b61bf13be6420cc005679ff6981c6e87190a75c2c309b3e`.
- Android x86_64: `799393d34d9e0e697288819a4955b44a9643ad10e99e57859f62073c692f7ef7`.

The resolved version, repository, consumer copy and native digests are recorded
in `/tmp/rocky-bounded-holds-consumer.json`. Fresh Kotlin and consumer checks
are next; package construction alone is not their qualification.

`cargo check -p rocky-sdk --features qualification --all-targets` passes
(16.16 s). Fresh Kotlin `check` passes, including JVM and iOS simulator tests
(202 tasks, 2m54s). The shopping-app check is now running from the consumer copy
against the verified final version, after all Rust and SDK native builds ended.
Logs: `/tmp/rocky-bounded-holds-qualification-storage.log`,
`/tmp/rocky-bounded-holds-kotlin-storage.log`, and
`/tmp/rocky-bounded-holds-consumer-check-final.log`.

The consumer check against the verified candidate failed: Android/JVM unit
checks passed 25/25, but the JVM integration run passed 68/70 in 52.376 s.
`SchemaUpgradeTest.a trusted device migrates version 6 and a version 6 member
keeps syncing` failed its successful-reconcile assertion after reopening the
member with the new application schema (line 138). `SealsTest.a full member
keeps observing main while two trusted nodes seal and one opens a branch`
found its background failure deferred completed (line 145). These are assertion
failures against this candidate, not observations of the earlier mismatched
native artifact. Their cause and baseline attribution remain unestablished.

The check script stopped before application builds. Those stages were then
run separately against the same final version: both desktop JVM and Android
application builds pass. Temporary diagnostics in the consumer copy now print
the failed reconcile report and completed background exception, preserving the
original assertions. A diagnostic consumer rerun is in progress; isolated or
later passes will not erase the failed full run. No tracked consumer source
has changed. Logs: `/tmp/rocky-bounded-holds-consumer-desktop.log`,
`/tmp/rocky-bounded-holds-consumer-android.log`, and
`/tmp/rocky-bounded-holds-consumer-diagnostic.log`.

The diagnostic candidate rerun again passed Android/JVM unit checks 25/25
and JVM integration 68/70. The upgrade failure recurred at the same reopen
assertion (line 141 only because diagnostics added three lines): its report
contains `ResolveContact`, `ResolveRoute`, `I/O: connection lost`, and no peer
attempts. The seal-observer case passed on this run. Instead,
`ApprovalsTest.concurrentDecisionsConvergeAndMembersOnlyAsk` failed with
`UncertainCoordinatedWrite: Undecided`. No retry or assertion was weakened.

A real native baseline comparison was then built from the preserved clean
`pukwsswl` / `c8afed26`, parent phase-start `rzszssxw` / `0b097aa0`:

```sh
cargo build --manifest-path sdks/uniffi/Cargo.toml --locked --lib --features bindgen
```

The host debug/bindgen profile matches the candidate's packaged JVM host
library build. The baseline library SHA-256 is
`159d10c3a7ac4008b75145adbf578356f8558a1e21ccee704bb2ded9e69ba853`.
The UniFFI sources, manifest and lockfile have no diff between these revisions.
The same consumer/classes and diagnostics were used, overriding only its native
runtime through the generated binding's supported system property:

```sh
./kotlin test --project-dir . -p jvm -p android -m shared \
  --jvm-args='-Duniffi.component.rocky_uniffi.libraryOverride=/Users/david-bieregger/AgentWorkspaces/rocky-bounded-holds-baseline/sdks/uniffi/target/debug/librocky_uniffi.dylib -Djna.debug_load=true'
```

JNA's loader log confirms that exact absolute baseline library was found and
loaded; this run did not use the candidate jar's embedded native library or
the old Maven Local `0.1.0` runtime. Android/JVM unit checks passed 25/25;
baseline JVM integration passed 67/70. It reproduced the coordinated
`Undecided` failure, and two household cases failed with `submission identity
reused with different input`. The upgrade and seal-observer cases passed.
Log: `/tmp/rocky-bounded-holds-consumer-real-baseline.log`.

This establishes the coordinated failure on the native phase-start baseline.
It does not establish a cause for the candidate's repeated upgrade route
failure or initial observer failure. It is a native functional comparison,
not qualification of a baseline package, a performance pair or a comparison
against only phase 2's parent: the phase-start baseline also predates phase 1b.
Temporary diagnostics have been removed from the consumer copy. The baseline
workspace remains clean, and neither workspace was integrated or removed.

This continuation completes the migration-accounting bound and the previously
pending consumer provenance check. Phase 2 remains incomplete: other metadata
holders and their deterministic bounds, provisional bound calibration, the
cold receive sizing miss, sync/consumer failures, final L0–L4, paired desktop
sync and the 100k migration gate remain open. No new results for those final
qualification gates are implied by the package or baseline checks above.

## Handover: fallback hang attribution and remaining holders (2026-10-28)

Stopped at the owner's request. Work stays in `pkslrrwp` in
`~/AgentWorkspaces/rocky-bounded-holds`; no source change was made in this
continuation (temporary lock tracing was added and removed; `lock.rs` diff is
unchanged). A third workspace, `rocky-bounded-holds-parent`
(`~/AgentWorkspaces/rocky-bounded-holds-parent`), was added at the candidate's
parent `qyxtuntm` / `0f08fe4b` (phase 1b) for paired functional runs. It is
clean and may be forgotten with `jj workspace forget` once no longer needed.

### Advised-import fallback hang

`three_failed_advised_imports_fall_back_only_for_that_scope_and_checkpoint`
hangs (30 s timeout) when the whole `rocky-sdk --no-default-features`
integration set runs, and passes alone or with only `sync_tests`
(`--stress-count 4`, 4/4 passed). Host load came mostly from unrelated
projects' builds (1-min load 14–80 on 18 cores), so timings are noisy.

Back-to-back pair (ABAB), filter
`not (binary(migrations) | binary(faults) | binary(registry) | binary(revocations) | binary(group_secrets))`,
`cargo nextest run -p rocky-sdk --no-default-features --profile integration`:

| Round | Candidate `pkslrrwp` | Parent `qyxtuntm` |
| --- | --- | --- |
| 1 | 327/328, fallback timeout | 327/328, cut-join timeout |
| 2 | 327/328, fallback timeout | 328/328 |

Including the earlier unpaired runs, the candidate hung the fallback test in
6/6 full-package runs; the parent in 1/3 (that one overlapped another run).
The cut-join timeout also occurs on the parent and is a pre-existing flake,
not attributed to this phase. Log: `/tmp/rocky-bounded-holds-fallback-pair.log`.

The hung process uses ~3–4 % CPU (waiting, not spinning); a native sample shows
only the idle test thread and an occasional flusher `fsync`
(`/tmp/rbh-fallback-sample.txt`). A lock trace scoped to the test thread
(`/tmp/rocky-bounded-holds-fallback-locktrace.log`) shows ~54 checkpoint
page-storage acquisitions (`checkpoint/transfer.rs:1878`, plus `:1908`),
settle (`incoming.rs:40`) and `replica.rs:1781`, then a final SQL read
(`sql.rs:181`, `:686`, `:708`, `:733`) that acquires and releases normally.
No replica lock is requested afterwards, so the test waits on something
other than the replica lock after that read (likely the `SELECT count(*)`
after b's sync, or the following `compact`/`link_sessions` step). Next step:
add stage prints in the test after each await from b's `sync` onwards, and
inspect awaits that are not replica-lock acquisitions (flush waits, passive
WAL `optimize` wait, link/session channels). This is unresolved and blocks
`./scripts/check`.

### Remaining unbounded steady-state holders (source survey)

Not yet bounded, each still a single acquisition over whole tables:

- `schemas.rs` `schema_requirements` (required/missing schemas): scans all
  `secure_operations`/`secure_voids` headers and all `secure_held_frames`.
- `tip_observation.rs` `domain_headers`, shared by `seal_frontier`,
  `scan_earlier_epochs`, `tip_observation` and `tip_nodes`: the same
  operations/voids header scan. One validated paged header scan would bound
  all five callers above. Validation needs the watched tables plus authority,
  workspace tip, `context_epoch` and the domain's tip graph (no graph version
  counter exists yet; `graphs` is an in-memory map).
- Blocker computation (`blockers.rs`; only the readiness watch selection changed).
- Tip statement prerequisites (`tips.rs`), rebootstrap discovery
  (`compaction/rebootstrap.rs`), publication progress (`publication.rs`),
  schema retention (`schemas.rs` `drop_unrequired_schemas`), placements
  (`placements.rs`), restoration completion and custody frontier (`roles.rs`),
  SQL connection planning (`sql.rs:181`) and move-following after settlement.
  Held generation fingerprint, further held refresh and resolver bounding
  are skipped under the revised scope below; completed held work stays.
- Pruning and page reclamation have work constants but no per-acquisition
  deterministic tests or step measurements in this phase.

Reuse the pattern in `replica/bulk/storage.rs` and `replica/held/listing.rs`:
keyset pages under `Class::Background`, a `watch_writes` source checked on
every acquisition, `RetryExhausted` on relevant change, a `WorkUnit` counter
and a test pause hook. Callers that cannot surface `RetryExhausted` need an
internal bounded rescan; avoid global commit counters (they starve under
unrelated writes, as found for checkpoint preflight).

### Still open for the completion gate

Fallback hang above; consumer `SchemaUpgradeTest` reopen route failure
(candidate-only so far); the holders above; phone calibration of provisional
constants (`RECEIVE_REQUESTS`, `CLEANUP_ROWS`, `LISTING_ROWS`,
`STORAGE_RECORDS`); the fresh one-row receive sizing miss (15.597 ms p99 at a
bound of one); L0–L4 desktop and phone; paired desktop sync rows; the 100k
migration gate; final `./scripts/check`, Kotlin and shopping-app checks.
Nothing is integrated; no workspace was removed.

## Fallback timeout diagnosis (2026-10-28)

The advised-import fallback "hang" is a slowdown under disk load, not a
deadlock. Stage timestamps after each await (temporary, since removed) show
the test still progressing when it is killed: 64 local adds took 8.5 s, and
b's `peer.sync` was still running at the 30 s limit. Pure CPU load (36 busy
loops, load 62) did not reproduce it; four concurrent `dd`+`sync` writers did.

Same filter as the handover pair, default profile (60 s limit), four `dd`
writers, alternated candidate/parent:

| Round | Candidate `pkslrrwp` | Parent `qyxtuntm` |
| --- | ---: | ---: |
| 1 | 43.746 s | 31.703 s |
| 2 | 54.707 s | 29.959 s |

The parent sits at the 30 s integration limit under this load too; the
candidate exceeds it. Unloaded, ten stress runs each: candidate 0.898–1.026 s,
parent 0.796–0.868 s. b's sync stage averages 190 ms against 118 ms.

Temporary counters (since removed) in both trees during b's sync:

| Measure | Candidate | Parent |
| --- | ---: | ---: |
| WAL `F_FULLFSYNC` in the whole test | 91 | 91 |
| WAL checkpoints in the whole test | 0 | 0 |
| Replica acquisitions | 466 | 341 |
| Receive acquisitions | 64 × `group.rs:291`, 94.8 ms | 2 × `group.rs:147`, 49.1 ms |
| SQL statements | 6,974 | 6,295 |
| Committed write transactions (`BEGIN IMMEDIATE`) | 85 | 21 |

Device flushes are unchanged. The difference is one SQLite transaction per
received one-row frame (`RECEIVE_REQUESTS=1`, ready frames applied with
custody), against two grouped transactions before. Each commit adds its own
revision, admission-context and read-epoch writes and appends its modified
pages to the WAL. The same flush then has more dirty data to push, which is
why disk contention amplifies the gap. This is the expected cost of bounded
receive steps for small frames, not a lost wakeup; the earlier `fsync` sample
and low CPU fit this. It is not resolved: the test still exceeds its limit
under disk load. The candidate holds the replica twice as long in total per
one-row frame (1.48 ms against 0.77 ms per frame). The paired 100k member pull
below shows a larger regression elsewhere.

## Paired 100k member pull fails (2026-10-28)

Apple M5 Pro, same command as the preliminary guard above, release,
`rocky-sdk --no-default-features --features qualification`, alternated
baseline `rocky-bounded-holds-baseline` (phase-start sources) and candidate
`pkslrrwp`, one fixture each, host otherwise idle (1-min load 3–13 at start).
All fixture assertions passed in every run.

| Run | Tree | Member pull | Publication | Member main / WAL |
| --- | --- | ---: | ---: | ---: |
| 1 | Baseline | 9.886650709 s | 52.528 s | 67,682,304 / 43,144,672 B |
| 2 | Candidate | 28.092830625 s | 56.087 s | 68,947,968 / 20,624,752 B |
| 3 | Baseline | 9.992642291 s | 51.654 s | 67,911,680 / 37,871,072 B |
| 4 | Candidate | 29.825622083 s | 59.701 s | 69,050,368 / 18,540,032 B |

Member-pull ratio is 2.92 (mean 28.96 s against 9.94 s), against the allowed
1.10. **The desktop sync non-regression gate fails.** The preliminary
1.024 ratio predates the bounded checkpoint storage steps and does not apply.
Logs: `/tmp/rbh-sync-run{1,2,3,4}.log`.

The candidate's pull is dominated by checkpoint page storage:
`checkpoint/transfer.rs:1908` took 12,835 / 12,833 acquisitions (p50 0.935 /
1.000 ms, p99 5.089 / 5.352 ms, max 13.117 / 15.899 ms), one per
`STORE_ROWS=8` rows. A 12 s `sample` of the member pull
(`/tmp/rbh-pull-sample.txt`) puts most reactor time in SQLite commits
writing WAL frames (`pagerWalFrames` → `pwrite`, ~2,960 of ~7,300 reactor
samples). Each small commit rewrites the same table and index pages.
Per-step authority verification is ~350 samples; the per-step partial-row
rescan is smaller. The receive split above is not the main cause here.

`STORE_ROWS=8`'s doc comment claimed recorded calibration; none existed.
It now says provisional.

## Checkpoint storage-step calibration (2026-10-28)

New ignored `tests::constraints::measure_checkpoint_storage_steps`: two fresh
targets per size import a 2,048-child checkpoint after its one-row parent
scope, so every child is visible on storage (no dependent fan-out). A test-only
override sets the step size; every timed acquisition records
`checkpoint_store_row` and must carry exactly that many rows. Each sample is the
full storage acquisition, including staging lookup, authority verification,
partial-row validation, the transaction and revision publication. Test builds
use 16-row pages (`STEP_ROWS`), so 16 is the largest measurable step.

```sh
cargo test --release -p rocky-secure --lib --target aarch64-linux-android --no-run
# Push; run with TMPDIR=/data/local/tmp:
rocky-bounded-holds-tests --exact tests::constraints::measure_checkpoint_storage_steps \
  --ignored --nocapture --test-threads=1
```

| Rows per step | Samples | Pixel p50 / p95 / p99 / max (ms) | Desktop p50 / p95 / p99 / max (ms) |
| ---: | ---: | --- | --- |
| 1 | 4,096 | 1.920 / 4.166 / 6.787 / 30.835 | 0.449 / 0.539 / 0.710 / 3.924 |
| 2 | 2,048 | 2.979 / 6.645 / 10.134 / 30.828 | 0.530 / 0.624 / 0.799 / 1.079 |
| 4 | 1,024 | 6.332 / 9.143 / 9.834 / 14.450 | 0.690 / 0.863 / 1.025 / 1.245 |
| 8 | 512 | 10.723 / 12.855 / 14.041 / 19.670 | 0.934 / 1.104 / 1.308 / 1.400 |
| 16 | 256 | 16.920 / 19.550 / 20.172 / 21.353 | 1.323 / 1.601 / 1.770 / 2.039 |

Physical Pixel 11 Pro, Android 17, USB, foreground shell; power and thermal
state not controlled (passed in 97.58 s, `/tmp/rocky-bounded-holds-phone-store.log`).
Desktop: Apple M5 Pro, 1-min load 17.5 at start, a sanity run rather than
calibration. Phone marginal cost is about 0.8–1.0 ms per row at p50 on top of
~1.9 ms per step; desktop about 0.06 ms per row on ~0.39 ms.

Under the brief's rule (phone cost × bound ≤ 12.5 ms), the current 8-row step
**misses** at p99 14.041 ms; 4 rows (p99 9.834 ms) is the largest measured
step within it. The 1- and 2-row maxima (~31 ms) stay within the phone 100 ms
max target. A smaller step adds commits,
and commits are what made the desktop pull 2.9× slower at 8 rows.

This was the sizing conflict under the former brief: a phone-sized step
added the commits responsible for the pull regression. The owner has now
approved the throughput-first sizing below, so this is historical evidence,
not a pending decision. `STORE_ROWS=8` is the pre-change comparison point;
the parameterized pull and phone confirmation select its replacement.

## Approved throughput sizing and surviving scope (2026-10-28)

The owner approved the revised recommendation: phase 2 supplies the bounded
loops used by features_13 phase 2's interactive gate and phase 4's replay
receive. Select the smallest of 32/64/128 checkpoint rows whose paired 100k
member pull is within 110%, then measure its phone hold. Try 2–4 one-row
receive frames per transaction to reduce commit amplification. Compare step
holds to features_13's interactive p95/p99/max targets (desktop 16/50/100 ms,
phone 33/100/250 ms); this does not itself qualify interactive latency.
The previous 12.5 ms per-step sizing conflict is superseded. A full 256-row
fallback is allowed only if smaller steps fail and the interactive-waiter gate
is present; it is not an existing gate in this workspace.

Keep checkpoints, seals, compaction, moves, partitions and coordinated logs.
Finish the shared `domain_headers` scan (five callers, including two inside
write transactions), required schemas/retention, blockers, publication progress,
placements, restoration/custody frontier, rebootstrap discovery, tip
prerequisites, move-following, SQL connection planning, and deterministic
pruning/reclamation step tests. Preserve completed held work, but skip its
generation fingerprint, further held refresh and resolver bounding. The
existing fan-out exception lasts until features_13 phase 8 deletes the resolver.

Close against `ynnvllrn` / `4e4e2497`, the phase-start commit stated above.
Earlier `rzszssxw` / `0b097aa0` comparisons predate phase 1 instrumentation and
are retained as historical measurements, not the final baseline. The new
`rocky-bounded-holds-phase-start` workspace is an untouched child of
`4e4e2497`; use the same fixture, host, profile and load for each pair.
Main's roadmap deletion and the intent-shapes merge conflict stay outside
this isolated implementation. Evidence remains in `docs/features_12` until
phase 2 lands; features_13 phase 9 rechecks devices.

## Receive batch calibration (2026-10-28, revised sizing)

Physical Pixel 11 Pro, Android 17, arm64 release native tests with
`qualification`; USB, foreground shell, Device panel closed. Power, thermal
state and other phone activity were not controlled. Each cold profile uses
64 freshly opened targets; each warm profile uses 128 update batches on one
target. Each size has its own source log so every measured frame's own-chain
predecessors are present. All receipts must be applied and the final row images
must match. Times cover the `run_group` transaction through guard release;
explicit flush, grouping's payload inspection, WAL and later empty settle are
outside the timer. No whole-scenario interactive result is claimed.

| One-row frames per step | Profile | Samples | p50 / p95 / p99 / max (µs) |
| ---: | --- | ---: | --- |
| 1 | Cold | 64 | 14,264 / 15,440 / 16,866 / 16,866 |
| 1 | Warm | 128 | 7,313 / 8,566 / 10,030 / 11,385 |
| 2 | Cold | 64 | 18,664 / 20,241 / 20,983 / 20,983 |
| 2 | Warm | 128 | 11,986 / 13,730 / 14,390 / 14,984 |
| 4 | Cold | 64 | 22,785 / 24,103 / 25,575 / 25,575 |
| 4 | Warm | 128 | 20,062 / 22,794 / 24,286 / 28,958 |

`tests::group::measure_combined_receive_steps` passed in 50.03 s; nearest-rank
percentiles. Log: `/tmp/rbh-resize-receive-phone.log`. Four requests reduce
commit amplification and leave measured transaction headroom against the
phone 33/100/250 ms interactive targets. The actual write's remaining work
and gate behavior still need features_13 phase 2 qualification. Select
`RECEIVE_REQUESTS=4`; this adds no wall-clock deadline.

Only decoded one-row, non-bundle frames may share a step. Larger atomic
frames, bundles and payloads that cannot yet be opened run alone. The existing
request exception therefore stays narrow. Group histograms use the cohort's
row budget, so four ordinary rows are not mislabeled as oversized or fan-out.
The release group suite passes 23 tests (four measurements ignored), including
the new mixed-size per-acquisition assertion, one shared flush, and a failed
suffix retried after the first transaction committed.

The first larger storage sweep recorded valid 32/64/128 samples, then failed
because it also requested 256 full rows from a fixture whose byte-limited
pages are smaller than that. That failed sweep is not a passing calibration
run. The corrected sweep requests exactly the three proposed sizes and checks
all tail rows while fitting full steps separately. Earlier exploratory desktop
runs failed on a missing source-log prefix and a partial-page assertion;
neither is used as sizing evidence.

## Parameterized paired member pulls (2026-10-28, in progress)

Apple M5 Pro, macOS 27.0.1, 18 logical CPUs, arm64 release;
`rocky-sdk --no-default-features --features qualification`. The preserved
phase-start workspace has parent `ynnvllrn` / `4e4e2497` and no file changes.
The sweep uses the same unchanged 100k fixture and assertions in both trees.
`ROCKY_CHECKPOINT_STORE_ROWS` and `ROCKY_RECEIVE_REQUESTS` are validated
measurement overrides available only with `qualification`; normal artifacts
use the named constants. Logs and per-pair start loads are recorded under
`/tmp/rbh-throughput-sizing`, with `results.json` and the runner log
`/tmp/rbh-throughput-sizing.log`.

```sh
# Same full fixture in each workspace; candidate overrides vary by row bound.
ROCKY_CHECKPOINT_STORE_ROWS=32 ROCKY_RECEIVE_REQUESTS=4 \
  cargo test --release -p rocky-sdk --no-default-features --features qualification \
  --test migrations a_migration_of_100000_rows_converges_on_every_member \
  -- --exact --nocapture --test-threads=1
```

The 32-row pair passes every fixture assertion but fails the throughput gate:
9.625646166 s baseline versus 15.219035 s candidate (1.58109×). With one
fixture per tree, p50/p95/p99/max of each pull are that one value, not tail
estimates. The candidate has 3,759 row-storage acquisitions carrying exactly
100,000 rows in total; metadata has 5,666 separate acquisitions. Storage hold
p50/p95/p99/max is 1.986/5.274/6.739/14.738 ms. The 1.495 s base installation
is a transition and remains separate from ordinary holder targets.

The next baseline's preparation overlapped a focused test build, which was
interrupted before its member timer began. Its pull is 14.588175875 s, with
browser, Gradle and filesystem activity observed on the host; the source of
the remaining timing variation is not established. No candidate is selected
from that unfinished pair. Other builds and full verification are deferred
until the sweep ends; a final selected-bound pair is required on the settled
source. The interrupted compilation is not a test result.

The 64-row pair also passes all fixture assertions and fails the throughput
gate: 14.588175875 s baseline versus 19.425161792 s candidate (1.33157×).
At 22:54 local time the host was on battery, with battery `powermode=1`
and AC `powermode=0`; no thermal/performance warning was recorded by `pmset`.
Power history for the earlier measurements was not captured, so it cannot be
used to explain them conclusively. The runner now records power at each run's
start and end. A final pair must use a consistent power profile; no system
power setting was changed by the agent.

The original sweep completed all six fixtures. The 128-row candidate pulled
in 12.189190917 s against its baseline's 14.584027333 s, but the host changed
from battery to AC during candidate preparation. Its apparent 0.83579× pass
is exploratory, not a qualified comparison. The selected constant remains
pending a consistent-power pair using identical prepared input.

A user-coordinated fixture helper now separates preparation from the same
member-pull assertions. Its 8-row relocation/two-independent-replays regression
passed in the release/qualification build (2.27 s while baseline compilation
was active; this is not its final tier-budget check). The original 100k gate
still composes both helpers and retains every assertion. `measure-prepared-pull`
never builds and strips tuning overrides in its `--defaults` mode. It pins
binary hashes, alternates pair order and checks the fixture digest before and
after replay. Real 100k cache preparation and paired replay remain in progress.

The comparison workspaces were externally removed after/during the sweep;
the compiled processes completed and raw logs remain. The original path's
Git worktree registration remained stale, so Jujutsu created the replacement
`rocky-bounded-holds-phase-start-final` at parent `4e4e2497`. Its only file
differences from that commit are `tests/migrations.rs` and the prepared-pull
test module. Every production file, manifest and lockfile remains unchanged.
This test-only overlay permits identical assertions on the exact phase-start
runtime; it is not a features_13 baseline. Keep this workspace through final
qualification.

## Identical prepared input and metadata commit amplification (2026-10-28)

The 100k cache prepared successfully in 256.95 s. Both binaries replay fresh
copies with the same assertions, on AC, and the retained fixture digest is
unchanged. Baseline production is exactly `4e4e2497`; only the shared test
harness is overlaid. The first consistent-input screen records:

| Row bound | Baseline pull s | Candidate pull s | Ratio |
| ---: | ---: | ---: | ---: |
| 32 | 10.831397791 | 17.378929416 | 1.60450 |
| 64 | 9.637771583 | 13.266611958 | 1.37652 |
| 128 | 9.579850041 | 12.342481375 | 1.28838 |
| 256 (measurement probe only) | 9.581723167 | 11.679507542 | 1.21894 |

One pull per binary per row; its p50/p95/p99/max are that same value, not tail
estimates. Every fixture assertion passes, but **no row bound passes 110%**.
The earlier mixed-power 128-row result therefore cannot select a constant.
Logs: `/tmp/rbh-prepared-screen`; runner report includes binary digests, power
and loads. This fixture is now reusable, so additional pairs no longer repeat
migration preparation.

Rows alone do not remove commit amplification: `STORE_METADATA=1` still
commits each immutable creation header, binding set or stamp-author mapping
separately (5,666 metadata acquisitions in the earlier 32-row pull). The same
throughput-first sizing therefore also measures this existing named work bound.
`ROCKY_CHECKPOINT_STORE_METADATA` is qualification-only and stripped by default
confirmation alongside the other tuning variables. Metadata remains bounded by
records and the signed page's byte limit; authority/source checks and atomic
per-step commits are unchanged. Metadata samples now count inputs and full
holds per acquisition for phone confirmation. This adds no resolver work,
format change or exception. Final storage constants are still being selected.

The first metadata-16 screen did not establish a passing bound: 64 rows
failed at 1.23517×; 128 appeared to pass at 1.07407× while power changed
inside the candidate run and is excluded; 256 failed at 1.21057× on battery.
The prepared runner now marks power-inconsistent pairs and refuses to select
from them. It compares power-source labels rather than battery percentages.

Validation also repeated point lookups for every row. Checkpoint preflight now
loads stored creation clocks and direct-import collisions with bounded `IN`
queries per chunk; storage loads any existing prefix records the same way.
All prior checks remain: matching clocks, refusal before any conflicting page
prefix is written, exact canonical equality for retried rows, signed-page
membership and row counts, plus authority/source checks at every acquisition.
Queries read at most the named row chunk and preserve the empty-page case.
No format or admission rule changed. New paired measurements and the existing
checkpoint corruption, interruption and retry tests must validate this change.

Batched validation with metadata bound 16 passes 128 rows (11.668884125 /
11.931801875 s, ratio 0.97796); 64 rows fails (12.376463875 /
10.737605208 s, 1.15263). A smaller metadata bound of eight also selects 128:

| Row bound, metadata 8 | Baseline pull s | Candidate pull s | Ratio |
| ---: | ---: | ---: | ---: |
| 32 | 10.605832209 | 14.250489792 | 1.34365 |
| 64 | 10.424781708 | 12.897270125 | 1.23717 |
| 128 | 11.285772292 | 12.287209750 | 1.08873 |

All are single prepared-input pairs, with consistent battery-power labels
at every run boundary, identical fixture data and every original assertion.
Logs: `/tmp/rbh-prepared-batched-meta16` and
`/tmp/rbh-prepared-batched-meta8`. These screen the constants; the final gate
will use three alternating pairs of compiled defaults with every tuning
variable stripped. Metadata four is being screened before fixing that bound.

Metadata four fails at 128 rows: 11.411548000 /
10.247073333 s, ratio 1.11364, with consistent
power. The candidate now compiles `STORE_ROWS=128`, `STORE_METADATA=8` and
`RECEIVE_REQUESTS=4`; default-bound confirmation and fresh phone calibration
are next. Ordinary tests retain eight-row storage steps to cross interruption
boundaries cheaply; qualification measurements override rows with production
256-row wire pages. No full-page fallback or new quiet-start gate is needed
if the final repeated default-bound check passes.

The first three-pair compiled-default confirmation on AC fails narrowly:
mean candidate/baseline ratio 1.109019. Every pair has consistent AC
labels and all data assertions pass. Raw results remain in
`/tmp/rbh-prepared-defaults-ac`. This is not recorded as a passing gate.

The final metadata group (at most eight records) now commits in the first row
transaction (at most 128 rows), removing one metadata-only commit per page.
Earlier metadata groups remain separate bounded steps. Metadata is stored
before its rows in the same transaction; rollback invalidates the record and
author caches as before. Retried first-row chunks still commit the metadata
even when their rows are already present. All source, clock, collision,
canonical-equality, page-count and signature checks remain. Row calibration
measures this combined first-step hold; pure metadata timings exclude guards
that also store rows. A fresh compiled-default confirmation is required.

The temporary whole-fixture sweep driver was replaced by
`scripts/measure-prepared-pull`, which also supports compiled-default
confirmation. Only the reusable driver is retained; the original whole-fixture
logs and reports remain as evidence. Neither driver changes source workspaces
or builds during a timed replay.

## Compiled-default throughput gate (2026-10-28)

The final composed defaults (`STORE_ROWS=128`, `STORE_METADATA=8`,
`RECEIVE_REQUESTS=4`) pass the three-pair aggregate member-pull gate against
phase-start `4e4e2497`. All tuning variables are removed for both binaries.
Pairs alternate AB / BA / AB, use fresh copies of identical prepared input,
and retain every data, convergence, frame, wire and file assertion. Power
labels are AC at all twelve run boundaries; the fixture digest is unchanged.

| Pair | Order | Baseline pull s | Candidate pull s | Ratio |
| ---: | --- | ---: | ---: | ---: |
| 1 | baseline/candidate | 10.041058458 | 10.835837583 | 1.079153 |
| 2 | candidate/baseline | 9.528155750 | 10.489643833 | 1.100910 |
| 3 | baseline/candidate | 9.954100417 | 11.067832333 | 1.111887 |

Mean baseline 9.841104875 s; candidate 10.797771250 s;
ratio of means **1.097211**, within 1.10. This is a narrow
aggregate pass, not a claim that every individual pair passes. The table
retains the individual ratios. With three samples, nearest-rank p95 and p99
are the maximum, not tail estimates:

| Tree | Samples | p50 / p95 / p99 / max (s) |
| --- | ---: | --- |
| Phase start | 3 | 9.954100417 / 10.041058458 / 10.041058458 / 10.041058458 |
| Candidate | 3 | 10.835837583 / 11.067832333 / 11.067832333 / 11.067832333 |

Command: `scripts/measure-prepared-pull measure --baseline-binary <phase-start migrations binary> --candidate-binary <candidate migrations binary> --fixture /tmp/rbh-prepared --output /tmp/rbh-prepared-defaults-combined --defaults --samples 3`.
Host: Apple M5 Pro, macOS 27.0.1, arm64 release,
`rocky-sdk --no-default-features --features qualification`. Raw report:
`/tmp/rbh-prepared-defaults-combined/results.json`; it records binary hashes,
loads, power and per-run logs. The baseline workspace differs from
`4e4e2497` only by the shared test harness. Phone confirmation and final
workspace checks are recorded below. L0–L4 and the remaining metadata holders
are not qualified by this result.

## Fresh phone confirmation (2026-10-29)

Physical Pixel 11 Pro, Android 17, arm64 release native tests with
`qualification`; foreground wireless-ADB shell and `TMPDIR=/data/local/tmp`,
Device panel closed. Host builds do not run on the phone. Phone power,
thermals and other activity were not controlled. The deployed ELF SHA-256
matches the host build: `95bddbcbaa7eafe452bda54602e03187f835934e0fe064bdfdee1ece5a86875b`.
The wireless upload's completion acknowledgment stalled; the complete file's
size and digest were verified before use. Exploratory overlapping invocations
were discarded; the table below is a fresh single invocation.

The receive timer now includes group inspection, admission, ready application,
commit and guard release. It excludes the wait to acquire, explicit flush,
WAL maintenance and later empty settle. Fresh targets: 64 per batch size;
warm target: 128 update batches. Applied receipts and final row images are
asserted. Command: native test `tests::group::measure_combined_receive_steps`
with `--exact --ignored --nocapture --test-threads=1`.

| Frames / rows | Profile | Samples | p50 / p95 / p99 / max (µs) |
| ---: | --- | ---: | --- |
| 1 | Cold | 64 | 14,389 / 15,786 / 17,829 / 17,829 |
| 1 | Warm | 128 | 7,401 / 8,761 / 8,979 / 9,155 |
| 2 | Cold | 64 | 18,216 / 19,576 / 21,014 / 21,014 |
| 2 | Warm | 128 | 11,654 / 14,313 / 16,569 / 23,490 |
| 4 | Cold | 64 | 22,681 / 24,023 / 27,032 / 27,032 |
| 4 | Warm | 128 | 20,410 / 22,580 / 25,025 / 25,042 |

The test passed in 50.27 s; log:
`/tmp/rbh-sizing-phone-receive-confirmed.log`. Four-frame steps are recorded
against features_13's phone 33/100/250 ms interactive targets. Their p99
slightly exceeds the original 25 ms holder target; it is reported here,
not discarded. These are step holds, not L0–L4 interactive-write results.

The first full workspace check passed Clippy and all 207 unit tests. Integration
had 1,464 passes and two fixture failures: header-cache/stream-resume tests
expected several small pages, but globally enabling production-sized pages
under `qualification` made each fixture fit one. That test configuration is
repaired: ordinary tests keep 16-row export steps, while the explicit storage
calibration uses 256-row wire pages. The wire limit is separately named and
remains 256 in every build. Production behavior is unchanged by this test-only
repair. The phone calibration binary already uses the same 256-row pages and
production importer; its untimed fixture compaction uses the larger test steps.
The full check passes against the corrected source, as recorded below.

Checkpoint storage calibration passed its structural assertions in two fresh
phone runs: every step stays within its row/metadata input bound, all 4,096
child rows across two targets are accounted for, final projection counts match
and audit agrees. Parents are imported first, so these are ordinary steps with
no induced fan-out. Full row-step holds include their combined final metadata
group. Metadata-only rows below exclude combined guards. Page tails are checked
but excluded from full-row fits.

| Run | Rows per step | Samples | p50 / p95 / p99 / max (µs) |
| --- | ---: | ---: | --- |
| First | 32 | 120 | 13,784 / 29,230 / 372,504 / 393,088 |
| First | 64 | 56 | 24,875 / 79,992 / 408,318 / 408,318 |
| First | 128 | 18 | 24,684 / 464,560 / 464,560 / 464,560 |
| Repeat | 32 | 120 | 9,350 / 23,169 / 28,522 / 31,646 |
| Repeat | 64 | 56 | 21,607 / 49,261 / 49,528 / 49,528 |
| Repeat | 128 | 18 | 44,310 / 73,748 / 73,748 / 73,748 |

| Run / row bound | Metadata records | Samples | p50 / p95 / p99 / max (µs) |
| --- | ---: | ---: | --- |
| First / 32 | 8 | 14 | 4,393 / 8,377 / 8,377 / 8,377 |
| First / 64 | 8 | 14 | 2,450 / 3,932 / 3,932 / 3,932 |
| First / 128 | 8 | 14 | 2,470 / 5,195 / 5,195 / 5,195 |
| Repeat / 32 | 8 | 14 | 1,230 / 3,729 / 3,729 / 3,729 |
| Repeat / 64 | 8 | 14 | 1,227 / 2,229 / 2,229 / 2,229 |
| Repeat / 128 | 8 | 14 | 1,484 / 3,047 / 3,047 / 3,047 |

The first run passes in 54.16 s and records 0.37–0.46 s row-store outliers
at every tested bound, including **464.560 ms** for the selected 128-row step.
That exceeds features_13's phone p99 100 ms and max 250 ms interactive targets
and the original holder targets. The repeat passes in 23.98 s; its selected
step has median 44.310 ms and p99/max 73.748 ms. The 128-row median/p95 in
that profile also exceeds 33 ms; these are holder costs, not measured
interactive writes. With 18 full steps, p95/p99 are the maximum rather than a
tail estimate. Both runs remain in the table. The outlier cause is not
established, and no exception or sample exclusion is added to make them pass.
This confirms a measured work bound, not uniform phone latency qualification.
Features_13 phase 2 must measure the actual wait/write path; phase 9 rechecks
devices. Logs: `/tmp/rbh-sizing-phone-store-confirmed.log` and
`/tmp/rbh-sizing-phone-store-repeat.log`.

## Final smaller-bound cross-check (2026-10-29)

The final optimized 128-row binary was also screened at 32 and 64 rows
with eight metadata records and four receive frames, using the retained
fixture and the same phase-start binary. Every pair records AC at its four
boundaries, all original assertions pass and the fixture remains immutable.
There were no builds during these timed pairs. Samples alternate AB / BA / AB.

| Run | Samples | Mean baseline s | Mean candidate s | Ratio of means |
| --- | ---: | ---: | ---: | ---: |
| 32-row screen | 3 | 10.281024041 | 13.737789139 | 1.336228 |
| 64-row screen | 3 | 10.828124639 | 11.731935764 | 1.083469 |
| 64-row repeat | 3 | 11.010934583 | 12.201850694 | 1.108158 |
| 64-row compiled defaults | 3 | 11.185239306 | 12.512620930 | 1.118673 |
| All 64-row samples | 9 | 11.008099509 | 12.148802463 | 1.103624 |

The first 64-row screen narrowly passes in aggregate, with two individual
pairs failing and a 12.339 s baseline sample retained. A repeat crosses the
gate, so 64 was temporarily compiled as the default for a separate confirmation.
That no-override confirmation fails, as does the aggregate of all nine 64-row
pairs. No sample is discarded. The final source restores 128 rows, the smallest
tested bound with a passing compiled-default confirmation; its three-pair
result above remains the confirmation for the selected defaults.

Raw reports: `/tmp/rbh-prepared-final-smaller/results.json`,
`/tmp/rbh-prepared-final-64-repeat/results.json` and
`/tmp/rbh-prepared-defaults-64/results.json`. The first two use the exact
128-row candidate hash recorded above, with qualification overrides. The
third report pins its distinct temporary 64-row binary. That temporary binary
is not the selected candidate. Timing variability and the narrow 128-row pass
remain visible; this is throughput sizing evidence, not a tail-latency guarantee.

## Sizing implementation verification and remaining scope (2026-10-29)

The final `./scripts/check` passes workspace Clippy, all 207 unit tests and
all 1,466 integration tests (1,142 workspace plus 324 SDK without default
features). Integration wall time is 253.00 s within its 300 s tier budget;
the script's isolated checks also pass each individual test budget. Log:
`/tmp/rbh-sizing-check-confirmed.log`. No assertion or test budget was weakened.
The earlier two failures are fixed by keeping ordinary test fixtures small,
with explicit production-sized wire pages only in the storage calibration.
The temporary 64-row experiment was reverted; the final runtime matches the
128-row source covered by that full check. Later changes are comments and
roadmap evidence. `./scripts/lint` and `scripts/commitlint --jj` also pass;
lint uses the main workspace's `local/bin` tools on `PATH`. Lint log:
`/tmp/rbh-sizing-lint-final.log`.

Regression coverage now checks four-frame receive commits, isolation of an
indivisible larger frame, committed-prefix retry after a failed suffix, and
relocation followed by two independent replays of the same prepared fixture.
Pruning records candidate pages per acquisition; its rollback/reopen test
asserts two eight-candidate steps. Reclamation uses a real SQLite freelist,
asserts at most 4,096 pages reclaimed per step and verifies reopening. These
tests cover the recorded work units; they do not establish that every pruning
query visits only that many index entries or qualify device maintenance costs.

The retained comparison workspace is
`/Users/david-bieregger/AgentWorkspaces/rocky-bounded-holds-phase-start-final`.
`jj diff --from 4e4e2497 --summary` contains only `tests/migrations.rs` and
`tests/migrations/prepared_pull.rs` in `rocky-sdk`. Production code, manifests
and lockfile remain at the exact phase-start commit. The final default report
pins baseline SHA-256
`afbff39040f8da37bf6de031eb7eb1c2cf182d122c94aeba99c5a81bdcc2f0cd`,
candidate `36d5bb9eae1daafe07ea1a7825491e0dd30ca52fb6c5158b237d81244365967e`
and fixture `706943de637ee33e19c954366af5fe376644abca45cd60933e6fb9743d6508f4`.
The report's binary copies and `/tmp/rbh-prepared` are retained for review.

This closes the storage/receive sizing tranche and adds the pruning/reclamation
per-step regressions. Phase 2 remains in progress. Remaining implementation
includes the shared `domain_headers` scan and its transaction callers,
required schemas and retention, blockers, tip prerequisites, publication
progress, placements, restoration/custody frontier, rebootstrap discovery,
move-following and SQL connection planning. Further held refresh, held
generation fingerprint and resolver bounding are outside the revised scope;
completed held work remains. The existing dependent fan-out exception stands
until features_13 phase 8 removes the resolver.

L0–L4 interactive qualification, other paired sync rows, unresolved phone
storage outliers and remaining provisional maintenance/listing/storage bounds
still need evidence. Kotlin and application packages were not rebuilt for
this sizing tranche; earlier consumer results do not qualify this runtime.
Features_13's interactive gate and device recheck also remain future work.
