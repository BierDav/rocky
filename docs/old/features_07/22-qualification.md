# 22. Qualification

Status: not started.
Owner: both examples, their harnesses and device procedures, the audits.
Depends on every earlier phase. Wave 6.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the [release gate](README.md#release-gate), the acceptance lists of
[the mesh scope](scope-mesh.md#acceptance) and
[the iOS scope](scope-ios.md#acceptance),
[the shopping audit](../runtime/shopping-qualification.md) with its
[outstanding](../runtime/shopping-qualification.md#outstanding) entries and
[shopping on devices](../runtime/shopping-devices.md).

Earlier phases qualified their own slices on the hub, loopback, emulators
and the simulator, and each left its acceptance run in CI. This phase runs
the whole on real devices and real networks and records evidence.

## Settled

- This phase implements nothing and changes no format. A failing scenario is
  reported to the owning phase with its blocking contract; a repair lands
  there.
- Each mesh acceptance item is a CI run owned by the phase that gates on it:
  1 by phase 6, 2 by 7, 3 by 8, 4 by 5, 5 by 9, 6 by 15, 7 by 10, 8 by 12,
  9 by 13 and 10 by 16.
- Each revoked-device refusal is owned by the phase of its entry point: the
  session by 9, the mailbox by 12, the contact request by 14, the discovery
  topic by 15 and command delivery by 16.
- The iPhone has no local discovery in this roadmap. It joins by contacts,
  by the topic and through the relay.

## Devices and networks

- One iPhone and one Android phone, both physical and in personal use,
  driven with `agent-device`; the iOS simulator and Android emulators for
  every further device.
- Two trusted nodes and a packaged custody peer as processes on the
  qualification host, and three voters for the market.
- Two networks: the host's LAN and a second one, such as a phone's mobile
  data, joined only by an iroh relay.

## Ordered slices

1. Collect: a table in the audit naming, for mesh acceptance 1 to 10 and for
   each revoked-device refusal, the owning phase's command and its last CI
   run. An item without a command is reported, not written here.
2. The release gate on the two phones, item by item, each phone enrolled by
   a different trusted node.
3. Networks. The Android phone and a desktop process on one LAN with
   discovery alone, no contact exchanged. The iPhone on the same LAN joining
   by contact and by the topic. The pair on two networks through a relay.
   One phone moving between the networks mid-sync. Record what the relay and
   a LAN observer see.
4. The outstanding entries of the audit: the two-phone runs of the gate,
   moves and revocation, now as the cross-platform pair; the horizon through
   custody alone; scenario 6 on devices as the last-item race.
5. Lifecycle on both phones: background during a sync, a kill during a
   write, a resume after 10 minutes in the background.
6. Refusals on devices: a device revoked while offline is refused at the
   session, the mailbox, the contact request, the topic and command delivery
   when it returns, each by the owning phase's check run against the phone.
7. Update both audits, the device guide, the starter guide and the root
   README's status; switch every "not qualified" statement the evidence now
   covers and keep the rest.

## Completion gate

- Every scenario has a reproducible command or device procedure and
  recorded evidence with host, devices, OS versions and builds.
- Each item of the [release gate](README.md#release-gate) is shown, with no
  Rust source in either application.
- The collection table has a command and a passing CI run for all ten
  acceptance items and all five refusals, or names the phase that owes it.
- The audit lists no outstanding scenario, or names each remaining one with
  its blocking contract.
- `./scripts/check`, the Kotlin, native, packaged and example checks and the
  iOS simulator checks pass within the budgets of
  [phase 1](01-test-speed.md).
- The jj change of this phase touches guides, evidence and device
  procedures only.

## Not in this phase

- New tests for an acceptance item or refusal an earlier phase owes.
- Any format, session or exported-surface change, or a defect repair beyond
  reporting it.
- The 4,096-command soak on devices and a live public coordination service.

## Not claimed

Two physical phones of the same platform, more than two physical devices,
cellular-only operation without a relay, physical power loss, store builds
and local discovery on a physical iOS device. State each in the evidence.
