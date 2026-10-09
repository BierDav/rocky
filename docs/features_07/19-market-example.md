# 19. Market example

Status: not started.
Owner: a new `examples/market-app` and its harness; the audit and evidence
guides. Depends on [phase 4](04-small-fixes.md). Wave 1.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [typed commands](../runtime/typed-commands.md), the market schema in
[getting started](../runtime/getting-started.md),
[the end-to-end evidence](../runtime/end-to-end-evidence.md), scenario 6 in
[the shopping audit](../runtime/shopping-qualification.md#acceptance-scenarios),
[coordinator operations](../runtime/coordinator-operations.md) and
`scripts/coordinator-local`.

The shopping example already runs strong commands: its approvals are ordered
by the public coordination service. It has no contention, no executor
takeover and no voter faults, and its harness runs without a coordinator. A
Kotlin-only market application once ran those scenarios against three
voters; it was removed, and that evidence cannot be reproduced from the
tree. Restore a small one that the mesh and iOS phases can run.

## Settled

- Build from what the tree holds: `Reserve` and `Restock` in
  `examples/shoppinglist-app/shared/test/Reservation.kt` with
  `ReservationCommandTest`, the DAOs of
  `sdks/kotlin/room-qualification/src/Market.kt`, and the process control of
  the shopping harness (`qualification/src/Device.kt`). Move the fixtures
  into the new example; keep one copy.
- The schema is the starter guide's: `listings`, `memos`, `stock`, `holds`;
  handler names `market.reserve.v1` and `market.restock.v1`.
- The Android application stays, since the
  [release gate](README.md#release-gate) races two phones.
- The 4,096-command run is a soak outside the default check, started by its
  own command.
- `examples/inventory-app` is a Rust crate on another contract and is not an
  input.
- Devices close their coordinator sessions with `command_service_close`
  from phase 4.

## Ordered slices

1. The application, with no Rust source, in the structure of the shopping
   example: a `shared` multiplatform module with the schema, Kotlin handlers,
   Room DAOs reading the command-controlled tables and a keyed handler read;
   a desktop application and an Android application. `shared` uses no
   `java.*` API and no JVM-only dependency, so that
   [phase 21](21-shopping-and-market-on-ios.md) adds the iOS target only.
2. One screen: listings, stock, a reserve action and the ticket's state,
   including pending and `outcome_unknown`.
3. A harness: each device a separate JVM, three `rocky-voter` processes on
   loopback started as `scripts/coordinator-local` does, a packaged custody
   peer. Scenarios, each selectable by name:
   - `last-item`: two callers reserve the last item; one is accepted, the
     other gets a typed rejection kept across kill and reopen;
   - `loss`: a lost reply, a killed caller and a killed executor each
     complete the original ticket with one handler call;
   - `takeover`: the owner fences the executor and another continues at the
     same head;
   - `refused-writes`: DAO and SQL writes to command-controlled rows fail;
   - `voters`: leader loss continues; quorum loss pauses strong work while
     mergeable writes sync, and the pending ticket applies once after
     restore.
4. The soak: 4,096 commands as JVM processes, with the record and byte
   budgets of [the evidence](../runtime/end-to-end-evidence.md#device-limits)
   observed.
5. A check script and a CI job for slice 3, added after
   [phase 1](01-test-speed.md) restructured the workflow. The audit's
   scenario 6 names both commands.
6. Guides: the audit no longer calls shopping mergeable-only; the evidence
   guide names the reproducing commands.

## Completion gate

- Each of the five scenarios passes by its name and writes evidence with
  host, build and voter count; the CI job runs all five.
- No file under `examples/market-app` is Rust source or a Cargo manifest;
  the applications build from the packaged artifacts.
- The soak completes 4,096 commands, or stops at a named budget with the
  number reached and the budget recorded.
- [The end-to-end evidence](../runtime/end-to-end-evidence.md) lists, per
  scenario, the command that reproduces it or that it is not reproducible.
- Scenario 6 leaves the audit's outstanding list for JVM processes; the
  device run stays outstanding for [phase 22](22-qualification.md).
- The Android application installs on an emulator and completes one accepted
  reservation against the loopback voters.

## Not in this phase

- Reaching an executor that is not a configured peer
  ([phase 16](16-strong-commands-in-the-mesh.md)).
- The iOS target ([phase 21](21-shopping-and-market-on-ios.md)).
- The public coordination service, or any change to the coordinator.
- Removing or reworking shopping's approvals.
- Optimistic updates and partitioned command tables.
- More than the one screen, or a second copy of the fixtures.
