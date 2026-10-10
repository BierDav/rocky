package org.example.examples.shopping

import rocky.*

/**
 * [ShoppingDatabase]'s tables as Rocky replicates them; importing its Room export with [shoppingRoomOverlay] yields
 * the same definition. `id` and `row_kind` are implicit. A list, its categories and items form the `list` group:
 * one secret per list derives every table's key, so one envelope shares the whole list. `devices` names each
 * device's kind and `keys` carries list envelopes to them; both are written by trusted devices only. Each device
 * publishes its command identity in `identities`. Trusted devices alone read `invitations`, each one's open pairing
 * code, and `coordination`, the household's coordination tenant. `requests` and `decisions` change only through the [ApprovalCommands].
 */
val ShoppingSchema = schema("shopping", version = 5) {
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
    table("devices") {
        text("node")
        text("name")
        text("kind")
        primaryKey("node")
    }
    table("keys") {
        text("recipient")
        integer("list")
        text("epoch")
        blob("envelope")
        primaryKey("recipient", "list")
    }
    table("identities") {
        text("node")
        text("identity")
        primaryKey("node")
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
