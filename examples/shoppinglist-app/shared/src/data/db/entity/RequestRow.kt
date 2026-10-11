package org.example.examples.shopping.data.db.entity

import androidx.room.*

/** A confirmed request; only [AskApproval] writes it. [requester] is the asking device's command node. */
@Entity(tableName = "requests", indices = [Index(value = ["id"], unique = true, name = "requests_identity")])
data class RequestRow(
    @PrimaryKey val key: String,
    val title: String,
    val note: String,
    val requester: String,
    val id: Long,
    @ColumnInfo(name = "row_kind") val rowKind: Long = 0,
)
