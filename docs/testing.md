# Test tiers

Every Rust test belongs to one of three tiers, each with a budget that
`scripts/test-tiers` enforces. [Decision 0060](decisions/0060-test-tiers-and-budgets.md)
explains the model and the measurement; the measured runs are in the
[tier evidence](features_07/01-test-tiers-evidence.md).

| Tier | Per test, alone | Suite | Runs | Holds |
| --- | --- | --- | --- | --- |
| unit | 10 ms, in process | 10 s | `./scripts/check --unit`, `./scripts/check`, CI | logic on values in memory: no files, processes, sockets or runtimes |
| integration | 2 s | 5 min | `./scripts/check`, `--integration`, CI | SQLite files, runtimes, replicas over the in-process hub, single scenarios |
| e2e | none | 15 min | CI's `e2e` job; `./scripts/check --e2e` on demand | crash sweeps over every step, full-size limits, multi-process clusters, waits on production deadlines, soak tests, the fsync probe |

## Which tier a test is in

[`.config/test-tiers.toml`](../.config/test-tiers.toml) holds a nextest
filterset per tier. A test is e2e if the e2e filter matches, else integration
if that one matches, else unit. So a new library test is a unit test, and a new
test in a `tests/` target is an integration test, until a filter says
otherwise. The integration filter names modules; the e2e filter names tests.

Print a tier's tests, here for the workspace build:

```sh
scripts/test-tiers unit --list
```

`#[ignore]`d tests run only in e2e and only when its filter names them, like
the `soak_*` tests; the other ignored tests are crash-child helpers and explicit
measurements.

## How the budgets are judged

- Unit tests run in process with libtest, one binary after another, timed by
  libtest itself. Integration and e2e tests run under nextest and are timed
  from its JUnit report.
- A test over budget is re-run alone up to three times; its best time, divided
  by the host's load factor, decides. A test over ten times its budget fails at
  once.
- Locally, a test already confirmed alone is not re-run while that time,
  grown as much as its time in the run has, stays within three quarters of
  its budget (`target/test-tiers/confirmed-<tier>.json`). CI re-runs them all.
- A suite's wall time, divided by the load factor, must fit its budget.
- nextest still kills an integration test at 30 s, an e2e test at 10 min and
  a soak test at 30 min; the e2e suite budget binds first.

Each run writes every test's time to `target/test-tiers/<tier>-<build>.tsv`,
slowest first.

## When a test misses its budget

Make it faster first; move it only if it is a different kind of test.

- Build expensive state once per binary behind a `OnceLock` and copy it:
  keys, signed chains, template databases.
- Test a boundary on constructed state instead of driving hundreds of real
  operations; keep one full-size run in e2e.
- Use the in-process hub and its deterministic faults instead of TCP and
  process restarts; Tokio's paused clock for timeouts the code under test
  drives.
- Make sizes and iteration counts configurable through existing test
  configuration. Never change production defaults, limits or timeouts, and
  never weaken an assertion.

A unit test that needs files or a runtime belongs to integration: add its
module to the integration filter. An integration test that needs more than
2 s alone belongs to e2e: add it to the e2e filter with its measured time
in the evidence. Keep a cheaper variant in the faster tier where one exists.

An exception is the last resort. It raises one test's budget until it expires,
and every field is required:

```toml
[[exception]]
test = "rocky-sdk::registry a_test_name"  # binary id and test name
limit = "3s"
measured = "2.6s alone at load 4"
cause = "the 5 s write lease; fixed by …"
expires = 2026-11-01
```

## Cranelift

`scripts/cranelift install` installs a pinned nightly with the Cranelift
backend under `local/cranelift`. Once it is there, `./scripts/check --unit`
builds the workspace crates with Cranelift into `target/cranelift`, with
dependencies on LLVM; `scripts/cranelift cargo …` runs any cargo command the
same way. `ROCKY_CRANELIFT=0` turns it off.

After touching `rocky-core`, `./scripts/check --unit` took 11 s with Cranelift
against 33 to 38 s with LLVM, and the tests ran as fast. The edit loop builds
only the workspace; the full check adds `rocky-sdk` without default features. It is only
for the edit loop: on aarch64 macOS a panic that unwinds through
Cranelift-compiled frames aborts, so a test that catches a panic fails there.
The full check, Clippy and CI use LLVM. The second target directory takes about
10 GB.

## Kotlin tests

The JVM tests are not tiered by the checker yet; `./kotlin check` and the
shopping check run them with their phase 1 budgets. Their classification, for
when they are:

| Module | Tests | Tier | Why |
| --- | --- | --- | --- |
| `sdks/kotlin/rocky-core` | 5 | unit and integration | value mapping without the native library is unit; anything that loads it is integration |
| `sdks/kotlin/rocky-sqlite` | 2 | integration | opens SQLite files through the driver |
| `sdks/kotlin/room-qualification` | 13 | integration | Room on Rocky's file; `RoomGateTest` binds TCP ports and needs the free-port fix in [fix later](fix_later/README.md) before it runs in parallel |
| `sdks/kotlin/schema-qualification` | 2 | integration | about 55 s in phase 1, so over budget until made faster |
| `examples/shoppinglist-app/shared` | 28 | integration | devices in temporary directories, six at a time |
| `examples/market-app/marketShared` | 6 | integration | devices in temporary directories; each under 1.7 s alone, the native library loaded once per class |
| shopping and market qualification, packaged consumer | | e2e | several JVMs, voters and the custody peer; CI only |
