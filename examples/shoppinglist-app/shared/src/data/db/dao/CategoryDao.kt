package org.example.examples.shopping.data.db.dao

import androidx.room.*
import kotlinx.coroutines.flow.Flow
import org.example.examples.shopping.data.Positions
import org.example.examples.shopping.data.db.entity.Category

@Dao
interface CategoryDao {
    @Query("SELECT * FROM categories WHERE list = :list ORDER BY position, name, id")
    fun observe(list: Long): Flow<List<Category>>

    @Insert suspend fun insert(category: Category)

    @SkipQueryVerification
    @Query("SELECT rocky_next_id('categories', :list)")
    suspend fun nextId(list: Long): Long

    @Query("SELECT max(position) FROM categories WHERE list = :list")
    suspend fun lastPosition(list: Long): String?

    /** Adds the category after the last one. */
    @Transaction suspend fun add(list: Long, name: String): Category = Category(list, nextId(list), name, position = Positions.between(lastPosition(list), null)).also { insert(it) }

    @Query("DELETE FROM categories WHERE list = :list AND id = :id")
    suspend fun delete(list: Long, id: Long): Int
}
