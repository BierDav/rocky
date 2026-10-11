package org.example.examples.shopping.data.db.dao

import androidx.room.*
import kotlinx.coroutines.flow.Flow
import org.example.examples.shopping.data.db.entity.DecisionRow
import org.example.examples.shopping.data.db.entity.RequestRow

/** Confirmed rows: these DAOs only read, since SQL refuses every write to a command-controlled table. */
@Dao
interface ApprovalDao {
    @Query("SELECT * FROM requests ORDER BY id")
    fun requests(): Flow<List<RequestRow>>

    @Query("SELECT * FROM decisions")
    fun decisions(): Flow<List<DecisionRow>>
}
