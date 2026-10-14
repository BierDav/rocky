# 3. Removal

Status: proposed; not started.
Owner: `rocky-core` `classify`; `rocky-secure` `extends`, admission, stored
rows and projection switch; `rocky-sdk` grants (`Domain`). Depends on
[phase 2](02-adopting-newer-generations.md). Follow the
[execution contract](README.md#agent-execution-contract). Shares decision record 0068.

## Inputs and objective

Read [generations](../runtime/generations.md),
[mergeable constraints](../runtime/mergeable-constraints.md) and
[partitioned scopes](../runtime/partitioned-scopes.md).

`classify` refuses `RemovedTable` and `RemovedField`, and `extends` requires
every installed item to remain. After this phase a generation may remove any
table or field, and every device that adopts it drops them the same way. Its
signing waits for [confirmation](05-confirmed-rollouts.md).

## Settled

- **Classification.** A removed identity is allowed in an additive
  transition. Removing a key or partition column removes its table. The new
  schema must be valid on its own, so foreign keys, indexes, partition
  groups, `granted_like`, views and existence logs that name a
  removed item go with it.
- **Every adopting device.** In the switch transaction, strip removed field
  ids from `RowFields`; delete removed tables' stored rows, scope state,
  subscriptions and group keys; rebuild affected projections and
  constraints. Checkpoints sealed afterwards exclude them.
- **Field by field.** Every device ignores removed fields and tables in
  incoming frames and checkpoint pages and applies the rest, so a frame from
  a device that has not migrated yet converges everywhere.
- **Re-adding an identity.** A generation that re-adds a removed identity
  records its [baseline](02-adopting-newer-generations.md#settled). Every device ignores writes to the
  field stamped before it; the issuer's migration refills it after. A device
  adopting the generation pulls again the frames of the field's tables
  stamped after the baseline. If they were pruned meanwhile, its state
  differs from the finalized cut and it imports the finalized checkpoint.
- **Grants.** Renewal drops removed tables and fields from profiles, field
  grants, partition selectors and `granted_like`. Revocations and horizon
  statements that name them still verify.
- **Commands** stay unchangeable until [phase 11](11-removing-strong-commands.md)
  removes them.

## Ordered slices

1. `classify` and `extends` for removal.
2. Stored state, projection, constraints and checkpoints in the switch.
3. Field-by-field admission of frames and pages from unmigrated devices.
4. Grants; guides: generations, mergeable constraints, decision 0066 limits.

## Completion gate

- A generation removes a field, a unique index on it and a table another
  table references; after adopting it, every device's file has exactly the
  new schema and keeps every remaining row visible.
- No stored row, checkpoint page or frame a migrated device authors or seals
  contains a removed field id. Relayed frames stay as signed.
- A frame an unmigrated device sent with a removed and a kept field applies
  the kept field on every device.
- A device that went v7 → v8 → v9 and one that went v7 → v9, with a v7
  device's write to the re-added field relayed between them, end with the
  same value; a device that misses the horizon deadline imports the
  finalized checkpoint.
