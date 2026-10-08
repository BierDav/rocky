package rocky.sqlite

import androidx.sqlite.SQLiteConnection
import androidx.sqlite.SQLiteDriver
import androidx.sqlite.SQLiteException
import androidx.sqlite.driver.bundled.BundledSQLiteDriver
import androidx.sqlite.execSQL
import java.io.File
import java.nio.file.Files
import kotlin.test.*
import kotlinx.coroutines.runBlocking
import rocky.RockyDatabase
import rocky.schema

class RockySQLiteDriverTest {
    private val shopping = schema("shopping", version = 1) {
        table("lists") { text("key"); text("title"); primaryKey("key") }
        table("items") {
            text("key"); text("list"); text("title"); integer("completed"); blob("note", nullable = true)
            primaryKey("key")
            index("items_by_list", "list")
        }
    }

    private fun <T> withDatabase(block: suspend (RockyDatabase, SQLiteDriver) -> T): T = runBlocking {
        val directory: File = Files.createTempDirectory("rocky-sqlite").toFile()
        val database = RockyDatabase.local(directory, shopping)
        try {
            // The creator issues itself the table authors its inserts are signed under.
            for (table in listOf("lists", "items"))
                database.credentials.installPseudonym(database.credentials.assignPseudonym(database.credentials.pseudonymRequest(table)))
            block(database, RockySQLiteDriver(database))
        } finally { database.close(); directory.deleteRecursively() }
    }
    private fun SQLiteConnection.count(table: String): Long = prepare("SELECT count(*) FROM $table").use { it.step(); it.getLong(0) }
    private fun SQLiteConnection.reader(): SQLiteConnection = also { it.execSQL("PRAGMA query_only = 1") }
    /** Reserves the next row id of [table] in this connection's write transaction. */
    private fun SQLiteConnection.nextId(table: String): Long =
        prepare("SELECT rocky_next_id(?)").use { it.bindText(1, table); assertTrue(it.step()); it.getLong(0) }
    /** Runs [sql] with the first of consecutive ids reserved for it bound to its only parameter. */
    private fun SQLiteConnection.insertFrom(table: String, sql: String) =
        prepare(sql).use { it.bindLong(1, nextId(table)); assertFalse(it.step()) }
    private fun code(error: SQLiteException): Int = Regex("Error code: (\\d+)").find(error.message.orEmpty())!!.groupValues[1].toInt()

    @Test fun aTransactionWithSavepointsCommitsOnceAndReadersKeepTheirSnapshot() = withDatabase { _, driver ->
        val writer = driver.open("shopping")
        val reader = driver.open("shopping").reader()
        writer.execSQL("BEGIN IMMEDIATE TRANSACTION")
        assertTrue(writer.inTransaction())
        writer.prepare("INSERT INTO lists(key, title, id) VALUES(?, ?, ?)").use { insert ->
            insert.bindText(1, "l1"); insert.bindText(2, "Groceries"); insert.bindLong(3, writer.nextId("lists")); assertFalse(insert.step())
        }
        val ids = mutableListOf<Long>()
        writer.prepare("INSERT INTO items(key, list, title, completed, note, id) VALUES(?, 'l1', ?, 0, ?, ?)").use { insert ->
            for ((key, title) in listOf("i1" to "Milk", "i2" to "Eggs")) {
                insert.bindText(1, key); insert.bindText(2, title); insert.bindBlob(3, byteArrayOf(7))
                insert.bindLong(4, writer.nextId("items").also { ids += it })
                insert.step(); insert.reset()
            }
        }
        assertEquals(ids.first() + 1, ids.last(), "reservations of one table continue its author's sequence")
        writer.execSQL("SAVEPOINT 'nested'")
        writer.execSQL("DELETE FROM items")
        writer.execSQL("ROLLBACK TRANSACTION TO SAVEPOINT 'nested'")
        writer.execSQL("RELEASE SAVEPOINT 'nested'")
        assertEquals(2, writer.count("items"))
        writer.execSQL("END TRANSACTION")
        assertFalse(writer.inTransaction())

        // A read transaction keeps its snapshot while the writer commits.
        reader.execSQL("BEGIN DEFERRED TRANSACTION")
        assertEquals(2, reader.count("items"))
        writer.execSQL("DELETE FROM items WHERE key = 'i2'")
        assertEquals(2, reader.count("items"))
        reader.execSQL("END TRANSACTION")
        assertEquals(1, reader.count("items"))
        reader.prepare("SELECT key, title, note FROM items").use { rows ->
            assertEquals(listOf("key", "title", "note"), rows.getColumnNames())
            assertTrue(rows.step())
            assertEquals("Milk", rows.getText(1)); assertContentEquals(byteArrayOf(7), rows.getBlob(2))
            assertFalse(rows.step())
        }
        val refused = assertFailsWith<SQLiteException> { reader.execSQL("DELETE FROM items") }
        assertEquals(8, code(refused))
        // An id is reserved only inside the write transaction that inserts it.
        assertEquals(8, code(assertFailsWith<SQLiteException> { reader.nextId("items") }))
        assertEquals(1, code(assertFailsWith<SQLiteException> { writer.nextId("items") }))
        writer.close(); reader.close()
    }

