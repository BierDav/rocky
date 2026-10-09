package org.example.examples.shopping

import kotlinx.coroutines.flow.Flow
import rocky.GrantProfile
import rocky.RockyDatabase
import rocky.room.moveRockyRow
import rocky.room.settledOnRocky
import kotlin.io.encoding.Base64
import kotlin.uuid.ExperimentalUuidApi
import kotlin.uuid.Uuid

/**
 * Lists, categories and items as plain DAO reads and writes on Rocky's file: each write is one commit and one signed
 * bundle. Only a trusted device creates, renames or deletes lists; any device edits the lists it holds.
 */
class ShoppingRepository(private val database: ShoppingDatabase, private val rocky: RockyDatabase) {
    fun lists(): Flow<List<ShoppingList>> = database.lists().observe().settledOnRocky()

    /** Prepares the new list's group secret, then inserts it; returns the id Rocky issued. */
    suspend fun createList(title: String): Long {
        require(title.isNotBlank())
        return database.lists().create(rocky.partitions.nextId("lists"), title).id
    }
    suspend fun renameList(list: Long, title: String) {
        require(title.isNotBlank())
        database.lists().rename(list, title)
    }

    /** Hides the list's categories and items too. */
    suspend fun deleteList(list: Long) {
        database.lists().delete(list)
    }

    fun categories(list: Long): Flow<List<Category>> = database.categories().observe(list).settledOnRocky()
    suspend fun addCategory(list: Long, name: String): Long {
        require(name.isNotBlank())
        return database.categories().add(list, name).id
    }

    /** Items of the category keep their place in the list; only their category reads null. */
    suspend fun deleteCategory(list: Long, category: Long) {
        database.categories().delete(list, category)
    }

    fun observe(list: Long, search: String = "", includeCompleted: Boolean = true): Flow<List<ShoppingItem>> = database.items().observe(list, search.lowercase(), includeCompleted).settledOnRocky()

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
    suspend fun check(list: Long, id: Long, completed: Boolean) {
        database.items().check(list, id, completed)
    }
    suspend fun categorize(list: Long, id: Long, category: Long?) {
        database.items().categorize(list, id, category)
    }
    suspend fun delete(list: Long, id: Long) {
        database.items().delete(list, id)
    }

    /**
     * Moves an item to another held list in one commit, keeping its id and field stamps; its category reads null,
     * since categories stay in their list. A device that ticked the item offline sees its tick follow it. Retrying
     * with the same [handle] never moves twice.
     */
    suspend fun move(from: Long, id: Long, to: Long, handle: String = RockyDatabase.newHandle()) = database.moveRockyRow(rocky, handle, "items", id, listOf(from), listOf(to))

    /** One envelope per recipient node sharing the whole list; needs a trusted device. */
    suspend fun share(list: Long, recipients: List<String>): Map<String, ByteArray> = rocky.list(list).share(recipients)

    /** Installs a shared list's envelope; a list new to this device starts from a peer's checkpoint. */
    suspend fun accept(list: Long, envelope: ByteArray) = rocky.list(list).accept(envelope)

    /** For drivers without the `keys` table: [share] as text codes `list:<id>:<base64url envelope>`, for [acceptCode]. */
    suspend fun shareCodes(list: Long, devices: List<String>): Map<String, String> = share(list, devices).mapValues { (_, envelope) -> "list:$list:${Base64.UrlSafe.encode(envelope)}" }

    /** [shareCodes] for every other reading device this issuer enrolled; a custody peer relays lists without a key. */
    suspend fun shareWithDevices(list: Long): Map<String, String> {
        val own = rocky.session().node
        return shareCodes(list, rocky.devices().filter { !it.revoked && it.node != own && it.profile !is GrantProfile.Custody }.map { it.node })
    }
    suspend fun acceptCode(code: String): Long {
        val parts = code.trim().split(":")
        require(parts.size == 3 && parts[0] == "list") { "not a shared list code" }
        return parts[1].toLong().also { accept(it, Base64.UrlSafe.decode(parts[2])) }
    }
}
