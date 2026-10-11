package org.example.examples.shopping.data.db.entity

import androidx.room.*

/** A list is its own partition of the `list` group; Rocky issues [id]. [rowKind] is 0 for application rows. */
@Entity(tableName = "lists")
data class ShoppingList(
    @PrimaryKey val id: Long,
    val title: String,
    @ColumnInfo(name = "row_kind") val rowKind: Long = 0,
)
