# Test tiers: measurements

Evidence for [decision 0060](../decisions/0060-test-tiers-and-budgets.md) and
the [test tiers](../testing.md), measured on 2026-10-10 after
[phase 1](01-test-speed-evidence.md). The budgets are in
[development](../development.md#test-budgets).

## Host and method

The phase 1 host: MacBook Pro `Mac17,8`, Apple M5 Pro, 18 cores, 48 GB,
macOS 27.0.1, Rust 1.99.0, cargo-nextest 0.9.148, Python 3.14. The Cranelift
runs used `nightly-2026-10-10` (rustc 1.101.0-nightly). Other agents shared the
host: 1-minute load averages were 3 to 75 on 18 cores and are given with each
number as `load`. Each A/B alternated its two configurations back to back.
The disk filled once during these runs; every separate target directory
holds 10 to 40 GB.

## Where the time went

All 510 library tests, run in process with libtest (cargo test's harness),
binary by binary, at load 11 to 17: 130 under 10 ms, 309 between 10 ms and
2 s, 71 over 2 s. They took 56 s for `rocky-secure`, 22 s for
`rocky-coordinator` and 17 s for `rocky-runtime`; every other binary took
under 2 s. The slow ones sit in scenario modules: `rocky-secure`'s `tests::`,
`rocky-runtime`'s `*_tests` modules, and modules that open SQLite files.

nextest's per-test times include process start: `rocky-core`'s 50 library
tests took a median of 7 ms (6 to 15 ms) under nextest in phase 1 and 0.35 ms
in process.

The slow scenarios are CPU-bound: `tests::roles::a_far_behind_replica_catches_up_while_both_sides_keep_writing`
took 29.9 s real and 26.8 s user alone at load 4. A `sample` profile of it put
the most samples, after idle worker threads, in standard library and ciborium
generics instantiated in `rocky-core` and `rocky-credentials`
(`ptr::is_aligned_to`, the `from_raw_parts` and `copy_nonoverlapping`
precondition checks, `Vec::spec_extend`, ciborium's encoder and decoder). They
are compiled at the workspace crates' `opt-level = 0`, which the dependencies'
`opt-level = 2` does not reach. With those two crates at `opt-level = 1`, the
next largest entry was `__fcntl`: SQLite's `F_FULLFSYNC`.

## Changes and their effect

### `opt-level = 1` for `rocky-core` and `rocky-credentials`

Single tests alone, in seconds, without and with the change, at load 6 to 9:

| Test | Before | After |
| --- | --- | --- |
| `rocky-secure` `tests::roles::a_partial_replica_holds_only_its_subscription` | 5.07 | 1.07 |
| `rocky-secure` `tests::existence::horizon_finalization_and_retention_plateau` | 17.47 | 7.29 |
| `rocky-secure` `tests::generations::a_local_plan_survives_an_interruption_at_every_step` | 4.30 | 1.87 |
| `rocky-runtime` `instrumentation_tests::checkpoint_intervals_are_recorded_in_monotonic_order_when_instrumented` | 2.47 | 2.55 |
| `rocky-sdk::faults` `a_slow_reader_holds_the_writer_back_without_loss` | 4.84 | 2.61 |
| `rocky-sdk::shared_lists` `a_joined_writer_obtains_its_authors_and_activity_shows_the_list_arriving` | 2.29 | 0.89 |
| `rocky-sdk::registry` `concurrent_enrollments_and_renewals_at_two_issuers_converge` | 2.91 | 1.35 |
| `rocky-sdk::bootstrap` `join_restores_large_rows_across_checkpoint_pages` | 6.61 | 2.57 |
| `rocky-coordinator` `raft_store::tests::openraft_storage_contract` | 10.77 | 10.31 |

Rebuilding every workspace test binary after touching a crate, in seconds:
`rocky-core` 15, 17 without and 12, 10 with; `rocky-credentials` 13, 15
without and 11, 11 with (load 8 to 27). A cold build of the workspace tests
took 96 s at load 4.

Adding `rocky-storage`, `rocky-secure` and `rocky-runtime` at `opt-level = 1`
changed the same tests by 0 to 25 % (for example 7.40 to 6.60 s and 2.57 to
2.05 s, load 9 to 20) and rebuilt those crates optimized; it was not kept.

### Cranelift for the edit loop

Rebuilding every workspace test binary after touching `rocky-core`:

| Run | LLVM | Cranelift, dependencies on LLVM |
| --- | --- | --- |
| 1 | 28 s, load 42 | 12 s, load 53 |
| 2 | 18 s, load 59 | 11 s, load 74 |

A cold build of the workspace tests took 116 s with Cranelift for the
workspace crates, 52 s with every crate on Cranelift (load 11 to 14).
`rocky-secure`'s library tests ran in 59 and 58 s from the Cranelift build
against 61 and 63 s from LLVM (load 19 to 21). With every crate on Cranelift
they ran in 69 and 73 s against 61 and 57 s, because the crypto dependencies
lost their optimization.

On aarch64 macOS, Cranelift-compiled code cannot unwind a panic into a
`catch_unwind` or a thread join: `rocky-sdk` `commands::tests::the_host_bounds_a_call_and_survives_a_stuck_or_crashing_handler`
aborted with `failed to initiate panic, error 5`, and so did a minimal program
that joins a panicking thread. So only `scripts/check --unit` uses Cranelift.

### Not adopted

- Maelstrom: Linux-only.
- A test switch for `fullfsync`: production durability.
- Tokio's paused clock and the in-process hub for the remaining waits: the
  waits are production deadlines and leases (phase 1 found the same).

## Tier runs

The final runs, back to back, with a warm build; both builds of the tests
(the workspace, and `rocky-sdk` without default features) count toward each
tier:

| Command | Wall | Load | Tier result |
| --- | --- | --- | --- |
| `./scripts/check` | 249 s | 4.4 to 8.2 | unit: 132 passed in 0.12 s; integration: 682 passed in 97.8 s, none over 2 s alone (slowest 1.52 s) |
| `./scripts/check --e2e` | 273 s | 3.5 to 5.5 | 55 passed in 272.4 s, of 900 s |
| `./scripts/check --from` the `--build` archives, `CI=true` | 222 s | 5.6 to 11.9 | the same 132 and 682 passed; every test over budget re-run (the CI path; its e2e run was not repeated) |
| `./scripts/check --unit` after touching `rocky-core` | 11 s, 11 s on Cranelift; 33 s, 38 s on LLVM | 11 to 19 | 118 passed in under 0.1 s |
| `./scripts/lint` | 25 s | 7 to 9, beside a check | |
| `./kotlin check` | 95 s | 3 to 7 | 54 passed |
| `./sdks/native/check` | 6 s | 6 | 2 passed |
| `./examples/shoppinglist-app/check` | 60 s | 6 to 7 | 28 passed |

Before the change the same tests were 708 + 159 in `./scripts/check` (241 to
300 s) and 2 soak tests in `--slow` (360 to 531 s). Now 132 + 682 + 53 run in
the three tiers, plus the same 2 soak tests in e2e: 869, none fewer.
`./kotlin check` needed the Android runtime republished first (90 s).

An earlier check, before four file-writing unit tests moved to integration,
failed on them: 10.0 to 13.6 ms alone at load 9 (`rocky-coordinator`
`machine::public_tests`, `rocky-credentials` `keys::tests`, `rocky-storage`
`database::tests`).

Inside the integration run tests take 5 to 15 times longer than alone, since
each scenario starts its own runtimes and writer threads: for example
`rocky-runtime` `retention_tests::a_failure_at_any_step_leaves_a_file_that_reopens_and_audits`
took 11.5 s in the run and 0.41 s alone. Re-running every such test alone
took about 90 s per check (64.9 s of tests, 159 s in all, load 3 to 4), which
the local record of confirmed tests avoids.

### The probes

Temporary tests in `todo-app`, removed afterwards:

- Unit, a 30 ms sleep and an 11 s sleep: `scripts/test-tiers unit` exited 1
  with `over the unit budget` for both (34.0 ms alone; 11.00 s, not re-run)
  and `the unit suite took 11.09s scaled by load, over its 10.00s budget`
  (load 3).
- Integration, a 3 s sleep in a `tests/` target: exited 1 with
  `over the integration budget: todo-app::tier_probe over_the_integration_budget, 3.00s`
  (load 3 to 4).
- E2E, an ignored `soak_` test: sleeping 660 s it passed, because the soak
  override lets a test run 30 minutes in every profile; sleeping 960 s,
  `scripts/test-tiers e2e` exited 1 with
  `the e2e suite took 981.26s scaled by load, over its 900.00s budget`
  (load 2 to 5).

## Classification

Unit: the library and binary tests outside the integration filter, 174 in the
workspace build. Integration: every `tests/` target plus the modules the filter
names. E2E: 41 tests, listed in `.config/test-tiers.toml`:

- 23 of the 24 long tests phase 1 named, which took 30 s or more under load;
  the 24th, `rocky-secure`
  `tests::checkpoint::offline_replica_learns_it_is_behind_and_rebootstraps_with_its_local_edits`,
  now fits the integration budget;
- the two soak tests and the fsync probe;
- 11 more over 2 s alone with the opt-level change, at load 3 to 9: the
  coordinator's Raft storage contract and its crash-boundary cluster test
  (21 and 25 s in the run, not re-run), the persistent-voter cluster test
  (6.6 s), the restored-service fence test (3.8 s) and the applications quorum
  test (2.2 s); the service drain test (5.4 s; it asserts a drain of 4 to
  15 s); the three SDK fault tests and the bootstrap join (2.5 to 2.7 s); the
  chunk transport test (2.1 s); and the instrumentation interval test (2.4 s,
  nearly all waiting);
- 4 within 25 % of the budget alone, which would flap: 1.58, 1.75, 1.78 and
  1.97 s.

The two partitioned revocation tests are among the 23; they also fail
intermittently (below).

No test was deleted, merged or newly ignored; the two soak tests' ignore reason
now names the e2e tier instead of `--slow`.

## Failures seen

- `rocky-sdk::revocations` `partitioned_issuers_revocations_merge_when_{a,b}_syncs_first`
  fail intermittently, more often when the host is quiet: 2 of 8 nextest runs
  at load 9 to 20, most direct runs at load 5 (`revocations.rs:169`, the
  member holds 0 statements instead of 2 after its sync). It predates this
  work and needs a fix in the test or the revocation exchange.
- The nextest integration profile killed the 8 longest scenarios at 30 s before
  they moved to e2e.
