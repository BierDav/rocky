package org.example.examples.shopping

import kotlin.io.encoding.Base64
import kotlin.io.encoding.ExperimentalEncodingApi
import kotlin.uuid.ExperimentalUuidApi
import kotlin.uuid.Uuid
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.retryWhen
import kotlinx.serialization.json.*
import rocky.*
import rocky.room.moveRockyRow

/**
 * [ShoppingDatabase]'s tables as Rocky replicates them; importing its Room export with [shoppingRoomOverlay] yields
 * the same definition. `id` and `row_kind` are implicit. A list, its categories and items form the `list` group:
 * one secret per list derives every table's key, so one envelope shares the whole list.
 */
val ShoppingSchema = schema("shopping", version = 3) {
    table("lists") { text("title"); primaryKey("id"); partitionBy("id", group = "list") }
    table("categories") {
        integer("list"); text("name")
        primaryKey("list", "id"); partitionBy("list", group = "list")
        reference("list", "lists", onDelete = Delete.Cascade)
    }
    table("items") {
        integer("list"); text("key"); text("title"); boolean("completed"); integer("category", nullable = true)
        primaryKey("list", "key"); partitionBy("list", group = "list")
        unique("list", "title", onConflict = Conflict.LatestWins)
        index("index_items_list_category", "list", "category")
        reference("list", "lists", onDelete = Delete.Cascade)
        reference(listOf("list", "category"), "categories", onDelete = Delete.SetNull)
    }
}

/** The Rocky attributes a Room export cannot hold: the group and the title policy. */
val shoppingRoomOverlay: SchemaBuilder.() -> Unit = {
    table("lists") { partitionBy("id", group = "list") }
    table("categories") { partitionBy("list", group = "list") }
    table("items") { partitionBy("list", group = "list"); onConflict("index_items_list_title", Conflict.LatestWins) }
}

/** The `list` group partition of [list], as partition requests name it. */
fun listPartition(list: Long): JsonArray = JsonArray(listOf(buildJsonObject { put("type", "integer"); put("value", list.toString()) }))

/**
 * Plain DAO reads and writes on Rocky's file: each write is one commit and one signed bundle. The device needs its
 * authors for the three tables, see [installShoppingAuthor].
 */
class ShoppingRepository(private val database: ShoppingDatabase, private val runtime: RockyDatabase) {
    fun lists(): Flow<List<ShoppingList>> = database.lists().observe().settled()
    /** Prepares the new list's group secret, then inserts it; returns the id Rocky issued. */
    suspend fun createList(title: String): Long {
        require(title.isNotBlank())
        val prepared = runtime.partitions.next("lists").getValue("id").jsonPrimitive.content.toLong()
        return database.lists().create(prepared, title).id
    }
    suspend fun renameList(list: Long, title: String) {
        require(title.isNotBlank())
        database.lists().rename(list, title)
    }
    /** Hides the list's categories and items too. */
    suspend fun deleteList(list: Long) { database.lists().delete(list) }
    /**
     * One call shares the whole list, categories and items included, with each recipient device node: an envelope
     * per recipient, which the application delivers and the recipient passes to [accept]. Needs an issuer.
     */
    suspend fun share(list: Long, recipients: List<String>): Map<String, ByteArray> =
        runtime.partitions.share("list", listPartition(list), JsonArray(recipients.map(::JsonPrimitive)))
            .getValue("envelopes").jsonArray.associate { shared ->
                val envelope = shared.jsonObject
                envelope.getValue("recipient").jsonPrimitive.content to
                    envelope.getValue("envelope").jsonArray.map { it.jsonPrimitive.int.toByte() }.toByteArray()
            }
    /**
     * Installs a shared list's secret. A list the device does not hold yet starts from a peer's checkpoint on the
     * next sync, so the device needs no earlier key epoch or author; a held list only gains the new epoch.
     */
    suspend fun accept(list: Long, envelope: ByteArray) {
        val partition = listPartition(list)
        val held = runtime.partitions.status("list", partition).getValue("tables").jsonArray
            .all { it.jsonObject["holding"]?.jsonPrimitive?.contentOrNull == "held" }
        runtime.partitions.install("list", partition, envelope)
        if (!held) runtime.partitions.bootstrap("list", partition)
    }
    /** [share] with every other reading device this issuer enrolled, as codes for [acceptCode]. */
    suspend fun shareWithDevices(list: Long): Map<String, String> {
        val own = runtime.session().node
        // A custody peer holds no list key: it relays the list without an envelope.
        val devices = runtime.devices().filter { !it.revoked && it.node != own && it.profile !is GrantProfile.Custody }.map { it.node }
        return shareCodes(list, devices)
    }
    /** [share] with [devices], as codes for [acceptCode]: `list:<id>:<envelope>`. */
    @OptIn(ExperimentalEncodingApi::class)
    suspend fun shareCodes(list: Long, devices: List<String>): Map<String, String> {
        if (devices.isEmpty()) return emptyMap()
        return share(list, devices).mapValues { (_, envelope) -> "list:$list:${Base64.UrlSafe.encode(envelope)}" }
    }
    @OptIn(ExperimentalEncodingApi::class)
    suspend fun acceptCode(code: String): Long {
        val parts = code.trim().split(":")
        require(parts.size == 3 && parts[0] == "list") { "not a shared list code" }
        val list = parts[1].toLong()
        accept(list, Base64.UrlSafe.decode(parts[2]))
        return list
    }

