package org.example.examples.shopping.data

import kotlinx.coroutines.flow.Flow
import org.example.examples.shopping.data.db.ShoppingDatabase
import org.example.examples.shopping.data.db.entity.Category
import org.example.examples.shopping.data.db.entity.ShoppingItem
import org.example.examples.shopping.data.db.entity.ShoppingList
import org.example.examples.shopping.data.db.list
import rocky.GrantProfile
import rocky.RecipientDelivery
import rocky.RockyDatabase
import rocky.room.moveRockyRow
import rocky.room.settledOnRocky
import kotlin.uuid.ExperimentalUuidApi
import kotlin.uuid.Uuid

/**
 * Lists, categories and items as plain DAO reads and writes on Rocky's file: each write is one commit and one signed
 * bundle. Only a trusted device creates, renames or deletes lists; any device edits the lists it holds.
 */
class ShoppingRepository(private val database: ShoppingDatabase, private val rocky: RockyDatabase) {
    fun lists(): Flow<List<ShoppingList>> = database.lists().observeAll().settledOnRocky()

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

    fun observe(list: Long, search: String = "", includeCompleted: Boolean = true): Flow<List<ShoppingItem>> = database.items().observeAll(list, search.lowercase(), includeCompleted).settledOnRocky()

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

    /**
     * Shares a held list with [recipients] by node; members choose a discovered peer. Sync carries each envelope to its device
     * alone, directly or through the custody peer, which installs it and starts a list new to it from a checkpoint.
     */
    suspend fun share(list: Long, recipients: List<String>) {
        rocky.list(list).share(recipients)
    }

    /** Shares [list] with every other reading device in the issuer registry, and returns them; custody holds no key. */
    suspend fun shareWithDevices(list: Long): List<String> {
        val own = rocky.session().node
        val devices = rocky.devices().filter { !it.revoked && it.node != own && it.profile !is GrantProfile.Custody }.map { it.node }
        share(list, devices)
        return devices
    }

    /** How far each envelope of [list] this device shared travelled; no other device knows. */
    suspend fun deliveries(list: Long): List<RecipientDelivery> = rocky.list(list).status().recipients
}
