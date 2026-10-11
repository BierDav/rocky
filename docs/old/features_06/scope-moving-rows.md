# Scope: moving rows between partitions

Status: agreed 2026-10-08; implemented by [phase 8](08-row-moves.md). Builds on
[constraints on partitioned tables](scope-partitioned-constraints.md).
Owner: `rocky-secure` frames, apply and constraint resolution; the SDK and
Kotlin write surface.

## Problem

Partition columns are immutable primary-key columns
([design rationale](../runtime/partitioned-scopes.md#design-rationale)), so an application
moves a row by deleting it in partition A and inserting it in B. The insert is
a new incarnation ([design rationale](../runtime/logical-schema.md#row-identity)): an
update another device made offline to the old row names a deleted incarnation
and is lost.

Example: Pia moves "milk" from "Weekly" to "Weekend" while Quinn, offline,
ticks it off in "Weekly". After sync the milk in "Weekend" is unticked. In an
ERP this happens constantly: an order line moves to another order, a task to
another project.

Where the boundary does not need to be the thing rows move between, partition
by a coarser, stable column and keep the changing one as an ordinary column.
The move is then a plain update. This request covers the remaining case.

## What I need

A move operation with these properties:

- **Atomic.** One transaction deletes the row in A and inserts it in B. The
  insert carries every field's value with its original stamp, and a
  `moved from` link naming A's tuple. The row keeps its `id`.
- **Late updates follow.** A device that authored an update to the old row
  without having seen the move, and that holds B with the needed write right,
  issues that update again against the new row with its original stamp.
  Latest-wins then decides as if the row had never moved. Only the author's
  own device does this, so no update is issued twice.
- **No new exposure.** A device holding only A sees a deletion and learns
  nothing about B. The link is sealed under A's key inside B's insert, so a
  device holding only B sees that the row was moved in, with its original
  author's pseudonym and creation stamp, not from where.
- **Children move with their parent.** The runtime collects every row that
  belongs to the moved row, following `CASCADE` references inside the group
  transitively, and moves them in the same transaction. The application names
  the parent only; there is no separate call.
- **References out of the moved rows.** A `SET NULL` reference to a row that
  stays in A, such as an item's category, is nulled. A `NOT NULL` reference to
  such a row refuses the move.
- **All or nothing.** The move is refused as a whole when it exceeds the
  transaction limits or when the mover does not hold a child table of the
  partition. The error names the cause and the table. Recovering from it is
  the application's concern; Rocky offers no workaround.
  Unique indexes and references resolve in B as for any insert.
- **Chains.** A row moved twice is followed across both links.

## Decided 2026-10-08

- **The `id` is kept, with a proof B can verify.** Revised 2026-10-08.
  `id` is application-visible and every reference names the parent's
  partition columns plus `id`, so it must not change. Admission today
  refuses an `id` whose author is not the signer
  ([design rationale](../runtime/logical-schema.md#row-identity)), and a device holding
  only B cannot open the link. B's insert therefore carries the original
  author's signed creation commitment for the `id`, verifiable without A's
  key; B admits a foreign-author `id` only with it. This reveals the
  original author's pseudonym and creation stamp to readers of B, not A's
  tuple. Every partitioned table allocates `id`s per author and table, so
  the original author never reissues a moved `id`. Decision 0038 settles
  replaying one proof into a second partition.
- **Missed updates are found by field stamps.** A device follows its own
  update when that update's stamp is newer than the stamp the moved row
  carries for the field. Ordinary updates carry nothing new.
- **Until the horizon.** A late update follows a move only while the
  [mergeable horizon](../runtime/mergeable-horizon.md) of A has not
  finalized the move. Afterwards it is dropped like any edit behind the
  horizon.
- **Trusted profile.** The same operation and rules apply.

- **Only the author follows.** The device that authored a missed update
  issues it again, under its own signature. No device signs for another. An
  update whose author's device never learns of the move is not followed.
- **Large payloads stay outside rows.** Applications store files externally
  and keep a pointer row, so a move stays within the transaction limits.

## Formats

The frame change gets a decision record;
[no compatibility](README.md#no-backward-compatibility) with the earlier frame
is kept. Checkpoints must carry the links that the horizon has not yet
finalized.

## Acceptance

1. Two phones hold A and B. One moves a row, the other ticks it offline; both
   converge on the ticked row in B.
2. A phone holding only A sees the row disappear and receives nothing of B.
3. A phone that ticked offline and holds only A: the tick is dropped on every
   device, deterministically.
4. Edits made in B after the move with a later stamp win over a followed
   update with an earlier one.
5. Moving an item moves its attachments and nulls its category; a device
   holding only B reads the attachments.
6. A device that left out the attachments table is refused with an error
   naming that table; nothing is written.
