# 5. Status, Kotlin and lifecycle

Status: not started.
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
