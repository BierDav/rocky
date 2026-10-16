package rocky.sqlite

import androidx.sqlite.SQLiteConnection
import androidx.sqlite.SQLiteDriver
import androidx.sqlite.SQLiteStatement
import androidx.sqlite.throwSQLiteException
import kotlinx.coroutines.runBlocking
import rocky.RockyBinding
import rocky.RockyDatabase
import rocky.internal.native.NativeConnection
import rocky.internal.native.NativeException

/**
 * An `androidx.sqlite` driver on Rocky's own file. Every connection runs on [database]: a writer is a secure SQL
 * session, so one transaction is one commit and one signed bundle; after `PRAGMA query_only = 1`, as ORMs mark
 * their readers, a connection steps snapshot cursors and never holds the writer. The file
 * name a caller opens names nothing; the data is the database's. [profile] names an ORM version whose housekeeping
 * statements the connections answer, as `rocky-room` passes `room-2.8.4`. With [binding] every connection reads a
 * kept seal's state through views shaped like the live tables, and writes fail with SQLite's read-only code 8.
 * See `docs/runtime/sql-driver.md`.
 */
class RockySQLiteDriver(
    private val database: RockyDatabase,
    private val profile: String? = null,
    private val transactionTimeoutMillis: Long? = null,
    private val binding: RockyBinding? = null,
) : SQLiteDriver {
    /** Callers pool connections themselves; one connection is never used concurrently. */
    override val hasConnectionPool: Boolean get() = false
    override fun open(fileName: String): SQLiteConnection = RockySQLiteConnection(this)

    internal fun connect(readOnly: Boolean): NativeConnection = native {
        database.driverConnection(readOnly, profile, transactionTimeoutMillis, binding)
    }
}

/** Runs one native call for a blocking driver method, raising statement failures as SQLite does. */
internal fun <T> native(call: suspend () -> T): T = try {
    runBlocking { call() }
} catch (error: NativeException.Sql) {
    throwSQLiteException(error.code, error.detail)
} catch (error: NativeException.Failure) {
    throwSQLiteException(1, error.detail)
} catch (error: rocky.RockyException) {
    throwSQLiteException(1, error.message ?: error.code)
}

internal class RockySQLiteConnection(private val driver: RockySQLiteDriver) : SQLiteConnection {
    private var connection: NativeConnection? = null
    private var closed = false

    internal fun native(): NativeConnection {
        if (closed) throwSQLiteException(21, "connection is closed")
        return connection ?: driver.connect(readOnly = false).also { connection = it }
    }
    override fun prepare(sql: String): SQLiteStatement = RockySQLiteStatement(this, sql)
    override fun inTransaction(): Boolean = connection?.let { native { it.inTransaction() } } ?: false
    override fun close() {
        if (closed) return
        closed = true
        connection?.let {
            try {
                native { it.shutdown() }
            } finally {
                it.destroy()
            }
        }
        connection = null
    }
}
