# Phase 3 catch-up evidence

Measured on macOS arm64 in the `p03-catch-up-cost` jj workspace, based on
`main` 140bc94a, with the pinned Rust 1.99.0 debug test profile. Other builds
shared the host. These are local qualification results, not WAN or phone
latency claims. The [partition guide](../../../../docs/runtime/partitioned-scopes.md#four-table-profile)
contains the budget and chain-byte handoff to Phase 11.

## Four-table fixture

`before-unsampled.log` and `after-unsampled.log` record the stage times:
45.558 → 18.388 s for registration and authoring, 177.858 → 49.690 s for
catch-up, and 49.881 → 8.577 s for checkpoints. The budgets are 60, 100 and
48 s. The earlier roadmap's 86/339/121 s run used a different host load;
its durations are not the denominator of this profile.

`after-contended.log` preserves the first continuation-thread rerun, with
workspace tests, the 10,000-operation soak and native Android/JVM rebuilds
overlapping it. It passed the fixture's correctness assertions but took
131.872/146.991/34.576 s: registration/authoring and catch-up missed their
review budgets. `after-qualification.log` reruns after those builds finish: 32.910/84.143/
10.105 s, meeting all three budgets. The slower result is retained rather
than discarded. `after-final.log` measures the final build with receipt-retry
recovery: 28.507/68.200/8.911 s, again within every budget.

`before-sampled.log` and `after-sampled.log` are separate, sampled runs.
Sampling adds overhead: their stage times differ from the unsampled ones.
The six `*.txt.gz` files retain the measured test thread's complete call
tree, with process addresses and other threads omitted. Counts and symbol
names are unchanged. `attribution.json` preserves category counts and entry
points by caller. Each sample belongs to the outermost named category on
its stack, so chain verification includes its nested signature and CBOR
work. The broad credential category also includes chain signing-byte
construction; the guide separates that from authority checks.

Named costs account for 91.3%, 98.8% and 98.0% of the before stages, and
84.8%, 92.5% and 90.0% after. Estimated cost seconds multiply the sample
share by the corresponding unsampled wall time; these are estimates, not
independent subphase timers. Round-trip waiting is sampled separately; this
fixture uses loopback TCP. The guide's chain-byte seconds further apportion
encoding/signing samples by the measured byte share, rather than claiming
that every byte has identical CPU cost.

Reproduce the fixture and collect a new profile:

```sh
cargo test -p rocky-secure --lib measure_200_partition_group_sync --locked -- --ignored --nocapture
mkdir -p local/p03-profile
ROCKY_PROFILE_DIR="$PWD/local/p03-profile" cargo test -p rocky-secure --lib measure_200_partition_group_sync --locked -- --ignored --nocapture
```

Recompute an attribution, including chain callers:

```sh
python3 benchmarks/runtime-scaling/evidence/catch-up-2026-10-10/analyze.py \
  benchmarks/runtime-scaling/evidence/catch-up-2026-10-10/after-catch_up.txt.gz \
  tests::group_sync 49.690 chain 2 30
```

Use `encoding` instead of `chain` to inspect encoded types. Sum every entry
for a type, including `Header<Extensions>` hashing, signing, verification
and `to_bytes`, rather than using only its largest caller. The unsampled
after log records exact frame/header/chain/payload and checkpoint bytes;
random credentials and sealing make their byte counts vary slightly between
runs. The fixture remains ignored and its row/reference/reopen checks are
unchanged.

## Writer and interruption regressions

There are two distinct costs. The SDK regression
`a_room_write_commits_while_a_renewal_waits_for_the_replica` polls renewal
until it holds settings and waits for the transaction's replica lock. It
then inserts and commits using the Room profile. The original per-statement
settings read forms a lock cycle that ends only when the SQL lease expires;
the fixed profile reads the schema version between transactions.
`room-unfixed.log` runs this regression with the original connection source
from the parent change and fails with code 4 (`Timeout`); `room-fixed.log`
passes after restoring the fixed source. No delay
is used to arrange this interleaving, and the lease limit is not increased.

Separately, `unfixed-growth.txt` records a 1,280-operation catch-up with a
5.440 s longest BEGIN-to-COMMIT and a 5.346 s lock-probe wait. The 2,560-case
in `unfixed-2560.txt` reached the native read timeout. The old pending pass
reexamined all waiting frames and kept the replica across its bounded
transactions. The new pass examines log heads and releases the replica
between committed transactions; Tokio's FIFO mutex admits an already
waiting writer first.

`an_interrupted_catch_up_resumes_and_applies_every_operation_once` uses an
apply-transaction budget, not sleeps. It interrupts after the first commit,
then after each successive apply transaction on reopen, until all 320
operations converge. A second regression retries the same receipt without
reopening: duplicate and covered receipts resume pending passes before their
replies leave. `retry-unfixed.log` records the missing resumption (62 of 160
rows materialized); `catch-up-final.log` records both recovery paths passing. It checks row values, no remaining pending operation,
and exactly 320 applications in scope state. The short catch-up test runs
640 operations beside a writer. The ignored soak runs 10,000 by default:

```sh
cargo test -p rocky-secure --locked --lib catch_up -- --nocapture
cargo test -p rocky-secure --locked --lib soak_a_writer_commits_within_the_write_limit_during_a_10000_operation_catch_up -- --ignored --nocapture
cargo test -p rocky-sdk --locked a_room_write_commits_while_a_renewal_waits_for_the_replica -- --nocapture
```

The soak's authoring futures are polled into the group queue in birth order
while the replica is locked, so scheduling cannot reorder row identities.
Writers queue BEGIN directly after every second incoming receipt; a watch
event arranges this interleaving without a sleep or a pre-BEGIN replica
query. The earlier tight, unbounded writer loop exhausted the retained-byte
limit (`soak-unbounded-writer.log`). It is retained as a failed fixture run;
the final stimulus bounds writer traffic by incoming receipt events.
The frame-count and five-second assertions remain, and the soak also checks
zero lag and exactly one scope-state application per incoming or authored
operation. Its continuously queued mutex probe reports the longest acquisition wait,
a conservative measure including scheduling and the writer's own hold,
rather than an instrumented CPU timer inside apply.

`soak-final.log` records all 10,000 operations converging, zero lag, exactly
once application and 239 SQL writes: catch-up 169.282 s, longest
BEGIN-to-COMMIT 0.205 s and longest observed replica acquisition wait 0.707 s.
The earlier, more contended `soak-first-pass.log` also passed, with 239 writes,
1.932 s longest write and 2.619 s longest replica wait. Both stay below the
5 s default write limit; neither run retries a writer transaction.

## Device and final checks

The [two-phone procedure](../../../../docs/runtime/shopping-devices.md#two-phone-procedure)
is run on the two Phase 3 Android 15 arm64 emulators. Each run checks both
devices' final list values and records every retried write. The native
runtime and APK are rebuilt from this workspace before qualification.
`devices-preliminary.json` and `devices-prior-gate.json` preserve the earlier
attempts, including two failed convergences with zero retried writes. The
second reports `sync Unavailable peer unreachable: Connection refused
(os error 111)` after the enrollment host closed. The final procedure removes
that host's contact, leaving the same custody relay and list/move assertions;
per-peer sync diagnostics identify remaining unreachable contacts. No sync
worker, retry count or lease bound changes. `devices-before-markers.json`
records another interrupted gate: two passes, then `adb logcat -c` failed
before sync. Unique per-run step markers now delimit the driver logs without
clearing logcat; the procedure keeps every convergence assertion. Raw phone
logs and state include share codes and secrets and stay outside version
control.

`android-artifacts.json` records the final APK/AAR hashes and byte-identical
embedded native libraries for both supported Android ABIs. The workspace
feature check rebuilt the custody executable without default features during
the series; both default/iroh and TCP-only builds used the final sources.
Both observed hashes are retained; per-run executable hashes were not captured.

`check-final.log` records workspace Clippy, 713 passing workspace tests,
documentation tests and 160 passing SDK tests without default features.
`secure-final.log` records 191 passing tests with 10 ignored measurements.
The final `./scripts/lint` and `scripts/commitlint --jj` also passed.
The final 10,000-operation soak passed. The final device series stopped at
attempt 18 after 17 passes: B could not reach custody, and its `adb reverse`
list was empty while A's mappings remained (`device-route-failure.json`). No
write timed out or was retried. `devices-final.json` records 20 full passes
in 21 attempts, with zero retried writes in every attempt and passing durations 28.427–89.354 s. Three
further complete procedures passed after the route failure. Every procedure
keeps its original list/move convergence checks; no write limit, driver retry
count or assertion is relaxed. The [device guide](../../../../docs/runtime/shopping-devices.md#emulator-pair-2026-10-10)
records the emulator topology. Physical phones, WAN behavior and hosted CI
were not verified in this workspace.
