package org.example.examples.shopping.data.db.dao

import androidx.room.*
import kotlinx.coroutines.flow.Flow
import org.example.examples.shopping.data.db.entity.Category

@Dao
interface CategoryDao {
    @Query("SELECT * FROM categories WHERE list = :list ORDER BY name, id")
    fun observe(list: Long): Flow<List<Category>>

    @Insert suspend fun insert(category: Category)

    @SkipQueryVerification
    @Query("SELECT rocky_next_id('categories', :list)")
    suspend fun nextId(list: Long): Long

    @Transaction suspend fun add(list: Long, name: String): Category = Category(list, nextId(list), name).also { insert(it) }

    @Query("DELETE FROM categories WHERE list = :list AND id = :id")
    suspend fun delete(list: Long, id: Long): Int
}
