package rocky

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.merge
import kotlinx.serialization.json.*

/**
 * Group partition lifecycle. A group is named by `partitionBy(..., group = ...)`, or by the
 * table's name when omitted; values fill its slots in `partitionBy` declaration order.
 * One secret per group partition and epoch derives every member table's key.
 */
class Partitions internal constructor(private val client: RockyClient) {
    // Local key delivery metadata can change without changing the public database status.
    internal fun changes(): Flow<Unit> = merge(
        client.statusChanges().map { Unit },
        client.activity().filter { it is RockyEvent.PartitionChanged || it is RockyEvent.EnvelopesDelivered || it is RockyEvent.Renewed }.map { Unit },
    ).conflate()

    /** Current state in [groups] order, sharing status and activity waits across all groups. */
    fun observeStatuses(groups: List<GroupPartition>): Flow<List<GroupStatus>> = changes().map {
        groups.map { it.status() }
    }.retryUnavailableReads().distinctUntilChanged()

    private suspend fun byGroup(type: String, group: String, values: JsonArray, body: JsonObjectBuilder.() -> Unit = {}): JsonObject = client.call(
        request(type) {
            put("group", group)
            put("values", values)
            body()
        },
    )
    private suspend fun byTable(type: String, table: String, values: JsonArray): JsonObject = client.call(
        request(type) {
            put("table", table)
            put("values", values)
        },
    )
    private fun JsonObjectBuilder.putOmit(tables: List<String>) {
        put("omit", JsonArray(tables.map(::JsonPrimitive)))
    }
    suspend fun grant(device: JsonObject, grants: JsonArray, delegations: Int = 1): JsonObject = client.call(
        request("partition_grant") {
            put("device", device)
            put("partitions", grants)
            put("delegations", delegations)
        },
    )
    suspend fun delegate(device: JsonObject, grants: JsonArray, delegations: Int = 1): JsonObject = client.call(
        request("partition_delegate") {
            put("device", device)
            put("partitions", grants)
            put("delegations", delegations)
        },
    )
    suspend fun role(partial: Boolean, fullNodes: JsonArray = JsonArray(emptyList()), custody: Int = 0) {
        client.call(
            request("partition_role") {
                put(
                    "role",
                    buildJsonObject {
                        put("role", if (partial) "Partial" else "Full")
                        put("full_nodes", fullNodes)
                        put("custody", custody)
                    },
                )
            },
        )
    }

    /**
     * A full replica holds every partition it receives; a partial one only those it subscribes to, and may evict them.
     * [fullNodes] names peers trusted to hold everything; [custody] of them must apply history before it is pruned.
     */
    suspend fun role(role: ReplicaRole, fullNodes: Collection<String> = emptyList(), custody: Int = 0) = role(role == ReplicaRole.Partial, JsonArray(fullNodes.map(::JsonPrimitive)), custody)

    /** Prepare the next own-id partition's group secret before starting the inserting transaction. */
    suspend fun next(table: String): JsonObject = client.call(request("partition_next") { put("table", table) })

    /**
     * Prepares the group secret of [table]'s next own-id partition on an issuer and returns that id: insert the row
     * with it in the next transaction, where `nextId(table)` must return the same id.
     */
    suspend fun nextId(table: String): Long = next(table).getValue("id").jsonPrimitive.content.toLong()

    /** One partition of [group], such as one shopping list; [values] fill the group's slots in `partitionBy` order. */
    fun group(group: String, vararg values: Any?): GroupPartition = GroupPartition(this, group, JsonArray(values.map(::cell)))

    /** One table's scope and tuple, with its group address. */
    suspend fun address(table: String, values: JsonArray): JsonObject = byTable("partition_address", table, values)
    suspend fun complete(table: String, values: JsonArray): Boolean = byTable("partition_complete", table, values).getValue("complete").jsonPrimitive.boolean

    /** On an issuer: prepares the group secret at the current epoch. */
    suspend fun create(group: String, values: JsonArray): JsonObject = byGroup("partition_create", group, values)

    /**
     * On a whole-group reader: saves one envelope of the current epoch per recipient node key, which sync delivers to that
     * device alone, directly or through a mailbox; a revoked recipient is refused.
     */
    suspend fun share(group: String, values: JsonArray, recipients: JsonArray, epoch: KeyEpoch? = null): JsonObject = byGroup("partition_share", group, values) {
        put("recipients", recipients)
        epoch?.let { put("epoch", it.json()) }
    }

    /** Registers the addresses without a key, as a custody node. */
    suspend fun register(group: String, values: JsonArray, omit: List<String> = emptyList()): JsonObject = byGroup("partition_register", group, values) { putOmit(omit) }

