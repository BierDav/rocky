package org.example.examples.shopping

import kotlin.io.encoding.Base64
import kotlin.io.encoding.ExperimentalEncodingApi
import kotlin.uuid.ExperimentalUuidApi
import kotlin.uuid.Uuid
import kotlinx.coroutines.flow.Flow
import kotlinx.serialization.json.*
import rocky.*

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
internal fun listPartition(list: Long): JsonArray = JsonArray(listOf(buildJsonObject { put("type", "integer"); put("value", list.toString()) }))

/**
 * Plain DAO reads and writes on Rocky's file: each write is one commit and one signed bundle. The device needs its
 * authors for the three tables, see [installShoppingAuthor].
 */
class ShoppingRepository(private val database: ShoppingDatabase, private val runtime: RockyDatabase) {
    fun lists(): Flow<List<ShoppingList>> = database.lists().observe()
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
    /** [share] with every other enrolled device, as codes for [acceptCode]: `list:<id>:<envelope>`. */
    @OptIn(ExperimentalEncodingApi::class)
    suspend fun shareWithDevices(list: Long): Map<String, String> {
        val own = runtime.session().node
        val devices = runtime.devices().filter { !it.revoked && it.node != own }.map { it.node }
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

    fun categories(list: Long): Flow<List<Category>> = database.categories().observe(list)
    suspend fun addCategory(list: Long, name: String): Long {
        require(name.isNotBlank())
        return database.categories().add(list, name).id
    }
    /** Items of the category keep their place in the list; only their category reads null. */
    suspend fun deleteCategory(list: Long, category: Long) { database.categories().delete(list, category) }

    fun observe(list: Long, search: String = "", includeCompleted: Boolean = true): Flow<List<ShoppingItem>> =
        database.items().observe(list, search.lowercase(), includeCompleted)
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
}
