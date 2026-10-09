# 17. Shopping without a roster

Status: not started.
Owner: `examples/shoppinglist-app` and its harness; the starter guide.
Depends on [phase 4](04-issuer-registry.md),
[phase 9](09-discovery-and-partner-choice.md) and
[phase 11](11-direct-key-delivery.md). Wave 4.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the applications](scope-mesh.md#7-the-applications), the shopping
example's README, `ShoppingSchema.kt`, the Devices and Sync screens and
[the audit](../runtime/shopping-qualification.md).

The example replicates `devices`, `keys` and `identities` to every member:
names, kinds and node keys of all devices, and which device received which
list. Remove the roster and use what the earlier phases provide.

## Ordered slices

1. Schema: remove `keys`; list envelopes travel by
   [direct delivery](11-direct-key-delivery.md). Remove `devices` and
   `identities` from member grants. State what replaces each use: device
   names and kinds for trusted nodes come from the registry, with an
   application label stored where only issuers read it.
2. Sharing a list names a recipient the sharer can identify without a roster.
   A trusted node picks from the registry. Decide and state what a member who
   may share sees: a contact it exchanged, for example by QR, not a list of
   the household.
3. Screens: a member's Devices tab shows **This device** and the peers it met
   by a local label only. The full tab, approvals and revocation stay on
   trusted nodes. Sync shows discovered peers and why each was chosen.
4. Pairing: a new device needs one contact; it finds the rest. Remove the
   steps that added contacts in both directions.
5. The harness and the device procedure follow the new flows; the Room
   schema export and the required-schema check are regenerated.

## Completion gate

- The harness scenarios `gate`, `exclusion-revocation`, `relink` and
  `creator-loss` pass on the new schema.
- A member's database file, searched after the full `gate`, contains no node
  key, name or kind of another device, and no envelope for another device.
- A member shares a list with a contact it exchanged; a trusted node shares
  with any device.
- A device paired with one contact syncs with a second device it was never
  told about.
- The Android application passes the device procedure on two emulators.
- The example's README, [getting started](../runtime/getting-started.md) and
  the audit describe the new flows.

## Not in this phase

The iOS application ([phase 18](18-shopping-and-market-on-ios.md)) and
physical devices ([phase 19](19-qualification.md)).
