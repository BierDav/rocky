package rocky.room.qualification

import androidx.room.PooledConnection
import androidx.room.Room
import androidx.room.deferredTransaction
import androidx.room.immediateTransaction
import androidx.room.useReaderConnection
import androidx.room.useWriterConnection
import androidx.sqlite.SQLiteException
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import rocky.*
import rocky.room.ROOM_PROFILE
import rocky.room.setRockyDatabase
import rocky.sqlite.RockySQLiteDriver
import java.io.File
import java.net.ServerSocket
import java.nio.file.Files
import java.util.concurrent.TimeUnit
import kotlin.test.*

/** What one Room transaction publishes, and what peers, readers and ordinary SQLite see of it. */
class RoomGateTest {
    private val root = generateSequence(File(System.getProperty("user.dir"))) { it.parentFile }.first { File(it, "project.yaml").isFile }
    private val schema = schemaFromRoom(
        "shop",
        File(root, "sdks/kotlin/room-qualification/schemas/rocky.room.qualification.ShopDatabase/1.json").readText(),
    )
    private fun tcp(): PeerTransport = PeerTransport.Tcp(ServerSocket(0).use { "127.0.0.1:${it.localPort}" })
    private fun item(key: String, title: String, list: String = "l1") = ShopItem(key, list, title)
    private fun room(directory: File, database: RockyDatabase): ShopDatabase = Room.databaseBuilder<ShopDatabase>(File(directory, "shop").path).setRockyDatabase(database).build()

    /** Inspect the live file in another process so SQLite's lock bookkeeping is shared correctly. */
    private fun <T> file(directory: File, block: (FileReader) -> T): T = block(FileReader(File(directory, "rocky.sqlite")))
    private class FileReader(private val path: File) {
        fun rows(sql: String): List<List<String?>> {
            val process = ProcessBuilder("sqlite3", "-readonly", "-json", "-cmd", ".explain off", path.path, sql).start()
            check(
                process.waitFor(10, TimeUnit.SECONDS).also { finished ->
                    if (!finished) process.destroyForcibly().waitFor()
                },
            ) { "SQLite inspection timed out" }
            val output = process.inputStream.bufferedReader().readText()
            val error = process.errorStream.bufferedReader().readText()
            check(process.exitValue() == 0) { error }
            if (output.isBlank()) return emptyList()
            return Json.parseToJsonElement(output).jsonArray.map { row ->
                row.jsonObject.values.map { it.jsonPrimitive.contentOrNull }
            }
        }
    }

    /** Bundles this device authored, as the Rust driver test counts them. */
    private fun bundles(directory: File): Long = file(directory) { it.rows("SELECT count(DISTINCT handle) FROM secure_bundle_handles").single().single()!!.toLong() }

    private class Devices(val directory: File, val a: RockyDatabase, val b: RockyDatabase, val roomA: ShopDatabase, val roomB: ShopDatabase)
    private fun <T> pair(block: suspend Devices.() -> T): T = runBlocking {
        val directory = Files.createTempDirectory("room-gate").toFile()
        val a = RockyDatabase.create(File(directory, "a"), schema, tcp(), automaticSync = false)
        try {
            a.installAuthors()
            val b = RockyDatabase.join(
                File(directory, "b"),
                schema,
                a.pairing(GrantProfile.Writer(setOf("lists", "items"))).uri,
                tcp(),
                automaticSync = false,
            )
            try {
                b.installAuthors(issuer = a)
                val roomA = room(File(directory, "a"), a)
                val roomB = room(File(directory, "b"), b)
                try {
                    Devices(directory, a, b, roomA, roomB).block()
                } finally {
                    roomA.close()
                    roomB.close()
                }
            } finally {
                b.close()
            }
        } finally {
            a.close()
            directory.deleteRecursively()
        }
    }

