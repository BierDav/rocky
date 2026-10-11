package org.example.examples.shopping.data.db.dao

import androidx.room.*
import kotlinx.coroutines.flow.Flow
import org.example.examples.shopping.data.db.entity.ShoppingList

@Dao
interface ListDao {
    /** The held lists: `row_kind = 0` asks for a partial result, since a device need not hold every list. */
    @Query("SELECT * FROM lists WHERE row_kind = 0 ORDER BY id")
    fun observeAll(): Flow<List<ShoppingList>>

    @Insert suspend fun insert(list: ShoppingList)

    /** Verifies the id `partitions.nextId("lists")` prepared, inside the inserting transaction. */
    @SkipQueryVerification
    @Query("SELECT rocky_next_id('lists')")
    suspend fun nextId(): Long

    @Transaction suspend fun create(prepared: Long, title: String): ShoppingList {
        val id = nextId()
        check(id == prepared) { "list id $prepared was taken; prepare again" }
        return ShoppingList(id, title).also { insert(it) }
    }

    @Query("UPDATE lists SET title = :title WHERE id = :id")
    suspend fun rename(id: Long, title: String): Int

    @Query("DELETE FROM lists WHERE id = :id")
    suspend fun delete(id: Long): Int
}
