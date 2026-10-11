package rocky.room.qualification

import androidx.room.*

@Entity(
    tableName = "move_items",
    primaryKeys = ["list", "key"],
    indices = [Index(value = ["list", "id"], unique = true, name = "move_items_identity")],
)
data class MoveItem(
    val list: String,
    val key: String,
    val title: String,
    val id: Long,
    @ColumnInfo(name = "row_kind") val rowKind: Long = 0,
)

@Dao interface MoveDao {
    @Query("SELECT * FROM move_items WHERE list=:list AND row_kind=0 ORDER BY key")
    suspend fun items(list: String): List<MoveItem>
}

@Database(entities = [MoveItem::class], version = 1, exportSchema = true)
@ConstructedBy(MoveDatabaseConstructor::class)
abstract class MoveDatabase : RoomDatabase() {
    abstract fun moves(): MoveDao
}
expect object MoveDatabaseConstructor : RoomDatabaseConstructor<MoveDatabase> {
    override fun initialize(): MoveDatabase
}
