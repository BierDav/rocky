package org.example.examples.market

import androidx.room.*
import kotlinx.coroutines.flow.Flow

/** Something offered, keyed by the item [key] its stock row uses. Mergeable; Rocky issues [id]. */
@Entity(tableName = "listings", indices = [Index(value = ["id"], unique = true, name = "listings_identity")])
data class Listing(@PrimaryKey val key: String, val title: String, val id: Long, @ColumnInfo(name = "row_kind") val rowKind: Long = 0)

/** A buyer's note on a [listing]. Mergeable, so it is written and synced while strong work waits. */
@Entity(tableName = "memos", indices = [Index(value = ["id"], unique = true, name = "memos_identity")])
data class Memo(@PrimaryKey val key: String, val listing: String, val note: String, val id: Long, @ColumnInfo(name = "row_kind") val rowKind: Long = 0)

/** The confirmed stock of item [key]. Command-controlled: only a confirmed [Reserve] or [Restock] changes it. */
@Entity(tableName = "stock", indices = [Index(value = ["id"], unique = true, name = "stock_identity")])
data class Stock(@PrimaryKey val key: String, val remaining: Long, val id: Long, @ColumnInfo(name = "row_kind") val rowKind: Long = 0)

/** A confirmed hold of [quantity] of [item]. Command-controlled, like [Stock]. */
@Entity(tableName = "holds", indices = [Index(value = ["id"], unique = true, name = "holds_identity")])
data class Hold(@PrimaryKey val key: String, val item: String, val quantity: Long, val id: Long, @ColumnInfo(name = "row_kind") val rowKind: Long = 0)

/** A listing with its confirmed stock, which is null before the first [Restock] is applied here. */
data class Offer(val key: String, val title: String, val remaining: Long?)

@Dao
interface ListingDao {
    @Query("SELECT listings.`key` AS `key`, listings.title AS title, stock.remaining AS remaining FROM listings LEFT JOIN stock ON stock.`key` = listings.`key` ORDER BY listings.title, listings.`key`")
    fun offers(): Flow<List<Offer>>

    @Insert suspend fun insert(listing: Listing)

    @SkipQueryVerification
    @Query("SELECT rocky_next_id('listings')")
    suspend fun nextId(): Long

    @Transaction suspend fun add(key: String, title: String) = insert(Listing(key, title, nextId()))
}

@Dao
interface MemoDao {
    @Query("SELECT * FROM memos ORDER BY `key`")
    fun observe(): Flow<List<Memo>>

    @Insert suspend fun insert(memo: Memo)

    @SkipQueryVerification
    @Query("SELECT rocky_next_id('memos')")
    suspend fun nextId(): Long

    @Transaction suspend fun add(key: String, listing: String, note: String) = insert(Memo(key, listing, note, nextId()))
}

/**
 * Reads the command-controlled tables. Rocky refuses every write to them with `SQLITE_AUTH`; [overwrite] is such a
 * write, kept so qualification shows the refusal through a DAO. See `docs/runtime/confirmed-tables.md`.
 */
@Dao
interface StockDao {
    @Query("SELECT * FROM stock ORDER BY `key`")
    fun observe(): Flow<List<Stock>>

    @Query("SELECT * FROM holds ORDER BY `key`")
    fun holds(): Flow<List<Hold>>

    @Upsert suspend fun overwrite(stock: Stock)
}

/** The current Room schema; no earlier version is read. */
@Database(entities = [Listing::class, Memo::class, Stock::class, Hold::class], version = 1, exportSchema = true)
@ConstructedBy(MarketDatabaseConstructor::class)
abstract class MarketDatabase : RoomDatabase() {
    abstract fun listings(): ListingDao
    abstract fun memos(): MemoDao
    abstract fun stock(): StockDao
}

@Suppress("KotlinNoActualForExpect")
expect object MarketDatabaseConstructor : RoomDatabaseConstructor<MarketDatabase> {
    override fun initialize(): MarketDatabase
}
