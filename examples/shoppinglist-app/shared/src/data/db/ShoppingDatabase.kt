package org.example.examples.shopping.data.db

import androidx.room.*
import org.example.examples.shopping.data.db.dao.ApprovalDao
import org.example.examples.shopping.data.db.dao.CategoryDao
import org.example.examples.shopping.data.db.dao.ItemCategoryDao
import org.example.examples.shopping.data.db.dao.ListDao
import org.example.examples.shopping.data.db.dao.ShoppingDao
import org.example.examples.shopping.data.db.entity.Category
import org.example.examples.shopping.data.db.entity.DecisionRow
import org.example.examples.shopping.data.db.entity.ItemCategory
import org.example.examples.shopping.data.db.entity.RequestRow
import org.example.examples.shopping.data.db.entity.ShoppingItem
import org.example.examples.shopping.data.db.entity.ShoppingList

/**
 * Room on Rocky's file: the version is the replicated schema's, [ShoppingSchema], and Room never migrates it. The
 * trusted-only tables are not Room's: members have no grant on them; see [TrustedRows].
 */
@Database(
    entities = [
        ShoppingList::class, Category::class, ShoppingItem::class, ItemCategory::class,
        RequestRow::class, DecisionRow::class,
    ],
    version = 8,
    exportSchema = true,
)
@ConstructedBy(ShoppingDatabaseConstructor::class)
abstract class ShoppingDatabase : RoomDatabase() {
    abstract fun lists(): ListDao
    abstract fun categories(): CategoryDao
    abstract fun items(): ShoppingDao
    abstract fun itemCategories(): ItemCategoryDao
    abstract fun approvals(): ApprovalDao
}

@Suppress("KotlinNoActualForExpect")
expect object ShoppingDatabaseConstructor : RoomDatabaseConstructor<ShoppingDatabase> {
    override fun initialize(): ShoppingDatabase
}
