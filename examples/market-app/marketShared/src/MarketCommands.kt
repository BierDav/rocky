package org.example.examples.market

import rocky.*

/** A typed reservation: stock never goes below zero, and a shortfall is a typed rejection. */
data class ReserveArguments(val item: String, val quantity: Long, val hold: String)
data class RestockArguments(val item: String, val quantity: Long)
data class Remaining(val remaining: Long)
data class Shortfall(val remaining: Long, val requested: Long)

/** Shared by both commands. An object of its own, so no command waits on this file's other values to initialize. */
private object Shared {
    val remaining = Shape(listOf(Field("remaining", FieldType.Integer)), { value: Remaining -> listOf(value.remaining) }, { Remaining(it[0] as Long) })

    /** One confirmed sequence orders every command over these tables. */
    val tables = listOf("stock", "holds")
}

object Reserve : Command<ReserveArguments, Remaining, Shortfall>(
    name = "Reserve",
    handler = "market.reserve.v1",
    arguments = Shape(
        listOf(Field("item", FieldType.Text), Field("quantity", FieldType.Integer), Field("hold", FieldType.Text)),
        { listOf(it.item, it.quantity, it.hold) },
        { ReserveArguments(it[0] as String, it[1] as Long, it[2] as String) },
    ),
    accepted = Shared.remaining,
    rejected = Shape(
        listOf(Field("remaining", FieldType.Integer), Field("requested", FieldType.Integer)),
        { listOf(it.remaining, it.requested) },
        { Shortfall(it[0] as Long, it[1] as Long) },
    ),
    // Who holds what is private to readers of `holds`; `stock` is public.
    invocation = "holds",
    result = "holds",
    resources = Shared.tables,
)
object Restock : Command<RestockArguments, Remaining, Unit>(
    name = "Restock",
    handler = "market.restock.v1",
    arguments = Shape(
        listOf(Field("item", FieldType.Text), Field("quantity", FieldType.Integer)),
        { listOf(it.item, it.quantity) },
        { RestockArguments(it[0] as String, it[1] as Long) },
    ),
    accepted = Shared.remaining,
    rejected = Shape.none,
    invocation = "stock",
    result = "stock",
    resources = Shared.tables,
)

/** Both commands, in one set: one confirmed sequence orders them. */
val MarketCommands = CommandSet(listOf(Reserve, Restock))

/**
 * The market as Rocky replicates it; [MarketDatabase]'s Room export imports to the same definition with
 * [marketRoomOverlay]. `listings` and `memos` merge; `stock` and `holds` change only through [Reserve] and [Restock].
 */
val MarketSchema = schema("market") {
    table("listings") {
        text("key")
        text("title")
        primaryKey("key")
    }
    table("memos") {
        text("key")
        text("listing")
        text("note")
        primaryKey("key")
    }
    table("stock") {
        text("key")
        integer("remaining")
        primaryKey("key")
        tableClass = TableClass.CommandControlled
    }
    table("holds") {
        text("key")
        text("item")
        integer("quantity")
        primaryKey("key")
        tableClass = TableClass.CommandControlled
    }
    commands = MarketCommands
}

/** What a Room export cannot hold: the class of the command tables and the commands. */
val marketRoomOverlay: SchemaBuilder.() -> Unit = {
    table("stock") { tableClass = TableClass.CommandControlled }
    table("holds") { tableClass = TableClass.CommandControlled }
    commands = MarketCommands
}

/**
 * A command row is its key and columns, then `id` and `row_kind`. Every executor must derive the same `id`: an
 * existing row keeps its own and a new one takes the next after the table's largest. Command tables stay far below
 * the scan limit, since each result carries the whole table.
 */
private fun CommandContext.put(table: String, existing: List<Any?>?, vararg values: Any?): Write {
    val id = existing?.let { it[it.size - 2] as Long }
        ?: ((scan(table, limit = 1024).maxOfOrNull { it[it.size - 2] as Long } ?: (1L shl 32)) + 1)
    return Write.Put(table, values.toList() + listOf(id, 0L))
}

/** A `stock` row is its `key`, which names the item, then `remaining`. */
private fun List<Any?>?.remaining(): Long = this?.get(1) as Long? ?: 0

/**
 * Takes [ReserveArguments.quantity] from the item's stock into the hold. A hold key names one item: reserving under it
 * again adds to that hold, and naming another item is refused as a shortfall of the whole request.
 */
val reserve = Reserve handledBy { context, arguments ->
    val stock = context.get("stock", arguments.item)
    val left = stock.remaining()
    // A `holds` row is its `key`, then `item` and `quantity`.
    val hold = context.get("holds", arguments.hold)
    if (arguments.quantity <= 0 || arguments.quantity > left || (hold != null && hold[1] != arguments.item)) {
        Decision.reject(Shortfall(left, arguments.quantity))
    } else {
        val held = hold?.get(2) as Long? ?: 0
        Decision.accept(
            Remaining(left - arguments.quantity),
            listOf(
                context.put("stock", stock, arguments.item, left - arguments.quantity),
                context.put("holds", hold, arguments.hold, arguments.item, held + arguments.quantity),
            ),
        )
    }
}
val restock = Restock handledBy { context, arguments ->
    val stock = context.get("stock", arguments.item)
    val left = stock.remaining() + arguments.quantity
    if (arguments.quantity <= 0) Decision.reject(Unit) else Decision.accept(Remaining(left), listOf(context.put("stock", stock, arguments.item, left)))
}

/** The handlers an executing device composes. */
val MarketHandlers = listOf(reserve, restock)
