package rocky

import kotlinx.serialization.json.*

/**
 * Group partition lifecycle. A group is named by `partitionBy(..., group = ...)`, or by the
 * table's logical name when omitted; values fill its slots in `partitionBy` declaration order.
 * One secret per group partition and epoch derives every member table's key.
 */
class Partitions internal constructor(private val client: RockyClient) {
    private suspend fun byGroup(type: String, group: String, values: JsonArray, body: JsonObjectBuilder.() -> Unit = {}): JsonObject =
        client.call(request(type) { put("group", group); put("values", values); body() })
    private suspend fun byTable(type: String, table: String, values: JsonArray): JsonObject =
        client.call(request(type) { put("table", table); put("values", values) })
    private fun JsonObjectBuilder.putOmit(tables: List<String>) { put("omit", JsonArray(tables.map(::JsonPrimitive))) }
    suspend fun grant(device: JsonObject, grants: JsonArray, delegations: Int = 1): JsonObject =
        client.call(request("partition_grant") { put("device", device); put("partitions", grants); put("delegations", delegations) })
    suspend fun delegate(device: JsonObject, grants: JsonArray, delegations: Int = 1): JsonObject =
        client.call(request("partition_delegate") { put("device", device); put("partitions", grants); put("delegations", delegations) })
    suspend fun role(partial: Boolean, fullNodes: JsonArray = JsonArray(emptyList()), custody: Int = 0) {
        client.call(request("partition_role") { put("role", buildJsonObject { put("role", if (partial) "Partial" else "Full"); put("full_nodes", fullNodes); put("custody", custody) }) })
    }
    /** Prepare the next own-id partition's group secret before starting the inserting transaction. */
    suspend fun next(table: String): JsonObject = client.call(request("partition_next") { put("table", table) })
    /** One table's scope and tuple, with its group address. */
    suspend fun address(table: String, values: JsonArray): JsonObject = byTable("partition_address", table, values)
    suspend fun complete(table: String, values: JsonArray): Boolean = byTable("partition_complete", table, values).getValue("complete").jsonPrimitive.boolean
    /** On an issuer: prepares the group secret at the current epoch. */
    suspend fun create(group: String, values: JsonArray): JsonObject = byGroup("partition_create", group, values)
    /** On an issuer: one envelope of the current epoch per recipient node key; a revoked recipient is refused. */
    suspend fun share(group: String, values: JsonArray, recipients: JsonArray, epoch: Long? = null): JsonObject =
        byGroup("partition_share", group, values) { put("recipients", recipients); epoch?.let { put("epoch", it) } }
    /** Requires whole-partition read on every table of the group. */
    suspend fun install(group: String, values: JsonArray, envelope: ByteArray, epoch: Long? = null): JsonObject =
        byGroup("partition_install", group, values) { put("envelope", bytes(envelope)); epoch?.let { put("epoch", it) } }
    /** Registers the addresses without a key, as a custody node. */
    suspend fun register(group: String, values: JsonArray, omit: List<String> = emptyList()): JsonObject =
        byGroup("partition_register", group, values) { putOmit(omit) }
    /** Holds the group partition except the tables left out; a table a held one references cannot be left out. */
    suspend fun subscribe(group: String, values: JsonArray, omit: List<String> = emptyList()) { byGroup("partition_subscribe", group, values) { putOmit(omit) } }
    /** Subscribes and records checkpoint-first intent for the next sync. */
    suspend fun bootstrap(group: String, values: JsonArray, omit: List<String> = emptyList()): JsonObject =
        byGroup("partition_bootstrap", group, values) { putOmit(omit) }
    /** Stops following every table except those kept; a kept table keeps what it references. */
    suspend fun unsubscribe(group: String, values: JsonArray, keep: List<String> = emptyList()) { byGroup("partition_unsubscribe", group, values) { putOmit(keep) } }
    suspend fun evict(group: String, values: JsonArray, keep: List<String> = emptyList()): JsonObject =
        byGroup("partition_evict", group, values) { putOmit(keep) }
    /** Held secret epochs and each member table's key epochs and holding. */
    suspend fun status(group: String, values: JsonArray): JsonObject = byGroup("partition_status", group, values)
}
