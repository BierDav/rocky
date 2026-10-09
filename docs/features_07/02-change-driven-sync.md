# 2. Change-driven sync and bounded apply

Status: not started.
Owner: Kotlin `rocky-core` automatic sync and lifecycle; `rocky-sdk` sync
worker; `rocky-secure` apply. No dependency. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the two entries on Kotlin automatic sync and on Room writes in
[fix later](../fix_later/README.md),
[change-driven sync](../runtime/always-on-peer.md#change-driven-sync),
[sync](../runtime/sdk-enrollment.md#sync), the
[transport bounds](../runtime/sdk-transports.md#bounds) and
[Android lifecycle](../runtime/android.md#lifecycle).

Kotlin's default `automaticSync` calls `syncOnce` every 500 ms and dials every
peer. `enterBackground` waits for the running pass, so an unreachable peer
delays the pause by up to 15 s per route; a stopped peer made one sync take
about 75 s. A Room write timed out behind background sync in two of nine
emulator runs. A mesh multiplies the peers, and iOS suspends a process that
does not pause promptly.

## Ordered slices

1. Automatic sync runs on the native change-driven worker that
   `syncContinuously()` uses; the polling loop is removed. `syncOnce` stays.
2. A pause cancels the running pass and returns within a stated bound,
   whatever the network does. A cancelled pass leaves no partial row and is
   repeated after resume.
3. A peer that failed to answer is not dialed again by every pass. The worker
   backs off per route and retries on a connectivity signal
   ([sync wakeups](../runtime/sdk-transports.md#sync-wakeups)).
4. Measure the longest replica hold while applying incoming pages during
   catch-up, on an emulator. Apply received pages in bounded transactions that
   yield to a waiting writer.
5. Remove both entries from fix later and update the guides.

## Completion gate

- With one of three peers stalled on the hub, a pass that reaches the other
  two completes without waiting for the stalled dial, and later passes do not
  dial it until the back-off or a connectivity signal allows.
- `enterBackground` returns within its bound while a dial is stalled; no pass
  runs afterwards; `enterForeground` syncs at once.
- A cut at every point of a cancelled pass loses no local row and leaves no
  partial one.
- A writer waiting behind a 10,000-operation catch-up commits within the write
  limit; the longest hold is recorded.
- An idle database with unreachable peers makes no dial between signals.
- Twenty emulator-pair runs of the device procedure show no write timeout.

## Not in this phase

Choosing which peers to sync with ([phase 9](09-discovery-and-partner-choice.md))
and reads on the writer thread
([known costs](../runtime/scaling-limits.md#known-costs)).
