# 5. Status, Kotlin and lifecycle

Status: implemented and verified (2026-10-12); all phase 5 completion gates passed.
Owner: `rocky-sdk` readiness and network status; Kotlin `rocky-core`
(`RuntimePort.kt`, `RockyDatabase.kt`, `RockyLifecycle.kt`); the shopping
example's Sync and Devices tabs. Depends on [phase 2](02-link-session-and-signals.md);
lands after [phase 4](04-remove-per-pass-sessions.md) if both touch the
status shape. Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [Android lifecycle](../runtime/android.md), [SDK activity](../runtime/activity.md)
and [queries and observation](../runtime/queries-and-observation.md).

Applications need to show whether a peer is live, and a phone must not hold
links while it is in the background.

## Settled

- Each peer in the network status gains `link`: `open`, `opening` or
  `closed`, with the last ping round trip and when the link opened. The
  status token changes when a link opens or closes. It does not change on
  each ping.
- The activity feed records `link_opened` and `link_closed` with the reason.
- Kotlin exposes the same fields on its peer status type. No new observer:
  the existing status and readiness flows carry them.
- **Android:** `enterBackground` disables the worker, which closes every
  link within the pause bound of [change-driven sync](../runtime/always-on-peer.md#change-driven-sync).
  `enterForeground` enables it and reopens links. There are no background links.
- **Desktop:** links stay open while the application runs and is online.
- The shopping app shows a live badge per peer on the Sync and Devices tabs
  and shows "live" in the top badge while any link is open.

## Completion gate

- The status reports `open` after the first pass and `closed` within the
  liveness bound after the peer stops.
- An Android instrumented test, or the device harness, shows no open link
  after `enterBackground` and an open link after `enterForeground`.
- The shopping app check compiles both targets with the new fields.

## Implementation and evidence (2026-10-12)

Status, Kotlin and Android lifecycle gates are verified. Per-peer `link` state,
Unix opening time and last ping round trip reach network status and readiness.
Opening/closing advance tokens and wake existing flows; pings do neither.
Activity carries typed open/close reasons. The Sync and Devices tabs show Live
per peer, and any open link makes the top badge Live. Backgrounding drains
links and refuses later admission until foregrounding; borrowed transports
remain open.

| Slice / check | Result | Limits |
| --- | --- | --- |
| Native status/liveness | Paused-clock memory-hub test reports open after reconciliation, unchanged tokens on ping, and a waiting readiness observer receives closed at 60 s | No physical-peer liveness measurement |
| Opening cancellation / pause | Cancelling a dial clears opening; pause acknowledges only after links close, under 1 s; background rejects incoming links and foreground reopens | Native memory-hub coverage |
| `cargo test -p rocky-sdk --lib node::sync_tests::change_driven::live_links --locked` | 30 passed, 4.46 s | Targeted development run; later full checks cover final code |
| `cargo test -p rocky-sdk --lib node::sync_tests::mesh --locked` | 7 passed, 1.32 s, including retained handover hints | In-process transport |
| `./scripts/check --unit` | 149 passed, 90.7 ms | Development unit tier |
| Final `./scripts/check` | Exit 0; strict Clippy/formatting, 165 unit tests (105.5 ms), 923 integration tests (699 default + 224 no-iroh, 153.35 s within 300 s) | No e2e or doc tier |
| `./scripts/lint` | Exit 0 | Static checks |
| `sdks/kotlin/gradlew -p sdks/kotlin publishToMavenLocal` | Exit 0; JVM, Android and iOS library publications; final rebuild 4 min 32 s | iOS library build, no iOS runtime qualification |
| `sdks/kotlin/gradlew -p sdks/kotlin :rocky-core:jvmTest :qualification:android-discovery:qualify -Procky.android.avd=Pixel_9_API_37.2` | Exit 0, 1 min 6 s; all 13 JVM tests passed; emulator logs background closed / foreground open and Activity discovery-lock transitions | API 37.2 arm64 emulator, local TCP; no physical phone or cellular |
| `./examples/shoppinglist-app/check` | Exit 0; required hand/Room schema equivalence and native install, 37 JVM + 6 Android tests, desktop and Android app builds | Loopback consumer tests; no real-network application run |
| Final badge compilation | `kotlin build --project-dir examples/shoppinglist-app -m desktopApp -m androidApp`: exit 0 | Compilation only |

The JVM lifecycle tests now explicitly return `Unit`; previously their inferred
non-void return types meant they were compiled but not discovered. Both probes
ran: live background/foreground (0.797 s) and stalled-dial cancellation
(0.399 s) in the first explicit run. The final full JVM run includes both.

## Prerequisite and tooling repairs

Full and partial devices have identical SDK grants. Inferring full before the
invitation's application description was recorded could share an unshared
list with a partial device. Undescribed members now default to partial; only
an explicit full description enables every-list delivery. The new deterministic
consumer regression checks the fallback and the explicit full transition.

Contact handover previously consumed hints after an earlier successful pass,
leaving a later device with only its enrollment upstream. It now retains at
most 16 recently successful peers. A paused-clock regression exercises two
earlier passes before a new device obtains the partner route.

Existing native tests now wait for a fresh completed sync state rather than
treating the next link-status notification as a completed pass. State, retry,
dial-count, permission, row and timeout assertions remain unchanged.

The wrapper URL named Gradle 9.8.1, but pinned the checksum for 9.8.0. The pin
was corrected using the [official checksum reference](https://gradle.org/release-checksums/),
with integrity verification retained. After publication, Kotlin CLI 0.13's
stale extracted Maven-local AARs also required regeneration; the
[consumer guide](../../examples/shoppinglist-app/README.md#run-and-pair) records
that workaround. Published archives were preserved.

Earlier parallel consumer runs also intermittently timed out observing a list
key/items and logged Room authority-refresh errors. Failure diagnostics retain
network, holding, raw-row and activity evidence without relaxing the 60 s data
assertion. The final diagnostic run passed all 37 + 6 tests; these checks do not
prove all intermittent Room refresh behavior has been eliminated.

Phase 6 has not started. No cellular latency, idle-cost, Internet traversal or
physical-phone qualification is claimed.