    @Test fun aRoomTransactionIsOneBundleOnPeersAndAnOversizedOneFailsWhole() = pair {
        val before = bundles(File(directory, "a"))
        roomA.shop().createList(ShopList("l1", "Groceries"), listOf(item("i1", "Milk"), item("i2", "Eggs"), item("i3", "Jam")))
        assertEquals(before + 1, bundles(File(directory, "a")))
        // Registered before the sync, so the bundle's commit on the peer is among the events.
        val events = b.driverTableEvents()
        assertEquals(ConnectionState.Connected, b.syncOnce().state)
        val applied = withTimeout(10_000) {
            var change = events.next()
            while ("items" !in change.tables) change = events.next()
            change
        }
        assertEquals(setOf("items", "lists"), applied.tables.toSet(), "one commit applied the parent and its children")
        assertEquals(3, roomB.shop().withItems("l1")?.items?.size)

        // The debug native build steps 2,049 single-row inserts slower than the 5 s default lease, which is not
        // what this checks: a longer lease, one reservation and the sequences that follow it.
        val bulk = Room.databaseBuilder<ShopDatabase>(File(directory, "bulk").path)
            .setDriver(RockySQLiteDriver(a, ROOM_PROFILE, transactionTimeoutMillis = 60_000)).setQueryCoroutineContext(Dispatchers.IO).build()
        val oversized = try {
            assertFailsWith<SQLiteException> {
                bulk.useWriterConnection { writer ->
                    writer.immediateTransaction {
                        val first = bulk.shop().nextId("items")
                        bulk.shop().insertItems(List(2_049) { item("big$it", "Bulk $it").copy(id = first + it) })
                    }
                }
            }
        } finally {
            bulk.close()
        }
        assertContains(oversized.message.orEmpty(), "Error code: 18")
        assertEquals(before + 1, bundles(File(directory, "a")), "nothing published, nothing split")
        assertEquals(3, roomA.shop().items("l1").size)
        b.syncOnce()
        assertEquals(3, roomB.shop().items("l1").size)
    }

    @Test fun aDaoObtainsEachIdBeforeItsInsertInOneTransaction() = pair {
        // Counted from table events, not the file: a second SQLite in this process must not hold it during sync.
        val events = a.driverTableEvents()
        val created = roomA.shop().createList(ShopList("l1", "Groceries"), listOf(item("i1", "Milk"), item("i2", "Eggs")))!!
        assertEquals(setOf("items", "lists"), withTimeout(10_000) { events.next() }.tables.toSet(), "reservations and inserts commit once")
        // An id is the table author's next sequence: the first list, then consecutive items.
        assertEquals(1L, created.list.id and 0xFFFF_FFFFL)
        val ids = created.items.sortedBy { it.key }.map { it.id }
        assertEquals(listOf(1L, 2L), ids.map { it and 0xFFFF_FFFFL })
        assertNotEquals(created.list.id ushr 32, ids.first() ushr 32, "each table has its own author")
        assertEquals(ConnectionState.Connected, b.syncOnce().state)
        assertEquals(created, roomB.shop().withItems("l1"), "peers see the issued ids")
        // Another device issues under its own author.
        val fromB = roomB.shop().createItems(listOf(item("i3", "Jam"))).single()
        assertNotEquals(ids.first() ushr 32, fromB.id ushr 32)
        assertEquals(1L, fromB.id and 0xFFFF_FFFFL)
        // A rolled-back transaction releases its reservation.
        var released = 0L
        assertFailsWith<IllegalStateException> {
            roomA.useWriterConnection { writer ->
                writer.immediateTransaction {
                    released = roomA.shop().nextId("items")
                    error("undo")
                }
            }
        }
        assertEquals(3L, released and 0xFFFF_FFFFL)
        assertEquals(released, roomA.shop().createItems(listOf(item("i4", "Bread"))).single().id)
        // Outside a write transaction there is nothing to reserve an id for.
        val outside = assertFailsWith<SQLiteException> { roomA.shop().nextId("items") }
        assertContains(outside.message.orEmpty(), "rocky_next_id needs a write transaction")
        // An entity whose id Rocky did not issue is refused, and nothing is published.
        assertFailsWith<SQLiteException> { roomA.shop().insertItems(listOf(item("i5", "Unissued"))) }
        assertFailsWith<SQLiteException> { roomA.shop().insertItems(listOf(item("i5", "Skipped").copy(id = released + 2))) }
        assertEquals(ConnectionState.Connected, b.syncOnce().state)
        assertEquals(listOf("i1", "i2", "i3", "i4"), roomB.shop().items("l1").map { it.key }.sorted())
        assertEquals(roomB.shop().items("l1"), roomA.shop().items("l1"))
    }

    @Test fun aRowInsertedAndDeletedInOneTransactionPublishesNothing() = pair {
        roomA.shop().createList(ShopList("l1", "Groceries"), emptyList())
        val before = bundles(File(directory, "a"))
        roomA.useWriterConnection { writer ->
            writer.immediateTransaction {
                val inserted = roomA.shop().createItems(listOf(item("i1", "Milk"))).single()
                roomA.shop().delete(inserted)
            }
        }
        assertEquals(before, bundles(File(directory, "a")))
        assertTrue(roomA.shop().items("l1").isEmpty())
    }

    @Test fun aWholeEntityUpdatePublishesOnlyChangedColumns() = pair {
        roomA.shop().createList(ShopList("l1", "Groceries"), listOf(item("i1", "Milk")))
        b.syncOnce()
        val stale = roomA.shop().items("l1").single()
        // Both devices write the whole entity, each changing one column, before they sync.
        roomA.shop().update(stale.copy(title = "Oat milk"))
        roomB.shop().update(roomB.shop().items("l1").single().copy(completed = true, quantity = 3))
        assertEquals(ConnectionState.Connected, b.syncOnce().state)
        val expected = stale.copy(title = "Oat milk", completed = true, quantity = 3)
        assertEquals(expected, roomA.shop().items("l1").single())
        assertEquals(expected, roomB.shop().items("l1").single())
    }

