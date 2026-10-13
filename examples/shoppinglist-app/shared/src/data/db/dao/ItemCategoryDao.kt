package org.example.examples.shopping.data.db.dao

import androidx.room.*
import kotlinx.coroutines.flow.Flow
import org.example.examples.shopping.data.db.entity.ItemCategory

@Dao
interface ItemCategoryDao {
    @Query("SELECT * FROM item_categories WHERE list = :list ORDER BY item, category")
    fun observe(list: Long): Flow<List<ItemCategory>>

    @Insert suspend fun insert(link: ItemCategory)

    @SkipQueryVerification
    @Query("SELECT rocky_next_id('item_categories', :list)")
    suspend fun nextId(list: Long): Long

    @Query("SELECT count(*) FROM item_categories WHERE list = :list AND item = :item AND category = :category")
    suspend fun count(list: Long, item: Long, category: Long): Int

    /** Links the item once; linking it again does nothing. */
    @Transaction suspend fun link(list: Long, item: Long, category: Long) {
        if (count(list, item, category) == 0) insert(ItemCategory(list, item, category, nextId(list)))
    }

    @Query("DELETE FROM item_categories WHERE list = :list AND item = :item AND category = :category")
    suspend fun unlink(list: Long, item: Long, category: Long): Int

    @Query("DELETE FROM item_categories WHERE list = :list AND item = :item")
    suspend fun unlinkAll(list: Long, item: Long): Int
}
