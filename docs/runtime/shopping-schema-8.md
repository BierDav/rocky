# Shopping schema 8: ordered categories

Implemented. Version 8 of the [shopping example](../../examples/shoppinglist-app/README.md)
orders each list's categories. It is the example's first schema change published
through [seals](generations.md): a migration that closes `main`'s tip.

## What changed

| Version 7 | Version 8 |
| --- | --- |
| Categories sorted by name | `categories.position`, required, default `''`; sorted by `position, name, id` |

New categories go after the last one (`Positions.between`). An app older than
version 8 keeps reading and writing categories; its new ones read the default
position `''` and sort first.

## The migration

```kotlin
migration(8) {
    sql("UPDATE categories SET position = printf('%09d5', (SELECT count(*) FROM categories AS earlier WHERE earlier.list = categories.list AND (earlier.name < categories.name OR (earlier.name = categories.name AND earlier.id < categories.id))))")
}
```

Existing categories are placed by name. A migration closes the tip it leaves:
the trusted device that publishes it signs the `close` seal, opens the migration
tip and moves the bookmark ([migrations](generations.md#migrations)).

## Trying and publishing

1. A trusted device opens `dev` on `main`'s latest seal and restarts with the
   version 8 app: opening publishes version 8 on `dev` and runs the migration
   there. Devices on `main` never fetch it.
2. To publish, it follows `main`. The app seals `main` and reopens, which
   migrates it. With networking switched off, nothing leaves the device until
   it is switched on ([shopping on seals](shopping-seals.md#publishing-a-schema)).
3. A member that wrote `main` meanwhile gets that write back as a held change
   once it follows the migration; its app, version 7 or 8, re-authors it.

## Evidence

The [qualification harness](shopping-qualification.md) runs this as `publish`:
app version 7 on the member and phone, version 8 on the trusted desktop. The
example's `check` compares the hand-written schema with Room's version 8 export.
