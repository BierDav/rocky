# 0060: Three test tiers with enforced budgets

Status: accepted.
Date: 2026-10-10.

## Context

Slow tests slowed every edit loop: 708 workspace tests took 2,110 s of test
time, and almost every library test ran a scenario with SQLite files, replicas
or processes. The budgets are fixed:

| Tier | Per test | Suite | Runs |
| --- | --- | --- | --- |
| unit | 10 ms | 10 s | every edit loop, `scripts/check`, CI |
| integration | 2 s | 5 min | `scripts/check` before finishing work, CI |
| end to end (e2e) | none | 15 min | CI only |

The reference host is shared with other agents: load averages of 2 to 90 on 18
cores, and inside a parallel run a scenario takes 5 to 15 times as long as
alone. nextest runs each test in
its own process, so its per-test times include process start: `rocky-core`'s 50
library tests took a median of 7 ms under nextest and 0.35 ms in process.

## Decision

**Membership.** `.config/test-tiers.toml` gives each tier a nextest filterset.
A test is e2e if the e2e filter matches, else integration if that filter
matches, else unit. Membership is total by construction; a new library test
starts as a unit test, a new `tests/` test as an integration test. Integration
rules name modules (the `rocky-secure` scenario module, `rocky-runtime`'s
`*_tests` modules, the storage and SDK modules that open files), not tests.
The e2e filter names tests, each over 1.5 s alone when it was classified.

**Measurement.** `scripts/test-tiers` lists a tier with `cargo nextest list`.
Unit tests run in process with libtest, binary by binary as `cargo test` does,
and their time is libtest's own `exec_time`, without process start; its JSON
output is unstable, so `RUSTC_BOOTSTRAP=1` is set for the test binaries only,
never for rustc. Integration and e2e tests run under nextest, timed from its
JUnit report. A suite's time is the wall time of its runs over both builds
(the workspace, and `rocky-sdk` without default features).

**Jitter.** The load factor of a run is `max(1, min(load before, load after) /
cores)`, with the 1-minute load average.

- A test over its budget in the tier run is re-run alone, in process, up to
  three times; it fails only if its best time divided by the load factor of
  that re-run is still over budget. Load only adds time, so the best of a few
  isolated runs is the stable estimate.
- A test over ten times its budget after scaling by the run's load factor
  fails without a re-run. The factor is lower than the slowdown inside the run,
  where the tier's own tests compete, so the shortcut needs that margin.
- Locally, a test confirmed under budget alone is not re-run while that alone
  time, multiplied by the growth of its time in the run since, stays within
  three quarters of its budget. Without this, re-runs of tests that are fine
  alone doubled the integration tier's time. CI keeps no such record.
- A suite fails when its wall time divided by the run's load factor exceeds
  its budget.
- nextest's `slow-timeout` stays as a backstop that kills hung tests: 30 s in
  the integration profile, 10 minutes in e2e and 30 for a soak test. In e2e
  the suite budget is what binds.

**Exceptions.** A `[[exception]]` entry raises one test's budget and must give
the test, limit, measured time, cause and an expiry date; an expired entry
fails the check. There were none when this was accepted.

**Builds.** `rocky-core` and `rocky-credentials` build at `opt-level = 1` in the
dev profile. Generic code such as serialization and byte encoding is compiled
in the crate that instantiates it, so the dependency `opt-level = 2` did not
reach it; profiles of scenario tests showed it and debug precondition checks
dominating. It made scenario tests 2 to 5 times faster with no slower
incremental build. `scripts/cranelift` installs a pinned nightly whose
Cranelift backend builds the workspace crates for `scripts/check --unit` only,
with dependencies on LLVM; that edit loop builds only the workspace, not also
`rocky-sdk` without default features.

## Consequences

- `scripts/check` runs formatting, Clippy, unit and integration; `--unit`,
  `--integration` and `--e2e` run one tier. `--slow` is gone: the soak tests
  are e2e. CI's `check` job runs unit and integration from the build archives,
  and the `e2e` job, which replaced `slow`, runs on pushes to `main`.
- A test that misses its budget is made faster or, if it is a different kind
  of test, moved to the tier that matches it with its measured time.
- Python 3.11 or newer is needed for `scripts/test-tiers` (`tomllib`).
- E2E tests no longer run before local work finishes. Their coverage depends
  on CI, which no hosted run has verified yet.

## Rejected

- **Maelstrom** for integration tests: it is Linux-only, built on namespaces,
  and the reference host is macOS.
- **nextest wall time for unit tests**: process start takes most of the 10 ms.
- **Cranelift for every build**: on aarch64 macOS a panic that unwinds through
  Cranelift-compiled frames aborts (`failed to initiate panic, error 5`), which
  broke a test that catches a handler's panic; all crates on Cranelift also made
  `rocky-secure`'s tests 20 to 28 % slower. So the full check and CI use LLVM.
- **`opt-level = 1` for more workspace crates**: adding `rocky-storage`,
  `rocky-secure` and `rocky-runtime` saved under 25 % more and rebuilds larger
  crates optimized.
- **A test switch for SQLite's `fullfsync`**: `F_FULLFSYNC` is the next largest
  wait, but it is production durability; changing it for tests needs its own
  decision.
- **Re-running every offender on every local check**: about 90 s more per
  integration run. **Judging the run's times directly**: it failed most
  scenarios, which are slow there only because they compete with their tier.
