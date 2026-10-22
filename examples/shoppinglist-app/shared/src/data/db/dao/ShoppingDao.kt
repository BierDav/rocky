package org.example.examples.shopping.data.db.dao

import androidx.room.*
import kotlinx.coroutines.flow.Flow
import org.example.examples.shopping.data.Positions
import org.example.examples.shopping.data.db.entity.ItemCategory
import org.example.examples.shopping.data.db.entity.ShoppingItem

@Dao
interface ShoppingDao {
    @Query("SELECT * FROM items WHERE list = :list AND (:includeCompleted OR completed = 0) AND instr(lower(title), :search) > 0 ORDER BY position, id")
    fun observeAll(list: Long, search: String = "", includeCompleted: Boolean = true): Flow<List<ShoppingItem>>

    /** The items once, in list order, for a read-only view of a seal. */
    @Query("SELECT * FROM items WHERE list = :list ORDER BY position, id")
    suspend fun all(list: Long): List<ShoppingItem>

    @Insert suspend fun insert(item: ShoppingItem)

    /** Rocky reserves the next id of [list] inside the caller's write transaction; a rollback releases it. */
    @SkipQueryVerification
    @Query("SELECT rocky_next_id('items', :list)")
    suspend fun nextId(list: Long): Long

    @Query("SELECT max(position) FROM items WHERE list = :list")
    suspend fun lastPosition(list: Long): String?

    @Insert suspend fun insertLink(link: ItemCategory)

    @SkipQueryVerification
    @Query("SELECT rocky_next_id('item_categories', :list)")
    suspend fun nextLinkId(list: Long): Long

    /** Inserts the item after the last one, in [categories], in one transaction. */
    @Transaction suspend fun add(list: Long, key: String, title: String, categories: Set<Long> = emptySet()): ShoppingItem {
        val item = ShoppingItem(list, key, title, completed = false, id = nextId(list), position = Positions.between(lastPosition(list), null))
        insert(item)
        for (category in categories) insertLink(ItemCategory(list, item.id, category, nextLinkId(list)))
        return item
    }

    @Query("UPDATE items SET title = :title WHERE list = :list AND id = :id")
    suspend fun rename(list: Long, id: Long, title: String): Int

    @Query("UPDATE items SET completed = :completed WHERE list = :list AND id = :id")
    suspend fun check(list: Long, id: Long, completed: Boolean): Int

    @Query("UPDATE items SET position = :position WHERE list = :list AND id = :id")
    suspend fun place(list: Long, id: Long, position: String): Int

    /** Writes [ids]' keys in this order, in one transaction. */
    @Transaction suspend fun placeAll(list: Long, ids: List<Long>) {
        for ((id, position) in ids.zip(Positions.spread(ids.size))) place(list, id, position)
    }

    @Query("DELETE FROM items WHERE list = :list AND id = :id")
    suspend fun delete(list: Long, id: Long): Int
}
