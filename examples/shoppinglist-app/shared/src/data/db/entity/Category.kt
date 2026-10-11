package org.example.examples.shopping.data.db.entity

import androidx.room.*

/** A category of one list; its identity is the list and the [id] Rocky issues in that list. */
@Entity(
    tableName = "categories",
    primaryKeys = ["list", "id"],
    foreignKeys = [ForeignKey(entity = ShoppingList::class, parentColumns = ["id"], childColumns = ["list"], onDelete = ForeignKey.CASCADE, deferred = true)],
)
data class Category(
    val list: Long,
    val id: Long,
    val name: String,
    @ColumnInfo(name = "row_kind") val rowKind: Long = 0,
)
