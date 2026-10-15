# Shopping schema 7: ordered items in several categories

Implemented. Version 7 of the [shopping example](../../examples/shoppinglist-app/README.md)
orders a list's items and lets an item belong to any number of its list's
categories. It is the example's first upgrade of an existing household and uses
both [required fields](generations.md#required-fields) and
[tables granted like another](generations.md#tables-granted-like-another).

## What changed

| Version 6 | Version 7 |
| --- | --- |
| Items sorted by `id` | `items.position`, required, default `''`; sorted by `position, id` |
| `items.category`, one per item | `item_categories(list, item, category)`, in the `list` group |
| Members granted `categories` and `items` | `item_categories` granted like `items` |

`position` holds decimal keys compared as text (`Positions` in the app). Moving
an item writes one key between its new neighbours, so concurrent moves of
different items merge and concurrent moves of one item keep the later.
Two items placed at one spot concurrently share a key and sort by `id`; the next
move between them renumbers the list in one transaction.

A link's key is the item and category, so two devices adding the same link
keep one row. Deleting a category or an item hides its links. Moving an item to
another list first removes its links, since categories belong to their list.

`items.category` stays: Rocky cannot drop a field yet. Version 7 never writes
it, and the migration clears it.

## The migration

`ShoppingSchema` declares one migration for version 7. The first trusted device
that opens a version 6 household with the new app runs it:

```kotlin
migration(7) {
    sql("UPDATE items SET position = printf('%09d5', (SELECT count(*) FROM items AS earlier WHERE earlier.list = items.list AND earlier.id < items.id))")
    insert("item_categories", "SELECT list, id AS item, category FROM items WHERE category IS NOT NULL")
    sql("UPDATE items SET category = NULL WHERE category IS NOT NULL")
}
```

`insert` adds a row for each selected row with an `id` Rocky issues in its
partition; plain SQL cannot issue ids. Existing items keep their order of
creation. The writes are ordinary signed writes of the trusted device, which
every other device receives like any other.

## What a household sees

1. The first trusted device to open version 7 signs it, then migrates in it
   before it syncs ([migrations](generations.md#migrations)). It needs every
   list; a trusted device holds all of them.
2. Every other device moves its database to version 7 when it meets a device
   that runs it, whatever app version it runs; one that opens version 7
   offline opens at version 6 until then. Edits it made under version 6 are
   kept and sent.
3. A device still running the version 6 app keeps syncing on its version 7
   database. Items it adds, or edits it made before the trusted device
   migrated, read the default position `''` and sort first.

Full and partial members keep their enrollment and adopt version 7 without an
issuer; their next renewal grants them `item_categories` because it follows
`items`. Partial members still hold
only the lists shared with them.

## Evidence and limits

`SchemaUpgradeTest` creates a version 6 household with a trusted device and a full
member and upgrades the trusted device. The member, still running the version 6
app, syncs: its database moves to version 7, its offline item arrives, and an
item it adds reads the default position. Updated, it links, reorders and adds
items. The example's `check` compares the hand-written schema with Room's
version 7 export.

Two trusted devices opening version 7 without reaching each other each
migrate; their writes merge, the later order winning. Upgrade one trusted
device first.
