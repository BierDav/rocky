# 4. Held changes

Status: implemented; all three completion gates passed. See
[evidence](04-held-changes-evidence.md) and
[decision 0069](../decisions/0069-held-changes-and-void-records.md).
Owner: `rocky-secure` author logs, outbox and admission; `rocky-sdk` status
and the Kotlin API. Depends on [phase 3](03-removal.md). Follow the
[execution contract](README.md#agent-execution-contract). Takes decision record 0069.

## Inputs and objective

Read [secure sessions](../runtime/secure-sessions.md),
and [SQL sessions](../runtime/sql-sessions.md).

A device may have committed changes that touch an item a newer generation
removed, before it learned of the removal and migrated. Sending them half would split a
transaction; holding them would block its author logs. After this phase such
changes are held whole, their frames voided, and the application re-authors
or drops them. The issuer that removed the items never saw these changes and
could not have migrated them, so only the application can.

## Settled

- **What is held.** When it migrates past a removal, every bundle never offered to any
  peer that writes a removed field to a non-default value or touches a
  removed table is held, whole. Frames already offered, including by a push that failed
  midway, are left to the [field-by-field rule](03-removal.md#settled).
- **Local effects roll back.** Holding a bundle reverts its effects on kept
  tables in the author's projection to the admitted state, so the device
  matches its peers at once. The held change keeps its values for
  re-authoring.
- **Void records.** The device signs a void record for the held frames'
  sequences in each author log. Receivers treat voided sequences as consumed,
  so later frames of those logs apply. A void never covers an offered frame.
- **Held changes as data.** The API lists each held change: table, row id,
  fields with values and when it was made. The application
  shows a warning with the count.
- **What it would overwrite.** For each held row the API also returns the
  current values of the fields the change writes, with when and by whom each
  was last changed, so the application can show what a re-author replaces.
- **Re-author or drop.** A newer app version maps a held change to its schema
  and re-authors it as an ordinary new bundle with a new timestamp, or drops
  it. Both remove it from the list. No backdated stamp is ever signed.

## Ordered slices

1. Decision record 0069: void records and their admission.
2. Holding and voiding on learning a removal; never-offered tracking.
3. Held-change list, current values with their last change, re-author and
   drop in the SDK and Kotlin.
4. Guides: secure sessions, SQL sessions.

## Completion gate

- An old device commits a bundle touching a removed table and a kept one,
  then learns the removal: the bundle is held, its frames voided, its
  kept-table effects rolled back locally, and the device's later edits to the
  kept table reach every peer.
- After the app update, the held change lists the newer value another device
  wrote meanwhile; re-authoring it wins over that value on every device.
- A void for an offered frame is refused.
