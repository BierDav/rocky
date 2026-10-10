# Fix later

Known defects that are not scheduled. Each entry states the scenario, the
site and a proposed fix. Scheduled work is in
[the roadmap](../features_07/README.md); local defects with an obvious fix are
in its [small fixes](../features_07/04-small-fixes.md). Remove an entry when it
is fixed or moved into a phase.

## Kotlin automatic sync polls every peer

`sdks/kotlin/rocky-core/src/native/RockyDatabase.kt`: the default
`automaticSync` calls `syncOnce` every 500 ms while foregrounded, which dials
every peer. `enterBackground` waits for the running pass under the lifecycle
mutex, so an unreachable peer delays the pause by up to 15 s per route.

Fix: drive it from the native change-driven worker that `syncContinuously()`
already uses, and make the pause cancel the running pass.

## A Room write times out behind background sync

Seen twice in nine runs of the [emulator pair](../runtime/shopping-devices.md#emulator-pair-2026-10-09):
right after a device came online, a Room write transaction failed with code 4
(`transaction was rolled back: Timeout`), most likely because background sync
held the replica past the 5 s write limit while applying incoming frames, as
the polling above repeats passes. Not yet measured. Nothing was committed, so the driver retries.

Fix: apply received pages in bounded transactions that yield to a waiting
writer, and measure the longest replica hold during catch-up on a device.

## Reads and status do whole-table work on the writer thread

`query()`, `observe()`, `strong_status` and `strong_results` scan a whole
projection or ledger on the writer thread. Listed under
[known costs](../runtime/scaling-limits.md#known-costs).

Fix: page them, or serve them from the read connection.

## A command table's checkpoint page carries whole credential chains

`rocky-secure/src/replica/checkpoint/transfer.rs`, `checkpoint_page`: the first
page holds the scope's `StrongState` whole, and its `latest` confirmed command
embeds the caller's and the executor's credential chains. In the shopping
example one command already makes that 49 KiB of the page's 63 KiB budget
(its sealed plaintext must fit one 64 KiB package). Larger grant sets or
receipts would exceed it again, and the scope could not bootstrap.

Fix: send the latest command beside the page, or reference it by hash and let
the peer fetch it with its result, and encode byte strings as CBOR byte strings
so the budget no longer reserves two bytes per stored byte.
