package rocky.rejected

import rocky.*

// The typed commands every rejected fixture misuses.
data class ReserveArguments(val item: String, val quantity: Long, val hold: String)
data class RestockArguments(val item: String, val quantity: Long)
data class Remaining(val remaining: Long)
data class Shortfall(val remaining: Long, val requested: Long)

private val remaining = Shape(listOf(Field("remaining", FieldType.Integer)), { value: Remaining -> listOf(value.remaining) }, { Remaining(it[0] as Long) })

object Reserve : Command<ReserveArguments, Remaining, Shortfall>(
    name = "Reserve",
    handler = "rejected.reserve.v1",
    arguments = Shape(
        listOf(Field("item", FieldType.Text), Field("quantity", FieldType.Integer), Field("hold", FieldType.Text)),
        { listOf(it.item, it.quantity, it.hold) },
        { ReserveArguments(it[0] as String, it[1] as Long, it[2] as String) },
    ),
    accepted = remaining,
    rejected = Shape(
        listOf(Field("remaining", FieldType.Integer), Field("requested", FieldType.Integer)),
        { listOf(it.remaining, it.requested) },
        { Shortfall(it[0] as Long, it[1] as Long) },
    ),
    invocation = "holds",
    result = "holds",
    resources = listOf("stock", "holds"),
)
