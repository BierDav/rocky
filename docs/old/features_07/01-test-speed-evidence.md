# Test speed: baseline and result

Evidence for [phase 1](01-test-speed.md), measured on 2026-10-10. The
budgets these numbers set are in [development](../development.md#test-budgets).

## Host and method

- MacBook Pro `Mac17,8`, Apple M5 Pro, 18 cores (6 super, 12 performance),
  48 GB, macOS 27.0.1.
- Rust 1.99.0 (the `rust-toolchain.toml` version), cargo-nextest 0.9.148,
  Kotlin CLI 0.13.0 with Kotlin 2.4.20; debug test profile, dependencies at
  `opt-level = 2`.
- The host was shared with other agents' builds and tests throughout: load
  averages were 14 to 90 on 18 cores. Per-test times vary by a factor of two
  to three between runs for that reason; compare runs taken back to back.
- Wall times are from start to exit of each command in a jj workspace whose
  `target` directory started empty. "Warm" means a second run with no source
  change.
- CI times could not be read. GitHub starts no job ("recent account payments
  have failed or your spending limit needs to be increased"), and the Gitea
  mirror's Actions API needs a token this host does not hold.

## Baseline

| Command | Wall | Passing | Notes |
| --- | --- | --- | --- |
| `./scripts/lint` | 9 s | all modules | no build |
| `./scripts/check`, cold | 691 s | 708 + 3 doc + 159 | Clippy 60 s, workspace test build 177 s, its tests 209 s, no-defaults build 145 s, its tests 87 s |
| `./scripts/check`, warm | 535 s | 708 + 3 doc + 159 | workspace tests 351 s, no-defaults 93 s; another warm run's workspace tests took 235 s |
| `./scripts/check --slow` | 534 s | 2 | one soak test is 517 s of it |
| `./kotlin check` | 372 s, warm 311 s | 54 | fails after the tests on a stale Android runtime in Maven local |
| `./sdks/native/check` | 119 s, warm 2 s | 2 | |
| `./examples/shoppinglist-app/check` | 344 s | 28 | `shared`'s 27 tests run one at a time in 295 s |
| `./examples/shoppinglist-app/qualify` | 388 s | 4 scenarios | gate 197 s |

Two warm runs of `./scripts/check` failed on load alone: the coordinator's
fsync probe (`mean < 50 ms`, 91 s under load) and a `rocky-secure` fixture
whose chain links read the clock separately, so a link issued after a second
boundary outlived its parent ("delegation expands authority"). The fixture
now reads the clock once per process.

### Compile, link and run

| Step | Time |
| --- | --- |
| Relink one integration test binary after touching it | 3 s |
| Rebuild `rocky-sdk` and its 31 test binaries after touching its library | 16 s |
| Rebuild every workspace test binary after touching `rocky-core` | 42 s |
| First `-p <package>` build after a workspace build | 20 to 50 s once, for its own feature set |
| Run the workspace tests | 209 to 351 s |

Run time dominates; link time does not. Merging integration tests into fewer
binaries was therefore dropped: it would save seconds and edit files phases 2
to 6 also edit.

The sum of the workspace test times was 2,750 s, 153 s over 18 threads, yet the
run took 235 s: the longest tests are in the last binaries, so they started
last and ended the run alone, and the fsync probe waited for every running test
to finish before it ran alone.

### Slowest tests

In the warm baseline run, in seconds; the second column is the test alone.

| Test | Run | Alone |
| --- | --- | --- |
| `rocky-runtime` `stored_state_tests::open_does_not_replay_and_a_domain_outgrows_the_former_quota` | 144 | 7 |
| `rocky-sdk::group_secrets` `an_interrupted_rotation_resumes_to_one_epoch_across_the_group` | 139 | 29 |
| `rocky-runtime` `retention_tests::death_inside_any_step_leaves_a_file_that_opens_and_audits` | 121 | 10 |
| `rocky-sdk::registry` `a_full_registry_refuses_the_257th_device_and_syncs_to_a_new_issuer` | 120 | 36 |
| `rocky-secure` `tests::roles::a_far_behind_replica_catches_up_while_both_sides_keep_writing` | 117 | 48 |
| `rocky-secure` `tests::checkpoint::process_exit_inside_any_checkpoint_step_leaves_a_file_that_opens_and_audits` | 100 | 22 |
| `rocky-secure` `tests::checkpoint::retained_bytes_trigger_checkpoints_and_pruning_then_backpressure` | 82 | |
| `rocky-secure` `tests::existence::horizon_finalization_and_retention_plateau` | 74 | |
| `rocky-sdk::shopping_release` `two_phones_never_online_together_pass_the_release_gate_through_custody` | 69 | 27 |
| `rocky-secure` `tests::roles::a_sync_larger_than_a_session_continues_on_the_next` | 68 | |
| `rocky-secure` `tests::roles::interrupted_transfers_resume_and_a_replacement_full_node_converges` | 56 | |
| `rocky-sdk::revocations` `partitioned_issuers_revocations_merge_when_b_syncs_first` | 56 | |
| `rocky-runtime` `audit_tests::observer_and_retained_history_quotas_preserve_existing_work` | 56 | |
| `rocky-sdk::driver` `bundle_limits_and_the_lease_fail_the_transaction_without_splitting_it` | 54 | |
| `rocky-sdk::revocations` `partitioned_issuers_revocations_merge_when_a_syncs_first` | 53 | |
| `rocky-coordinator::public` `outages_and_restarts_keep_state_and_restore_is_fenced` | 53 | 50 |
| `rocky-secure` `tests::strong_history::executor_and_reader_cross_commands_with_a_window_and_recover_from_the_base` | 47 | |
| `rocky-coordinator::public_runtime` `an_application_runs_strong_commands_on_a_registered_domain` | 42 | |
| `rocky-secure` `tests::partition_foreign_keys::row_moves::process::six_scenarios_across_device_processes` | 40 | |
| `rocky-coordinator` `raft_store::probe::coordinator_state_settles_after_one_retention_window` | 39 | 23 |

The slowest binaries by summed test time: `rocky-secure`'s library tests
1,559 s (187 tests), `rocky-runtime`'s 657 s (159), `rocky-sdk::registry` 237 s
(7), `rocky-sdk::group_secrets` 193 s (8), `rocky-sdk::shopping_release` 133 s
(3), `rocky-sdk::revocations` 109 s (2).

Most of them are CPU-bound and slow only under contention. Two wait in
production code, which this phase does not change:

- `outages_and_restarts_keep_state_and_restore_is_fenced` spends 50 s even
  alone, mostly in the coordinator client's 12 s request deadline, which the
  quorum-outage checks wait out twice, and in voter restarts.
- `a_full_registry_refuses_the_257th_device_and_syncs_to_a_new_issuer` spends
  49 s on 253 grants (about 190 ms each) and 25 s on one sync under load.

### Per package

`cargo nextest run -p <package>` run time after the build, in seconds:
`rocky-core` 0.5, `rocky-credentials` 0.2, `rocky-storage` 1.4, `rocky-iroh`
0.1, `rocky-runtime` 17, `rocky-secure` 60, `rocky-sdk` 74,
`rocky-coordinator` 84.

## Changes

- `.config/nextest.toml`: the default profile terminates a test after 60 s;
  CI allows 120 s because its runners could not be measured. The 24 tests that
  ran 30 s or more in a baseline run are named: they start first, at most
  eight at once, and may take 10 minutes. The fsync probe runs first, alone,
  instead of waiting for the run to drain. The soak limit moved to the default
  profile, so `--slow` keeps it locally.
- `examples/shoppinglist-app/shared` runs its JVM tests six at a time
  (`testResources@jvm/junit-platform.properties`); each test opens its own
  devices in a temporary directory.
- The CI `check` and `slow` jobs compile nothing when their archives arrive,
  so they restore the Rust cache only when one is missing.
- The jobs after `build` use two composite actions, `reuse-build` and
  `record-pass`, the one place a later phase adds a job
  ([adding a CI job](../development.md#adding-a-ci-job)).
- The `rocky-secure` fixture reads the clock once per process.

Dropped, with their cause:

- Fewer, larger integration binaries: link and compile take seconds against
  minutes of run time (above).
- Tokio's paused clock: none of the slowest tests waits on a sleep of its own.
  Their waits are the coordinator client's request deadline and Raft elections
  in voter processes, and the 5 s write lease; changing those is production
  code.
- Fixtures shared per binary: not attempted. Where measured, the cost is the
  scenario itself (the registry test's grants and sync are 74 of its 83 s),
  and sharing fixtures edits test files that phases 2 to 4 also edit.

## Result

| Command | Baseline | Result | Passing |
| --- | --- | --- | --- |
| `./scripts/lint` | 9 s | 5 to 8 s | all modules |
| `./scripts/check`, warm | 535 s | 250 s, 300 s; 241 s rebased | 708 + 3 doc + 159 |
| `./scripts/check --slow` | 534 s | 531 s, 360 s | 2 |
| `./kotlin check`, warm | 311 s, failing | 110 s, 127 s | 54 |
| `./sdks/native/check`, warm | 2 s | 2 to 3 s | 2 |
| `./examples/shoppinglist-app/check`, warm | 344 s | 82 to 97 s | 28 |
| `./examples/shoppinglist-app/qualify` | 388 s | not rerun; unchanged | 4 scenarios |

No test was deleted, merged or ignored by this phase. The result runs predate
`main`'s commit that made the doc tests opt-in (`./scripts/check --doc`);
after rebasing, `./scripts/check` runs 708 + 159 and the 3 doc tests pass with
`cargo test --workspace --doc`. The shopping `shared` tests take 73
to 104 s concurrently instead of 295 s; `./kotlin check` now ends with them,
since Amper runs the modules in parallel.

The workspace tests alone, run back to back under similar load with the
`ci` profile, alternating the configurations:

| Configuration | Run 1 | Run 2 |
| --- | --- | --- |
| Before | 328 s, the fsync probe failed | 224 s, a coordinator cluster test failed |
| Long tests first, uncapped | 210 s, a coordinator cluster test failed | 220 s |
| Long tests first, eight at a time (kept) | 265 s | 176 s |

Per package after the build, in seconds: `rocky-coordinator` 72,
`rocky-sdk` 56, `rocky-secure` 58, `rocky-runtime` 18, every other package
under 2. The coordinator misses the 60 s loop budget by the fsync probe, about
20 s alone, plus `outages_and_restarts_keep_state_and_restore_is_fenced`, its
largest test, about 50 s even alone.

### The per-test limit

A temporary `rocky-core` test sleeping 150 s, removed afterwards:

- `./scripts/check` reported it `SLOW` at 30 s and `TIMEOUT [60.010s]`,
  cancelled the run and exited 100.
- `cargo nextest run --profile ci` reported `TIMEOUT [120.005s]` and exited
  100. The CI workflow runs that profile; no hosted run was observed.

### Failures seen under load

Each passed on rerun and none is caused by this phase:

- the coordinator's fsync probe (`mean < 50 ms`), with the previous
  configuration as well and once in the final run while it ran alone (a 56 ms
  mean), so the disk was busy with other processes;
- `rocky-coordinator::cluster` `crashes_at_log_commit_apply_reply_and_snapshot_boundaries`
  with the previous configuration, and
  `certified_lost_reply_survives_another_executors_advance` (`UnknownDomain`
  right after the domain was registered) with the new one;
- `rocky-sdk::driver` `bundle_limits_and_the_lease_fail_the_transaction_without_splitting_it`
  (the 5 s write lease) and the 257th-device registry test, when the long
  tests all started together; capping them at eight followed;
- `RoomGateTest` once, `Address already in use`, recorded in
  [fix later](../fix_later/README.md#kotlin-tests-choose-tcp-ports-that-another-process-can-take).
