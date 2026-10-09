package org.example.examples.shopping

import rocky.*

/**
 * [ShoppingDatabase]'s tables as Rocky replicates them; importing its Room export with [shoppingRoomOverlay] yields
 * the same definition. `id` and `row_kind` are implicit. A list, its categories and items form the `list` group:
 * one secret per list derives every table's key, so one envelope shares the whole list. `devices` names each
 * device's kind and `keys` carries list envelopes to them; both are written by trusted devices only.
 */
val ShoppingSchema = schema("shopping", version = 4) {
    table("lists") { text("title"); primaryKey("id"); partitionBy("id", group = "list") }
    table("categories") {
        integer("list"); text("name")
        primaryKey("list", "id"); partitionBy("list", group = "list")
        reference("list", "lists", onDelete = Delete.Cascade)
    }
    table("items") {
        integer("list"); text("key"); text("title"); boolean("completed"); integer("category", nullable = true)
        primaryKey("list", "key"); partitionBy("list", group = "list")
        unique("list", "title", onConflict = Conflict.LatestWins)
        index("index_items_list_category", "list", "category")
        reference("list", "lists", onDelete = Delete.Cascade)
        reference(listOf("list", "category"), "categories", onDelete = Delete.SetNull)
    }
    table("devices") { text("node"); text("name"); text("kind"); primaryKey("node") }
    table("keys") { text("recipient"); integer("list"); integer("epoch"); blob("envelope"); primaryKey("recipient", "list") }
}

/** The Rocky attributes a Room export cannot hold: the group and the title policy. */
val shoppingRoomOverlay: SchemaBuilder.() -> Unit = {
    table("lists") { partitionBy("id", group = "list") }
    table("categories") { partitionBy("list", group = "list") }
    table("items") { partitionBy("list", group = "list"); onConflict("index_items_list_title", Conflict.LatestWins) }
}

/** The tables of a list's group. */
val ShoppingTables = listOf("lists", "categories", "items")

/** The `list` group partition of [list]: its key, sharing and holding. */
fun RockyDatabase.list(list: Long): GroupPartition = partitions.group("list", list)
