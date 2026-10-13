# Shopping schema 7: ordered items in several categories

Implemented. Version 7 of the [shopping example](../../examples/shoppinglist-app/README.md)
orders a list's items and lets an item belong to any number of its list's
categories. It is the example's first upgrade of an existing household and uses
both [breaking generations](generations.md#breaking-generations) and
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
every other device receives in its checkpoint of the cut.

## What a household sees

1. The first trusted device to open version 7 migrates and cuts. It needs every
   list; a trusted device holds all of them.
2. Each other device that opens version 7 fetches the generation from a trusted
   device; it needs the network for that, and keeps opening version 6 offline.
   It then imports the trusted device's lists. Edits it made under version 6
   that the trusted device had not received are gone.
3. A device still running version 6 is refused by every upgraded device until
   its app is updated.

Full and partial members keep their enrollment: renewal into version 7 grants
them `item_categories` because it follows `items`. Partial members still hold
only the lists shared with them.

## Evidence and limits

`SchemaUpgradeTest` creates a version 6 household with a trusted device and a full
member, upgrades both and checks the order, the links, the member's dropped
offline item, and that the member links, reorders and adds items. The example's
`check` compares the hand-written schema with Room's version 7 export.

Two trusted devices opening version 7 without reaching each other would each
cut; upgrade one trusted device first. A device behind the cut sees failed
connections, not an upgrade prompt.
