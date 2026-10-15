package rocky

import kotlinx.serialization.json.*

internal fun handleBytes(handle: String): JsonArray {
    require(handle.length == 64 && handle.all { it in "0123456789abcdef" }) { "handle must be 32 bytes of lowercase hex" }
    return JsonArray(handle.chunked(2).map { JsonPrimitive(it.toInt(16)) })
}
internal fun cell(value: Any?): JsonObject = buildJsonObject {
    when (value) {
        null -> put("type", "null")

        is Boolean -> {
            put("type", "integer")
            put("value", if (value) "1" else "0")
        }

        is Int -> {
            put("type", "integer")
            put("value", value.toString())
        }

        is Long -> {
            put("type", "integer")
            put("value", value.toString())
        }

        is Double -> {
            require(!value.isNaN()) { "NaN is not a value" }
            // Negative zero has no canonical form.
            val bits = if (value == 0.0) 0L else value.toRawBits()
            put("type", "real")
            put("value", bits.toULong().toString(16).padStart(16, '0'))
        }

        is String -> {
            put("type", "text")
            put("value", value)
        }

        is ByteArray -> {
            put("type", "blob")
            put("value", bytes(value))
        }

        else -> throw IllegalArgumentException("unsupported value type")
    }
}
