package rocky

import kotlinx.serialization.json.*

/** A query result with SQLite value types. Accessors refuse a different type instead of coercing it. */
class SqlRow internal constructor(private val values: List<Any?>) {
    val size: Int get() = values.size
    operator fun get(index: Int): Any? = values[index]
    fun long(index: Int): Long = values[index] as Long
    fun text(index: Int): String = values[index] as String
    fun boolean(index: Int): Boolean = when (long(index)) {
        0L -> false
        1L -> true
        else -> throw IllegalArgumentException("Boolean cell must be 0 or 1")
    }

    internal companion object {
        fun decode(row: JsonElement): SqlRow = SqlRow(
            row.jsonArray.map { value ->
                val cell = value.jsonObject
                when (val type = cell.getValue("type").jsonPrimitive.content) {
                    "null" -> null
                    "integer" -> cell.getValue("value").jsonPrimitive.content.toLong()
                    "text" -> cell.getValue("value").jsonPrimitive.content
                    "real" -> Double.fromBits(cell.getValue("value").jsonPrimitive.content.toULong(16).toLong())
                    "blob" -> cell.getValue("value").jsonArray.map { it.jsonPrimitive.int.toByte() }.toByteArray()
                    else -> throw IllegalArgumentException("Unknown SQL cell type $type")
                }
            },
        )
    }
}
