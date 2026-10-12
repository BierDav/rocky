# 5. Status, Kotlin and lifecycle

Status: implemented; native targeted tests pass, Kotlin/app/device gates blocked by Gradle integrity.
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

## Implementation and evidence

The native slice adds link state to network peers and readiness, records open
and close reasons, measures ping round trips, and clears cancelled openings.
Readiness and sync status wake on transitions; neither token changes on pings.
On 2026-10-12:

- `cargo test -p rocky-sdk --lib three_missing_pongs_close_and_reconnect_with_backoff --locked`: one passed, 0.18 s in the final rerun; open status after reconciliation, ping-token stability and a readiness wait waking with closed status at 60 s.
- `cargo test -p rocky-sdk --lib link_status_reports_opening --locked`: one passed, 0.94 s; cancelling an in-flight dial clears opening status and changes readiness.
- `cargo test -p rocky-sdk --lib node::sync_tests::change_driven::live_links --locked`: 30 passed, 4.46 s. Existing authority, simultaneous dial, transport, push and retry regressions remain green.
- `cargo test -p rocky-sdk --lib background_refuses_new_links_until_foreground --locked`: one passed, 0.21 s; pause closes links under one second, rejects new inbound links, and foreground reopens.
- `./scripts/check --unit`: 149 passed; unit wall 78.3 ms, budget 10 s.
- `./scripts/lint`: exit 0. Full `./scripts/check` is still running.

The Kotlin/UI slice exposes `Peer.link`, readiness peers and typed link activity.
Sync and Devices show Live per peer; the top badge uses readiness to show Live
while any link is open. JVM and Android lifecycle probes use readiness rather
than sleeps and assert background closure and foreground reopening. Native
background admission also rejects later links, beyond draining existing ones.

Gradle 9.8.1 fails distribution integrity
verification before compilation: the wrapper pins
`bafd5ce9cfaea0fbccfdc8439a1ac42fbd4cd9c89dc9a988228d8a2639a58e6c`,
but the download hashes to
`dce76f55f8e251a3a1f130eb120f30b3d271de2b76c9b0729d316b5a1b6dc01f`.
No integrity check was bypassed. The shopping check sees the old published SDK
and fails with unresolved `LinkState`, `Peer.link` and `Readiness.peers`.
Attempted commands:

- `sdks/kotlin/gradlew -p sdks/kotlin :rocky-core:jvmTest`: exit 1 at distribution verification; no Kotlin compilation or tests.
- `sdks/kotlin/gradlew -p sdks/kotlin :qualification:android-discovery:qualify -Procky.android.avd=Pixel_9_API_37.2`: exit 1 at the same verification; no APK build or emulator run.
- `./examples/shoppinglist-app/check`: exit 1 in `requiredSchema` compilation against the old Maven-local SDK; JVM/Android shared tests and desktop build did not run.

New Kotlin artifacts, both app targets and Android lifecycle execution have
not been verified. Restore a distribution matching the trusted wrapper pin (or
review and correct the pin against the official distribution), publish the SDK
with `publishToMavenLocal`, then rerun the SDK, shopping and Android commands.
Phase 6 has not started.