    fun categories(list: Long): Flow<List<Category>> = database.categories().observe(list).settled()
    suspend fun addCategory(list: Long, name: String): Long {
        require(name.isNotBlank())
        return database.categories().add(list, name).id
    }
    /** Items of the category keep their place in the list; only their category reads null. */
    suspend fun deleteCategory(list: Long, category: Long) { database.categories().delete(list, category) }

    fun observe(list: Long, search: String = "", includeCompleted: Boolean = true): Flow<List<ShoppingItem>> =
        database.items().observe(list, search.lowercase(), includeCompleted).settled()
    /** Returns the id Rocky issued in [list]. */
    @OptIn(ExperimentalUuidApi::class)
    suspend fun add(list: Long, title: String, category: Long? = null): Long {
        require(title.isNotBlank())
        return database.items().add(list, Uuid.random().toString(), title, category).id
    }
    suspend fun rename(list: Long, id: Long, title: String) {
        require(title.isNotBlank())
        database.items().rename(list, id, title)
    }
    suspend fun check(list: Long, id: Long, completed: Boolean) { database.items().check(list, id, completed) }
    suspend fun categorize(list: Long, id: Long, category: Long?) { database.items().categorize(list, id, category) }
    suspend fun delete(list: Long, id: Long) { database.items().delete(list, id) }
    /**
     * Moves an item to another held list in one commit, keeping its id and field stamps; its category reads null,
     * since categories stay in their list. A device that ticked the item offline sees its tick follow it. Retrying
     * with the same [handle] never moves twice.
     */
    suspend fun move(from: Long, id: Long, to: Long, handle: String = RockyDatabase.newHandle()) =
        database.moveRockyRow(runtime, handle, "items", id, listOf(from), listOf(to))
}

/**
 * A read transaction fails while sync applies a bundle it received in part, after an authority change such as a
 * newly installed list secret, and while a list is unavailable. Observing again keeps a screen on the last
 * committed rows instead of ending its flow; see docs/runtime/room-integration.md#observation.
 */
private fun <T> Flow<T>.settled(): Flow<T> = retryWhen { cause, attempt ->
    val message = cause.message.orEmpty()
    val transient = TRANSIENT_READS.any { it in message }
    if (transient) delay(minOf(50L shl minOf(attempt, 4L).toInt(), 1_000L))
    transient
}
private val TRANSIENT_READS = listOf("held partition is incomplete", "session authority changed", " is not available: ")
