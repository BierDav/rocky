# Phase 5 handoff: Phase 9 invoice baseline

Historical handoff status: blocked by a pre-existing Phase 9 failure (2026-10-15).
The subsequent split and successful verification are recorded below.

Only `/Users/david-bieregger/AgentWorkspaces/rocky-02` was changed. The inherited
Phase 5 change is `zlrwrvzz` (`8cd702bb`), on Phase 9 `uywtwqpr` (`37d094d3`).
It already includes proposed Phase 9 delivery retry and void-counter repairs;
those edits were preserved, without further production changes.

## Reproduction

The same single-test command was used on each revision:

```sh
cargo test -p rocky-sdk --no-default-features --test coordinated \
  trusted_devices_number_concurrent_invoices_without_gaps --locked -- --exact --nocapture
```

On the inherited Phase 5 tree, the initial run passed:

```text
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.81s
```

An empty temporary change `swlqxrzs` (`9385253f`) was created with
`jj new uywtwqpr`. The first baseline run passed in 0.54 s. A further 20 runs
produced **8 passes and 12 failures**: eight `no invoice committed`, three
`secure_voids` uniqueness errors and one incomplete projection. Exact excerpts:

```text
BASELINE RUN 3, exit 101
thread 'trusted_devices_number_concurrent_invoices_without_gaps' (69599908) panicked at crates/rocky-sdk/tests/coordinated.rs:95:5:
no invoice committed
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.67s

BASELINE RUN 6, exit 101
thread 'trusted_devices_number_concurrent_invoices_without_gaps' (69600422) panicked at crates/rocky-sdk/tests/coordinated.rs:86:17:
{"code":"storage","message":"SQLite storage: UNIQUE constraint failed: secure_voids.scope, secure_voids.author, secure_voids.sequence","retryable":true}

BASELINE RUN 8, exit 101
thread 'trusted_devices_number_concurrent_invoices_without_gaps' (69601692) panicked at crates/rocky-sdk/tests/coordinated.rs:110:10:
keys: Object {"code": String("missing_dependencies"), "message": String("SQL session: items is not available: Incomplete"), "retryable": Bool(true)}
```

The complete 20-run output is `/tmp/rocky-02-phase9-baseline-repeat.log`.
An orphaned prior-session `./scripts/check` was discovered using this workspace's
Cargo target. It was terminated before baseline run 1 completed; runs 2–20 had
no overlap with that check. Its mixed-revision results are invalid as a gate.

After `jj edit zlrwrvzz`, only the empty temporary baseline change was abandoned.
Three further runs on the inherited Phase 5 tree all passed, in 0.64 s, 0.52 s
and 0.52 s. Output: `/tmp/rocky-02-phase5-handoff-repeat.log`.
Including the initial sample, the inherited tree passed 4/4 runs. This sample
qualifies neither the proposed Phase 9 repairs nor their timing under full load.

The handoff documentation change passed `./scripts/lint` (exit 0; six lint
self-tests) and `scripts/commitlint --jj @` (exit 0).

## Disposition and limits

The unchanged Phase 9 parent reproduces the exact blocker intermittently.
Phase 5 is therefore not required to cause it. Per the requested stop condition,
no Phase 9 code was changed and no remaining implementation or full check was
attempted after establishing that prerequisite failure. The previous full run's
174 passing unit tests, 1,028 passing integration tests and one failed invoice
test remain historical results, not a successful gate for this handoff.

The inherited Phase 9 repairs remain mixed into `zlrwrvzz`; their ownership and
qualification must be resolved before declaring Phase 5 complete. No test sleeps,
retries, assertions, deadlines, tiers or format versions were changed here.
Manual dialog interaction, mobile and real-network rollout qualification remain
unproven. Phase 12 owns the full shopping rollout across application versions.

## Subsequent split verification (2026-10-15)

The original handoff record above is preserved as history. The repair is now
isolated in `ptxlnlqx` on unchanged `uywtwqpr`; Phase 5 `zlrwrvzz` follows it,
and this evidence change `woxunxwp` remains on top. No other work was moved.
Repair-only verification passed 21/21 consecutive unchanged invoice runs and
all eight secure coordinated tests. Phase 5 lint passed six self-tests, its
SDK Cargo test passed 310 tests (zero failed, eight ignored), and one full
check passed 174 unit and 1,031 integration tests with all timing gates met.
See [the split results](05-confirmed-rollouts-evidence.md#split-verification-2026-10-15)
and [repair evidence](09-coordinated-writes-evidence.md#invoice-liveness-and-void-counter-correction-2026-10-15).

The previous stop condition is resolved by these checks. Manual dialog,
mobile and real-network qualification limits above remain unchanged.
