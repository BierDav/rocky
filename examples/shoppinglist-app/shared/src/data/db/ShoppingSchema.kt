package org.example.examples.shopping.data.db

import org.example.examples.shopping.data.coordination.ApprovalCommands
import rocky.*

/** Room tables plus trusted-only device management; list envelopes travel by direct delivery. */
val ShoppingSchema = schema("shopping", version = 6) {
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
        primaryKey("list", "key")
        partitionBy("list", group = "list")
        unique("list", "title", onConflict = Conflict.LatestWins)
        index("index_items_list_category", "list", "category")
        reference("list", "lists", onDelete = Delete.Cascade)
        reference(listOf("list", "category"), "categories", onDelete = Delete.SetNull)
    }
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
    table("requests") { tableClass = TableClass.CommandControlled }
    table("decisions") { tableClass = TableClass.CommandControlled }
    trustedTables()
    commands = ApprovalCommands
}

/** The tables of a list's group. */
val ShoppingTables = listOf("lists", "categories", "items")

/** The `list` group partition of [list]: its key, sharing and holding. */
fun RockyDatabase.list(list: Long): GroupPartition = partitions.group("list", list)
