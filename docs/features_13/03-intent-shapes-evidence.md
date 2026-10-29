# 3. Intent shapes: evidence

Status: measurements and full validation finished (2026-10-29). The phase's
completion gate remains unmet: the full check has a timing-budget failure and
the shopping checks have baseline functional failures. No gate or budget was
weakened. The JSON and tables record the complete rerun after the `OR ABORT` fix.

## Workspace and capture

Workspace `~/AgentWorkspaces/rocky-intent-shapes`, change `koqrvqzn`
(`feat(sdk): intent shapes`), unpushed. The stale workspace was updated, its
code-equivalent divergent revision removed while preserving this evidence, and
`lnqqxstq` resolved by retaining main's deletion of `docs/features_12`. Both this
workspace and the default workspace are conflict-free. Future rewrites of the
bounded-holds parent may reintroduce the deletion conflicts.

- `rocky-secure/src/shapes.rs` is compiled under
  `cfg(any(test, feature = "intent-shapes"))`. SDK and UniFFI forward the feature;
  default UniFFI feature-tree inspection confirms it is absent. Default packaged
  builds do not collect shapes. No persisted or wire format changes.
- SQL capture records successful statements and bound values before partition
  rewriting, savepoint rollback, replicated rows by partition, and commit origin
  after the engine COMMIT. Moves are captured too. Reads count as statements;
  their application-computed results reach replay only through bound values.
- Classification follows the brief's precedence: refused, coordinated,
  constrained, keyed, predicate, split. **Keyed still requires the full primary
  key.** Shopping's unique `(list, id)` filters remain predicates when the primary
  key is `(list, key)`. No settled contract was broadened.
- Algebra counts include plain `INSERT OR ABORT`, but exclude replace/ignore and
  upsert semantics. Captured rows determine partitions and collapse detection
  tracks repeated keyed cell/row writes. Moves count toward seal cadence.
- Capture writes nothing into the database. With `ROCKY_INTENT_SHAPES=<dir>`,
  each committed shape appends to `<dir>/<ROCKY_INTENT_SHAPES_SOURCE>.jsonl`.
- `crates/rocky-secure/examples/intent_shapes.rs` aggregates per-source classes,
  algebra, exclusions, the 5% operation threshold, the per-source refusal stop
  rule and busiest process's 60-second shopping window.
- `scripts/intent-shapes --shopping` copies the application into a temporary
  project and uses the specified Maven Local SDK plus the feature-enabled native
  library. It aggregates even after scenario failure, preserving that exit status.

## Reproduction

Host: macOS 27.0.1 arm64, debug builds. The final capture and validation commands
ran sequentially without competing Cargo builds. Timing values below are
functional-validation evidence, not paired performance claims.

```sh
ROCKY_SDK_VERSION=0.1.0-bounded-holds-pkslrrwp-20261028b scripts/intent-shapes --shopping
```

All six SDK sources pass. Shopping reports five passes and three baseline
failures; the runner exits 1 and still writes the complete JSON and tables.
Raw capture and scenario results are under `local/intent-shapes`.

Baseline reproduction uses the same temporary shopping project with the Maven
Local version above, loading a native library built without capture:

```sh
cargo build --locked --manifest-path sdks/uniffi/Cargo.toml --lib \
  --target-dir /tmp/intent-shapes-baseline-target
ROCKY_SCENARIOS=gate,branch,race-rollback \
ROCKY_SHOPPING_PROJECT=<temporary shopping project> \
ROCKY_EVIDENCE="$PWD/local/intent-shapes-baseline" \
JAVA_TOOL_OPTIONS=-Djna.library.path=/tmp/intent-shapes-baseline-target/debug \
  examples/shoppinglist-app/qualify
```

| Scenario | Without capture | With capture |
| --- | --- | --- |
| gate | foreign node in `rocky.sqlite-wal`, 18,887 ms | same audit failure, 11,826 ms |
| branch | member remains on main with pending follow to dev, 87,136 ms | same transition failure, 68,732 ms |
| race-rollback | devices fail to converge on one tip, 169,061 ms | same tip convergence failure |

Baseline raw results: `local/intent-shapes-baseline/evidence.json`. These failures
can occur independently of capture and remain outside this measurement phase.

## Full validation

- `./scripts/lint`: passes, using the default workspace's `local/bin` on PATH.
- `scripts/commitlint --jj`: passes, including the merge's corrected Conventional
  Commit description.