    /** Holds the group partition except the tables left out; a table a held one references cannot be left out. */
    suspend fun subscribe(group: String, values: JsonArray, omit: List<String> = emptyList()) {
        byGroup("partition_subscribe", group, values) { putOmit(omit) }
    }

    /** Subscribes and records checkpoint-first intent for the next sync. */
    suspend fun bootstrap(group: String, values: JsonArray, omit: List<String> = emptyList()): JsonObject = byGroup("partition_bootstrap", group, values) { putOmit(omit) }

    /** Stops following every table except those kept; a kept table keeps what it references. */
    suspend fun unsubscribe(group: String, values: JsonArray, keep: List<String> = emptyList()) {
        byGroup("partition_unsubscribe", group, values) { putOmit(keep) }
    }
    suspend fun evict(group: String, values: JsonArray, keep: List<String> = emptyList()): JsonObject = byGroup("partition_evict", group, values) { putOmit(keep) }

    /** Held secret epochs, each member table's key epochs and holding, and on the sharer each recipient's delivery. */
    suspend fun status(group: String, values: JsonArray): JsonObject = byGroup("partition_status", group, values)
}

enum class ReplicaRole { Full, Partial }

/**
 * One partition of a group: one secret derives the key of every member table, so one envelope shares all of them.
 * See `docs/runtime/partitioned-scopes.md#group-secrets`.
 */
class GroupPartition internal constructor(private val partitions: Partitions, val group: String, val values: JsonArray) {
    /**
     * On a whole-group reader: saves delivery of the current epoch to each recipient and returns the epoch. Sync delivers each envelope to
     * its recipient alone, which installs it; a partition it does not hold yet starts from a peer's checkpoint.
     */
    suspend fun share(recipients: Collection<String>): KeyEpoch? {
        if (recipients.isEmpty()) return null
        return KeyEpoch.fromJson(partitions.share(group, values, JsonArray(recipients.map(::JsonPrimitive))).getValue("epoch"))
    }

    /** The secret epochs held here and how each member table is held. */
    suspend fun status(): GroupStatus = partitions.status(group, values).let { status ->
        GroupStatus(
            status.getValue("epochs").jsonArray.map(KeyEpoch::fromJson),
            status.getValue("tables").jsonArray.map { it.jsonObject }.map { table ->
                TableHolding(
                    table.getValue("table").jsonPrimitive.content,
                    table.getValue("partition").jsonPrimitive.content,
                    table["holding"]?.jsonPrimitive?.contentOrNull,
                    table.getValue("epochs").jsonArray.map(KeyEpoch::fromJson),
                    table["current"]?.takeUnless { it is JsonNull }?.let(KeyEpoch::fromJson),
                )
            },
            status["current"]?.takeUnless { it is JsonNull }?.let(KeyEpoch::fromJson),
            status["recipients"]?.jsonArray.orEmpty().map { it.jsonObject }.map { recipient ->
                RecipientDelivery(
                    recipient.getValue("node").jsonPrimitive.content,
                    KeyEpoch.fromJson(recipient.getValue("epoch")),
                    DeliveryState.valueOf(recipient.getValue("state").jsonPrimitive.content.replaceFirstChar(Char::uppercase)),
                )
            },
        )
    }
    fun observeStatus(): Flow<GroupStatus> = partitions.changes().map { status() }.retryUnavailableReads().distinctUntilChanged()

    suspend fun subscribe(omit: List<String> = emptyList()) = partitions.subscribe(group, values, omit)
    suspend fun unsubscribe(keep: List<String> = emptyList()) = partitions.unsubscribe(group, values, keep)
    suspend fun evict(keep: List<String> = emptyList()): JsonObject = partitions.evict(group, values, keep)
}

/**
 * A group partition's held secret [epochs] and its member [tables]. [recipients] lists the envelopes this device
 * sent, which only the sharer knows.
 */
data class GroupStatus(val epochs: List<KeyEpoch>, val tables: List<TableHolding>, val current: KeyEpoch?, val recipients: List<RecipientDelivery> = emptyList()) {
    /** Every member table is held here. */
    val isHeld: Boolean get() = tables.all { it.holding == "held" }

    /** This device holds a secret, so it reads the partition. */
    val isKeyed: Boolean get() = epochs.isNotEmpty()
}

/**
 * How one member table of a group partition is held: `held`, `left`, `evicting`, `evicted`, or null when never
 * subscribed. [partition] is the id `RockyEvent` scopes name it by; [keyEpochs] are the table key epochs held.
 */
data class TableHolding(val table: String, val partition: String, val holding: String?, val keyEpochs: List<KeyEpoch>, val current: KeyEpoch?)

/** How far one envelope this device shared travelled: still here, held by a mailbox, or acknowledged by [node]. */
data class RecipientDelivery(val node: String, val epoch: KeyEpoch, val state: DeliveryState)

enum class DeliveryState { Pending, Deposited, Delivered }
