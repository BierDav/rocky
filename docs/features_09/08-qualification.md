# 8. Qualification

Status: proposed; not started.
Owner: `examples/shoppinglist-app` and its qualification harness. Depends on
[phases 4](04-held-changes.md) and [7](07-migrations-at-any-size.md). Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [shopping schema 7](../runtime/shopping-schema-7.md) and
[shopping qualification](../runtime/shopping-qualification.md).

Schema 7 left `items.category` cleared and unused because nothing could
remove it. This phase removes it with real app versions on real devices,
while older versions keep syncing.

## Settled

- **Schema 8** removes `items.category` and `index_items_list_category`. The
  trusted desktop confirms the rollout in the app's dialog.
- **Devices.** A trusted desktop moves to app version 8. A member desktop
  keeps app version 6 and an Android phone app version 7; both databases
  migrate to 8 when they learn of it.
- **Held change.** The member on 6, offline, sets an item's category the
  old way before it learns of schema 8. On learning, the change is held and
  the app warns.
- **Update.** The member's app warns that it should update. It updates to 8,
  shows what the held change would overwrite and re-authors it as an
  `item_categories` link.

## Ordered slices

1. Schema 8 and its Room export; `SchemaUpgradeTest` for 7 → 8 with members
   on 6 and 7.
2. The held category change on 6.
3. The member's update and re-authoring.
4. Device qualification on desktop and Android; the guides above.

## Completion gate

- After migrating, the apps on 6 and 7 add, move and complete items, and
  every device ends with the same lists.
- No device's file has `items.category` after it migrates; the app on 6
  fails only where it sets a category, and warns that it should update.
- The re-authored category link appears on every device on 7 and 8.
- `./examples/shoppinglist-app/check` passes; measured sync timings are
  recorded with the qualification evidence.