- `cargo test --locked -p rocky-secure --lib shapes`: six selected tests pass,
  including five classifier tests. Coverage includes rollback and failed statements.
- `cargo test --locked -p rocky-secure --features intent-shapes --example
  intent_shapes`: cadence regression passes (moves, time-window boundaries,
  process separation, migration/internal exclusions).
- `cargo clippy --locked -p rocky-secure -p rocky-sdk --all-targets --features
  intent-shapes -- -D warnings`: passes.
- Controlled runner smoke check: qualification exit 7 still invokes aggregation
  and the runner returns 7.
- `./scripts/check`: formatting and Clippy pass; 212 unit tests pass. The first
  full run has 1,464 integration passes and a sync-advice timeout in both feature
  configurations, plus migration-fixture budget overruns. The sync-advice test
  passes alone in 0.846 s.
- `NEXTEST_TEST_THREADS=8 ./scripts/check`: all 212 unit and 1,466 integration
  executions pass (workspace: 1,142; SDK without defaults: 324). Integration wall
  time is 246.37 s within its 300 s budget. Exit status remains **1** because
  `rocky-sdk::migrations prepared_pull::relocated_fixture_can_be_pulled_twice`
  measures 2.03 s alone against its 2.00 s limit in the workspace configuration;
  the other configuration passes at 1.99 s. No full-check pass is claimed.
- `sdks/kotlin/gradlew -p sdks/kotlin check`: **passes**, 202 actionable tasks.
  This clears the earlier service-test timeout caused by a concurrent Cargo build.
- Shopping check with `ROCKY_SHOPPING_PROJECT=<temporary shopping project>` and
  `JAVA_TOOL_OPTIONS=-Djna.library.path=/tmp/intent-shapes-baseline-target/debug`:
  schema preflight passes; test summaries report 25/25 and 69/70 passes.
  `concurrentDecisionsConvergeAndMembersOnlyAsk` fails with
  `UncertainCoordinatedWrite: Undecided`. The earlier replication/bootstrap
  failures pass in this sequential run. The command exits 1.
- Because the shopping script stops on that failure, its remaining builds were
  run explicitly with the same project:
  `examples/shoppinglist-app/kotlin build --project-dir <project> -m desktopApp`
  and `-m androidApp`. **Both pass.** Android shared/test compilation passes;
  no physical-device or emulator qualification is claimed.

