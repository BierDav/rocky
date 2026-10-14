# 3. Removal

Status: implemented (2026-10-14); see [evidence](#evidence).
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

- **Classification.** A removed table or field is allowed in an additive
  transition. A rename is a removal of the old name and an addition of the
  new one; values are not carried over unless the issuer's migration copies
  them. Removing a key or partition column removes its table. The new
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
- **Re-adding a name.** A generation that re-adds a removed name, which
  keeps its meaning and type ([decision 0067](../decisions/0067-names-are-identities.md)),
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
5. Pull-again after adoption and an earlier learned baseline (A), and the
   real Kotlin Room removed-column gate carried from phase 2 (B).
6. Derive added tables' first reader keys from the publisher key, a fixed
   domain-separation tag, domain and table name; qualify issuers signing
   apart and record the publisher-key trade-off in decision 0068 (C).
7. Name features_09 as the accepted roadmap in root AGENTS.md and this
   roadmap's README; retain features_08 as history (D).

## Completion gate

- A generation removes a field, a unique index on it and a table another
  table references; after adopting it, every device's file has exactly the
  new schema and keeps every remaining row visible.
- No stored row, checkpoint page or frame a migrated device authors or seals
  contains a removed field id. Relayed frames stay as signed.
- A frame an unmigrated device sent with a removed and a kept field applies
  the kept field on every device.
- An old Room app opens a database whose generation removed a column it
  declares; a DAO on an unchanged table works and one on the removed column
  fails with a SQLite error (carried from [phase 2](02-adopting-newer-generations.md)).
- A device that learns an earlier baseline of a re-adding generation pulls
  again the frames of the field's tables stamped after it (carried from
  phase 2).
- A device that went v7 → v8 → v9 and one that went v7 → v9, with a v7
  device's write to the re-added field relayed between them, end with the
  same value; a device that misses the horizon deadline imports the
  finalized checkpoint.
- Two issuers adding the same table apart derive the same first reader key
  and read each other's rows; revocation continues rotating later epochs.
  Decision 0068 records that publisher-key holders can derive first keys.
- Root AGENTS.md and the roadmap README name features_09 as accepted and
  features_08 as history. Discovery still refuses cuts; an application that
  declares one adopts it at open until phase 6 removes cuts.

## Evidence

The guide [removal](../runtime/removal.md) and decision record
[0068](../decisions/0068-adopting-newer-generations.md). Against the gate:

- Field, unique index and referenced table removed; exact new schema on
  every file, rows visible: `rocky-sdk` `tests/removal.rs`
  `a_generation_removes_a_field_its_unique_index_and_a_referenced_table`.
- No stored row or checkpoint holds a removed field id: the same test and
  `rocky-secure` `a_generation_removes_a_field_and_a_table_on_every_replica_that_adopts_it`,
  which also seals a checkpoint after the switch.
- An unmigrated device's frame applies its kept field on every device: both
  tests above.
- Old Room app (B): Kotlin `RoomGenerationsTest`.
- Earlier baseline pulls again (A): `rocky-secure`
  `a_readded_field_ignores_earlier_writes_and_pulls_again_the_later_ones`
  (`learn_baseline` admits a write stamped between the two baselines).
- v1 → v2 → v3 and v1 → v3 with a v1 write relayed: `rocky-sdk`
  `devices_that_reach_a_readded_field_by_different_steps_agree_on_it`; the
  secure test relays through a migrated replica and a replica that missed
  every frame imports the finalized checkpoint and agrees.
- Derived reader keys (C): `issuers_that_sign_one_schema_apart_end_on_one_generation`
  asserts both issuers hold one identical first `tags` epoch and read each
  other's rows; it fails with random keys.
- Roadmap (D): root `AGENTS.md`, `README.md` and the roadmap README.

Stamps became hybrid time and the file identity moved to v16 so stamps
compare with baselines; frame formats are unchanged.
