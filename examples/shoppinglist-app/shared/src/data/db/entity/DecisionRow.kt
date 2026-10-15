package org.example.examples.shopping.data.db.entity

import androidx.room.*

/** The first coordinated decision on request [key]. */
@Entity(tableName = "decisions", indices = [Index(value = ["id"], unique = true, name = "decisions_identity")])
data class DecisionRow(
    @PrimaryKey val key: String,
    val approved: Boolean,
    val decider: String,
    val id: Long,
    @ColumnInfo(name = "row_kind") val rowKind: Long = 0,
)
