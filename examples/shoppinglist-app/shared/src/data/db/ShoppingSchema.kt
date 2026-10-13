package org.example.examples.shopping.data.db

import org.example.examples.shopping.data.coordination.ApprovalCommands
import rocky.*

/**
 * Room tables plus trusted-only device management; list envelopes travel by direct delivery. Version 7 orders items
 * and gives them any number of categories; see `docs/runtime/shopping-schema-7.md`.
 */
val ShoppingSchema = schema("shopping", version = 7) {
    table("lists") {
        text("title")
        primaryKey("id")
        partitionBy("id", group = "list")
    }
    table("categories") {
        integer("list")
        text("name")
        primaryKey("list", "id")
        partitionBy("list", group = "list")
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
    table("requests") {
        text("key")
        text("title")
        text("note")
        text("requester")
        primaryKey("key")
        tableClass = TableClass.CommandControlled
    }
    table("decisions") {
        text("key")
        boolean("approved")
        text("decider")
        primaryKey("key")
        tableClass = TableClass.CommandControlled
    }
    trustedTables()
    commands = ApprovalCommands
}

/** What a Room export cannot hold: the group, the title policy, the commands and the trusted-only tables. */
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
    table("requests") { tableClass = TableClass.CommandControlled }
    table("decisions") { tableClass = TableClass.CommandControlled }
    trustedTables()
    commands = ApprovalCommands
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

/** The tables of a list's group. */
val ShoppingTables = listOf("lists", "categories", "items", "item_categories")

/** The `list` group partition of [list]: its key, sharing and holding. */
fun RockyDatabase.list(list: Long): GroupPartition = partitions.group("list", list)
