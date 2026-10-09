package org.example.examples.shopping

import androidx.room.*
import kotlinx.coroutines.flow.Flow

/** A list is its own partition of the `list` group; Rocky issues [id]. [rowKind] is 0 for application rows. */
@Entity(tableName = "lists")
data class ShoppingList(
    @PrimaryKey val id: Long,
    val title: String,
    @ColumnInfo(name = "row_kind") val rowKind: Long = 0,
)

/** A category of one list; its identity is the list and the [id] Rocky issues in that list. */
@Entity(
    tableName = "categories", primaryKeys = ["list", "id"],
    foreignKeys = [ForeignKey(entity = ShoppingList::class, parentColumns = ["id"], childColumns = ["list"], onDelete = ForeignKey.CASCADE, deferred = true)],
)
data class Category(
    val list: Long,
    val id: Long,
    val name: String,
    @ColumnInfo(name = "row_kind") val rowKind: Long = 0,
)

/**
 * An item of [list], keyed by the [key] the writer chooses. A title is unique per list, the later write winning;
 * deleting the list hides the item, and deleting its [category] nulls only that reference.
 */
@Entity(
    tableName = "items", primaryKeys = ["list", "key"],
    indices = [
        Index(value = ["list", "id"], unique = true, name = "items_identity"),
        Index(value = ["list", "title"], unique = true),
        Index(value = ["list", "category"]),
    ],
    foreignKeys = [
        ForeignKey(entity = ShoppingList::class, parentColumns = ["id"], childColumns = ["list"], onDelete = ForeignKey.CASCADE, deferred = true),
        ForeignKey(entity = Category::class, parentColumns = ["list", "id"], childColumns = ["list", "category"], onDelete = ForeignKey.SET_NULL, deferred = true),
    ],
)
data class ShoppingItem(
    val list: Long,
    val key: String,
    val title: String,
    val completed: Boolean,
    val category: Long? = null,
    val id: Long,
    @ColumnInfo(name = "row_kind") val rowKind: Long = 0,
)

/** What a trusted device recorded about an enrolled device: its [name] and [kind], see [NodeKind]. */
@Entity(tableName = "devices", indices = [Index(value = ["id"], unique = true, name = "devices_identity")])
data class DeviceRow(
    @PrimaryKey val node: String,
    val name: String,
    val kind: String,
    val id: Long,
    @ColumnInfo(name = "row_kind") val rowKind: Long = 0,
)

/** A list's secret at [epoch], sealed to [recipient]: only that device can open [envelope]. */
@Entity(tableName = "keys", primaryKeys = ["recipient", "list"], indices = [Index(value = ["id"], unique = true, name = "keys_identity")])
class ListKey(
    val recipient: String,
    val list: Long,
    val epoch: Long,
    val envelope: ByteArray,
    val id: Long,
    @ColumnInfo(name = "row_kind") val rowKind: Long = 0,
)

@Dao
interface ListDao {
    /** The held lists: `row_kind = 0` asks for a partial result, since a device need not hold every list. */
    @Query("SELECT * FROM lists WHERE row_kind = 0 ORDER BY title, id") fun observe(): Flow<List<ShoppingList>>
    @Insert suspend fun insert(list: ShoppingList)
    /** Verifies the id `partitions.nextId("lists")` prepared, inside the inserting transaction. */
    @SkipQueryVerification @Query("SELECT rocky_next_id('lists')") suspend fun nextId(): Long
    @Transaction suspend fun create(prepared: Long, title: String): ShoppingList {
        val id = nextId()
        check(id == prepared) { "list id $prepared was taken; prepare again" }
        return ShoppingList(id, title).also { insert(it) }
    }
    @Query("UPDATE lists SET title = :title WHERE id = :id") suspend fun rename(id: Long, title: String): Int
    @Query("DELETE FROM lists WHERE id = :id") suspend fun delete(id: Long): Int
}

