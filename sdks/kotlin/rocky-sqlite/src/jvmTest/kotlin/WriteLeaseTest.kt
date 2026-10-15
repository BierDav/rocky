package rocky.sqlite

import androidx.sqlite.SQLiteConnection
import androidx.sqlite.SQLiteDriver
import androidx.sqlite.SQLiteException
import androidx.sqlite.execSQL
import kotlinx.coroutines.runBlocking
import rocky.RockyDatabase
import rocky.schema
import java.io.File
import java.nio.file.Files
import kotlin.test.*

class WriteLeaseTest {
    private val shopping = schema("shopping", version = 1) {
        table("lists") {
            text("key")
            text("title")
            primaryKey("key")
        }
        table("items") {
            text("key")
            text("list")
            text("title")
            integer("completed")
            blob("note", nullable = true)
            primaryKey("key")
            index("items_by_list", "list")
        }
    }

    private fun <T> withDatabase(block: suspend (RockyDatabase, SQLiteDriver) -> T): T = runBlocking {
        val directory: File = Files.createTempDirectory("rocky-sqlite").toFile()
        val database = RockyDatabase.local(directory, shopping)
        try {
            block(database, RockySQLiteDriver(database))
        } finally {
            database.close()
            directory.deleteRecursively()
        }
    }
    private fun SQLiteConnection.count(table: String): Long = prepare("SELECT count(*) FROM $table").use {
        it.step()
        it.getLong(0)
    }
    private fun SQLiteConnection.reader(): SQLiteConnection = also { it.execSQL("PRAGMA query_only = 1") }

    /** Reserves the next row id of [table] in this connection's write transaction. */
    private fun SQLiteConnection.nextId(table: String): Long = prepare("SELECT rocky_next_id(?)").use {
        it.bindText(1, table)
        assertTrue(it.step())
        it.getLong(0)
    }

    /** Runs [sql] with the first of consecutive ids reserved for it bound to its only parameter. */
    private fun SQLiteConnection.insertFrom(table: String, sql: String) = prepare(sql).use {
        it.bindLong(1, nextId(table))
        assertFalse(it.step())
    }
    private fun code(error: SQLiteException): Int = Regex("Error code: (\\d+)").find(error.message.orEmpty())!!.groupValues[1].toInt()

    @Test fun theWriteLeaseEndsTheTransactionAsAnAbort() = withDatabase { database, _ ->
        val writer = RockySQLiteDriver(database, transactionTimeoutMillis = 200).open("shopping")
        writer.execSQL("BEGIN IMMEDIATE TRANSACTION")
        writer.insertFrom("lists", "INSERT INTO lists(key, title, id) VALUES('held', 'x', ?)")
        // The lease ends on its own: wait for it, bounded.
        val deadline = System.nanoTime() + 5_000_000_000
        var aborted: SQLiteException? = null
        while (aborted == null && System.nanoTime() < deadline) {
            aborted = try {
                writer.count("lists")
                Thread.sleep(20)
                null
            } catch (error: SQLiteException) {
                error
            }
        }
        assertEquals(4, code(assertNotNull(aborted)))
        assertTrue(writer.inTransaction())
        writer.execSQL("ROLLBACK TRANSACTION")
        assertEquals(0, writer.count("lists"))
        writer.close()
    }
}
