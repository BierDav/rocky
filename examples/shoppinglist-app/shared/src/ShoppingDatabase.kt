package org.example.examples.shopping

import androidx.room.*
import kotlinx.coroutines.flow.Flow

/** Rocky issues [id]; [rowKind] is 0 for application rows. */
@Entity(tableName = "shopping_items", indices = [Index(value = ["completed", "title"])])
data class ShoppingItem(
    @PrimaryKey val id: Long,
    val title: String,
    val completed: Boolean,
    @ColumnInfo(defaultValue = "''") val searchTitle: String = title.lowercase(),
    @ColumnInfo(name = "row_kind") val rowKind: Long = 0,
)

@Dao
interface ShoppingDao {
    @Query("SELECT * FROM shopping_items WHERE (:includeCompleted OR completed = 0) AND instr(searchTitle, :search) > 0 ORDER BY completed, title, id")
    fun observe(search: String = "", includeCompleted: Boolean = true): Flow<List<ShoppingItem>>
    @Query("SELECT * FROM shopping_items ORDER BY id") suspend fun all(): List<ShoppingItem>
    @Insert suspend fun insert(item: ShoppingItem)
    /** Rocky reserves the next id inside the caller's write transaction; a rollback releases it. */
    @SkipQueryVerification @Query("SELECT rocky_next_id('shopping_items')") suspend fun nextId(): Long
    /** Obtains the id and inserts the item in one transaction. */
    @Transaction suspend fun add(title: String): ShoppingItem = ShoppingItem(nextId(), title, completed = false).also { insert(it) }
    @Query("UPDATE shopping_items SET title = :title, searchTitle = :searchTitle WHERE id = :id")
    suspend fun rename(id: Long, title: String, searchTitle: String): Int
    @Query("UPDATE shopping_items SET completed = :completed WHERE id = :id") suspend fun check(id: Long, completed: Boolean): Int
    @Query("DELETE FROM shopping_items WHERE id = :id") suspend fun delete(id: Long): Int
}

/** Room on Rocky's file: the version is the replicated schema's, [ShoppingSchema], and Room never migrates it. */
@Database(entities = [ShoppingItem::class], version = 2, exportSchema = true)
@ConstructedBy(ShoppingDatabaseConstructor::class)
abstract class ShoppingDatabase : RoomDatabase() {
    abstract fun items(): ShoppingDao
}

@Suppress("KotlinNoActualForExpect")
expect object ShoppingDatabaseConstructor : RoomDatabaseConstructor<ShoppingDatabase> {
    override fun initialize(): ShoppingDatabase
}
