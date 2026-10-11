package org.example.examples.shopping.data.db.entity

import androidx.room.*

/**
 * An item of [list], keyed by the [key] the writer chooses. A title is unique per list, the later write winning;
 * deleting the list hides the item, and deleting its [category] nulls only that reference.
 */
@Entity(
    tableName = "items",
    primaryKeys = ["list", "key"],
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
