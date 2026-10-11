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

`sdks/kotlin/qualification/room` and `examples/shoppinglist-app/shared` tests
pick a listen address with `ServerSocket(0)`, close it and pass the port to
`PeerTransport.Tcp`, which binds it later. On a host running other tests, a
process can take the port in between; `RoomGateTest` failed once on
2026-10-10 with `Address already in use (os error 48)` during `sdks/kotlin/gradlew -p sdks/kotlin check`.

Fix: let `PeerTransport.Tcp` listen on port 0 and advertise the bound port,
and use that in the tests.

## Six e2e tests fail on main since explicit passes respect back-off

On main `a7d24a97`, before phase 15, `./scripts/check --e2e` fails in
`rocky-sdk`: `faults` (`a_cut_stream_loses_nothing_and_applies_nothing_in_part`,
`inbound_streams_over_budget_are_dropped_and_served_again_later`),
`revocations` (both `partitioned_issuers_revocations_merge_when_…`),
`shopping_release`
(`two_phones_never_online_together_pass_the_release_gate_through_custody`) and,
run alone, `service`
(`devices_run_typed_commands_through_a_registered_service_domain`). Three
report `peer unreachable: all selected contacts are in backoff`; the others
miss rows or a peer that a sync should have brought. The cause was not
traced. A likely one: since
[phase 14](../features_07/14-contacts-and-partner-choice-evidence.md) an
explicit `sync` no longer dials a backed-off contact, and these fixtures still
expect it to. The tier is not part of `./scripts/check`, so later phases did
not see it.

Fix: trace one case; if that is the cause, have each fixture signal
connectivity (`connection` with `online`) before the sync it expects to dial,
as the bootstrap and borrowed-endpoint fixtures already do. The Kotlin twin
of the borrowed-endpoint fixture failed for that reason and was repaired that
way in phase 15.
