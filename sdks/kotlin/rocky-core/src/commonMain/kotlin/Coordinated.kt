package rocky

import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*

/**
 * Statements of one coordinated write. Reads see this device's committed state, which holds every committed entry of
 * the domain's coordinated log, and the transaction's own writes. See `docs/runtime/coordinated-writes.md`.
 */
interface CoordinatedSql : SqlWrite {
    /** The rows of one statement, each cell as the JSON contract encodes it. */
    suspend fun query(sql: String, vararg values: Any?): JsonArray
}

/** A committed coordinated write: its log position and entry hash, and what the block returned. */
data class CoordinatedCommit<T>(val position: Long, val hash: String, val value: T)

/** This device's progress on its domain's coordinated log. */
data class CoordinatedStatus(val trusted: Boolean, val position: Long, val undecided: Int)

/**
 * Runs `block` as one coordinated write under `handle`. A failure is a [RockyException]: `conflict`, `aborted`,
 * `behind` and `undecided` are retried with a new handle, `below_minimum` and `coordinator_unavailable` wait for the
 * in-sync set or the coordinator; nothing failed is ever reported committed.
 */
internal suspend fun <T> RockyClient.coordinated(handle: JsonArray, block: suspend CoordinatedSql.() -> T): CoordinatedCommit<T> {
    val transaction = object : CoordinatedSql {
        override suspend fun nextId(table: String): Long = nextId(table).toLong()
        override suspend fun nextId(table: String, vararg partitionValues: Any?): Long = nextId(table, JsonArray(partitionValues.map(::cell))).toLong()
        override suspend fun execute(sql: String, vararg values: Any?): Long = sql(sql, JsonArray(values.map(::cell))).getValue("changes").jsonPrimitive.content.toLong()
        override suspend fun query(sql: String, vararg values: Any?): JsonArray = sql(sql, JsonArray(values.map(::cell))).getValue("rows").jsonArray
    }
    call(
        request("sql") {
            put("sql", "BEGIN")
            put("coordinated", true)
        },
    )
    val value = try {
        transaction.block()
    } catch (error: Throwable) {
        // Fails harmlessly when the transaction already ended.
        withContext(NonCancellable) { runCatching { sql("ROLLBACK") } }
        throw error
    }
    val committed = call(
        request("sql") {
            put("sql", "COMMIT")
            put("handle", handle)
            put("coordinated", true)
        },
    ).getValue("committed").jsonObject
    return CoordinatedCommit(committed.getValue("position").jsonPrimitive.content.toLong(), committed.getValue("hash").jsonPrimitive.content, value)
}

internal suspend fun RockyClient.coordinatedStatus(): CoordinatedStatus = call(request("coordinated")).let {
    CoordinatedStatus(it.getValue("trusted").jsonPrimitive.boolean, it.getValue("position").jsonPrimitive.content.toLong(), it.getValue("undecided").jsonPrimitive.int)
}
