package org.example.examples.shopping.data.db

import rocky.*

/**
 * Room tables plus trusted-only device management; list envelopes travel by direct delivery. Version 7 orders items
 * and gives them any number of categories; version 8 orders categories. See `docs/runtime/shopping-schema-7.md` and
 * `docs/runtime/shopping-schema-8.md`.
 */
val ShoppingSchema = schema("shopping", version = 8) {
    table("lists") {
        text("title")
        primaryKey("id")
        partitionBy("id", group = "list")
    }
    table("categories") {
        integer("list")
        text("name")
        text("position", default = "")
        primaryKey("list", "id")
        partitionBy("list", group = "list")
        index("index_categories_list_position", "list", "position")
        reference("list", "lists", onDelete = Delete.Cascade)
    }
    table("items") {
        integer("list")
        text("key")
        text("title")
        boolean("completed")
        integer("category", nullable = true)
        text("position", default = "")
        primaryKey("list", "key")
        partitionBy("list", group = "list")
        unique("list", "title", onConflict = Conflict.LatestWins)
        index("index_items_list_category", "list", "category")
        index("index_items_list_position", "list", "position")
        reference("list", "lists", onDelete = Delete.Cascade)
        reference(listOf("list", "category"), "categories", onDelete = Delete.SetNull)
    }
    table("item_categories") {
        integer("list")
        integer("item")
        integer("category")
        primaryKey("list", "item", "category")
        partitionBy("list", group = "list")
        index("index_item_categories_list_category", "list", "category")
        reference(listOf("list", "item"), "items", onDelete = Delete.Cascade)
        reference(listOf("list", "category"), "categories", onDelete = Delete.Cascade)
        grantedLike = "items"
    }
    itemsMigration()
    categoriesMigration()
    table("requests") {
        text("key")
        text("title")
        text("note")
        text("requester")
        primaryKey("key")
    }
    table("decisions") {
        text("key")
        boolean("approved")
        text("decider")
        primaryKey("key")
    }
    trustedTables()
}

/** What a Room export cannot hold: the group, the title policy, the coordinator and the trusted-only tables. */
val shoppingRoomOverlay: SchemaBuilder.() -> Unit = {
    table("lists") { partitionBy("id", group = "list") }
    table("categories") { partitionBy("list", group = "list") }
    table("items") {
        partitionBy("list", group = "list")
        onConflict("index_items_list_title", Conflict.LatestWins)
    }
    table("item_categories") {
        partitionBy("list", group = "list")
        grantedLike = "items"
    }
    itemsMigration()
    categoriesMigration()
    trustedTables()
}

/**
 * Version 7, run once by the trusted device that upgrades first: each list's items are placed in their creation
 * order, and each item's one category becomes a link. `items.category` stays, unwritten, until a cut can drop it.
 */
private fun SchemaBuilder.itemsMigration() = migration(7) {
    sql("UPDATE items SET position = printf('%09d5', (SELECT count(*) FROM items AS earlier WHERE earlier.list = items.list AND earlier.id < items.id))")
    insert("item_categories", "SELECT list, id AS item, category FROM items WHERE category IS NOT NULL")
    sql("UPDATE items SET category = NULL WHERE category IS NOT NULL")
}

/**
 * Version 8, run once by the trusted device that publishes it: each list's categories are placed by name. A
 * migration closes the tip it leaves, so writes made on it meanwhile come back as held changes.
 */
private fun SchemaBuilder.categoriesMigration() = migration(8) {
    sql("UPDATE categories SET position = printf('%09d5', (SELECT count(*) FROM categories AS earlier WHERE earlier.list = categories.list AND (earlier.name < categories.name OR (earlier.name = categories.name AND earlier.id < categories.id))))")
}

/** The tables of a list's group. */
val ShoppingTables = listOf("lists", "categories", "items", "item_categories")

/** The `list` group partition of [list]: its key, sharing and holding. */
fun RockyDatabase.list(list: Long): GroupPartition = partitions.group("list", list)
