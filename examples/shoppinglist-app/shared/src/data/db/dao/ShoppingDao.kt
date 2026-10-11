package org.example.examples.shopping.data.db.dao

import androidx.room.*
import kotlinx.coroutines.flow.Flow
import org.example.examples.shopping.data.db.entity.ShoppingItem

@Dao
interface ShoppingDao {
    @Query("SELECT * FROM items WHERE list = :list AND (:includeCompleted OR completed = 0) AND instr(lower(title), :search) > 0 ORDER BY id")
    fun observeAll(list: Long, search: String = "", includeCompleted: Boolean = true): Flow<List<ShoppingItem>>

    @Insert suspend fun insert(item: ShoppingItem)

    /** Rocky reserves the next id of [list] inside the caller's write transaction; a rollback releases it. */
    @SkipQueryVerification
    @Query("SELECT rocky_next_id('items', :list)")
    suspend fun nextId(list: Long): Long

    /** Obtains the id and inserts the item in one transaction. */
    @Transaction suspend fun add(list: Long, key: String, title: String, category: Long? = null): ShoppingItem = ShoppingItem(list, key, title, completed = false, category = category, id = nextId(list)).also { insert(it) }

    @Query("UPDATE items SET title = :title WHERE list = :list AND id = :id")
    suspend fun rename(list: Long, id: Long, title: String): Int

    @Query("UPDATE items SET completed = :completed WHERE list = :list AND id = :id")
    suspend fun check(list: Long, id: Long, completed: Boolean): Int

    @Query("UPDATE items SET category = :category WHERE list = :list AND id = :id")
    suspend fun categorize(list: Long, id: Long, category: Long?): Int

    @Query("DELETE FROM items WHERE list = :list AND id = :id")
    suspend fun delete(list: Long, id: Long): Int
}
