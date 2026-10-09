package org.example.examples.shopping

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
    handler = "stock.reserve.v1",
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
    // The hold's owner is private to readers of `holds`; `stock` is public.
    invocation = "holds",
    result = "holds",
    resources = Shared.tables,
)
object Restock : Command<RestockArguments, Remaining, Unit>(
    name = "Restock",
    handler = "stock.restock.v1",
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

val ReservationSchema = schema("reservations") {
    table("stock") {
        text("key")
        integer("remaining")
        primaryKey("key")
    }
    table("holds") {
        text("key")
        text("item")
        integer("quantity")
        primaryKey("key")
    }
    commands = CommandSet(listOf(Reserve, Restock))
}

/** A `stock` row is its `key`, which names the item, then `remaining`, `id` and `row_kind`. */
private fun CommandContext.remaining(item: String): Long = get("stock", item)?.get(1) as Long? ?: 0

/** Every executor derives the same `id` for a key: author 1, and a sequence from the key. */
private fun row(vararg values: Any?): List<Any?> = values.toList() + listOf((1L shl 32) + (values[0].hashCode().toLong() and 0x7fffffff) + 1, 0L)

val reserve = Reserve handledBy { context, arguments ->
    val left = context.remaining(arguments.item)
    if (arguments.quantity <= 0 || arguments.quantity > left) {
        Decision.reject(Shortfall(left, arguments.quantity))
    } else {
        Decision.accept(
            Remaining(left - arguments.quantity),
            listOf(
                Write.Put("stock", row(arguments.item, left - arguments.quantity)),
                // A `holds` row is its `key`, then `item` and `quantity`.
                Write.Put("holds", row(arguments.hold, arguments.item, arguments.quantity)),
            ),
        )
    }
}
val restock = Restock handledBy { context, arguments ->
    val left = context.remaining(arguments.item) + arguments.quantity
    Decision.accept(Remaining(left), listOf(Write.Put("stock", row(arguments.item, left))))
}
