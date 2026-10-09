# 2. Change-driven sync

Status: not started.
Owner: Kotlin `rocky-core` automatic sync and lifecycle (`RockyDatabase.kt`,
`RockySession.kt`); the `rocky-sdk` sync worker in `node.rs` and
`scheduling.rs`; the always-on peer, which runs the same worker. No
dependency. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the entry on Kotlin automatic sync in
[fix later](../fix_later/README.md),
[change-driven sync](../runtime/always-on-peer.md#change-driven-sync),
[sync](../runtime/sdk-enrollment.md#sync), the
[transport bounds](../runtime/sdk-transports.md#bounds),
[sync wakeups](../runtime/sdk-transports.md#sync-wakeups) and
[Android lifecycle](../runtime/android.md#lifecycle).

Kotlin's default `automaticSync` calls `syncOnce` every 500 ms, and each pass
dials every peer in turn. `enterBackground` waits for the running pass, which
Kotlin runs as non-cancellable, so an unreachable peer delays the pause by up
to 15 s per route; with a stopped peer one desktop sync took about 75 s. The
native worker behind `syncContinuously()` has no timer while idle, but it
keeps one retry schedule for the whole pass, every local write resets it, and
disabling it does not interrupt a pass. A mesh multiplies the peers, and iOS
suspends a process that does not pause promptly.

## Settled

- A pass reconciles peers concurrently, at most four at a time. A sequential
  pass cannot finish while one dial is stalled.
- Back-off is per peer, not per route and not per pass: 1, 2, 4, 8, 16, 32
  and then 60 seconds, as the worker's schedule is today.
- A local write does not reset a peer's back-off. A connectivity signal,
  `setOnline(true)`, a change of that peer's configuration or an inbound
  session from that peer does.
- `syncOnce` and the native `sync` request ignore back-off, dial every peer
  and keep their response shape.
- The pause is `sync_start` with `enabled: false`. It cancels the running
  pass and returns within 1 s. No UniFFI export is added.
- The always-on peer gets the same behavior; its guide is updated with it.

## Ordered slices

1. Worker: per-peer back-off state replaces the single retry of the pass.
   The worker skips a peer whose back-off has not elapsed and sleeps until
   the earliest deadline; a pass with nothing to send and every peer backed
   off makes no dial.
2. Worker: peers reconcile concurrently within the bound. The pass reports
   its status once every dialed peer has answered or failed.
3. Cancellation: disabling the worker drops the running pass at its next
   await. The replica keeps what was committed; a frame is stored whole or
   not at all, and the next pass repeats the rest.
4. Kotlin: `automaticSync` enables the native worker and observes its status
   as `syncContinuously()` does; the polling loop is removed.
   `enterBackground` disables the worker and returns within the bound;
   `enterForeground` enables it, which starts a pass at once. `syncOnce`
   stays. These edits land before
   [phase 17](17-kotlin-native-binding.md) moves the native sources.
5. Remove the automatic-sync entry from fix later. Update the four guides
   named above: the 500 ms sentence, the retry schedule, the pause and the
   statement that a sync makes one attempt per route.

## Completion gate

All Rust items run on `rocky_sdk::transport::memory::Hub` under Tokio's
paused clock.

- With one of three peers `stalled`, the first pass reaches the other two and
  reports them before the stalled dial times out. Later passes do not dial
  the stalled peer until its back-off elapses, counted by the hub's dials.
- A local write during the back-off dials the two reachable peers only.
  `network_changed()` and an inbound session from the stalled peer each
  cause one dial of it at once.
- `sync` dials all three peers whatever their back-off.
- With a dial stalled, `sync_start` with `enabled: false` returns within 1 s
  and the hub counts no later dial. In Kotlin `enterBackground` returns
  within 1 s and `enterForeground` produces a pass without waiting for a
  timer.
- A pass cancelled at every `cut_after` byte offset of a session that pushes
  and pulls loses no local row and stores no partial frame; the following
  pass converges.
- An idle database whose peers are all unreachable makes no dial for one
  simulated hour without a signal.
- The always-on peer's configuration test and `./kotlin check` pass.

## Not in this phase

- Ranking peers, a cap on peers per pass or learned contacts
  ([phase 14](14-contacts-and-partner-choice.md)).
- Releasing the replica between apply transactions and the Room write
  timeout ([phase 3](03-catch-up-cost.md)).
- Background sync, push wakeups or socket rebinding on iOS
  ([phase 18](18-ios-packaging-keychain-lifecycle.md)).
- A coordinator watch protocol, a new UniFFI export or a changed `sync`
  response.
- Changes to `RockyLifecycle` on Android beyond what the pause requires.
- Reads on the writer thread
  ([known costs](../runtime/scaling-limits.md#known-costs)).
