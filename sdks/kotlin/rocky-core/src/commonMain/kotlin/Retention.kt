package rocky

import kotlinx.serialization.json.*

/** Peer references are opaque receipt identities, distinct from device keys. At least one copy is always required. */
data class RetentionPolicy(val required: Set<String> = emptySet(), val behind: Set<String> = emptySet(), val copies: Int = 1, val window: Long = 0) {
    init {
        require(copies >= 0 && window >= 0)
    }
    internal fun json(): JsonObject = buildJsonObject {
        put("required", JsonArray(required.sorted().map(::JsonPrimitive)))
        put("behind", JsonArray(behind.sorted().map(::JsonPrimitive)))
        put("copies", copies)
        put("window", window)
    }
    internal companion object {
        fun decode(value: JsonObject) = RetentionPolicy(
            value.getValue("required").jsonArray.map { it.jsonPrimitive.content }.toSet(),
            value.getValue("behind").jsonArray.map { it.jsonPrimitive.content }.toSet(),
            value.getValue("copies").jsonPrimitive.int,
            value.getValue("window").jsonPrimitive.long,
        )
    }
}

data class RetentionStatus(val policy: RetentionPolicy, val covered: Long, val uncovered: Long, val maintenanceError: String?) {
    internal companion object {
        fun decode(value: JsonObject) = RetentionStatus(
            RetentionPolicy.decode(value.getValue("policy").jsonObject),
            value.getValue("covered").jsonPrimitive.long,
            value.getValue("uncovered").jsonPrimitive.long,
            value["maintenance_error"]?.jsonPrimitive?.contentOrNull,
        )
    }
}

data class RetentionLimits(val hardBytes: Long, val handles: Int = 65_536) {
    init {
        require(hardBytes > 0 && handles > 0)
    }
    internal fun json() = buildJsonObject {
        put("hard_bytes", hardBytes)
        put("handles", handles)
    }
    internal companion object {
        fun decode(value: JsonObject) = RetentionLimits(value.getValue("hard_bytes").jsonPrimitive.long, value.getValue("handles").jsonPrimitive.int)
    }
}
