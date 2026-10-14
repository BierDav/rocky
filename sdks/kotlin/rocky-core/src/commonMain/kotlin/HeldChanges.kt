package rocky

import kotlinx.serialization.json.*

/**
 * Local changes a newer schema holds back because they write what it removed; they were never sent. Show [count] as a
 * warning; a newer application version re-authors each with `RockySession.reauthor` or drops it. See
 * `docs/runtime/held-changes.md`.
 */
data class HeldChanges(val changes: List<HeldChange>) {
    val count: Int get() = changes.size
    internal companion object {
        fun decode(result: JsonObject) = HeldChanges(result.getValue("changes").jsonArray.map { change(it.jsonObject) })
    }
}

/** One held transaction: its rows as it wrote them, made at [timeMicros]. */
data class HeldChange(val id: String, val timeMicros: Long, val rows: List<HeldRow>)

enum class HeldKind { Insert, Update, Delete }

/** A row a held change writes, named as its schema version named it; [removed] when the table is gone. */
data class HeldRow(val table: String, val removed: Boolean, val row: String, val kind: HeldKind, val fields: List<HeldField>)

/** A column of the held row. [current] is what re-authoring a [written], kept column replaces. */
data class HeldField(val name: String, val value: Any?, val written: Boolean, val removed: Boolean, val current: CurrentValue?)

/** A stored value and its last change: when, in microseconds, and the key that signed it. */
data class CurrentValue(val value: Any?, val timeMicros: Long, val author: String)

private fun change(change: JsonObject) = HeldChange(
    change.getValue("id").jsonPrimitive.content,
    change.getValue("time").jsonPrimitive.long,
    change.getValue("rows").jsonArray.map { element ->
        val row = element.jsonObject
        HeldRow(
            row.getValue("table").jsonPrimitive.content,
            row.getValue("removed").jsonPrimitive.boolean,
            row.getValue("row").jsonPrimitive.content,
            HeldKind.valueOf(row.getValue("kind").jsonPrimitive.content.replaceFirstChar { it.uppercase() }),
            row.getValue("fields").jsonArray.map { field(it.jsonObject) },
        )
    },
)

private fun field(field: JsonObject) = HeldField(
    field.getValue("name").jsonPrimitive.content,
    value(field.getValue("value").jsonObject),
    field.getValue("written").jsonPrimitive.boolean,
    field.getValue("removed").jsonPrimitive.boolean,
    (field["current"] as? JsonObject)?.let { current ->
        CurrentValue(
            (current["value"] as? JsonObject)?.let(::value),
            current.getValue("time").jsonPrimitive.long,
            current.getValue("author").jsonPrimitive.content,
        )
    },
)

/** A tagged cell of the call contract as a Kotlin value. */
private fun value(cell: JsonObject): Any? {
    val value = cell["value"]
    return when (cell.getValue("type").jsonPrimitive.content) {
        "null" -> null
        "integer" -> value!!.jsonPrimitive.content.toLong()
        "real" -> Double.fromBits(value!!.jsonPrimitive.content.toULong(16).toLong())
        "text" -> value!!.jsonPrimitive.content
        else -> value!!.jsonArray.map { it.jsonPrimitive.int.toByte() }.toByteArray()
    }
}
