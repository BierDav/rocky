package rocky.room

import androidx.room.RoomDatabase
import androidx.room.immediateTransaction
import androidx.room.useWriterConnection
import rocky.RockyDatabase
import rocky.RockyException

/**
 * Reserves Room's writer, prepares a partition key outside SQL, then reserves its id and runs [insert] in one transaction.
 * A competing creator retries preparation before [insert] runs. The callback runs once and uses ordinary DAOs.
 */
suspend fun <T> RoomDatabase.createRockyPartition(database: RockyDatabase, table: String, insert: suspend (Long) -> T): T = useWriterConnection { writer ->
    check(!writer.inTransaction()) { "Partition creation must start outside a Room transaction" }
    repeat(16) {
        val prepared = try {
            database.partitions.nextId(table)
        } catch (error: RockyException) {
            if (error.code != "conflict" || !error.retryable) throw error
            return@repeat
        }
        try {
            return@useWriterConnection writer.immediateTransaction {
                val id = usePrepared("SELECT rocky_next_id(?)") { statement ->
                    statement.bindText(1, table)
                    check(statement.step()) { "Rocky did not reserve a partition id" }
                    statement.getLong(0)
                }
                if (id != prepared) throw PartitionRace()
                insert(id)
            }
        } catch (_: PartitionRace) {
            // No application code ran and the reservation was rolled back.
        }
    }
    throw rocky.RockyException("conflict", true, "Partition creation repeatedly competed with another writer")
}

private class PartitionRace : Exception()
