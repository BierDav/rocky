# 1. Test speed

Status: implemented on 2026-10-10; completion gate met on the developer host,
with the [baseline and result](01-test-speed-evidence.md) and the
[budgets](../development.md#test-budgets). The warm `./scripts/check` went from
535 to 241–300 s, `./kotlin check` from 311 to 110–127 s and the shopping check
from 344 to 82–97 s, with every test still passing. The `rocky-coordinator` package
loop misses 60 s at 72 s; the cause and its largest test are named. Integration
binaries were not merged: link time does not dominate. No hosted CI run was
observed (GitHub starts no job for billing; the Gitea API needs a token), so the
CI budgets and the CI side of the per-test limit were checked only with the
`ci` profile locally.
Owner: `scripts/check`, `.config/nextest.toml`, `.github/workflows`,
`sdks/native/check`, the root `./kotlin check` and
`examples/shoppinglist-app/check`. No dependency. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [development](../development.md), `scripts/check`,
`.config/nextest.toml`, `.github/workflows/check.yml` and `release.yml`, and
the three check commands in the owner line. Checking is the slowest step of
every phase, and this roadmap adds an iOS matrix, a market harness and
multi-issuer concurrency tests. Make the development loop and the full gate
measurably faster without removing coverage, and leave a workflow layout that
later phases add jobs to.

## Settled

- The baseline is taken with the staged `scripts/check` (`--build` writes
  nextest archives, `--from` runs them) landed and described in
  [development](../development.md#shared-checks). If the CI section there
  still describes a single `check` job, correct it in slice 1.
- The baseline host is the developer machine that runs device work, an Apple
  silicon Mac, named with its model, core count and toolchain. CI times are
  read from the hosted `macos-latest` runner and the Gitea runner separately.
- Starting budgets, replaced by measured ones in slice 2 and never loosened
  beyond these without a stated cause:
  - targeted loop: the tests of any one package finish within 60 s after an
    incremental build;
  - `./scripts/check` with a warm build: 10 minutes;
  - CI from push to a green `check` job with a restored cache: 30 minutes,
    and `lint` 5 minutes;
  - a single test: 60 s, the existing slow-test period.
- Already done and not candidates: soak tests are ignored and run by
  `./scripts/check --slow`; CI caches Cargo, the Kotlin toolchain and the
  Android runtime; the `ci` profile terminates a test after ten 60-second
  periods.
- Merging integration tests into fewer binaries and sharing fixtures across
  tests edits files that phases 2 to 6 also edit. That work finishes before
  wave 1 starts or is dropped and recorded as dropped.
- The workflow restructuring lands before phases 17, 18, 19 and 21 add their
  CI jobs ([hazards](README.md#shared-format-hazards)).

## Ordered slices

1. Baseline: wall time of `./scripts/lint`, `./scripts/check`,
   `./scripts/check --slow`, `./kotlin check`, `./sdks/native/check`,
   `./examples/shoppinglist-app/check` and `qualify`, and of each CI job; the
   twenty slowest tests and test binaries; compile and link time against run
   time; the number of passing tests per command.
2. Replace the starting budgets with measured ones and record them in
   [development](../development.md), with the command that checks each.
3. Meet the budgets. Candidates, in the order the baseline justifies:
   - tests that wait on timeouts or sleeps move to Tokio's paused clock and
     the in-process hub's faults;
   - fixtures built once per binary instead of per test;
   - fewer, larger integration binaries where link time dominates, within the
     wave 0 limit above;
   - Kotlin and example checks that rebuild the native library reuse one
     build;
   - CI jobs that still compile what the `build` job archived.
4. The per-test limit also applies locally: the default nextest profile
   terminates a test that exceeds the budget of slice 2, with named overrides
   for the tests that legitimately need longer.
5. Workflow layout: one documented place where a later phase adds a job that
   reuses the build archives and caches, described in
   [development](../development.md#ci).

## Completion gate

- Baseline and result are recorded with host, toolchain and commands.
- Each budget is met, or the miss names its cause and its largest test.
- The number of passing tests of each command is not lower than at the
  baseline; a test deleted, merged or ignored is listed with its reason.
- A test that sleeps past the per-test limit fails `./scripts/check` locally
  and in CI, shown once with a deliberate probe that is then removed.
- `./scripts/lint`, `./scripts/check`, `./scripts/check --slow`,
  `./kotlin check`, `./sdks/native/check` and the shopping check pass.

## Not in this phase

- Faster production code, changed production timeouts or weaker assertions.
- A different test framework, remote build caches or new runners.
- Optimization levels of workspace crates without a measurement that
  justifies them.
- The iOS simulator job ([phase 17](17-kotlin-native-binding.md)), the iOS
  packaged-consumer job
  ([phase 18](18-ios-packaging-keychain-lifecycle.md)), the market job
  ([phase 19](19-market-example.md)) and the application simulator job
  ([phase 21](21-shopping-and-market-on-ios.md)).
- Provisioning or reconfiguring the Gitea runners.
