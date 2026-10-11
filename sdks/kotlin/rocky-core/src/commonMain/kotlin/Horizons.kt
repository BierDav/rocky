package rocky

import kotlinx.serialization.json.*

/** Issuer cuts, durable acknowledgments and finalized checkpoint retention. */
class Horizons internal constructor(private val client: RockyClient) {
    private suspend fun call(table: String, action: String, values: JsonArray? = null, body: JsonObjectBuilder.() -> Unit = {}): JsonObject = client.call(
        request("horizon") {
            put("collection", table)
            put("action", action)
            values?.let { put("values", it) }
            body()
        },
    )

    /** Defaults to retention's required peers. Sync first so their verified credential deadlines are available. */
    suspend fun declare(table: String, participants: Set<String>? = null, values: JsonArray? = null): JsonObject = call(table, "declare", values) {
        participants?.let { put("participants", JsonArray(it.sorted().map(::JsonPrimitive))) }
    }

    /** Canonical issuer-signed statement returned by declare/status, encoded as hex. */
    suspend fun admit(table: String, statement: String, values: JsonArray? = null): JsonObject = call(table, "admit", values) { put("statement", statement) }
    suspend fun status(table: String, values: JsonArray? = null): JsonObject = call(table, "status", values)

    /** Invoke periodically on the issuer; sync supplies cut acknowledgments, then this finalizes and prunes. */
    suspend fun maintain(table: String, maxOperations: Long = 1024, values: JsonArray? = null): JsonObject = call(table, "maintain", values) {
        require(maxOperations > 0)
        put("max_operations", maxOperations)
    }

    /** Refuses until the exact inputs and required acknowledgments or deadlines permit finalization. */
    suspend fun finalize(table: String, values: JsonArray? = null): JsonObject = call(table, "finalize", values)
}