@Dao
interface CategoryDao {
    @Query("SELECT * FROM categories WHERE list = :list ORDER BY name, id") fun observe(list: Long): Flow<List<Category>>
    @Insert suspend fun insert(category: Category)
    @SkipQueryVerification @Query("SELECT rocky_next_id('categories', :list)") suspend fun nextId(list: Long): Long
    @Transaction suspend fun add(list: Long, name: String): Category = Category(list, nextId(list), name).also { insert(it) }
    @Query("DELETE FROM categories WHERE list = :list AND id = :id") suspend fun delete(list: Long, id: Long): Int
}

@Dao
interface ShoppingDao {
    @Query("SELECT * FROM items WHERE list = :list AND (:includeCompleted OR completed = 0) AND instr(lower(title), :search) > 0 ORDER BY completed, title, id")
    fun observe(list: Long, search: String = "", includeCompleted: Boolean = true): Flow<List<ShoppingItem>>
    @Insert suspend fun insert(item: ShoppingItem)
    /** Rocky reserves the next id of [list] inside the caller's write transaction; a rollback releases it. */
    @SkipQueryVerification @Query("SELECT rocky_next_id('items', :list)") suspend fun nextId(list: Long): Long
    /** Obtains the id and inserts the item in one transaction. */
    @Transaction suspend fun add(list: Long, key: String, title: String, category: Long? = null): ShoppingItem =
        ShoppingItem(list, key, title, completed = false, category = category, id = nextId(list)).also { insert(it) }
    @Query("UPDATE items SET title = :title WHERE list = :list AND id = :id") suspend fun rename(list: Long, id: Long, title: String): Int
    @Query("UPDATE items SET completed = :completed WHERE list = :list AND id = :id") suspend fun check(list: Long, id: Long, completed: Boolean): Int
    @Query("UPDATE items SET category = :category WHERE list = :list AND id = :id") suspend fun categorize(list: Long, id: Long, category: Long?): Int
    @Query("DELETE FROM items WHERE list = :list AND id = :id") suspend fun delete(list: Long, id: Long): Int
}

@Dao
interface DeviceDao {
    @Query("SELECT * FROM devices ORDER BY name, node") fun observe(): Flow<List<DeviceRow>>
    @Insert suspend fun insert(device: DeviceRow)
    @SkipQueryVerification @Query("SELECT rocky_next_id('devices')") suspend fun nextId(): Long
    @Query("UPDATE devices SET name = :name, kind = :kind WHERE node = :node") suspend fun update(node: String, name: String, kind: String): Int
    @Transaction suspend fun record(node: String, name: String, kind: String) {
        if (update(node, name, kind) == 0) insert(DeviceRow(node, name, kind, nextId()))
    }
}

@Dao
interface KeyDao {
    @Query("SELECT * FROM keys") fun observe(): Flow<List<ListKey>>
    @Insert suspend fun insert(key: ListKey)
    @SkipQueryVerification @Query("SELECT rocky_next_id('keys')") suspend fun nextId(): Long
    @Query("UPDATE keys SET epoch = :epoch, envelope = :envelope WHERE recipient = :recipient AND list = :list")
    suspend fun update(recipient: String, list: Long, epoch: Long, envelope: ByteArray): Int
    @Transaction suspend fun deliver(recipient: String, list: Long, epoch: Long, envelope: ByteArray) {
        if (update(recipient, list, epoch, envelope) == 0) insert(ListKey(recipient, list, epoch, envelope, nextId()))
    }
}

/** Room on Rocky's file: the version is the replicated schema's, [ShoppingSchema], and Room never migrates it. */
@Database(entities = [ShoppingList::class, Category::class, ShoppingItem::class, DeviceRow::class, ListKey::class], version = 4, exportSchema = true)
@ConstructedBy(ShoppingDatabaseConstructor::class)
abstract class ShoppingDatabase : RoomDatabase() {
    abstract fun lists(): ListDao
    abstract fun categories(): CategoryDao
    abstract fun items(): ShoppingDao
    abstract fun devices(): DeviceDao
    abstract fun keys(): KeyDao
}

@Suppress("KotlinNoActualForExpect")
expect object ShoppingDatabaseConstructor : RoomDatabaseConstructor<ShoppingDatabase> {
    override fun initialize(): ShoppingDatabase
}
