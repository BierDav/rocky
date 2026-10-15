package rocky.room.qualification

import androidx.room.Room
import androidx.room.immediateTransaction
import androidx.room.useWriterConnection
import androidx.sqlite.SQLITE_DATA_BLOB
import androidx.sqlite.SQLITE_DATA_FLOAT
import androidx.sqlite.SQLITE_DATA_INTEGER
import androidx.sqlite.SQLITE_DATA_NULL
import androidx.sqlite.SQLiteDriver
import androidx.sqlite.SQLiteException
import androidx.sqlite.SQLiteStatement
import androidx.sqlite.driver.bundled.BundledSQLiteDriver
import kotlinx.coroutines.*
import rocky.RockyDatabase
import rocky.schemaFromRoom
import rocky.sqlite.RockySQLiteDriver
import rocky.testing.testEnvironment
import rocky.testing.testRepositoryRoot
import kotlin.test.*
import rocky.testing.TestFile as File
import rocky.testing.TestFiles as Files

/**
 * The acceptance reference for the pinned Room 2.8.4 / androidx.sqlite 2.6.2: Room's own statements, recorded
 * over the bundled driver, pinned as a fixture and replayed through Rocky's driver, and the same DAOs on Rocky.
 * Set ROCKY_RECORD_ROOM=1 to rewrite the fixture after an intended Room upgrade.
 */
class RoomDriverTest {
    private val root = File(testRepositoryRoot())
    private val module = File(root, "sdks/kotlin/qualification/room")
    private val exported get() = File(module, "schemas/rocky.room.qualification.ShopDatabase/1.json").readText()
    private val fixture = File(module, "fixtures/room-2.8.4.statements")

    /** Room keeps a lock file beside the name, whatever the driver. */
    private val names = Files.createTempDirectory("room-names").toFile()

    @AfterTest fun removeRoomNames() {
        names.deleteRecursively()
    }
    private fun room(driver: SQLiteDriver): ShopDatabase = Room.databaseBuilder<ShopDatabase>(File(names, "shop").path)
        .setDriver(driver).setQueryCoroutineContext(Dispatchers.IO).build()
    private fun item(key: String, title: String, id: Long, list: String = "l1") = ShopItem(key, list, title, price = 1.25, photo = byteArrayOf(1), id = id)

    /**
     * Every write kind and read shape of the DAO, with what each returned. The ids are those Rocky issues, so the
     * bundled reference runs the same statements: an insert takes the next id, a replacement is a new row, and an
     * update or upsert of an existing row keeps its id.
     */
    private suspend fun scenario(database: ShopDatabase): List<String> {
        val dao = database.shop()
        val ids = Ids()
        val log = mutableListOf<String>()
        val (i1, i2) = ids.item() to ids.item()
        dao.addList(ShopList("l1", "Groceries", ids.list()), listOf(item("i1", "Milk", i1), item("i2", "Eggs", i2), item("i3", "Jam", ids.item())))
        log += "ignored ${dao.insertIgnore(item("i1", "Duplicate", i1))}"
        dao.insertReplace(item("i3", "Marmalade", ids.item()))
        log += "updated ${dao.update(item("i1", "Oat milk", i1).copy(quantity = 2))}"
        dao.upsert(item("i2", "Eggs", i2).copy(completed = true))
        val i4 = ids.item()
        dao.upsert(item("i4", "Bread", i4))
        log += "completed ${dao.complete("l1", true)}"
        log += "relation ${dao.withItems("l1")}"
        log += "summary ${dao.summary()}"
        log += "deleted ${dao.delete(item("i4", "Bread", i4))}"
        log += "cleared ${dao.clearCompleted("l1")}"
        // A nested transaction rolls back to its savepoint, with the id it used; the outer one commits.
        database.useWriterConnection { writer ->
            writer.immediateTransaction {
                dao.insertList(ShopList("l2", "Hardware", ids.list()))
                val next = ids.item()
                runCatching {
                    withNestedTransaction {
                        dao.insertItems(listOf(item("i5", "Nails", next, "l2")))
                        error("undo")
                    }
                }
                dao.insertItems(listOf(item("i6", "Screws", next, "l2")))
            }
        }
        log += "l2 ${dao.items("l2")}"
        log += "missing ${dao.withItems("none")}"
        return log
    }

    private data class Run(val log: List<String>, val reopened: String, val recorder: RecordingDriver)

    /** The scenario, then a second Room instance on the same data. */
    private fun run(driver: () -> SQLiteDriver): Run = runBlocking {
        val recorder = RecordingDriver(driver())
        val first = room(recorder)
        val log = try {
            scenario(first)
        } finally {
            first.close()
        }
        val second = room(recorder)
        val reopened = try {
            second.shop().withItems("l2").toString()
        } finally {
            second.close()
        }
        Run(log, reopened, recorder)
    }
    private fun bundled(): Run {
        val directory = Files.createTempDirectory("room-bundled").toFile()
        try {
            val file = File(directory, "shop.sqlite").absolutePath
            return run {
                object : SQLiteDriver by BundledSQLiteDriver() {
                    override fun open(fileName: String) = BundledSQLiteDriver().open(file)
                }
            }
        } finally {
            directory.deleteRecursively()
        }
    }
    private fun <T> rocky(block: suspend (RockyDatabase) -> T): T = runBlocking {
        val directory = Files.createTempDirectory("room-rocky").toFile()
        val database = RockyDatabase.local(directory.path, schemaFromRoom("shop", exported))
        try {
            block(database)
        } finally {
            database.close()
            directory.deleteRecursively()
        }
    }

