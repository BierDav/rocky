# Phase 21: earlier investigation and runs

Historical evidence retained from the initial implementation and takeover.
[Current evidence](21-ios-applications-evidence.md) records the resumed run
and remaining completion gates.

## Paste investigation

`local/phase21-paste-crash.log` contains only “Error was captured in composition.”
and the shopping process ID, without an exception or stack. The original
throwing line cannot be established from that artifact. On the simulator the
previous custom paste receiver also failed to deliver text. Native target,
action and accessibility inspection is retained in the takeover paste logs.

The control now uses UIKit's `UITextView` receiver and the configuration
initializer of `UIPasteControl`, with a main-queue text-change observer removed
on composition disposal. Each paste clears the receiver before forwarding text;
its hidden input does not compete with the accessible Paste button. The complete
simulator procedure verifies shopping text arrival and two market bundle pastes
before creating the market and making a reservation. Neither path crashes in
that run. No global Keychain reset was used.

## Runs

Local runs use Xcode 27.0, agent-device 0.21.23, an arm64 iOS 27.0 simulator
(`rocky-p21`) and the `rocky-p19` Android emulator. Command logs are under
`local/phase21-*.log`; named device expectations are under
`local/phase21-evidence`. Build time is excluded from the device suite budget.

- `./scripts/check` exited 0 with four nextest workers: 146 unit tests and
  839 integration tests across the supported workspace/default-feature matrix.
- Both example JVM/Android checks and all four shopping harness scenarios
  passed. All six market harness scenarios passed, including takeover,
  refused writes and lost/restored service quorum.
- Both arm64 library targets and both simulator applications build. Native
  integration runs passed below 2 s, using real Room and Keychain for shopping.
- The complete single-simulator procedure passed shopping setup, Paste, offline
  Milk, background/resume and process restart, then market bundle Paste (twice),
  listing, confirmed stock/reservation and restart with its confirmed hold
  (`local/phase21-takeover-simulator-final.log`, `simulator.json`).
- The shopping custody procedure passed in 30.10 s
  (`local/phase21-takeover-shopping-pair9.log`,
  `pair/expectations-shopping.json`). Both devices display **Offline groceries**
  before disconnecting. **Milk** is absent on Android while the simulator is
  backgrounded, then arrives after foregrounding, without manual sync and with
  the enrollment host stopped. Host-to-emulator TCP forwarding is required
  alongside emulator-to-host reverse forwarding for custody to push updates.

The first workspace check, during heavy host load, timed out seven tests and
reported three integration budget misses: `strong_cut_tests::an_unknown_or_pruned_base_is_refused_before_any_operation_is_loaded`
(20.38 s), `change_driven::representative_session_cancellations_are_atomic_and_repeatable`
(22.02 s), and `formats::unsupported_replica_and_checkpoint_formats_refuse_before_writes`
(20.55 s). The four-worker rerun passed. Budgets and exceptions were unchanged.
Disk exhaustion later required deleting this workspace's regenerable Rust
incremental cache; no source or device state was removed for that cleanup.

The final normal check also passed, with 138.88 s of integration suite time.
Its concurrent run had 56 raw timings above their configured per-test limits;
`local/phase21-takeover-budget-overshoots.tsv` records all of them. The existing
local checker used prior isolated confirmations. A fresh
`CI=1 NEXTEST_TEST_THREADS=2 ./scripts/check` exited 0 with 146 unit and
839 integration tests (165.72 s integration suite), without local confirmations.
Its three raw overshoots passed fresh isolated checks: owner closure
2.40 → 1.47 s (2 s limit), tenant isolation 5.88 → 4.64 s (existing 5 s limit),
and range reconciliation 2.14 → 1.28 s (2 s limit). No budget or exception was
changed. Final lint and commitlint also exited 0.

