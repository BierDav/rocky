# 1. Test speed

Status: not started.
Owner: `scripts/check`, the nextest configuration, CI workflows and the Kotlin
check tooling. No dependency. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [development](../development.md), `scripts/check`, `.github/workflows`
and the check scripts of `sdks/native`, `sdks/kotlin` and the shopping
example. Checking is the slowest step of every phase, and this roadmap adds an
iOS matrix and multi-issuer concurrency tests. Make the development loop and
the full gate measurably faster without removing coverage.

## Ordered slices

1. Baseline on the qualification host: wall time of `./scripts/check`, of
   each Kotlin, native and example check, and of each CI job; the twenty
   slowest tests and test binaries; compile time against run time.
2. From the baseline, set a budget for the targeted loop
   (`cargo test -p <package>`), for `./scripts/check` and for CI, and record
   them in [development](../development.md).
3. Meet the budgets. Candidates, in the order the baseline justifies:
   - tests that wait on timeouts or sleeps move to injected clocks and the
     in-process hub's faults;
   - fixtures built once per binary instead of per test;
   - fewer, larger integration binaries where link time dominates;
   - measurement probes and soaks kept out of the default profile;
   - Kotlin checks that rebuild the native library reuse one build;
   - CI caches for Cargo, the Kotlin toolchain and the Android runtime.
4. A check that fails when a single test exceeds a stated duration, so the
   budget does not erode.

## Completion gate

- Baseline and result are recorded with host, toolchain and commands.
- The three budgets are met, or each miss names its cause.
- No test was deleted or ignored to meet a budget without a stated reason.
- The number of passing tests is not lower than at the baseline.
- `./scripts/check` and the Kotlin, native and example checks pass.

## Not in this phase

Faster production code, a different test framework and remote build caches.