    @Test fun readersNeverBlockTheWriter() = pair {
        roomA.shop().createList(ShopList("l1", "Groceries"), listOf(item("i1", "Milk")))
        suspend fun count(connection: PooledConnection) = connection.usePrepared("SELECT count(*) FROM items") {
            it.step()
            it.getLong(0)
        }
        val seen = roomA.useReaderConnection { reader ->
            reader.deferredTransaction {
                val first = count(reader)
                // The writer commits while this read transaction holds its snapshot, well inside the 5 s lease.
                // Outside this coroutine's context, which Room would bind to the reader connection.
                CoroutineScope(Dispatchers.IO).async { withTimeout(2_000) { roomA.shop().createItems(listOf(item("i2", "Eggs"))) } }.await()
                listOf(first, count(reader))
            }
        }
        assertEquals(listOf(1L, 1L), seen)
        assertEquals(2, roomA.shop().items("l1").size)
    }

    @Test fun theFileOpensWithOrdinarySqliteAndShowsRealKeysAndIndexes() = pair {
        roomA.shop().createList(ShopList("l1", "Groceries"), listOf(item("i1", "Milk"), item("i2", "Eggs")))
        file(File(directory, "a")) { sqlite ->
            assertContains(sqlite.rows("SELECT sql FROM sqlite_master WHERE name = 'items'").single().single()!!, "PRIMARY KEY")
            // name and pk of each column
            assertEquals(listOf("key", "1"), sqlite.rows("PRAGMA table_info(items)").first().let { listOf(it[1], it[5]) })
            assertEquals(listOf("items_by_list", "items_identity"), sqlite.rows("PRAGMA index_list(items)").map { it[1] }.filter { it!!.startsWith("items_") }.sortedBy { it })
            assertEquals(listOf("id"), sqlite.rows("PRAGMA index_info(items_identity)").map { it[2] })
            assertEquals(listOf("list", "title"), sqlite.rows("PRAGMA index_info(items_by_list)").map { it[2] })
            assertTrue(sqlite.rows("EXPLAIN QUERY PLAN SELECT title FROM items WHERE list = 'l1'").any { "items_by_list" in it.last().orEmpty() })
            assertEquals(listOf("Eggs", "Milk"), sqlite.rows("SELECT title FROM items WHERE list = 'l1' ORDER BY title").map { it.single() })
        }
    }

    @Test fun kotlinHorizonsFinalizeThroughTheNativeBridgeAndRoomKeepsConvergedRows() = pair {
        roomA.shop().createList(ShopList("l1", "Groceries"), listOf(item("i1", "Milk"), item("i2", "Eggs")))
        roomA.shop().delete(roomA.shop().items("l1").first { it.key == "i2" })
        assertEquals(ConnectionState.Connected, b.syncOnce().state)
        val declared = a.horizons.declare("items", setOf(b.session().node))
        assertFalse(declared.getValue("finalized").jsonPrimitive.boolean)
        b.horizons.admit("items", declared.getValue("statement").jsonPrimitive.content)
        assertEquals(ConnectionState.Connected, b.syncOnce().state)
        assertTrue(b.horizons.status("items").getValue("finalized").jsonPrimitive.boolean)
        assertTrue(a.horizons.finalize("items").getValue("finalized").jsonPrimitive.boolean)
        assertEquals(a.horizons.status("items").getValue("statement"), b.horizons.status("items").getValue("statement"))
        assertEquals(listOf("Milk"), roomA.shop().items("l1").map { it.title })
        assertEquals(roomA.shop().items("l1"), roomB.shop().items("l1"))
        val state = "SELECT hex(scope),hex(digest),rows,tombstones FROM secure_scope_state ORDER BY scope"
        val memberState = file(File(directory, "b")) { it.rows(state) }
        val issuerState = file(File(directory, "a")) { it.rows(state) }
        assertEquals(2, memberState.size, "the member holds the two application tables")
        assertEquals(3, issuerState.size, "the issuer also holds its private registry")
        val sharedScopes = memberState.map { it.first() }.toSet()
        assertEquals(memberState, issuerState.filter { it.first() in sharedScopes })
        assertEquals(listOf(listOf("0")), file(File(directory, "a")) { it.rows("SELECT sum(tombstones) FROM secure_scope_state") })
        assertTrue(a.horizons.maintain("items", 1).getValue("finalized").jsonPrimitive.boolean)
    }
}
