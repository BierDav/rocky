# Fix later

Known defects that are not scheduled. Each entry states the scenario, the
site and a proposed fix. Scheduled work is in
[the roadmap](../features_07/README.md); local defects with an obvious fix are
in its [small fixes](../features_07/04-small-fixes.md). Remove an entry when it
is fixed or moved into a phase.

## Reads and status do whole-table work on the writer thread

`query()`, `observe()`, `strong_status` and `strong_results` scan a whole
projection or ledger on the writer thread. Listed under
[known costs](../runtime/scaling-limits.md#known-costs).

Fix: page them, or serve them from the read connection.

## Kotlin tests choose TCP ports that another process can take

`sdks/kotlin/room-qualification` and `examples/shoppinglist-app/shared` tests
pick a listen address with `ServerSocket(0)`, close it and pass the port to
`PeerTransport.Tcp`, which binds it later. On a host running other tests, a
process can take the port in between; `RoomGateTest` failed once on
2026-10-10 with `Address already in use (os error 48)` during `./kotlin check`.

Fix: let `PeerTransport.Tcp` listen on port 0 and advertise the bound port,
and use that in the tests.
