package rocky

import kotlinx.serialization.json.*

/** Concrete scope/key lifecycle; values are in the schema's partition column order. */
class Partitions internal constructor(private val client: RockyClient) {
    private suspend fun call(type: String, table: String, values: JsonArray, body: JsonObjectBuilder.() -> Unit = {}): JsonObject =
        client.call(request(type) { put("table", table); put("values", values); body() })
    suspend fun grant(device: JsonObject, grants: JsonArray, delegations: Int = 1): JsonObject =
        client.call(request("partition_grant") { put("device", device); put("partitions", grants); put("delegations", delegations) })
    suspend fun delegate(device: JsonObject, grants: JsonArray, delegations: Int = 1): JsonObject =
        client.call(request("partition_delegate") { put("device", device); put("partitions", grants); put("delegations", delegations) })
    suspend fun role(partial: Boolean, fullNodes: JsonArray = JsonArray(emptyList()), custody: Int = 0) {
        client.call(request("partition_role") { put("role", buildJsonObject { put("role", if (partial) "Partial" else "Full"); put("full_nodes", fullNodes); put("custody", custody) }) })
    }
    /** Prepare the next own-id partition key before starting the inserting transaction. */
    suspend fun next(table: String): JsonObject = client.call(request("partition_next") { put("table", table) })
    suspend fun address(table: String, values: JsonArray): JsonObject = call("partition_address", table, values)
    suspend fun create(table: String, values: JsonArray, epoch: Long = 1): JsonObject = call("partition_create", table, values) { put("epoch", epoch) }
    suspend fun register(table: String, values: JsonArray): JsonObject = call("partition_register", table, values)
    suspend fun install(table: String, values: JsonArray, envelope: ByteArray, epoch: Long = 1): JsonObject =
        call("partition_install", table, values) { put("envelope", bytes(envelope)); put("epoch", epoch) }
    suspend fun envelopes(table: String, values: JsonArray, recipients: JsonArray, epoch: Long = 1): JsonObject =
        call("partition_envelopes", table, values) { put("recipients", recipients); put("epoch", epoch) }
    suspend fun subscribe(table: String, values: JsonArray) { call("partition_subscribe", table, values) }
    suspend fun unsubscribe(table: String, values: JsonArray) { call("partition_unsubscribe", table, values) }
    suspend fun evict(table: String, values: JsonArray): JsonObject = call("partition_evict", table, values)
    suspend fun complete(table: String, values: JsonArray): Boolean = call("partition_complete", table, values).getValue("complete").jsonPrimitive.boolean
}
