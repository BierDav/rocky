# Buffered default evidence

Phase-start: `fc2be3ad` (`koqrvqzn`), after the requested rebase.
Implementation change: `ypuvvumu`.
Host: Apple M5 Pro, arm64 macOS, desktop. No physical device qualification.

## Slice 1

Verified the existing `settle_commits` background task flushes committed
revisions, and connection durability maps directly to the secure commit class.
The Rust convenience transaction uses Buffered; explicit connections retain
Flushed. No storage or publication semantics change.

## Slice 2

Release L0 probe, both modes, 50 warm-up writes and 600 measured writes per
mode at 100 ms intervals. Command (baseline and implementation):

```sh
cargo test -p rocky-sdk --features qualification --test migrations --release l0_idle -- --ignored --nocapture
```

The initial command omitted `--features qualification` and selected zero tests;
it is excluded. The corrected baseline passed (one ignored test selected).
The SDK default was restored only after its baseline library had compiled.
Baseline code is the phase-start implementation (plus the new doc comment).

Both runs share this host with Rust/Kotlin compilation and another active agent;
this is L0 database load on a loaded host, not idle-host qualification.

Baseline, microseconds, 600 samples per row:

| Mode / metric | p50 | p95 | p99 | max |
| --- | ---: | ---: | ---: | ---: |
| Flushed write | 6937 | 30313 | 52641 | 77248 |
| Flushed observer | 5 | 6 | 17 | 3310 |
| Buffered write | 16419 | 79309 | 222296 | 960202 |
| Buffered observer | 5 | 12 | 75 | 1187 |

Implementation, microseconds, 600 samples per row (probe passed):

| Mode / metric | p50 | p95 | p99 | max |
| --- | ---: | ---: | ---: | ---: |
| Flushed write | 38544 | 130256 | 609929 | 947055 |
| Flushed observer | 5 | 39 | 208 | 2875 |
| Buffered write | 18704 | 140510 | 209741 | 280304 |
| Buffered observer | 5 | 35 | 117 | 450 |

Observer p95 meets 5 ms. Write p95 does not meet 3 ms; idle-host
qualification remains required. Host load differs across sampling windows,
so these runs establish no performance improvement claim.

## Validation

- Release qualified L0 probe: passed, both modes.
- `./scripts/lint`: fails because `ktlint` is unavailable; Rust formatting passes.
- `./scripts/check`: formatting and clippy passed; unit tier passed (212 tests);
  integration: 1464 passed (1141 workspace, 323 without default features),
  with one state-advice timeout in each configuration. Failing test:
  `node::sync_tests::state_advice::three_failed_advised_imports_fall_back_only_for_that_scope_and_checkpoint`.
  The runner reported load 385.4 on 18 cores for integration. Budget reruns
  passed; suite time was 79.07 s load-adjusted against a 300 s budget.
  Overall check failed because of the two timeouts.
- Kotlin check: failed, 1 of 31 core JVM tests (`ServiceOwnerTest` tenant quota
  case: `service command timed out`). Schema qualification passed.
- Shopping check: reported a timeout in `SealsTest` waiting for a new list to
  bootstrap after returning to main. The process then stopped producing output
  and was terminated; Android and desktop builds were not reached.
- SDK application tests: all 6 passed (compiled test binary, 0.85 s).
- `./scripts/commitlint --jj`: passed.
- Physical-device qualification: not verified.