    @Test fun pagesStreamAndStatementsRerunAfterReset() = withDatabase { _, driver ->
        val writer = driver.open("shopping")
        writer.execSQL("BEGIN IMMEDIATE TRANSACTION")
        writer.insertFrom("lists", "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 700) " +
            "INSERT INTO lists(key, title, id) SELECT printf('l%04d', i), 'list ' || i, ? + i - 1 FROM n")
        writer.execSQL("END TRANSACTION")
        val reader = driver.open("shopping").reader()
        reader.prepare("SELECT key FROM lists WHERE title LIKE ? ORDER BY key").use { query ->
            query.bindText(1, "list%")
            var rows = 0
            while (query.step()) rows++
            assertEquals(700, rows)
            // A reset runs the statement again with the same bindings; an early reset drops the rest.
            query.reset()
            assertTrue(query.step()); assertEquals("l0001", query.getText(0))
            query.reset(); query.clearBindings(); query.bindText(1, "none")
            assertFalse(query.step())
        }
        writer.close(); reader.close()
    }

    @Test fun bundleLimitsAndDeniedStatementsAreSQLiteErrors() = withDatabase { _, driver ->
        val writer = driver.open("shopping")
        writer.execSQL("BEGIN IMMEDIATE TRANSACTION")
        writer.insertFrom("lists", "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2049) " +
            "INSERT INTO lists(key, title, id) SELECT 'l' || i, 'x', ? + i - 1 FROM n")
        val oversized = assertFailsWith<SQLiteException> { writer.execSQL("END TRANSACTION") }
        assertEquals(18, code(oversized))
        assertFalse(writer.inTransaction())
        assertEquals(0, writer.count("lists"))
        writer.execSQL("BEGIN IMMEDIATE TRANSACTION")
        val duplicate = assertFailsWith<SQLiteException> {
            writer.insertFrom("lists", "INSERT INTO lists(key, title, id) VALUES('a', 'x', ?)")
            writer.insertFrom("lists", "INSERT INTO lists(key, title, id) VALUES('a', 'y', ?)")
        }
        assertEquals(1555, code(duplicate))
        writer.execSQL("ROLLBACK TRANSACTION")
        // An id Rocky did not issue is refused.
        assertFailsWith<SQLiteException> { writer.execSQL("INSERT INTO lists(key, title, id) VALUES('b', 'x', 0)") }
        assertEquals(0, writer.count("lists"))
        for (sql in listOf("PRAGMA user_version", "CREATE TABLE extra(id TEXT)", "SELECT * FROM sqlite_master")) {
            assertEquals(23, code(assertFailsWith<SQLiteException>(sql) { writer.execSQL(sql) }), sql)
        }
        writer.close()
    }

    @Test fun theWriteLeaseEndsTheTransactionAsAnAbort() = withDatabase { database, _ ->
        val writer = RockySQLiteDriver(database, transactionTimeoutMillis = 200).open("shopping")
        writer.execSQL("BEGIN IMMEDIATE TRANSACTION")
        writer.insertFrom("lists", "INSERT INTO lists(key, title, id) VALUES('held', 'x', ?)")
        // The lease ends on its own: wait for it, bounded.
        val deadline = System.nanoTime() + 5_000_000_000
        var aborted: SQLiteException? = null
        while (aborted == null && System.nanoTime() < deadline) {
            aborted = try { writer.count("lists"); Thread.sleep(20); null } catch (error: SQLiteException) { error }
        }
        assertEquals(4, code(assertNotNull(aborted)))
        assertTrue(writer.inTransaction())
        writer.execSQL("ROLLBACK TRANSACTION")
        assertEquals(0, writer.count("lists"))
        writer.close()
    }

    @Test fun gettersConvertAsTheBundledEngine() = withDatabase { _, driver ->
        val rocky = driver.open("shopping")
        val bundled = BundledSQLiteDriver().open(":memory:")
        val values: List<(androidx.sqlite.SQLiteStatement) -> Unit> = listOf(
            { it.bindNull(1) }, { it.bindLong(1, 42) }, { it.bindLong(1, Long.MIN_VALUE) },
            { it.bindDouble(1, 0.1) }, { it.bindDouble(1, 1.0) }, { it.bindDouble(1, -2.5e-7) }, { it.bindDouble(1, 1e20) },
            { it.bindDouble(1, 123456789.123456789) }, { it.bindDouble(1, 9.3e18) },
            { it.bindText(1, "12abc") }, { it.bindText(1, " -7.5e2x") }, { it.bindText(1, "abc") }, { it.bindText(1, "") },
            { it.bindText(1, "99999999999999999999") }, { it.bindBlob(1, "3.5".encodeToByteArray()) }, { it.bindBlob(1, ByteArray(0)) },
        )
        for ((index, bind) in values.withIndex()) {
            // Each getter reads a fresh row: the engine caches conversions, so their order would matter.
            fun read(connection: SQLiteConnection) = listOf<(androidx.sqlite.SQLiteStatement) -> Any>(
                { it.getColumnType(0) }, { it.isNull(0) }, { it.getLong(0) }, { it.getDouble(0) }, { it.getText(0) },
                { it.getBlob(0).toList() },
            ).map { getter -> connection.prepare("SELECT ?").use { bind(it); assertTrue(it.step()); getter(it) } }
            assertEquals(read(bundled), read(rocky), "value $index")
        }
        // Rocky's scalar codec has one zero: a negative zero reads back as 0.0.
        rocky.prepare("SELECT ?").use { it.bindDouble(1, -0.0); assertTrue(it.step()); assertEquals(0.0.toBits(), it.getDouble(0).toBits()) }
        bundled.close(); rocky.close()
    }
}
