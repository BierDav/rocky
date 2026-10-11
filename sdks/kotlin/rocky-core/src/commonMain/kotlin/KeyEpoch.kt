package rocky

import kotlinx.serialization.json.*

/** Complete encryption identity; greater identities win, with lower hashes winning within a generation. */
data class KeyEpoch(val generation: Long, val secretHash: String) : Comparable<KeyEpoch> {
    init {
        require(generation > 0) { "key generation must be positive" }
        require(secretHash.length == 64 && secretHash.all { it in '0'..'9' || it in 'a'..'f' }) { "key hash must be 32 hexadecimal bytes" }
    }
    override fun compareTo(other: KeyEpoch): Int = generation.compareTo(other.generation).takeUnless { it == 0 } ?: other.secretHash.compareTo(secretHash)
    override fun toString(): String = "$generation.$secretHash"
    internal fun json(): JsonObject = buildJsonObject {
        put("generation", generation)
        put("secret_hash", JsonArray(secretHash.chunked(2).map { JsonPrimitive(it.toInt(16)) }))
    }
    companion object {
        fun fromJson(value: JsonElement): KeyEpoch {
            val epoch = value.jsonObject
            val hash = epoch.getValue("secret_hash").jsonArray.map { it.jsonPrimitive.int }
            require(hash.size == 32 && hash.all { it in 0..255 }) { "key hash must be 32 bytes" }
            return KeyEpoch(epoch.getValue("generation").jsonPrimitive.long, hash.joinToString("") { it.toString(16).padStart(2, '0') })
        }
    }
}