    @Test fun roomSendsTheRecordedStatements() {
        val shapes = bundled().recorder.shapes()
        if (testEnvironment("ROCKY_RECORD_ROOM") == "1") fixture.writeText(shapes.joinToString("\n", postfix = "\n"))
        assertTrue(fixture.isFile, "record the fixture with ROCKY_RECORD_ROOM=1")
        assertEquals(fixture.readLines().filter { it.isNotBlank() }, shapes, "Room's statements changed; requalify before upgrading Room")
    }

    @Test fun theRecordedTraceRunsThroughTheRockyDriver() {
        val recorded = bundled().recorder
        val expected = replay(recorded) { file ->
            object : SQLiteDriver by BundledSQLiteDriver() {
                override fun open(fileName: String) = BundledSQLiteDriver().open(file)
            }
        }
        val answered = rocky { database -> replay(recorded) { RockySQLiteDriver(database, "room-2.8.4") } }
        for ((index, step) in recorded.trace.withIndex()) {
            val (rocky, engine) = answered[index] to expected[index]
            if (housekeeping(step.sql)) {
                // Answered, never denied; values differ where Rocky already installed the schema.
                assertNull(rocky.failure, "housekeeping ${step.sql} failed with ${rocky.failure}")
            } else {
                assertEquals(engine, rocky, "statement $index: ${step.sql} ${step.bindings}")
            }
        }
        assertTrue(recorded.trace.count { housekeeping(it.sql) } > 20)
    }

    @Test fun roomDaosRunOnRockysFile() {
        val reference = bundled()
        val live = rocky { database -> run { RockySQLiteDriver(database, "room-2.8.4") } }
        assertEquals(reference.log, live.log)
        assertEquals(reference.reopened, live.reopened)
        val failures = live.recorder.trace.filter { it.failure != null }
        // Only Room's upsert probes a duplicate key, and fails as SQLite does; nothing is denied.
        assertTrue(failures.isNotEmpty() && failures.all { it.sql.startsWith("INSERT INTO `items`") }, "$failures")
        assertEquals(reference.recorder.trace.filter { it.failure != null }, failures)
        assertTrue(live.recorder.trace.filter { housekeeping(it.sql) }.all { it.failure == null })
    }

    private data class Outcome(val rows: List<List<Any?>>, val failure: Int?)

    /** Runs the trace's steps in order, each on the connection it ran on. */
    private fun replay(recorded: RecordingDriver, driver: (String) -> SQLiteDriver): List<Outcome> {
        val directory = Files.createTempDirectory("room-replay").toFile()
        try {
            val target = driver(File(directory, "replay.sqlite").absolutePath)
            val connections = mutableMapOf<Int, androidx.sqlite.SQLiteConnection>()
            return recorded.trace.map { step ->
                val connection = connections.getOrPut(step.connection) { target.open("replay") }
                try {
                    connection.prepare(step.sql).use { statement ->
                        step.bindings.forEachIndexed { index, value -> bind(statement, index + 1, value) }
                        val rows = mutableListOf<List<Any?>>()
                        while (statement.step()) rows += (0 until statement.getColumnCount()).map { value(statement, it) }
                        Outcome(rows, null)
                    }
                } catch (error: SQLiteException) {
                    Outcome(emptyList(), RecordingDriver.code(error))
                }
            }.also { connections.values.forEach { it.close() } }
        } finally {
            directory.deleteRecursively()
        }
    }
    private fun bind(statement: SQLiteStatement, index: Int, value: Any?) = when (value) {
        null -> statement.bindNull(index)
        is Long -> statement.bindLong(index, value)
        is Double -> statement.bindDouble(index, value)
        is String -> statement.bindText(index, value)
        is List<*> -> statement.bindBlob(index, ByteArray(value.size) { value[it] as Byte })
        else -> error("binding $value")
    }
    private fun value(statement: SQLiteStatement, index: Int): Any? = when (statement.getColumnType(index)) {
        SQLITE_DATA_NULL -> null
        SQLITE_DATA_INTEGER -> statement.getLong(index)
        SQLITE_DATA_FLOAT -> statement.getDouble(index)
        SQLITE_DATA_BLOB -> statement.getBlob(index).toList()
        else -> statement.getText(index)
    }
    private fun housekeeping(sql: String): Boolean {
        val upper = sql.trim().uppercase()
        return listOf("PRAGMA", "CREATE", "DROP", "BEGIN", "END", "COMMIT", "ROLLBACK", "SAVEPOINT", "RELEASE").any(upper::startsWith) ||
            listOf("sqlite_master", "room_master_table", "room_table_modification_log").any { it in sql }
    }
}
