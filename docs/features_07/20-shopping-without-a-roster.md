# 20. Shopping without a roster

Status: not started.
Owner: `examples/shoppinglist-app` and its harness; the starter guide and
the audit. Depends on [phase 5](05-issuer-registry.md),
[phase 12](12-direct-key-delivery.md), [phase 13](13-relayed-horizon.md) and
[phase 14](14-contacts-and-partner-choice.md). Wave 5.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the applications](scope-mesh.md#7-the-applications), the shopping
example's README, `ShoppingSchema.kt`, `NodeKind.kt`, `TrustedRows.kt`,
`Household.kt`, `Coordination.kt`, the Devices and Sync screens and
[the audit](../runtime/shopping-qualification.md).

The example replicates `devices`, `keys` and `identities` to every member:
names, kinds and node keys of all devices, which device received which list,
and every device's command identity. Remove the roster and use what the
earlier phases provide.

## Settled

- `keys` is removed; list envelopes travel by direct delivery (phase 12).
- `devices` moves to the trusted-only tables beside `invitations` and
  `coordination`. It holds name and kind per node; members hold no grant on
  it. The registry's opaque label carries the same name and kind, so an
  issuer names a device before the table syncs. Nothing else of the
  application enters the registry.
- `identities` is removed. A device's command identity comes from its
  registry record (phase 5); trusted devices register callers with the
  coordinator from Kotlin `devices()`.
- `requests.requester` and `decisions.decider` stay, and members keep
  reading them. They hold command identities, which are not node keys and
  which a member cannot map to a device without the registry. This is the
  one stated exception to "no row naming another device". A member shows
  **This device** or **Another device**; a trusted device shows the name.
- A member shares a list only with a contact it scanned. A trusted node
  shares with any device of the registry.
- A member's labels for peers it met are local and never replicated.
- Horizons name their participants from the registry through phase 13;
  `Household` no longer declares horizons on `devices` and `identities`.
- The gates hold with contacts alone; discovery
  ([phase 15](15-discovery.md)) is not a prerequisite.

## Ordered slices

1. Schema and grants: the changes above in `ShoppingSchema`, the Room
   entities and `NodeKind.profile`. Members keep read on `lists` and
   `decisions`, their rights on `categories`, `items` and `requests`, and
   nothing else. Regenerate the Room schema export and the required-schema
   check.
2. `Household` and `Coordination` read devices and command identities from
   `devices()` and the trusted table; on a member both paths are absent
   rather than empty.
3. Sharing: the Share action on a member opens a scanner for the recipient's
   contact code; on a trusted node it lists the registry.
4. Screens: a member's Devices tab shows **This device** and the peers it
   met, by local label. The full tab, approvals of devices and revocation
   stay on trusted nodes. Sync lists the peers known and the reason each was
   ranked, as phase 14 reports it.
5. Pairing: a new device needs one contact and reaches the others by contact
   handover (phase 14). Remove the steps that added contacts in both
   directions.
6. The harness and the Android device procedure follow the new flows. If
   [phase 21](21-shopping-and-market-on-ios.md) has landed, write the iOS
   pair, share and revoke steps and rerun its simulator check.

## Completion gate

- The harness scenarios `gate`, `exclusion-revocation`, `relink` and
  `creator-loss` pass on the new schema.
- After the full `gate`, a byte search of a member's database file and
  directory finds no other device's node key, name or kind, and no envelope
  addressed to another device. Command identities in `requests` and
  `decisions` are the listed exception.
- A member opening `devices`, `keys` or `identities` by SQL gets "no such
  table".
- A member shares a list with a contact it scanned; a trusted node shares
  with a device it never met, picked from the registry.
- A device paired with one contact syncs with a second device it was never
  told about, with discovery disabled.
- A member asks for an approval, a trusted device that never synced a roster
  table decides it, and both read the decision.
- A horizon finalizes on a member that met only the custody peer.
- The Android application passes the device procedure on two emulators.
- The example's README, [getting started](../runtime/getting-started.md) and
  the audit describe the new flows.

## Not in this phase

- Registry fields for application data beyond the label, or showing the
  registry to members.
- Any change to an SDK format, session message or grant rule.
- Removing approvals, or hiding command identities from members.
- A replicated contact or label table.
- Local or topic discovery in the application's flows.
- Physical devices ([phase 22](22-qualification.md)).
