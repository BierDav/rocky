package org.example.examples.shopping.data.db.dao

import androidx.room.*
import kotlinx.coroutines.flow.Flow
import org.example.examples.shopping.data.db.entity.DecisionRow
import org.example.examples.shopping.data.db.entity.RequestRow

/** Reads ordinary request rows and committed coordinated decisions. */
@Dao
interface ApprovalDao {
    @Query("SELECT * FROM requests ORDER BY id")
    fun requests(): Flow<List<RequestRow>>

    @Query("SELECT * FROM decisions")
    fun decisions(): Flow<List<DecisionRow>>
}
