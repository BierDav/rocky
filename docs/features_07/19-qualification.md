# 19. Qualification

Status: not started.
Owner: both examples, their harnesses and device procedures, the audits.
Depends on every earlier phase. Wave 5.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the [release gate](README.md#release-gate), the acceptance lists of
[the mesh scope](scope-mesh.md#acceptance) and
[the iOS scope](scope-ios.md#acceptance),
[the shopping audit](../runtime/shopping-qualification.md) with its
[outstanding](../runtime/shopping-qualification.md#outstanding) entries and
[shopping on devices](../runtime/shopping-devices.md).

Earlier phases qualified their own slices on the hub, loopback, emulators and
the simulator. This phase runs the whole on real devices and real networks
and records evidence.

## Devices and networks

- One iPhone and one Android phone, both physical and in personal use, driven
  with `agent-device`; the iOS simulator and Android emulators for every
  further device.
- Two trusted nodes and a packaged custody peer as processes on the
  qualification host, and three voters for the market.
- Two networks: the host's LAN and a second one, such as a phone's mobile
  data, joined only by an iroh relay.

## Ordered slices

1. Mesh acceptance 1 to 10 in CI as multi-process runs, each with a command.
2. The release gate on the two phones, item by item, each phone enrolled by a
   different trusted node.
3. Networks: the pair on one LAN with discovery alone; then on two networks
   through a relay; then one phone moving between them mid-sync. Record what
   the relay and a LAN observer see.
4. The outstanding entries of the audit: the two-phone runs of the gate,
   moves and revocation, now as the cross-platform pair; the horizon through
   custody alone; scenario 6 on devices as the last-item race.
5. Lifecycle on both phones: background during a sync, a kill during a
   write, a resume after a long suspension.
6. Refusal regressions: a revoked device at every entry point (session,
   renewal, discovery topic, mailbox, command delivery), and every refusal
   the scopes list under not requested.
7. Update both audits, the device guide, the starter guide and the root
   README's status; switch every "not qualified" statement the evidence now
   covers and keep the rest.

## Completion gate

- Every scenario has a reproducible command or device procedure and recorded
  evidence with host, devices, OS versions and builds.
- The [release gate](README.md#release-gate) is shown with no Rust source in
  either application.
- The audit lists no outstanding scenario, or names each remaining one with
  its blocking contract.
- `./scripts/check`, the Kotlin, native, packaged and example checks and the
  iOS simulator check pass, within the budgets of
  [phase 1](01-test-speed.md).

## Not claimed

Two physical phones of the same platform, more than two physical devices,
cellular-only operation without a relay, physical power loss and store
builds. State each in the evidence.
