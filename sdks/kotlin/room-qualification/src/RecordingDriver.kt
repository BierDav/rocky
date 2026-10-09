package rocky.room.qualification

import androidx.sqlite.SQLiteConnection
import androidx.sqlite.SQLiteDriver
import androidx.sqlite.SQLiteException
import androidx.sqlite.SQLiteStatement
import java.util.Collections
import java.util.concurrent.atomic.AtomicInteger

/**
 * Test tooling, not a write path: records what an ORM sends a driver. Each execution is one step with the
 * connection it ran on, its SQL, its bindings and the SQLite code it failed with.
 */
class RecordingDriver(private val delegate: SQLiteDriver) : SQLiteDriver {
    data class Step(val connection: Int, val sql: String, val bindings: List<Any?>, val failure: Int?)
    val trace: MutableList<Step> = Collections.synchronizedList(mutableListOf())
    private val connections = AtomicInteger()
    override val hasConnectionPool: Boolean get() = delegate.hasConnectionPool
    override fun open(fileName: String): SQLiteConnection = Connection(connections.incrementAndGet(), delegate.open(fileName))

    /**
     * The distinct statements of the trace, as a fixture spells them: on a reader, after `PRAGMA query_only = 1`
     * marked its connection, or on a writer.
     */
    fun shapes(): List<String> {
        val readers = mutableSetOf<Int>()
        return trace.toList().map { step ->
            val kind = if (step.connection in readers) "reader " else "writer "
            if (step.sql.replace(" ", "").equals("PRAGMAquery_only=1", ignoreCase = true)) readers += step.connection
            kind + step.sql.trim()
        }.distinct().sorted()
    }

    private inner class Connection(private val id: Int, private val connection: SQLiteConnection) : SQLiteConnection {
        override fun prepare(sql: String): SQLiteStatement = Statement(id, sql, connection.prepare(sql))
        override fun inTransaction(): Boolean = connection.inTransaction()
        override fun close() = connection.close()
    }

    private inner class Statement(private val connection: Int, private val sql: String, private val statement: SQLiteStatement) : SQLiteStatement by statement {
        private val bindings = sortedMapOf<Int, Any?>()
        private var running = false
        override fun bindBlob(index: Int, value: ByteArray) {
            bindings[index] = value.toList()
            statement.bindBlob(index, value)
        }
        override fun bindDouble(index: Int, value: Double) {
            bindings[index] = value
            statement.bindDouble(index, value)
        }
        override fun bindLong(index: Int, value: Long) {
            bindings[index] = value
            statement.bindLong(index, value)
        }
        override fun bindText(index: Int, value: String) {
            bindings[index] = value
            statement.bindText(index, value)
        }
        override fun bindNull(index: Int) {
            bindings[index] = null
            statement.bindNull(index)
        }
        override fun clearBindings() {
            bindings.clear()
            statement.clearBindings()
        }
        override fun reset() {
            running = false
            statement.reset()
        }
        override fun step(): Boolean {
            val first = !running
            running = true
            val values = (1..(bindings.keys.maxOrNull() ?: 0)).map { bindings[it] }
            return try {
                statement.step().also { if (first) trace.add(Step(connection, sql, values, null)) }
            } catch (error: SQLiteException) {
                if (first) trace.add(Step(connection, sql, values, code(error)))
                throw error
            }
        }
    }

    companion object {
        fun code(error: SQLiteException): Int = Regex("Error code: (\\d+)").find(error.message.orEmpty())?.groupValues?.get(1)?.toInt() ?: -1
    }
}
