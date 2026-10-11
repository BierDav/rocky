package org.example.examples.shopping.data.coordination

import rocky.*

/** A purchase a device asks the household to approve; the asking device chooses [key]. */
data class Ask(val key: String, val title: String, val note: String)

/** A trusted device's decision on the request [key]. */
data class Decide(val key: String, val approve: Boolean)

/** Why a command changed nothing: `taken`, `missing`, or the decision that already stands, `approved` or `declined`. */
data class Refusal(val reason: String)

/** Shared by both commands. An object of its own, so no command waits on this file's other values to initialize. */
private object ApprovalShapes {
    val refusal = Shape(listOf(Field("reason", FieldType.Text)), { value: Refusal -> listOf(value.reason) }, { Refusal(it[0] as String) })

    /** One confirmed sequence orders every request and decision. */
    val tables = listOf("decisions", "requests")
}

/** Any device may ask: members hold `Invoke` on `requests`. */
object AskApproval : Command<Ask, Unit, Refusal>(
    name = "AskApproval",
    handler = "shopping.ask.v1",
    arguments = Shape(
        listOf(Field("key", FieldType.Text), Field("title", FieldType.Text), Field("note", FieldType.Text)),
        { listOf(it.key, it.title, it.note) },
        { Ask(it[0] as String, it[1] as String, it[2] as String) },
    ),
    accepted = Shape.none,
    rejected = ApprovalShapes.refusal,
    invocation = "requests",
    result = "requests",
    resources = ApprovalShapes.tables,
)

/** Only trusted devices decide: only they hold `Invoke` on `decisions`. */
object DecideApproval : Command<Decide, Unit, Refusal>(
    name = "DecideApproval",
    handler = "shopping.decide.v1",
    arguments = Shape(
        listOf(Field("key", FieldType.Text), Field("approve", FieldType.Boolean)),
        { listOf(it.key, it.approve) },
        { Decide(it[0] as String, it[1] as Boolean) },
    ),
    accepted = Shape.none,
    rejected = ApprovalShapes.refusal,
    invocation = "decisions",
    result = "decisions",
    resources = ApprovalShapes.tables,
)

val ApprovalCommands = CommandSet(listOf(AskApproval, DecideApproval))

/** Every executor derives the same `id` for a key: author 1, and a sequence from the key. */
private fun row(vararg values: Any?): List<Any?> = values.toList() + listOf((1L shl 32) + (values[0].hashCode().toLong() and 0x7fffffff) + 1, 0L)

/** A request row is `key`, `title`, `note` and the asking device's command node, then `id` and `row_kind`. */
val askApproval = AskApproval handledBy { context, ask ->
    if (ask.title.isBlank() || context.get("requests", ask.key) != null) {
        Decision.reject(Refusal("taken"))
    } else {
        Decision.accept(Unit, listOf(Write.Put("requests", row(ask.key, ask.title, ask.note, context.requestNode))))
    }
}

/** The first decision on a request stands; a later one, from any device, is rejected with it. */
val decideApproval = DecideApproval handledBy { context, decide ->
    val decided = context.get("decisions", decide.key)
    when {
        context.get("requests", decide.key) == null -> Decision.reject(Refusal("missing"))

        // A decision row is `key`, `approved` (0 or 1) and the deciding device's command node.
        decided != null -> Decision.reject(Refusal(if (decided[1] == 1L) "approved" else "declined"))

        else -> Decision.accept(Unit, listOf(Write.Put("decisions", row(decide.key, decide.approve, context.requestNode))))
    }
}
val ApprovalHandlers = listOf(askApproval, decideApproval)