At this measurement run, the migration-fixture budget, shopping coordinated
write and three qualification scenarios still failed. The subsequent
[shared validation repair](#shared-validation-repair-2026-10-29) clears these
failures; the measured corpus below remains from the original run.

## Sources

| Source | Captured | Note |
| --- | --- | --- |
| Shopping qualification scenarios | yes | 8 JVM scenarios; `gate`, `branch` and `race-rollback` failed (see below) |
| SDK tests `application`, `coordinated`, `group_secrets`, `partitions`, `shared_lists`, `shopping_release` | yes | all pass with the capture |
| SDK tests `sql`, `queries`, `typed`, `domains` | no | they use the trusted runtime (`rocky-runtime`) and commit no secure SQL |
| inventory, todo, employees, directory examples | no | reducer applications on `rocky-core`; no SQL session exists |

The brief lists the four reducer examples as sources. They have no SQL
write transactions, so there is nothing to capture. Phase 4 should note
that they lie outside the intent model. Typed-row writes through
`SecureTransaction` (todo-desktop) bypass SQL and are not captured either.

Shopping qualification attempts all eight scenarios. Five pass; `gate` fails
its member-directory audit and `branch` and `race-rollback` time out on tip
transitions. All three also fail with the capture feature disabled. Transactions
committed before each failure are included; the corpus does not prove successful
distributed qualification.

## Complete rerun (2026-10-29)

| Source | Transactions | Refused | Coordinated | Constrained | Keyed | Predicate | Split | Statements (mean / max) | Collapse | Not replicated | Migration | Rocky-internal |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| sdk-application | 5 | 0 | 0 | 0 | 4 | 1 | 0 | 60.80 / 300 | 0 | 0 | 0 | 1 |
| sdk-coordinated | 15 | 0 | 14 | 1 | 0 | 0 | 0 | 1.93 / 2 | 0 | 0 | 1 | 4 |
| sdk-group_secrets | 14 | 0 | 0 | 13 | 1 | 0 | 0 | 1.00 / 1 | 0 | 0 | 1 | 83 |
| sdk-partitions | 9 | 0 | 0 | 4 | 5 | 0 | 0 | 0.78 / 1 | 0 | 0 | 1 | 2 |
| sdk-shared_lists | 38 | 0 | 0 | 21 | 16 | 1 | 0 | 1.00 / 1 | 0 | 1 | 3 | 14 |
| sdk-shopping_release | 20 | 0 | 0 | 11 | 9 | 0 | 0 | 0.90 / 1 | 0 | 1 | 0 | 7 |
| shopping-qualification | 51 | 0 | 1 | 35 | 12 | 3 | 0 | 1.63 / 3 | 2 | 1 | 4 | 23 |
| **total** | 152 | 0 | 15 | 85 | 47 | 5 | 0 | 3.24 / 300 | 2 | 3 | 10 | 134 |

| Source | Add | Assign | Append | Keyed delete | Fresh insert | Algebra only |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sdk-application | 0 | 0 | 0 | 0 | 4 | 4 |
| sdk-coordinated | 0 | 0 | 0 | 0 | 15 | 15 |
| sdk-group_secrets | 0 | 1 | 0 | 0 | 13 | 14 |
| sdk-partitions | 0 | 0 | 0 | 0 | 7 | 7 |
| sdk-shared_lists | 0 | 0 | 0 | 1 | 36 | 37 |
| sdk-shopping_release | 0 | 1 | 0 | 2 | 15 | 18 |
| shopping-qualification | 0 | 2 | 0 | 2 | 43 | 45 |
| **total** | 0 | 4 | 0 | 5 | 133 | 140 |

Statement means below 1 come from moves, which commit without SQL
statements. The 300-statement transaction is a bulk insert in
`application.rs`.

## Measured decisions

- **Stop rule:** not triggered. No source refuses any transaction.
- **Phase 6 operation set:** only `fresh_insert` reaches 5% (133 / 152).
  `assign` (4), `keyed_delete` (5) and `add` (0) fall short. Most writes
  are constrained, mainly unique indexes and foreign keys on the shopping
  tables. Few are keyed, and no source increments a counter. Phase 6
  therefore runs with a smaller set. The full-primary-key classification remains unchanged.
- **Seal cadence:** the busiest shopping process committed 13 transactions
  in 60 s. That is automated qualification, faster than human editing,
  and far below 1,024. `ROUTINE_SEAL_TRANSACTIONS` stays at 1,024.


## Shared validation repair (2026-10-29)

The earlier failure reports above describe the original measurement run.
The repair keeps its captured corpus unchanged.

- Fixture relocation skips the separate additive-switch measurement, which
  remains in the migration qualification gates. Both independent pulls remain.
- Slot acquisition reports another writer's stored proposal as `Behind`, so
  retrying clients wait safely. Own unresolved proposals remain `Undecided`.
- Retained-seal offers, holding proofs and checkpoint pages authorize scope
  access across branches, including the authorization repeated after export.
  Live replication still requires the same tip.
- A losing migration adopts the winning bookmark before closing publications;
  publications outside its new path cannot seal that path repeatedly.
- Closing after compaction accepts pruning covered by a base on the inherited
  seal path; pruning without that coverage remains refused.
- The privacy audit gates foreign labels, absent roster tables and outgoing
  envelopes. Public signer keys in signed protocol evidence are reported in
  `member-protocol-keys.json`, rather than treated as leaked roster rows.

`./scripts/lint` passes. The isolated shopping approvals test passes. The full
shopping qualification run passes all eight scenarios, including `gate`,
`branch` and `race-rollback`; the latter converges all four devices and holds
back the member's write past the rollback seal. The Rust regressions for
other-writer slot acquisition and cross-branch seal pulling pass; the latter
also checks closing after compaction.

`NEXTEST_TEST_THREADS=8 ./scripts/check` passes: 212 unit executions and
1,467 integration executions (1,143 workspace and 324 SDK without defaults).
Integration wall time is 243.18 s against the 300 s budget. The relocation
fixture budget is accepted using the runner's isolated confirmation of
629.4 ms after load adjustment (factor 3.4), against its unchanged 2.00 s limit.
Host load exceeded 400 on 18 CPUs during validation; these are not idle-host
measurements. Commit lint also passes.

Evidence is retained under `local/phase02-03-repair/`, including
`check-final.log` and `qualification-final/evidence.json`. These fixes do not
resolve phase 2's separate bounded-holds prerequisite audit.
